//! Configuration: config file, environment, precedence, platform paths (see docs/configuration.md).
//!
//! [`Settings::load`] resolves every non-secret setting from four layers —
//! command-line flags ([`CliOverrides`]) > environment variables > the TOML config
//! file > defaults — and records which layer supplied each value. The environment
//! is read through an [`EnvSnapshot`] so resolution is a pure function of its inputs.
//!
//! Rules:
//! * Config file: `--config` > `IRIS_CONFIG` > platform default. A missing default
//!   file is fine; an explicitly requested file that does not exist is
//!   `config_invalid`. Unknown keys, wrong types, and credential-like keys (at any
//!   depth) are `config_invalid` naming the file and the key.
//! * Every value present in any layer is validated, even if a higher layer
//!   overrides it: a bad environment value is `config_invalid` naming the variable;
//!   a bad flag value is `invalid_argument` naming the flag.
//! * `~` is expanded in paths from every layer. Relative paths from flags resolve
//!   against the current directory; paths in environment variables and in the
//!   config file must be absolute (or start with `~/`), so where jobs and outputs
//!   live never depends on the directory a command happens to run in.
//! * Credentials come only from `OPENAI_API_KEY` / `GEMINI_API_KEY` and are held as
//!   [`Secret`]s; nothing here ever formats their values.
//! * Per-provider settings (`[providers.<id>]`, `IRIS_<PROVIDER>_BASE_URL`) are
//!   resolved for every provider in [`ProviderId::ALL`]; each provider's id,
//!   variable names, and default base URL come from [`ProviderId`], so nothing here
//!   lists providers by name.
//!
//! Besides the foundation modules, `config` uses `catalog` (a configured model must
//! be a known model), `output::results` (the `config show` and `config path`
//! results), and `http` (the client settings and timeouts it resolves); see the
//! layering in docs/architecture.md.

#![warn(missing_docs)]

mod env;
mod file;
mod paths;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

pub use env::{
    ENV_CONFIG, ENV_LOG, ENV_OUTPUT_DIR, ENV_POLL_INTERVAL, ENV_STATE_DIR, ENV_STORE_PROMPTS,
    ENV_WAIT_TIMEOUT, EnvSnapshot, SETTING_VARS,
};
pub use paths::{Platform, PlatformPaths, expand_tilde, platform_paths};

use crate::catalog;
use crate::domain::{Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpSettings, Timeouts};
pub use crate::output::results::SettingSource;
use crate::output::results::{ConfigPathResult, ConfigShowResult, CredentialView, SettingView};
use crate::redact;
use crate::secret::Secret;

/// Config file key of the caller wait limit for video jobs (`wait_timeout` in the
/// `[video]` table), as `config show` names the setting.
pub const KEY_WAIT_TIMEOUT: &str = "video.wait_timeout";
/// Config file key of the poll interval for video jobs, like [`KEY_WAIT_TIMEOUT`].
pub const KEY_POLL_INTERVAL: &str = "video.poll_interval";

/// Default caller wait limit for video jobs.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
/// Default poll interval for video jobs.
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Smallest accepted poll interval.
pub const MIN_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Default per-request timeout for synchronous generation.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Default per-request timeout for an async job submission (Veo).
pub const DEFAULT_SUBMIT_TIMEOUT: Duration = Duration::from_secs(60);
/// Default tracing filter.
pub const DEFAULT_LOG_FILTER: &str = "warn";

/// A resolved value and the layer it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<T> {
    /// The effective value.
    pub value: T,
    /// The layer that supplied it (flag, env, file, or default).
    pub source: SettingSource,
}

/// Flag-level inputs from the command line (parsed by the CLI layer).
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    /// `--config <PATH>`.
    pub config_path: Option<PathBuf>,
    /// `-d, --out-dir <DIR>`.
    pub out_dir: Option<PathBuf>,
    /// `--timeout <DURATION>` (caller wait limit).
    pub wait_timeout: Option<Duration>,
    /// `--poll-interval <DURATION>`.
    pub poll_interval: Option<Duration>,
    /// Number of `-v` flags (0 = not given).
    pub verbose: u8,
}

/// Resolved settings of one provider.
#[derive(Debug, Clone)]
pub struct ProviderSettings {
    /// The provider these settings belong to.
    pub provider: ProviderId,
    /// API base URL. Credentials are sent only to this origin.
    pub base_url: Resolved<Url>,
    /// Timeout of one synchronous generation request (before the upload allowance,
    /// [`crate::http::upload_allowance`]).
    pub request_timeout: Resolved<Duration>,
    /// Timeout of one async job submission request (before the upload allowance).
    /// Only providers with video models accept a configured value.
    pub submit_timeout: Resolved<Duration>,
}

/// Every resolved non-secret setting, plus the credentials found in the environment.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Config file path (whether or not it exists).
    pub config_file: Resolved<PathBuf>,
    /// True if the config file existed and was loaded.
    pub config_file_exists: bool,
    /// Absolute default output directory. The one exception: when it is the
    /// default (the current directory) and the current directory is unknown
    /// ([`EnvSnapshot::cwd`] fails, e.g. it was deleted), it is `.`, which cannot be
    /// written to; a command that writes there fails before sending anything
    /// (the CLI checks up front).
    pub output_dir: Resolved<PathBuf>,
    /// Absolute state directory (jobs live in `<state_dir>/jobs`).
    pub state_dir: Resolved<PathBuf>,
    /// The model of `image generate` and `image edit` without `-m/--model`
    /// (`image.model`, config file only), as the id `-m` would send; `None` when the
    /// file names none. See [`Settings::model`].
    pub image_model: Resolved<Option<String>>,
    /// The model of `video generate` without `-m/--model` (`video.model`, config file
    /// only), like `image_model`.
    pub video_model: Resolved<Option<String>>,
    /// Caller wait limit for video jobs (`video.wait_timeout`).
    pub wait_timeout: Resolved<Duration>,
    /// Poll interval for video jobs (`video.poll_interval`, at least 2s).
    pub poll_interval: Resolved<Duration>,
    /// Keep prompt text in job records (`jobs.store_prompts`).
    pub store_prompts: Resolved<bool>,
    /// Tracing filter directives for the log subscriber.
    pub log_filter: Resolved<String>,
    /// Settings of every provider in [`ProviderId::ALL`] (`providers.<id>`); see
    /// [`Settings::provider`].
    providers: BTreeMap<ProviderId, ProviderSettings>,
    credentials: BTreeMap<ProviderId, Secret>,
}

impl Settings {
    /// Resolve all settings. See the module documentation for the rules.
    pub fn load(cli: &CliOverrides, env: &EnvSnapshot) -> Result<Settings, IrisError> {
        let defaults = default_paths(env);

        // Config file location and contents.
        let config_file = layered(
            cli.config_path.as_deref().map(|p| flag_path(p, "--config", env)).transpose()?,
            env_path(env, ENV_CONFIG)?,
            None,
            || defaults.as_ref().map(|d| d.config_file.clone()).map_err(Clone::clone),
        )?;
        let path = config_file.value.as_path();
        let loaded = file::load(path)?;
        if loaded.is_none() && config_file.source != SettingSource::Default {
            let origin = if config_file.source == SettingSource::Flag { "--config" } else { ENV_CONFIG };
            return Err(file::file_error(path, format!("does not exist (requested by {origin})")));
        }
        let config_file_exists = loaded.is_some();
        let cfg = loaded.unwrap_or_default();

        let output_dir = layered(
            cli.out_dir.as_deref().map(|p| flag_path(p, "--out-dir", env)).transpose()?,
            env_path(env, ENV_OUTPUT_DIR)?,
            cfg.output_dir.as_deref().map(|v| file_path(path, "output_dir", v, env)).transpose()?,
            // The current directory; `.` when it is unknown, so commands that do not
            // write outputs still work (see `Settings::output_dir`).
            || Ok(env.cwd().map(Path::to_path_buf).unwrap_or_else(|_| PathBuf::from("."))),
        )?;
        let state_dir = layered(
            None,
            env_path(env, ENV_STATE_DIR)?,
            cfg.state_dir.as_deref().map(|v| file_path(path, "state_dir", v, env)).transpose()?,
            || defaults.as_ref().map(|d| d.state_dir.clone()).map_err(Clone::clone),
        )?;

        let image_ops = [Operation::ImageGenerate, Operation::ImageEdit];
        let image_model = layered(
            None,
            None,
            cfg.image.model.as_deref().map(|v| file_model(path, v, &image_ops).map(Some)).transpose()?,
            || Ok(None),
        )?;
        let video_model = layered(
            None,
            None,
            cfg.video
                .model
                .as_deref()
                .map(|v| file_model(path, v, &[Operation::VideoGenerate]).map(Some))
                .transpose()?,
            || Ok(None),
        )?;

        let wait_timeout = layered(
            cli.wait_timeout.map(|d| positive(d).map_err(|m| flag_error("--timeout", m))).transpose()?,
            env_parsed(env, ENV_WAIT_TIMEOUT, |v| parse_duration(v).and_then(positive))?,
            cfg.video
                .wait_timeout
                .as_ref()
                .map(|v| file_duration(path, KEY_WAIT_TIMEOUT, v, positive))
                .transpose()?,
            || Ok(DEFAULT_WAIT_TIMEOUT),
        )?;
        let poll_interval = layered(
            cli.poll_interval
                .map(|d| min_poll(d).map_err(|m| flag_error("--poll-interval", m)))
                .transpose()?,
            env_parsed(env, ENV_POLL_INTERVAL, |v| parse_duration(v).and_then(min_poll))?,
            cfg.video
                .poll_interval
                .as_ref()
                .map(|v| file_duration(path, KEY_POLL_INTERVAL, v, min_poll))
                .transpose()?,
            || Ok(DEFAULT_POLL_INTERVAL),
        )?;
        let store_prompts =
            layered(None, env_parsed(env, ENV_STORE_PROMPTS, parse_bool)?, cfg.jobs.store_prompts, || {
                Ok(false)
            })?;

        let unset = file::ProviderSection::default();
        let providers = ProviderId::ALL
            .iter()
            .map(|&p| {
                let section = cfg.providers.get(p.as_str()).unwrap_or(&unset);
                provider_settings(p, section, env, path).map(|s| (p, s))
            })
            .collect::<Result<BTreeMap<_, _>, IrisError>>()?;

        let verbose = match cli.verbose {
            0 => None,
            1 => Some("warn,iris=debug".to_string()),
            _ => Some("warn,iris=trace".to_string()),
        };
        let log_filter = layered(verbose, env_parsed(env, ENV_LOG, parse_log_filter)?, None, || {
            Ok(DEFAULT_LOG_FILTER.to_string())
        })?;

        Ok(Settings {
            config_file,
            config_file_exists,
            output_dir,
            state_dir,
            image_model,
            video_model,
            wait_timeout,
            poll_interval,
            store_prompts,
            log_filter,
            providers,
            credentials: env.credentials().clone(),
        })
    }

    /// The configured model for `op`: `image.model` for the image operations,
    /// `video.model` for video ([`Operation::model_config_key`]).
    pub fn model(&self, op: Operation) -> &Resolved<Option<String>> {
        match op {
            Operation::ImageGenerate | Operation::ImageEdit => &self.image_model,
            Operation::VideoGenerate => &self.video_model,
        }
    }

    /// Settings of one provider.
    pub fn provider(&self, provider: ProviderId) -> &ProviderSettings {
        // `load` resolves every provider in `ProviderId::ALL`, and only `load` can
        // build a `Settings`. A unit test in `domain` checks that `ALL` lists every
        // `ProviderId` variant, so no provider can be missing here.
        &self.providers[&provider]
    }

    /// Mutable settings of one provider, for embedders and tests that adjust
    /// resolved settings (the other settings are public fields).
    pub fn provider_mut(&mut self, provider: ProviderId) -> &mut ProviderSettings {
        self.providers.get_mut(&provider).expect("Settings::load resolves every provider")
    }

    /// Settings of every provider, in [`ProviderId::ALL`] order.
    pub fn providers(&self) -> impl Iterator<Item = &ProviderSettings> {
        ProviderId::ALL.iter().map(|p| self.provider(*p))
    }

    /// `<state_dir>/jobs`.
    pub fn jobs_dir(&self) -> PathBuf {
        self.state_dir.value.join("jobs")
    }

    /// Timeouts for a provider: default timeouts with `request_timeout` as the
    /// synchronous generation timeout and `submit_timeout` as the job submission
    /// timeout.
    pub fn timeouts(&self, provider: ProviderId) -> Timeouts {
        let p = self.provider(provider);
        Timeouts { generate: p.request_timeout.value, submit: p.submit_timeout.value, ..Timeouts::default() }
    }

    /// Settings for [`crate::http::HttpClient::new`]. The connect timeout is a
    /// client-level setting shared by all providers; it is derived from
    /// [`Settings::timeouts`] (the longest provider connect timeout) so both agree.
    pub fn http_settings(&self) -> HttpSettings {
        let connect_timeout = ProviderId::ALL
            .iter()
            .map(|p| self.timeouts(*p).connect)
            .max()
            .unwrap_or(Timeouts::default().connect);
        HttpSettings { connect_timeout, ..HttpSettings::default() }
    }

    /// The credential for `provider`, if its environment variable was set.
    pub fn credential(&self, provider: ProviderId) -> Option<&Secret> {
        self.credentials.get(&provider)
    }

    /// Whether the credential for `provider` is present (never its value).
    pub fn credential_present(&self, provider: ProviderId) -> bool {
        self.credentials.contains_key(&provider)
    }

    /// The credential for `provider`, or `missing_credentials` (exit 3) naming the
    /// environment variable. Call after all other local validation.
    pub fn require_credential(&self, provider: ProviderId) -> Result<Secret, IrisError> {
        self.credential(provider).cloned().ok_or_else(|| {
            let var = provider.credential_env();
            IrisError::new(
                ErrorCode::MissingCredentials,
                format!("{var} is not set; Iris reads the {} key only from this variable", provider.display_name()),
            )
            .with_provider(provider)
            .with_detail("env_var", var)
            .with_hint(format!(
                "export {var}=<your key> in the environment that runs iris (never pass keys as arguments or put \
                 them in the config file)"
            ))
        })
    }

    /// Rows for `config show`: every setting with its source and environment
    /// variable, and credential presence. Values never include secrets.
    pub fn describe(&self) -> (Vec<SettingView>, Vec<CredentialView>) {
        let path = |p: &Path| serde_json::Value::String(p.display().to_string());
        let dur = |d: Duration| serde_json::Value::String(humantime::format_duration(d).to_string());
        let url = |u: &Url| serde_json::Value::String(redact::redact_url(u.as_str()));
        let opt =
            |v: &Option<String>| v.clone().map(serde_json::Value::String).unwrap_or(serde_json::Value::Null);

        let mut rows = vec![
            row("config_file", path(&self.config_file.value), &self.config_file.source, Some(ENV_CONFIG)),
            row("output_dir", path(&self.output_dir.value), &self.output_dir.source, Some(ENV_OUTPUT_DIR)),
            row("state_dir", path(&self.state_dir.value), &self.state_dir.source, Some(ENV_STATE_DIR)),
            row(
                Operation::ImageGenerate.model_config_key(),
                opt(&self.image_model.value),
                &self.image_model.source,
                None,
            ),
            row(
                Operation::VideoGenerate.model_config_key(),
                opt(&self.video_model.value),
                &self.video_model.source,
                None,
            ),
            row(
                KEY_WAIT_TIMEOUT,
                dur(self.wait_timeout.value),
                &self.wait_timeout.source,
                Some(ENV_WAIT_TIMEOUT),
            ),
            row(
                KEY_POLL_INTERVAL,
                dur(self.poll_interval.value),
                &self.poll_interval.source,
                Some(ENV_POLL_INTERVAL),
            ),
            row(
                "jobs.store_prompts",
                serde_json::Value::Bool(self.store_prompts.value),
                &self.store_prompts.source,
                Some(ENV_STORE_PROMPTS),
            ),
        ];
        let video_providers = catalog::providers_for(Operation::VideoGenerate);
        for p in self.providers() {
            let prefix = format!("providers.{}", p.provider.as_str());
            rows.push(row(
                &format!("{prefix}.base_url"),
                url(&p.base_url.value),
                &p.base_url.source,
                Some(p.provider.base_url_env()),
            ));
            rows.push(row(
                &format!("{prefix}.request_timeout"),
                dur(p.request_timeout.value),
                &p.request_timeout.source,
                None,
            ));
            // Job submissions exist only for providers with video models.
            if video_providers.contains(&p.provider) {
                rows.push(row(
                    &format!("{prefix}.submit_timeout"),
                    dur(p.submit_timeout.value),
                    &p.submit_timeout.source,
                    None,
                ));
            }
        }
        rows.push(row(
            "log",
            serde_json::Value::String(self.log_filter.value.clone()),
            &self.log_filter.source,
            Some(ENV_LOG),
        ));

        let credentials = ProviderId::ALL
            .iter()
            .map(|p| CredentialView {
                env: p.credential_env().to_string(),
                present: self.credential_present(*p),
            })
            .collect();
        (rows, credentials)
    }

    /// The `config show` result.
    pub fn config_show(&self) -> ConfigShowResult {
        let (settings, credentials) = self.describe();
        ConfigShowResult {
            config_file: self.config_file.value.display().to_string(),
            config_file_exists: self.config_file_exists,
            settings,
            credentials,
        }
    }

    /// The `config path` result.
    pub fn config_path(&self) -> ConfigPathResult {
        ConfigPathResult {
            config_file: self.config_file.value.display().to_string(),
            state_dir: self.state_dir.value.display().to_string(),
            jobs_dir: self.jobs_dir().display().to_string(),
        }
    }

    /// Warnings about the configuration itself: currently one
    /// `non_default_base_url` warning per provider whose base URL is not the
    /// default (its API key is sent there). `config show` and `doctor` print these.
    pub fn warnings(&self) -> Vec<Warning> {
        self.providers().filter_map(|p| self.base_url_warning(p.provider)).collect()
    }

    /// The `non_default_base_url` warning for `provider`, if its base URL
    /// is not the default: it names the host its API key is sent to.
    pub fn base_url_warning(&self, provider: ProviderId) -> Option<Warning> {
        let p = self.provider(provider);
        if p.base_url.value.as_str() == default_base_url(provider).as_str() {
            return None;
        }
        let origin = match p.base_url.source {
            SettingSource::Env => format!("from {}", provider.base_url_env()),
            SettingSource::File => "from the config file".to_string(),
            SettingSource::Flag => "from a flag".to_string(),
            SettingSource::Default => "default".to_string(),
        };
        let insecure = if p.base_url.value.scheme() == "http" { " over unencrypted HTTP" } else { "" };
        Some(Warning::new(
            crate::domain::WarningCode::NonDefaultBaseUrl,
            format!(
                "providers.{}.base_url is {} ({origin}); {} is sent to that host{insecure}",
                provider.as_str(),
                redact::redact_url(p.base_url.value.as_str()),
                provider.credential_env()
            ),
        ))
    }

    /// Add `provider`'s `non_default_base_url` warning to `warnings`
    /// (once, however often it is called) when its base URL is not the default.
    /// Every command that attaches `provider`'s credential to a request calls this
    /// first, so each use of a key at a non-default host is reported, not only
    /// `config show` and `doctor`.
    pub fn warn_non_default_base_url(&self, provider: ProviderId, warnings: &mut Vec<Warning>) {
        if let Some(warning) = self.base_url_warning(provider)
            && !warnings.contains(&warning)
        {
            warnings.push(warning);
        }
    }
}

/// The default base URL of a provider ([`ProviderId::default_base_url`]), parsed.
pub fn default_base_url(provider: ProviderId) -> Url {
    Url::parse(provider.default_base_url()).expect("default base URLs are valid")
}

/// Parse a duration: humantime syntax (`90s`, `10m`, `1h 30m`) or plain seconds.
/// Shared with the CLI's value parsers.
pub fn parse_duration(text: &str) -> Result<Duration, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("empty duration".to_string());
    }
    if t.bytes().all(|b| b.is_ascii_digit()) {
        return t.parse::<u64>().map(Duration::from_secs).map_err(|_| format!("duration '{t}' is too large"));
    }
    humantime::parse_duration(t)
        .map_err(|_| format!("invalid duration '{t}' (expected e.g. 90s, 10m, 1h, or plain seconds)"))
}

/// Parse a boolean: `true/false`, `1/0`, `yes/no`, `on/off` (case-insensitive).
pub fn parse_bool(text: &str) -> Result<bool, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        other => Err(format!("invalid boolean '{other}' (expected true or false)")),
    }
}

/// Validate a provider base URL: `https`, or plain `http` for a loopback host only
/// (`localhost`, 127.0.0.0/8, `::1`: a local mock server or proxy, see
/// [`crate::http::is_loopback`]); a host; no userinfo, query, or fragment. A
/// trailing `/` on a non-root path is removed.
pub fn parse_base_url(text: &str) -> Result<Url, String> {
    let mut url = Url::parse(text.trim()).map_err(|e| format!("invalid URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("unsupported URL scheme '{}' (expected https)", url.scheme()));
    }
    if url.host().is_none() {
        return Err("the URL has no host".to_string());
    }
    if url.scheme() == "http" && !crate::http::is_loopback(&url) {
        return Err(format!(
            "plain http is allowed only for a loopback host (localhost, 127.0.0.0/8, ::1), not '{}'; use \
             https, or the API key would cross the network unencrypted",
            url.host_str().unwrap_or_default()
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL must not contain a user name or password".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("the URL must not contain a query string or fragment".to_string());
    }
    let trimmed = url.path().trim_end_matches('/').to_string();
    if !trimmed.is_empty() {
        url.set_path(&trimmed);
    }
    Ok(url)
}

fn provider_settings(
    provider: ProviderId,
    section: &file::ProviderSection,
    env: &EnvSnapshot,
    path: &Path,
) -> Result<ProviderSettings, IrisError> {
    let prefix = format!("providers.{}", provider.as_str());
    let key = |k: &str| format!("{prefix}.{k}");
    let base_url = layered(
        None,
        env_parsed(env, provider.base_url_env(), parse_base_url)?,
        section
            .base_url
            .as_deref()
            .map(|v| parse_base_url(v).map_err(|m| file::key_error(path, &key("base_url"), m)))
            .transpose()?,
        || Ok(default_base_url(provider)),
    )?;
    let request_timeout = layered(
        None,
        None,
        section
            .request_timeout
            .as_ref()
            .map(|v| file_duration(path, &key("request_timeout"), v, positive))
            .transpose()?,
        || Ok(DEFAULT_REQUEST_TIMEOUT),
    )?;
    let submits_jobs = catalog::providers_for(Operation::VideoGenerate).contains(&provider);
    let submit_timeout = layered(
        None,
        None,
        section
            .submit_timeout
            .as_ref()
            .map(|v| {
                if !submits_jobs {
                    return Err(file::key_error(
                        path,
                        &key("submit_timeout"),
                        format!("{provider} has no video models, so Iris never submits a job to it"),
                    ));
                }
                file_duration(path, &key("submit_timeout"), v, positive)
            })
            .transpose()?,
        || Ok(DEFAULT_SUBMIT_TIMEOUT),
    )?;
    Ok(ProviderSettings { provider, base_url, request_timeout, submit_timeout })
}

/// Pick the highest layer that is present.
fn layered<T>(
    flag: Option<T>,
    env: Option<T>,
    file: Option<T>,
    default: impl FnOnce() -> Result<T, IrisError>,
) -> Result<Resolved<T>, IrisError> {
    Ok(if let Some(value) = flag {
        Resolved { value, source: SettingSource::Flag }
    } else if let Some(value) = env {
        Resolved { value, source: SettingSource::Env }
    } else if let Some(value) = file {
        Resolved { value, source: SettingSource::File }
    } else {
        Resolved { value: default()?, source: SettingSource::Default }
    })
}

fn default_paths(env: &EnvSnapshot) -> Result<PlatformPaths, IrisError> {
    let home = env.home().ok_or_else(|| {
        IrisError::new(ErrorCode::ConfigInvalid, "cannot determine the home directory (HOME is not set)")
            .with_hint("set HOME, or set IRIS_CONFIG and IRIS_STATE_DIR explicitly")
    })?;
    Ok(platform_paths(env.platform(), home, env.var("XDG_CONFIG_HOME"), env.var("XDG_STATE_HOME")))
}

fn env_error(var: &str, message: impl std::fmt::Display) -> IrisError {
    IrisError::new(
        ErrorCode::ConfigInvalid,
        redact::scrub(&format!("environment variable {var}: {message}")).into_owned(),
    )
    .with_detail("env_var", var)
    .with_hint(format!("fix or unset {var}"))
}

fn flag_error(flag: &str, message: impl std::fmt::Display) -> IrisError {
    IrisError::invalid(format!("{flag}: {message}")).with_detail("flag", flag)
}

/// A variable's value, or `config_invalid` if it is not valid UTF-8.
fn env_value<'a>(env: &'a EnvSnapshot, var: &str) -> Result<Option<&'a str>, IrisError> {
    if env.is_non_unicode(var) {
        return Err(env_error(var, "is not valid UTF-8"));
    }
    Ok(env.var(var))
}

fn env_parsed<T>(
    env: &EnvSnapshot,
    var: &str,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<Option<T>, IrisError> {
    env_value(env, var)?.map(|v| parse(v).map_err(|m| env_error(var, m))).transpose()
}

/// A path variable: absolute after `~` expansion, like a path in the config file.
/// A relative one would resolve against whatever directory each command runs in
/// (a state directory that moves loses its jobs), so it is `config_invalid`.
fn env_path(env: &EnvSnapshot, var: &str) -> Result<Option<PathBuf>, IrisError> {
    env_value(env, var)?
        .map(|v| {
            let p = expand_tilde(Path::new(v), env.home()).map_err(|m| env_error(var, m))?;
            if !p.is_absolute() {
                return Err(env_error(var, format!("'{v}' must be an absolute path or start with ~/")));
            }
            Ok(p)
        })
        .transpose()
}

/// A path flag: `~`-expanded, then a relative path is resolved against the current
/// directory (`io_error` naming the flag if that is unknown).
fn flag_path(p: &Path, flag: &str, env: &EnvSnapshot) -> Result<PathBuf, IrisError> {
    if p.as_os_str().is_empty() {
        return Err(flag_error(flag, "empty path"));
    }
    let p = expand_tilde(p, env.home()).map_err(|m| flag_error(flag, m))?;
    if p.is_absolute() {
        return Ok(p);
    }
    Ok(env.cwd().map_err(|e| e.with_detail("flag", flag))?.join(p))
}

/// Paths in the config file must be absolute after `~` expansion.
fn file_path(file: &Path, key: &str, value: &str, env: &EnvSnapshot) -> Result<PathBuf, IrisError> {
    let p = expand_tilde(Path::new(value), env.home()).map_err(|m| file::key_error(file, key, m))?;
    if !p.is_absolute() {
        return Err(file::key_error(
            file,
            key,
            format!("'{value}' must be an absolute path or start with ~/"),
        ));
    }
    Ok(p)
}

fn file_duration(
    file: &Path,
    key: &str,
    value: &toml::Value,
    check: impl FnOnce(Duration) -> Result<Duration, String>,
) -> Result<Duration, IrisError> {
    let parsed = match value {
        toml::Value::String(s) => parse_duration(s),
        toml::Value::Integer(i) if *i >= 0 => Ok(Duration::from_secs(*i as u64)),
        _ => Err("expected a duration such as \"10m\" or a number of seconds".to_string()),
    };
    parsed.and_then(check).map_err(|m| file::key_error(file, key, m))
}

/// A model named in the config file for the operations `ops` (`image.model` for
/// the image operations, `video.model` for video): a catalog id or alias, matched
/// exactly as `-m/--model` is, of a model that implements at least one of them.
/// Returns the id `-m` would send for it: the canonical id for a nickname, a dated
/// snapshot as given. A name Iris declines gets the reason and replacements `-m`
/// gives it, for the key's operations.
fn file_model(file: &Path, value: &str, ops: &[Operation]) -> Result<String, IrisError> {
    let key = ops[0].model_config_key();
    let listed: Vec<String> = ops.iter().map(|op| format!("`iris models list --operation {op}`")).collect();
    let hint = format!("set {key} to a model listed by {}", listed.join(" or "));
    let Ok(resolved) = catalog::resolve(value, None) else {
        let hint = match catalog::declined(value) {
            Some(declined) => declined.hint(ops, &hint),
            None => hint,
        };
        return Err(file::key_error(file, key, format!("unknown model '{value}'")).with_hint(hint));
    };
    if !ops.iter().any(|op| resolved.spec.supports(*op)) {
        let supported: Vec<&str> = resolved.spec.operations.iter().map(|op| op.as_str()).collect();
        let wanted: Vec<&str> = ops.iter().map(|op| op.as_str()).collect();
        return Err(file::key_error(
            file,
            key,
            format!(
                "model '{}' does not support {} (supports: {})",
                resolved.spec.id,
                wanted.join(" or "),
                supported.join(", ")
            ),
        )
        .with_hint(hint));
    }
    Ok(resolved.id)
}

fn parse_log_filter(text: &str) -> Result<String, String> {
    let t = text.trim();
    tracing_subscriber::EnvFilter::try_new(t).map_err(|e| format!("invalid log filter '{t}': {e}"))?;
    Ok(t.to_string())
}

fn positive(d: Duration) -> Result<Duration, String> {
    if d.is_zero() { Err("must be greater than zero".to_string()) } else { Ok(d) }
}

fn min_poll(d: Duration) -> Result<Duration, String> {
    if d < MIN_POLL_INTERVAL {
        Err(format!("must be at least {}", humantime::format_duration(MIN_POLL_INTERVAL)))
    } else {
        Ok(d)
    }
}

fn row(key: &str, value: serde_json::Value, source: &SettingSource, env_var: Option<&str>) -> SettingView {
    SettingView { key: key.to_string(), value, source: source.clone(), env_var: env_var.map(str::to_string) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_accept_humantime_and_plain_seconds() {
        assert_eq!(parse_duration("90"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_duration(" 10m "), Ok(Duration::from_secs(600)));
        assert_eq!(parse_duration("1h 30m"), Ok(Duration::from_secs(5400)));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("-5s").is_err());
    }

    #[test]
    fn booleans() {
        for t in ["true", "TRUE", "1", "yes", "on"] {
            assert_eq!(parse_bool(t), Ok(true), "{t}");
        }
        for f in ["false", "0", "No", "off"] {
            assert_eq!(parse_bool(f), Ok(false), "{f}");
        }
        assert!(parse_bool("maybe").is_err());
    }

    #[test]
    fn base_urls_are_validated_and_normalized() {
        assert_eq!(
            parse_base_url("https://api.openai.com/v1/").unwrap().as_str(),
            "https://api.openai.com/v1"
        );
        assert_eq!(parse_base_url("http://127.0.0.1:8080").unwrap().as_str(), "http://127.0.0.1:8080/");
        assert_eq!(parse_base_url("http://localhost:8080/v1").unwrap().as_str(), "http://localhost:8080/v1");
        assert_eq!(parse_base_url("http://[::1]:8080").unwrap().as_str(), "http://[::1]:8080/");
        assert_eq!(
            parse_base_url("https://proxy.example/gemini/").unwrap().as_str(),
            "https://proxy.example/gemini"
        );
        for insecure in ["http://api.example.invalid", "http://10.0.0.1:8080/v1", "http://localhost.example"]
        {
            let e = parse_base_url(insecure).unwrap_err();
            assert!(e.contains("loopback"), "{insecure}: {e}");
        }
        assert!(parse_base_url("ftp://example.com").is_err());
        assert!(parse_base_url("https://user:pw@example.com").is_err());
        assert!(parse_base_url("https://example.com/?key=abc").is_err());
        assert!(parse_base_url("not a url").is_err());
    }

    #[test]
    fn the_default_submit_timeout_is_the_http_default() {
        assert_eq!(DEFAULT_SUBMIT_TIMEOUT, Timeouts::default().submit);
        assert_eq!(DEFAULT_REQUEST_TIMEOUT, Timeouts::default().generate);
    }

    #[test]
    fn defaults_match_the_contract() {
        assert_eq!(default_base_url(ProviderId::OpenAi).as_str(), "https://api.openai.com/v1");
        assert_eq!(
            default_base_url(ProviderId::Gemini).as_str(),
            "https://generativelanguage.googleapis.com/"
        );
        for p in ProviderId::ALL {
            assert_eq!(
                parse_base_url(p.default_base_url()).unwrap(),
                default_base_url(*p),
                "{p}: the default must round-trip through validation unchanged"
            );
            assert_eq!(p.base_url_env(), format!("IRIS_{}_BASE_URL", p.as_str().to_ascii_uppercase()));
        }
    }
}
