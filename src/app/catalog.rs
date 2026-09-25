//! The set of models the app resolves against.
//!
//! In production this is the built-in catalog. Tests can inject their own static
//! [`ModelSpec`]s. Either way every lookup runs the shared functions of
//! [`crate::catalog`] (`find_in`, `resolve_in`) over [`Catalog::models`], so both
//! follow the same rules.

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
        catalog::find_in(self.models(), id_or_alias)
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

    /// Resolve `--model` / `--capabilities-from` (see [`crate::catalog::resolve`] for
    /// the rules).
    pub fn resolve(&self, model: &str, capabilities_from: Option<&str>) -> Result<ResolvedModel, IrisError> {
        catalog::resolve_in(&self.models(), model, capabilities_from)
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
        let ids: Vec<&str> =
            catalog::all().flat_map(|m| std::iter::once(m.id).chain(m.aliases.iter().copied())).collect();
        for model in ids.iter().copied().chain(["no-such-model", "bad id!", ""]) {
            for caps in [None, Some("no-such-template")].into_iter().chain(ids.iter().copied().map(Some)) {
                match (builtin.resolve(model, caps), custom.resolve(model, caps)) {
                    (Ok(a), Ok(b)) => {
                        assert_eq!(a.id, b.id);
                        assert_eq!(a.spec.id, b.spec.id);
                        assert_eq!(a.source, b.source);
                    }
                    (Err(a), Err(b)) => {
                        assert_eq!(a.code, b.code, "{model} {caps:?}");
                        assert_eq!(a.message, b.message);
                    }
                    (a, b) => panic!("disagreement for {model} {caps:?}: {:?} vs {:?}", a.is_ok(), b.is_ok()),
                }
            }
        }
    }
}
