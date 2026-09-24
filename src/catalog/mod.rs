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
        let known: Vec<&str> = all().map(|m| m.id).collect();
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
    if model.is_empty()
        || model.len() > 200
        || !model.chars().all(|c| c.is_ascii_alphanumeric() || "-._/:@".contains(c))
    {
        return Err(IrisError::invalid(format!("model id '{model}' contains unsupported characters")));
    }
    Ok(ResolvedModel { id: model.to_string(), spec, source: CapabilitySource::Borrowed { from: spec.id } })
}

#[cfg(test)]
mod tests {
    use super::*;

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
