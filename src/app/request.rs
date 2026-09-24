//! Steps shared by every generation workflow (image generate/edit, video
//! generate): provider and model resolution (docs/configuration.md), prompt limits, output counts,
//! cost estimates, and dry-run plan pieces.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

use crate::artifacts::{media, paths};
use crate::catalog::{
    CapabilitySource, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionSource, RawOption,
    ResolvedModel, ResolvedOptions,
};
use crate::config::SettingSource;
use crate::domain::{CostEstimate, Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::jobs;
use crate::output::results::{PlanInput, PlanResult};
use crate::providers::{InputImage, InputRole};

use super::context::AppContext;

/// What the caller asked for, common to every generation command.
#[derive(Clone, Default)]
pub struct GenerationArgs {
    /// The prompt, already read from its source and checked to be non-empty.
    pub prompt: String,
    /// `--provider`.
    pub provider: Option<ProviderId>,
    /// `-m, --model` (catalog id, alias, or an unknown id with `capabilities_from`).
    pub model: Option<String>,
    /// `--capabilities-from <KNOWN_MODEL>`.
    pub capabilities_from: Option<String>,
    /// Typed option flags and `-O key=value`, unvalidated.
    pub options: Vec<RawOption>,
    /// `-o, --output`: exact file path. The directory (`-d`) comes from the settings.
    pub output: Option<PathBuf>,
    /// `--overwrite`.
    pub overwrite: bool,
    /// `--dry-run`: validate locally and return the plan; nothing is sent.
    pub dry_run: bool,
}

impl fmt::Debug for GenerationArgs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never show the prompt text.
        f.debug_struct("GenerationArgs")
            .field("prompt_chars", &self.prompt.chars().count())
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("capabilities_from", &self.capabilities_from)
            .field("options", &self.options.iter().map(|o| o.name.as_str()).collect::<Vec<_>>())
            .field("output", &self.output)
            .field("overwrite", &self.overwrite)
            .field("dry_run", &self.dry_run)
            .finish()
    }
}

/// Result of a generation command: the work was done, or (`--dry-run`) planned.
// Built once per command and immediately converted for printing; size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum GenerationOutcome<T> {
    Completed(T),
    Planned(PlanResult),
}

/// Resolve the provider and model (docs/configuration.md):
/// provider = `--provider` > provider of `--model` > `IRIS_IMAGE_PROVIDER` > file
/// `image.provider` > `openai` (video: the video provider); model = `--model` >
/// file `providers.<p>.<kind>_model` > catalog default. Adds the warnings
/// `unverified_model_capabilities` and `preview_model`.
///
/// A model resolved with `--capabilities-from` runs against the template's
/// capabilities but not its prices: its spec is a copy without price rules or
/// estimators, so every cost estimate for it (before and after the call) is `null`
/// with a `cost_estimate_unavailable` warning that says why.
pub(crate) fn resolve_model(
    ctx: &AppContext,
    op: Operation,
    args: &GenerationArgs,
    warnings: &mut Vec<Warning>,
) -> Result<ResolvedModel, IrisError> {
    let resolved = match args.model.as_deref() {
        Some(model) => ctx.catalog.resolve(model, args.capabilities_from.as_deref(), args.provider)?,
        None => {
            if args.capabilities_from.is_some() {
                return Err(IrisError::usage(
                    "--capabilities-from applies to an unknown --model; give --model <MODEL> as well",
                ));
            }
            let provider = match args.provider {
                Some(p) => p,
                None => default_provider(ctx, op)?,
            };
            let spec = default_spec(ctx, provider, op)?;
            ResolvedModel { id: spec.id.to_string(), spec, source: CapabilitySource::Catalog }
        }
    };
    let resolved = match resolved.source {
        CapabilitySource::Borrowed { .. } => {
            ResolvedModel { spec: without_prices(resolved.spec), ..resolved }
        }
        CapabilitySource::Catalog => resolved,
    };
    if let CapabilitySource::Borrowed { from } = resolved.source {
        warnings.push(Warning::new(
            "unverified_model_capabilities",
            format!(
                "'{}' is not in Iris's catalog; its capabilities are assumed to be those of '{from}' \
                 (unverified), so the provider may reject options or inputs Iris accepted",
                resolved.id
            ),
        ));
    }
    if resolved.spec.lifecycle == Lifecycle::Preview {
        warnings.push(Warning::new(
            "preview_model",
            format!(
                "{} is a preview model; its behavior, limits, and availability may change",
                resolved.spec.id
            ),
        ));
    }
    Ok(resolved)
}

fn default_provider(ctx: &AppContext, op: Operation) -> Result<ProviderId, IrisError> {
    if !op.is_async_job() {
        return Ok(ctx.settings.image_provider.value);
    }
    let providers = ctx.catalog.providers_for(op);
    providers
        .iter()
        .copied()
        .find(|p| *p == ProviderId::Gemini)
        .or_else(|| providers.first().copied())
        .ok_or_else(|| {
            IrisError::new(ErrorCode::UnsupportedOperation, format!("no model in this build supports {op}"))
                .with_hint("run `iris models list`")
        })
}

/// The model a generation command uses for `op` on `provider` when no `--model` is
/// given: the configured `providers.<provider>.image_model`/`video_model` if set,
/// else the catalog default. `None` when there is none for this provider and
/// operation.
pub(crate) fn effective_default(
    ctx: &AppContext,
    provider: ProviderId,
    op: Operation,
) -> Option<&'static ModelSpec> {
    default_spec(ctx, provider, op).ok().filter(|m| m.provider == provider && m.supports(op))
}

fn default_spec(
    ctx: &AppContext,
    provider: ProviderId,
    op: Operation,
) -> Result<&'static ModelSpec, IrisError> {
    let settings = ctx.settings.provider(provider);
    let (configured, kind) = if op.is_async_job() {
        (&settings.video_model, "video_model")
    } else {
        (&settings.image_model, "image_model")
    };
    if configured.source != SettingSource::Default
        && let Some(id) = &configured.value
    {
        return ctx.catalog.find(id).ok_or_else(|| {
            IrisError::new(
                ErrorCode::UnknownModel,
                format!("the configured default model '{id}' (providers.{provider}.{kind}) is not known"),
            )
            .with_hint("run `iris models list`, or pass --model")
        });
    }
    ctx.catalog.default_model(provider, op).ok_or_else(|| {
        let others: Vec<String> =
            ctx.catalog.providers_for(op).into_iter().map(|p| p.as_str().to_string()).collect();
        let hint = if others.is_empty() {
            format!("no model in this build supports {op}")
        } else {
            format!("providers with a model for {op}: {}", others.join(", "))
        };
        IrisError::new(
            ErrorCode::UnsupportedOperation,
            format!("provider '{provider}' has no model for {op}"),
        )
        .with_hint(hint)
    })
}

/// Prompt checks that need the model: non-empty, and within the declared limit.
pub(crate) fn check_prompt(spec: &ModelSpec, prompt: &str) -> Result<(), IrisError> {
    if prompt.trim().is_empty() {
        return Err(IrisError::invalid("the prompt is empty"));
    }
    if let Some(max) = spec.limits.max_prompt_chars {
        let chars = prompt.chars().count();
        if chars > max {
            return Err(IrisError::invalid(format!(
                "the prompt is {chars} characters long; model '{}' accepts at most {max}",
                spec.id
            ))
            .with_detail("prompt_chars", chars as u64)
            .with_detail("max_prompt_chars", max as u64));
        }
    }
    Ok(())
}

/// Local checks that need the request as a whole: the model's rules relating
/// several inputs (a mask's dimensions, the cap on an inline request), and an
/// explicit output format that contradicts the `-o` extension, named by where it
/// came from. Called by every generation command after its inputs are read and
/// before a dry run returns or a credential is needed, so `--dry-run` rejects what
/// the real run would reject before sending.
pub(crate) fn check_request<'a>(
    spec: &ModelSpec,
    common: &GenerationArgs,
    opts: &ResolvedOptions,
    inputs: impl IntoIterator<Item = &'a InputImage>,
) -> Result<(), IrisError> {
    crate::artifacts::check_request_inputs(&spec.inputs, &common.prompt, opts, inputs)?;
    check_output_format(common, opts)
}

/// `-o x.png` with an explicit `--format jpeg` or `-O format=jpeg` is a
/// contradiction; the message names the flag the user actually gave.
fn check_output_format(common: &GenerationArgs, opts: &ResolvedOptions) -> Result<(), IrisError> {
    let (Some(output), Some(format)) =
        (common.output.as_deref(), opts.get("format").and_then(|v| v.as_str()))
    else {
        return Ok(());
    };
    let Some(ext) = output.extension().and_then(|e| e.to_str()) else { return Ok(()) };
    let (Some(ext_type), Some(format_type)) =
        (media::media_type_for_extension(ext), paths::media_type_for_format(format))
    else {
        return Ok(());
    };
    if ext_type == format_type {
        return Ok(());
    }
    let given = match common.options.iter().find(|o| o.name == "format").map(|o| o.source) {
        Some(OptionSource::Flag(flag)) => format!("{flag} {format}"),
        Some(OptionSource::Generic) | None => format!("-O format={format}"),
    };
    Err(IrisError::invalid(format!("-o/--output extension '.{ext}' contradicts {given}"))
        .with_hint("make the -o extension and the requested format agree, or give only one of them")
        .with_detail("option", "format"))
}

/// Number of outputs requested: the explicit or default `count`, else 1.
pub(crate) fn effective_count(spec: &ModelSpec, op: Operation, opts: &ResolvedOptions) -> u32 {
    if !spec.options_for(op).any(|o| o.name == "count") {
        return 1;
    }
    spec.effective(opts, "count")
        .and_then(|v| v.as_int())
        .map(|n| n.clamp(1, i64::from(u32::MAX)) as u32)
        .unwrap_or(1)
}

/// The model's cost estimate for this request, if it can support one.
pub(crate) fn estimate(
    spec: &ModelSpec,
    op: Operation,
    opts: &ResolvedOptions,
    count: u32,
) -> Option<CostEstimate> {
    spec.estimate.and_then(|f| f(spec, &EstimateInput { operation: op, options: opts, count }))
}

/// Warning `cost_estimate_unavailable`.
pub(crate) fn cost_unavailable(spec: &ModelSpec) -> Warning {
    let message = match borrowed_template(spec) {
        Some(template) => format!(
            "no cost estimate: the model was given the capabilities of '{}' with --capabilities-from, and \
             that model's prices are not assumed to apply to it; check the provider's published prices",
            template.id
        ),
        None => format!(
            "no cost estimate is available for this request; see `iris models show {}` for the published prices",
            spec.id
        ),
    };
    Warning::new("cost_estimate_unavailable", message)
}

/// Specs of models resolved with `--capabilities-from`, one per template for the
/// life of the process: `(template, copy without prices)`.
static BORROWED: Mutex<Vec<(&'static ModelSpec, &'static ModelSpec)>> = Mutex::new(Vec::new());

/// `template` without price rules or cost estimators (capabilities, defaults, and
/// validation unchanged). The copy is created once per template and kept, so the
/// memory used is bounded by the catalog size.
fn without_prices(template: &'static ModelSpec) -> &'static ModelSpec {
    let mut borrowed = BORROWED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((_, copy)) = borrowed.iter().find(|(t, _)| std::ptr::eq(*t, template)) {
        return copy;
    }
    let copy: &'static ModelSpec =
        Box::leak(Box::new(ModelSpec { pricing: &[], estimate: None, estimate_usage: None, ..*template }));
    borrowed.push((template, copy));
    copy
}

/// The template of a spec made by [`without_prices`], if `spec` is one.
fn borrowed_template(spec: &ModelSpec) -> Option<&'static ModelSpec> {
    let borrowed = BORROWED.lock().unwrap_or_else(PoisonError::into_inner);
    borrowed.iter().find(|(_, copy)| std::ptr::eq(*copy, spec)).map(|(template, _)| *template)
}

/// The options a request runs with: every explicit value, plus the declared
/// default of each other option of `op` that has one. Veo is sent these values
/// (see the model catalog), and they are what a cost estimate is computed from; for other models
/// they are the documented provider defaults. Only for display and job records;
/// adapters get the explicit [`ResolvedOptions`] unchanged.
pub(crate) fn effective_options(spec: &ModelSpec, op: Operation, opts: &ResolvedOptions) -> ResolvedOptions {
    let mut all = opts.clone();
    for option in spec.options_for(op) {
        if !all.contains(option.name)
            && let Some(value) = spec.effective(opts, option.name)
        {
            all.insert(option.name, value);
        }
    }
    all
}

/// Options as shown in plans: the [`effective_options`], with free-text options
/// as `{sha256, chars}` unless prompt storage is enabled (same rule as job
/// records).
pub(crate) fn options_view(
    spec: &ModelSpec,
    op: Operation,
    opts: &ResolvedOptions,
    store_prompts: bool,
) -> Map<String, Value> {
    let mut map = jobs::request_metadata(
        spec,
        &effective_options(spec, op, opts),
        &InputCounts::default(),
        store_prompts,
    );
    map.remove("input_counts");
    map
}

/// One input of a dry-run plan.
pub(crate) fn plan_input(img: &InputImage) -> PlanInput {
    PlanInput {
        role: role_name(img.role).to_string(),
        path: img.path.display().to_string(),
        media_type: img.media_type.clone(),
        bytes: img.bytes.len() as u64,
    }
}

fn role_name(role: InputRole) -> &'static str {
    match role {
        InputRole::Image => "image",
        InputRole::Mask => "mask",
        InputRole::FirstFrame => "first_frame",
        InputRole::LastFrame => "last_frame",
        InputRole::Reference => "reference",
    }
}
