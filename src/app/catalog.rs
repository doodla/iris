//! The set of models the app resolves against.
//!
//! In production this is the built-in catalog, and every lookup delegates to the
//! contract functions in [`crate::catalog`] (`find`, `default_model`, `resolve`).
//! Tests can inject their own static [`ModelSpec`]s; resolution over a custom list
//! runs the same code (`crate::catalog::resolve_in`).

use crate::catalog::{self, ModelSpec, ResolvedModel};
use crate::domain::{Operation, ProviderId};
use crate::error::IrisError;

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
    /// [`crate::catalog::resolve`] for the rules; custom lists use the same code).
    pub fn resolve(
        &self,
        model: &str,
        capabilities_from: Option<&str>,
        provider: Option<ProviderId>,
    ) -> Result<ResolvedModel, IrisError> {
        match &self.custom {
            None => catalog::resolve(model, capabilities_from, provider),
            Some(models) => catalog::resolve_in(models, model, capabilities_from, provider),
        }
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
                for p in std::iter::once(None).chain(ProviderId::ALL.iter().copied().map(Some)) {
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
