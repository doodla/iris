//! The set of models the app resolves against.
//!
//! In production this is the built-in catalog, and every lookup delegates to the
//! contract functions in [`crate::catalog`] (`find`, `default_model`, `resolve`).
//! Tests can inject their own static [`ModelSpec`]s; lookups over a custom list
//! follow the same rules (a unit test checks both agree on the built-in list).

use crate::catalog::{self, CapabilitySource, ModelSpec, ResolvedModel};
use crate::domain::{Operation, ProviderId};
use crate::error::{ErrorCode, IrisError};

/// The models known to this app instance.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    custom: Option<Vec<&'static ModelSpec>>,
}

impl Catalog {
    /// The built-in catalog (`crate::catalog::all()`).
    pub fn builtin() -> Catalog {
        Catalog { custom: None }
    }

    /// A catalog with exactly `models` (tests, embedding).
    pub fn with_models(models: Vec<&'static ModelSpec>) -> Catalog {
        Catalog { custom: Some(models) }
    }

    /// All models, grouped by provider in declaration order.
    pub fn models(&self) -> Vec<&'static ModelSpec> {
        match &self.custom {
            None => catalog::all().collect(),
            Some(models) => models.clone(),
        }
    }

    /// Look up a model by id or alias.
    pub fn find(&self, id_or_alias: &str) -> Option<&'static ModelSpec> {
        match &self.custom {
            None => catalog::find(id_or_alias),
            Some(models) => {
                models.iter().copied().find(|m| m.id == id_or_alias || m.aliases.contains(&id_or_alias))
            }
        }
    }

    /// The provider's default model for an operation.
    pub fn default_model(&self, provider: ProviderId, op: Operation) -> Option<&'static ModelSpec> {
        match &self.custom {
            None => catalog::default_model(provider, op),
            Some(models) => {
                models.iter().copied().find(|m| m.provider == provider && m.default_for.contains(&op))
            }
        }
    }

    /// Providers with at least one model supporting `op`, sorted.
    pub fn providers_for(&self, op: Operation) -> Vec<ProviderId> {
        let mut out: Vec<ProviderId> =
            self.models().into_iter().filter(|m| m.supports(op)).map(|m| m.provider).collect();
        out.sort();
        out.dedup();
        out
    }

    /// Operations supported by at least one model of `provider`, in canonical order.
    pub fn operations_of(&self, provider: ProviderId) -> Vec<Operation> {
        let models = self.models();
        Operation::ALL
            .iter()
            .copied()
            .filter(|op| models.iter().any(|m| m.provider == provider && m.supports(*op)))
            .collect()
    }

    /// Resolve `--model` / `--capabilities-from` / `--provider` (see
    /// [`crate::catalog::resolve`] for the rules).
    pub fn resolve(
        &self,
        model: &str,
        capabilities_from: Option<&str>,
        provider: Option<ProviderId>,
    ) -> Result<ResolvedModel, IrisError> {
        match &self.custom {
            None => catalog::resolve(model, capabilities_from, provider),
            Some(_) => self.resolve_custom(model, capabilities_from, provider),
        }
    }

    /// The same rules as `catalog::resolve`, over the custom model list.
    fn resolve_custom(
        &self,
        model: &str,
        capabilities_from: Option<&str>,
        provider: Option<ProviderId>,
    ) -> Result<ResolvedModel, IrisError> {
        if let Some(spec) = self.find(model) {
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
            return Ok(ResolvedModel { id: spec.id.to_string(), spec, source: CapabilitySource::Catalog });
        }
        let Some(template) = capabilities_from else {
            let known: Vec<&str> = self.models().iter().map(|m| m.id).collect();
            return Err(IrisError::new(ErrorCode::UnknownModel, format!("unknown model '{model}'"))
                .with_hint(format!(
                    "known models: {}. To use a model Iris does not know yet, add --capabilities-from \
                     <KNOWN_MODEL> to declare which known model's capabilities it has",
                    known.join(", ")
                )));
        };
        let Some(spec) = self.find(template) else {
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
        Ok(ResolvedModel {
            id: model.to_string(),
            spec,
            source: CapabilitySource::Borrowed { from: spec.id },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The custom-list rules must agree with `catalog::resolve` on the built-in list.
    #[test]
    fn custom_resolution_matches_the_builtin_contract() {
        let builtin = Catalog::builtin();
        let custom = Catalog::with_models(catalog::all().collect());
        let mut inputs: Vec<(String, Option<String>, Option<ProviderId>)> = Vec::new();
        let ids: Vec<&str> =
            catalog::all().flat_map(|m| std::iter::once(m.id).chain(m.aliases.iter().copied())).collect();
        for id in ids.iter().copied().chain(["no-such-model", "bad id!", ""]) {
            for caps in [None, Some("no-such-template")].into_iter().chain(ids.iter().copied().map(Some)) {
                for p in [None, Some(ProviderId::OpenAi), Some(ProviderId::Gemini)] {
                    inputs.push((id.to_string(), caps.map(str::to_string), p));
                }
            }
        }
        for (model, caps, p) in inputs {
            let a = builtin.resolve(&model, caps.as_deref(), p);
            let b = custom.resolve(&model, caps.as_deref(), p);
            match (a, b) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.id, b.id);
                    assert_eq!(a.spec.id, b.spec.id);
                    assert_eq!(a.source, b.source);
                }
                (Err(a), Err(b)) => {
                    assert_eq!(a.code, b.code, "{model} {caps:?} {p:?}");
                    assert_eq!(a.message, b.message);
                }
                (a, b) => {
                    panic!("disagreement for {model} {caps:?} {p:?}: {:?} vs {:?}", a.is_ok(), b.is_ok())
                }
            }
        }
        for p in ProviderId::ALL {
            for op in Operation::ALL {
                assert_eq!(
                    builtin.default_model(*p, *op).map(|m| m.id),
                    custom.default_model(*p, *op).map(|m| m.id)
                );
            }
        }
        for op in Operation::ALL {
            assert_eq!(builtin.providers_for(*op), catalog::providers_for(*op));
        }
    }
}
