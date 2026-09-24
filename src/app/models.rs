//! `models list`, `models show`, `providers list`: capability inspection from the
//! catalog. Known capabilities come from the catalog; account-specific access is
//! reported separately and only checked on request (`--check-access`, a free
//! metadata call).

use crate::catalog::{CATALOG_AS_OF, ModelSpec, OptionKind, OptionSpec};
use crate::domain::{Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{
    AccessView, ConstraintView, InputsView, LimitsView, MaskRequirementsView, ModelCapabilities,
    ModelListResult, ModelShowResult, ModelSummary, OptionView, OutputsView, PriceView, ProviderListResult,
    ProviderView,
};
use crate::providers::AccountAccess;
use crate::redact;

use super::context::AppContext;
use super::request::effective_default;

/// `models list`, optionally filtered by provider and operation.
pub fn list(ctx: &AppContext, provider: Option<ProviderId>, operation: Option<Operation>) -> ModelListResult {
    let models = ctx
        .catalog
        .models()
        .into_iter()
        .filter(|m| provider.is_none_or(|p| m.provider == p))
        .filter(|m| operation.is_none_or(|op| m.supports(op)))
        .map(|m| summary(ctx, m))
        .collect();
    ModelListResult { models }
}

fn summary(ctx: &AppContext, m: &ModelSpec) -> ModelSummary {
    ModelSummary {
        id: m.id.to_string(),
        provider: m.provider,
        display_name: m.display_name.to_string(),
        aliases: m.aliases.iter().map(|a| a.to_string()).collect(),
        lifecycle: m.lifecycle,
        operations: m.operations.to_vec(),
        default_for: default_for(ctx, m),
    }
}

/// Operations for which `m` is the model used without `--model`: the configured
/// default of its provider when set, else the catalog default.
fn default_for(ctx: &AppContext, m: &ModelSpec) -> Vec<Operation> {
    m.operations
        .iter()
        .copied()
        .filter(|op| effective_default(ctx, m.provider, *op).is_some_and(|d| d.id == m.id))
        .collect()
}

/// `models show <MODEL>`: declared capabilities, options, constraints, defaults,
/// pricing, and access. With `check_access`, asks the provider (free metadata call)
/// whether the model is visible to the key; billing tier, credit, and organization
/// verification are not part of that check.
pub async fn show(
    ctx: &AppContext,
    model: &str,
    check_access: bool,
    warnings: &mut Vec<Warning>,
) -> Result<ModelShowResult, IrisError> {
    let spec = ctx.catalog.find(model).ok_or_else(|| {
        let known: Vec<&str> = ctx.catalog.models().iter().map(|m| m.id).collect();
        IrisError::new(ErrorCode::UnknownModel, format!("unknown model '{model}'")).with_hint(format!(
            "known models: {}",
            if known.is_empty() { "none".to_string() } else { known.join(", ") }
        ))
    })?;
    if spec.lifecycle == crate::catalog::Lifecycle::Preview {
        warnings.push(Warning::new(
            "preview_model",
            format!("{} is a preview model; its behavior, limits, and availability may change", spec.id),
        ));
    }
    let provider = spec.provider;
    let (account_access, checked_at) = if check_access {
        let adapter = ctx.provider(provider)?;
        let pctx = ctx.provider_context(provider)?;
        ctx.interrupt.arm();
        let seen = ctx.interrupt.count();
        let access = tokio::select! {
            result = adapter.check_access(spec.id, &pctx) => result?,
            () = ctx.interrupt.after(seen) => {
                return Err(IrisError::new(ErrorCode::Interrupted, "interrupted while checking model access"));
            }
        };
        (access, Some(ctx.now().to_string()))
    } else {
        (AccountAccess::NotChecked, None)
    };
    Ok(ModelShowResult { model: capabilities(ctx, spec, account_access, checked_at) })
}

fn capabilities(
    ctx: &AppContext,
    m: &ModelSpec,
    account_access: AccountAccess,
    checked_at: Option<String>,
) -> ModelCapabilities {
    ModelCapabilities {
        id: m.id.to_string(),
        provider: m.provider,
        display_name: m.display_name.to_string(),
        aliases: m.aliases.iter().map(|a| a.to_string()).collect(),
        lifecycle: m.lifecycle,
        operations: m.operations.to_vec(),
        default_for: default_for(ctx, m),
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
        access: AccessView {
            credential_env: m.provider.credential_env().to_string(),
            credential_present: ctx.settings.credential_present(m.provider),
            requirements: m.access_notes.iter().map(|n| n.to_string()).collect(),
            account_access,
            checked_at,
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
    OptionView {
        name: o.name.to_string(),
        kind: kind.to_string(),
        values,
        min,
        max,
        syntax,
        default: o.default.map(str::to_string),
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
