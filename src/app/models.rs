//! `models list`, `models show`, `providers list`: capability inspection from the
//! catalog. Known capabilities come from the catalog; account-specific access is
//! reported separately and only checked on request (`--check-access`, a free
//! metadata call).

use crate::catalog::{CATALOG_AS_OF, ModelSpec, OptionKind, OptionSpec, OptionValue};
use crate::config::EnvSnapshot;
use crate::domain::{Operation, ProviderId, Warning, WarningCode};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{
    AccessView, ConstraintView, InputsView, LimitsView, LowestEstimate, MaskRequirementsView,
    ModelCapabilities, ModelListResult, ModelShowResult, ModelSummary, OptionView, OutputsView, PriceView,
    ProviderListResult, ProviderView,
};
use crate::providers::AccountAccess;
use crate::redact;

use super::catalog::Catalog;
use super::context::AppContext;

/// `models list`, optionally filtered by provider and operation. It reads only the
/// catalog, so it needs no settings.
pub fn list(
    catalog: &Catalog,
    provider: Option<ProviderId>,
    operation: Option<Operation>,
) -> ModelListResult {
    let models = catalog
        .models()
        .into_iter()
        .filter(|m| provider.is_none_or(|p| m.provider == p))
        .filter(|m| operation.is_none_or(|op| m.supports(op)))
        .map(summary)
        .collect();
    ModelListResult { models }
}

fn summary(m: &ModelSpec) -> ModelSummary {
    ModelSummary {
        id: m.id.to_string(),
        provider: m.provider,
        display_name: m.display_name.to_string(),
        summary: m.summary.to_string(),
        aliases: m.aliases.iter().map(|a| a.to_string()).collect(),
        lifecycle: m.lifecycle,
        billing: m.billing,
        operations: m.operations.to_vec(),
        lowest_estimate: lowest_estimate(m),
    }
}

/// The model's cheapest single-output request and its estimate
/// ([`ModelSpec::lowest_estimate`]), as `models list`, `models show`, and the
/// `model_required` candidates report it.
pub(crate) fn lowest_estimate(m: &ModelSpec) -> Option<LowestEstimate> {
    let (options, cost_estimate) = m.lowest_estimate()?;
    Some(LowestEstimate {
        options: options.iter().map(|(name, value)| (name.clone(), value.clone())).collect(),
        cost_estimate,
    })
}

/// `models show <MODEL>`: declared capabilities, options (with their defaults),
/// constraints, pricing, and access requirements, with whether the provider's key is
/// set. It reads only the catalog and the environment, never the settings, so it runs
/// even when the config file is invalid; [`check_access`] adds the provider's answer.
pub fn show(
    catalog: &Catalog,
    env: &EnvSnapshot,
    model: &str,
    warnings: &mut Vec<Warning>,
) -> Result<ModelShowResult, IrisError> {
    let spec = catalog.find(model).ok_or_else(|| catalog.unknown(model))?;
    if spec.lifecycle == crate::catalog::Lifecycle::Preview {
        warnings.push(Warning::new(
            WarningCode::PreviewModel,
            format!("{} is a preview model; its behavior, limits, and availability may change", spec.id),
        ));
    }
    Ok(ModelShowResult { model: capabilities(spec, env.credential(spec.provider).is_some()) })
}

/// `models show --check-access`: asks the provider (free metadata call) whether the
/// model [`show`] described is visible to the key; billing tier, credit, and
/// organization verification are not part of that check.
pub async fn check_access(
    ctx: &AppContext,
    shown: &mut ModelShowResult,
    warnings: &mut Vec<Warning>,
) -> Result<(), IrisError> {
    let model = &mut shown.model;
    let adapter = ctx.provider(model.provider)?;
    let pctx = ctx.provider_context(model.provider)?;
    ctx.settings.warn_non_default_base_url(model.provider, warnings);
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let access = tokio::select! {
        result = adapter.check_access(&model.id, &pctx) => result?,
        () = ctx.interrupt.after(seen) => {
            return Err(IrisError::new(ErrorCode::Interrupted, "interrupted while checking model access"));
        }
    };
    model.access.account_access = access;
    model.access.checked_at = Some(ctx.now().to_string());
    Ok(())
}

fn capabilities(m: &ModelSpec, credential_present: bool) -> ModelCapabilities {
    ModelCapabilities {
        id: m.id.to_string(),
        provider: m.provider,
        display_name: m.display_name.to_string(),
        summary: m.summary.to_string(),
        aliases: m.aliases.iter().map(|a| a.to_string()).collect(),
        lifecycle: m.lifecycle,
        billing: m.billing,
        operations: m.operations.to_vec(),
        inputs: InputsView {
            max_input_images: m.inputs.max_input_images,
            input_media_types: m.inputs.input_media_types.iter().map(|t| t.to_string()).collect(),
            max_input_bytes: m.inputs.max_input_bytes,
            mask: m.inputs.mask.is_some(),
            mask_requirements: m.inputs.mask.map(|mask| MaskRequirementsView {
                media_types: mask.media_types.iter().map(|t| t.to_string()).collect(),
                max_bytes: mask.max_bytes,
                alpha_channel_required: mask.requires_alpha,
                same_size_as_first_image: mask.same_size_as_first_image,
            }),
            first_frame: m.inputs.first_frame,
            last_frame: m.inputs.last_frame,
            max_reference_images: m.inputs.max_reference_images,
            max_request_bytes: m.inputs.max_request.map(|limit| limit.max_bytes),
        },
        options: m.options.iter().map(option_view).collect(),
        constraints: m
            .validate
            .map(|rules| rules.constraints)
            .unwrap_or_default()
            .iter()
            .map(|c| ConstraintView {
                id: c.id.to_string(),
                options: c.options.iter().map(|o| o.to_string()).collect(),
                inputs: c.inputs.iter().map(|i| i.to_string()).collect(),
                description: c.description.to_string(),
            })
            .collect(),
        outputs: OutputsView {
            media_types: m.outputs.media_types.iter().map(|t| t.to_string()).collect(),
            max_count: m.outputs.max_count,
        },
        limits: LimitsView { max_prompt_chars: m.limits.max_prompt_chars },
        pricing: m
            .pricing
            .iter()
            .map(|p| PriceView {
                description: p.description.to_string(),
                unit: p.unit.to_string(),
                usd: p.usd,
                source_url: p.source_url.to_string(),
                as_of: p.as_of.to_string(),
            })
            .collect(),
        lowest_estimate: lowest_estimate(m),
        access: AccessView {
            credential_env: m.provider.credential_env().to_string(),
            credential_present,
            requirements: m.access_notes.iter().map(|n| n.to_string()).collect(),
            account_access: AccountAccess::NotChecked,
            checked_at: None,
        },
        capabilities_source: "catalog".to_string(),
        catalog_as_of: CATALOG_AS_OF.to_string(),
        docs_url: m.docs_url.to_string(),
    }
}

/// The JSON view of one declared option.
pub(crate) fn option_view(o: &OptionSpec) -> OptionView {
    let (kind, values, min, max, syntax) = match o.kind {
        OptionKind::Enum(values) => {
            ("enum", Some(values.iter().map(|v| v.to_string()).collect()), None, None, None)
        }
        OptionKind::Integer { min, max } => ("integer", None, Some(min), Some(max), None),
        OptionKind::Boolean => ("boolean", None, None, None, None),
        OptionKind::Text { max_chars } => {
            ("string", None, None, None, Some(format!("free text, at most {max_chars} characters")))
        }
        OptionKind::Pattern { syntax, .. } => ("string", None, None, None, Some(syntax.to_string())),
    };
    let max_chars = match o.kind {
        OptionKind::Text { max_chars } => Some(max_chars),
        _ => None,
    };
    // Every declared default parses with its option's kind (the catalog tests check
    // it); the string form is only a fallback that keeps the value visible.
    let default =
        o.default.map(|d| OptionValue::parse(&o.kind, d).unwrap_or_else(|_| OptionValue::Str(d.into())));
    OptionView {
        name: o.name.to_string(),
        kind: kind.to_string(),
        values,
        min,
        max,
        syntax,
        max_chars,
        default,
        flag: o.flag.map(str::to_string),
        operations: o.operations.to_vec(),
        description: o.description.to_string(),
    }
}

/// `providers list`: registered providers, their credential variable and whether
/// it is set (never its value), supported operations, and configured base URL.
pub fn providers(ctx: &AppContext) -> ProviderListResult {
    let providers = ctx
        .registry
        .all()
        .map(|p| {
            let id = p.id();
            ProviderView {
                id,
                display_name: id.display_name().to_string(),
                credential_env: id.credential_env().to_string(),
                credential_present: ctx.settings.credential_present(id),
                operations: ctx.catalog.operations_of(id),
                base_url: redact::redact_url(ctx.settings.provider(id).base_url.value.as_str()),
                docs_url: p.docs_url().to_string(),
            }
        })
        .collect();
    ProviderListResult { providers }
}
