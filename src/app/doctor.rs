//! `doctor`: local health checks (credential presence, configuration validity,
//! state and output directories, base URL overrides), plus optional free
//! metadata calls (`--check-access`). Never prints credential values.
//!
//! Every check has a unique `id`. The command succeeds (exit 0) whenever the
//! diagnostics ran; problems are reported through `healthy: false` and `error`
//! checks, never through the exit code.

use std::path::Path;

use crate::config::{EnvSnapshot, Settings};
use crate::domain::{Operation, ProviderId, Warning};
use crate::error::IrisError;
use crate::output::results::{CheckStatus, DoctorCheck, DoctorResult};
use crate::providers::AccountAccess;

use super::context::AppContext;
use super::request;

/// Inputs of `doctor`.
#[derive(Debug, Clone, Default)]
pub struct DoctorArgs {
    /// Perform free metadata calls to check model access.
    pub check_access: bool,
    /// Whether `GOOGLE_API_KEY` is set (presence only), which Iris ignores.
    pub google_api_key_present: bool,
}

/// What `doctor` runs against: a loaded context, or the configuration error and
/// the raw environment when the settings could not be loaded.
pub enum DoctorTarget<'a> {
    Loaded(&'a AppContext),
    Invalid { error: &'a IrisError, env: &'a EnvSnapshot },
}

/// Run every check. `healthy` is false if any check has status `error`; the
/// command itself still succeeds, since the diagnostics ran.
pub async fn run(target: DoctorTarget<'_>, args: &DoctorArgs, warnings: &mut Vec<Warning>) -> DoctorResult {
    let mut checks = Vec::new();
    let settings: Option<&Settings> = match &target {
        DoctorTarget::Loaded(ctx) => Some(&ctx.settings),
        DoctorTarget::Invalid { .. } => None,
    };

    match (&target, settings) {
        (_, Some(s)) if s.config_file_exists => {
            checks.push(ok("config", format!("loaded the config file {}", s.config_file.value.display())))
        }
        (_, Some(s)) => checks.push(ok(
            "config",
            format!("no config file at {}; built-in defaults apply", s.config_file.value.display()),
        )),
        (DoctorTarget::Invalid { error, .. }, None) => {
            let hint = error.hint.as_deref().map(|h| format!(" ({h})")).unwrap_or_default();
            checks.push(check("config", CheckStatus::Error, format!("{}{hint}", error.message)));
        }
        _ => {}
    }

    for provider in ProviderId::ALL {
        let present = match &target {
            DoctorTarget::Loaded(ctx) => ctx.settings.credential_present(*provider),
            DoctorTarget::Invalid { env, .. } => env.credential(*provider).is_some(),
        };
        let var = provider.credential_env();
        let id = format!("credentials.{provider}");
        checks.push(if present {
            ok(&id, format!("{var} is set"))
        } else {
            check(
                &id,
                CheckStatus::Warning,
                format!("{var} is not set; {provider} commands will fail with missing_credentials"),
            )
        });
    }
    if args.google_api_key_present {
        checks.push(check(
            "credentials.google_api_key",
            CheckStatus::Warning,
            "GOOGLE_API_KEY is set but ignored; Iris reads the Gemini key only from GEMINI_API_KEY"
                .to_string(),
        ));
    }

    if let DoctorTarget::Loaded(ctx) = &target {
        let s = &ctx.settings;
        checks.push(dir_check("state_dir", "state directory", &s.state_dir.value));
        checks.push(dir_check("output_dir", "output directory", &s.output_dir.value));
        let config_warnings = s.warnings();
        for provider in ProviderId::ALL {
            let id = format!("base_url.{provider}");
            let base = &s.provider(*provider).base_url.value;
            match config_warnings.iter().find(|w| w.message.starts_with(&format!("providers.{provider}."))) {
                Some(w) => checks.push(check(&id, CheckStatus::Warning, w.message.clone())),
                None => checks.push(ok(&id, format!("{provider} API base URL is the default ({base})"))),
            }
        }
        warnings.extend(config_warnings);
        match ctx.store.list() {
            Ok(listing) if listing.warnings.is_empty() => {
                checks.push(ok("jobs", format!("{} local job record(s) readable", listing.records.len())));
            }
            Ok(listing) => {
                checks.push(check(
                    "jobs",
                    CheckStatus::Warning,
                    format!(
                        "{} local job record(s) readable, {} unreadable (see warnings)",
                        listing.records.len(),
                        listing.warnings.len()
                    ),
                ));
                warnings.extend(listing.warnings);
            }
            Err(e) => checks.push(check("jobs", CheckStatus::Error, e.message.clone())),
        }
        if args.check_access {
            access_checks(ctx, &mut checks).await;
        }
    } else if args.check_access {
        checks.push(check(
            "access",
            CheckStatus::Warning,
            "access was not checked because the configuration is invalid".to_string(),
        ));
    }

    let healthy = checks.iter().all(|c| c.status != CheckStatus::Error);
    DoctorResult { healthy, checks }
}

/// One check per default model (`access.<provider>.<model>`): the model a command
/// uses without `--model` ([`request::effective_default`], the same resolution as
/// `default_for` and the generation commands), each model checked once. One check
/// per provider (`access.<provider>`) when its models could not be checked, or when
/// a configured default is not in the catalog. A model the metadata read finds is
/// only *visible to the key*: billing tier, credit, and organization verification
/// are not part of that read.
async fn access_checks(ctx: &AppContext, checks: &mut Vec<DoctorCheck>) {
    for provider in ProviderId::ALL {
        let id = format!("access.{provider}");
        if !ctx.settings.credential_present(*provider) {
            checks.push(check(
                &id,
                CheckStatus::Warning,
                format!("not checked: {} is not set", provider.credential_env()),
            ));
            continue;
        }
        let mut models: Vec<&str> = Vec::new();
        let mut unknown: Vec<String> = Vec::new();
        for op in Operation::ALL {
            match request::effective_default(ctx, *provider, *op) {
                Ok(Some(m)) if !models.contains(&m.id) => models.push(m.id),
                Ok(_) => {}
                Err(e) if !unknown.contains(&e.message) => unknown.push(e.message.clone()),
                Err(_) => {}
            }
        }
        if !unknown.is_empty() {
            checks.push(check(&id, CheckStatus::Error, format!("not checked: {}", unknown.join("; "))));
        }
        if models.is_empty() {
            if unknown.is_empty() {
                checks.push(check(&id, CheckStatus::Warning, "not checked: no default model".to_string()));
            }
            continue;
        }
        let (adapter, pctx) =
            match ctx.provider(*provider).and_then(|a| Ok((a, ctx.provider_context(*provider)?))) {
                Ok(pair) => pair,
                Err(e) => {
                    checks.push(check(&id, CheckStatus::Error, e.message.clone()));
                    continue;
                }
            };
        for model in models {
            let id = format!("access.{provider}.{model}");
            ctx.interrupt.arm();
            let seen = ctx.interrupt.count();
            let result = tokio::select! {
                r = adapter.check_access(model, &pctx) => r,
                () = ctx.interrupt.after(seen) => {
                    checks.push(check(&id, CheckStatus::Warning, "interrupted".to_string()));
                    return;
                }
            };
            checks.push(match result {
                Ok(AccountAccess::Available) => ok(
                    &id,
                    format!(
                        "{model} is visible to this key (model metadata only; billing tier, prepaid credit, \
                         and organization verification are not checked)"
                    ),
                ),
                Ok(AccountAccess::Unavailable) => check(
                    &id,
                    CheckStatus::Warning,
                    format!(
                        "{model} is not visible to this key or its project (the metadata read found no model)"
                    ),
                ),
                Ok(_) => check(
                    &id,
                    CheckStatus::Warning,
                    format!("could not determine whether {model} is visible to this key"),
                ),
                Err(e) => check(&id, CheckStatus::Error, format!("{model}: {} ({})", e.message, e.code)),
            });
        }
    }
}

/// Writable if it exists; creatable if its nearest existing ancestor is writable.
fn dir_check(id: &str, label: &str, dir: &Path) -> DoctorCheck {
    let shown = dir.display();
    if dir.exists() && !dir.is_dir() {
        return check(id, CheckStatus::Error, format!("{label} {shown} exists but is not a directory"));
    }
    let exists = dir.is_dir();
    let Some(probe_dir) = dir.ancestors().find(|a| a.is_dir()) else {
        return check(id, CheckStatus::Error, format!("{label} {shown} cannot be created"));
    };
    match tempfile::Builder::new().prefix(".iris-doctor-").tempfile_in(probe_dir) {
        Ok(_) if exists => ok(id, format!("{label} {shown} is writable")),
        Ok(_) => ok(id, format!("{label} {shown} does not exist yet; it will be created on first use")),
        Err(e) if exists => check(id, CheckStatus::Error, format!("{label} {shown} is not writable: {e}")),
        Err(e) => check(
            id,
            CheckStatus::Error,
            format!("{label} {shown} cannot be created ({} is not writable: {e})", probe_dir.display()),
        ),
    }
}

fn ok(id: &str, message: String) -> DoctorCheck {
    check(id, CheckStatus::Ok, message)
}

fn check(id: &str, status: CheckStatus, message: String) -> DoctorCheck {
    DoctorCheck { id: id.to_string(), status, message }
}
