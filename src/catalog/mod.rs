//! The model catalog: every model Iris knows, with declared capabilities.
//!
//! Model defaults are centralized here (`default_model`). Provider modules
//! (`openai`, `gemini`, `veo`) contribute their static declarations.

pub mod gemini;
pub mod openai;
pub mod options;
pub mod types;
pub mod veo;

pub use options::{InputCounts, OptionSource, OptionValue, RawOption, ResolvedOptions, validate_request};
pub use types::*;

use crate::domain::{Operation, ProviderId};
use crate::error::{ErrorCode, IrisError};

/// Date the built-in catalog (capabilities and prices) was last checked against
/// provider documentation.
pub const CATALOG_AS_OF: &str = "2026-09-24";

/// All built-in models, grouped by provider in a stable order.
pub fn all() -> impl Iterator<Item = &'static ModelSpec> {
    openai::MODELS.iter().chain(gemini::MODELS.iter()).chain(veo::MODELS.iter())
}

/// Look up a model by id or alias (case-sensitive ids; aliases are lowercase).
pub fn find(id_or_alias: &str) -> Option<&'static ModelSpec> {
    all().find(|m| m.id == id_or_alias || m.aliases.contains(&id_or_alias))
}

/// The provider's default model for an operation, if the provider supports it.
pub fn default_model(provider: ProviderId, op: Operation) -> Option<&'static ModelSpec> {
    all().find(|m| m.provider == provider && m.default_for.contains(&op))
}

/// The syntax of model ids `provider`'s adapter can send (checked for unknown ids
/// given with `--capabilities-from`; every catalog id satisfies it).
pub fn model_id_syntax(provider: ProviderId) -> ModelIdSyntax {
    match provider {
        ProviderId::OpenAi => openai::MODEL_ID_SYNTAX,
        ProviderId::Gemini => gemini::MODEL_ID_SYNTAX,
    }
}

/// Providers that implement an operation with at least one model.
pub fn providers_for(op: Operation) -> Vec<ProviderId> {
    let mut out: Vec<ProviderId> = all().filter(|m| m.supports(op)).map(|m| m.provider).collect();
    out.sort();
    out.dedup();
    out
}

/// How a model's capabilities were established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilitySource {
    /// Declared in the built-in catalog.
    Catalog,
    /// Unknown model id; capabilities borrowed from a known model via `--capabilities-from`.
    Borrowed { from: &'static str },
}

/// A resolved model: the id to send, and the declared capabilities that apply.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Model id sent to the provider (a catalog id, or the user's unknown id).
    pub id: String,
    pub spec: &'static ModelSpec,
    pub source: CapabilitySource,
}

/// True if `candidate` is `base` followed by a `-YYYY-MM-DD` snapshot date.
fn is_snapshot_of(base: &str, candidate: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix(base) else { return false };
    let b = suffix.as_bytes();
    b.len() == 11
        && b[0] == b'-'
        && b[5] == b'-'
        && b[8] == b'-'
        && [1, 2, 3, 4, 6, 7, 9, 10].iter().all(|&i| b[i].is_ascii_digit())
}

/// Resolve `--model` / `--capabilities-from` / `--provider` into a model.
///
/// * Known id or alias → catalog spec. If `provider` is given and differs → `invalid_argument`.
/// * Unknown id without `capabilities_from` → `unknown_model` (lists known models).
/// * Unknown id with `capabilities_from` naming a known model → that model's spec,
///   sending the unknown id (caller emits warning `unverified_model_capabilities`).
pub fn resolve(
    model: &str,
    capabilities_from: Option<&str>,
    provider: Option<ProviderId>,
) -> Result<ResolvedModel, IrisError> {
    let models: Vec<&'static ModelSpec> = all().collect();
    resolve_in(&models, model, capabilities_from, provider)
}

/// [`resolve`] over an explicit model list (the app's injectable catalog uses this so
/// tests and production share one set of rules).
pub fn resolve_in(
    models: &[&'static ModelSpec],
    model: &str,
    capabilities_from: Option<&str>,
    provider: Option<ProviderId>,
) -> Result<ResolvedModel, IrisError> {
    let find = |id: &str| models.iter().copied().find(|m| m.id == id || m.aliases.contains(&id));
    if let Some(spec) = find(model) {
        if capabilities_from.is_some() {
            return Err(IrisError::usage(format!(
                "--capabilities-from is only for models Iris does not know; '{model}' is a known model"
            )));
        }
        if let Some(p) = provider
            && p != spec.provider
        {
            return Err(IrisError::invalid(format!(
                "model '{}' belongs to provider '{}', not '{p}'",
                spec.id, spec.provider
            )));
        }
        // A dated snapshot alias (`<id>-YYYY-MM-DD`) pins that snapshot, so send it as given;
        // other aliases are Iris nicknames for the canonical id.
        let id = if is_snapshot_of(spec.id, model) { model } else { spec.id };
        return Ok(ResolvedModel { id: id.to_string(), spec, source: CapabilitySource::Catalog });
    }

    let Some(template) = capabilities_from else {
        let known: Vec<&str> = models.iter().map(|m| m.id).collect();
        return Err(IrisError::new(ErrorCode::UnknownModel, format!("unknown model '{model}'"))
            .with_hint(format!(
                "known models: {}. To use a model Iris does not know yet, add --capabilities-from <KNOWN_MODEL> \
                 to declare which known model's capabilities it has",
                known.join(", ")
            )));
    };
    let Some(spec) = find(template) else {
        return Err(IrisError::new(
            ErrorCode::UnknownModel,
            format!("--capabilities-from '{template}' is not a known model"),
        )
        .with_hint("run `iris models list`"));
    };
    if let Some(p) = provider
        && p != spec.provider
    {
        return Err(IrisError::invalid(format!(
            "--capabilities-from model '{}' belongs to provider '{}', not '{p}'",
            spec.id, spec.provider
        )));
    }
    let syntax = model_id_syntax(spec.provider);
    if !syntax.accepts(model) {
        return Err(IrisError::invalid(format!(
            "model id '{}' is not valid for provider {}: use {}",
            crate::redact::truncate(&crate::redact::scrub(model), 80),
            spec.provider,
            syntax.description
        ))
        .with_hint("check the --model value; nothing was sent"));
    }
    Ok(ResolvedModel { id: model.to_string(), spec, source: CapabilitySource::Borrowed { from: spec.id } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_id_and_alias_satisfies_its_providers_model_id_syntax() {
        for m in all() {
            let syntax = model_id_syntax(m.provider);
            for id in std::iter::once(m.id).chain(m.aliases.iter().copied()) {
                assert!(syntax.accepts(id), "{id}");
            }
        }
    }

    #[test]
    fn unknown_ids_follow_the_syntax_of_the_templates_provider() {
        let gemini = ["gemini-9.9-flash-image", "veo_4.0-x", "A1", &"a".repeat(128)];
        for id in gemini {
            assert!(resolve(id, Some("nano-banana"), None).is_ok(), "{id}");
            assert!(resolve(id, Some("veo"), None).is_ok(), "{id}");
        }
        for id in ["a:b", "bad/../id", "-lead", ".hidden", "a b", "a?b", "a%2Fb", "é", "", &"a".repeat(129)]
        {
            for template in ["nano-banana", "veo"] {
                let err = resolve(id, Some(template), None).unwrap_err();
                assert_eq!(err.code, ErrorCode::InvalidArgument, "{id} {template}");
            }
        }
        for id in ["ft:gpt-image-2:org:custom:1", "org/model@v2", "-x"] {
            assert!(resolve(id, Some("gpt-image-2"), None).is_ok(), "{id}");
        }
        for id in ["a b", "a?b", "", &"a".repeat(201)] {
            assert_eq!(
                resolve(id, Some("gpt-image-2"), None).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{id}"
            );
        }
    }

    #[test]
    fn snapshot_aliases_are_sent_as_given_and_nicknames_are_canonicalized() {
        for m in all() {
            for alias in m.aliases {
                let resolved = resolve(alias, None, None).unwrap();
                assert_eq!(resolved.spec.id, m.id);
                if is_snapshot_of(m.id, alias) {
                    assert_eq!(resolved.id, *alias, "snapshot {alias} must be pinned");
                } else {
                    assert_eq!(resolved.id, m.id, "nickname {alias} must resolve to the canonical id");
                }
            }
        }
        assert!(is_snapshot_of("gpt-image-2", "gpt-image-2-2026-04-21"));
        assert!(!is_snapshot_of("gpt-image-2", "gpt-image-2-2026-04"));
        assert!(!is_snapshot_of("gpt-image-2", "gpt-image-2.5-sunburst"));
    }
}
