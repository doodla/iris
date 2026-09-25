//! Steps shared by every generation workflow (image generate/edit, video
//! generate): model resolution (docs/configuration.md), prompt limits, output counts,
//! cost estimates, and dry-run plan pieces.

use std::fmt;
use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::artifacts::{media, paths};
use crate::catalog::{
    self, CapabilitySource, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionSource, RawOption,
    ResolvedModel, ResolvedOptions,
};
use crate::domain::{CostEstimate, ModelSource, Operation, Usage, Warning, WarningCode};
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

/// Resolve the model (docs/configuration.md): `-m/--model` if given, else the
/// model the config file names for `op` (`image.model` / `video.model`), else
/// `model_required`. Iris never chooses a model itself; the provider is the model's.
/// Returns the model and where it came from, and adds the warnings
/// `unverified_model_capabilities` and `preview_model`.
///
/// A model resolved with `--capabilities-from` runs against the template's
/// capabilities but not its prices: [`estimate`] and [`estimate_from_usage`] give
/// it no cost estimate (before or after the call), and [`estimate`] says why.
pub(crate) fn resolve_model(
    ctx: &AppContext,
    op: Operation,
    args: &GenerationArgs,
    warnings: &mut Vec<Warning>,
) -> Result<(ResolvedModel, ModelSource), IrisError> {
    let (resolved, source) = match args.model.as_deref() {
        Some(model) => {
            let resolved = ctx.catalog.resolve(model, args.capabilities_from.as_deref(), op)?;
            if !resolved.spec.supports(op) {
                let supported: Vec<&str> = resolved.spec.operations.iter().map(|o| o.as_str()).collect();
                return Err(IrisError::new(
                    ErrorCode::UnsupportedOperation,
                    format!(
                        "model '{}' does not support {op} (supports: {})",
                        resolved.spec.id,
                        supported.join(", ")
                    ),
                )
                .with_hint(format!("run `iris models list --operation {op}` and pass -m <MODEL>"))
                .with_detail("operation", op.as_str())
                .with_detail("candidates", ctx.catalog.candidates(Some(op))));
            }
            (resolved, ModelSource::Flag)
        }
        None => {
            if args.capabilities_from.is_some() {
                return Err(IrisError::usage(
                    "--capabilities-from applies to an unknown --model; give --model <MODEL> as well",
                ));
            }
            (configured_model(ctx, op)?, ModelSource::Config)
        }
    };
    if let CapabilitySource::Borrowed { from } = resolved.source {
        // An id that nearly names catalog models is more likely a slip than a new
        // model; it is still sent as typed (a new model can look like a near miss).
        let near: Vec<String> = catalog::suggestions(&ctx.catalog.models(), &resolved.id, Some(op))
            .iter()
            .map(|id| format!("-m {id}"))
            .collect();
        let slip = match near.as_slice() {
            [] => String::new(),
            [one] => format!("; did you mean {one}? --capabilities-from sends '{}' as typed", resolved.id),
            [first, rest @ ..] => format!(
                "; did you mean {first}{}? --capabilities-from sends '{}' as typed",
                rest.iter().map(|m| format!(" or {m}")).collect::<String>(),
                resolved.id
            ),
        };
        warnings.push(Warning::new(
            WarningCode::UnverifiedModelCapabilities,
            format!(
                "'{}' is not in Iris's catalog; its capabilities are assumed to be those of '{from}' \
                 (unverified), so the provider may reject options or inputs Iris accepted{slip}",
                resolved.id
            ),
        ));
    }
    if resolved.spec.lifecycle == Lifecycle::Preview {
        warnings.push(Warning::new(
            WarningCode::PreviewModel,
            format!(
                "{} is a preview model; its behavior, limits, and availability may change",
                resolved.spec.id
            ),
        ));
    }
    Ok((resolved, source))
}

/// The model as progress lines name it: its id, followed by the config key when the
/// config file chose it (`gemini-3.1-flash-image, config image.model`).
pub(crate) fn progress_model(model: &ResolvedModel, source: ModelSource, op: Operation) -> String {
    match source {
        ModelSource::Flag => model.id.clone(),
        ModelSource::Config => format!("{}, config {}", model.id, op.model_config_key()),
    }
}

/// The model the config file names for `op` (`image.model` or `video.model`, already
/// checked to be a catalog model of the right kind when the settings loaded), which
/// must implement `op`; `model_required` when the file names none.
fn configured_model(ctx: &AppContext, op: Operation) -> Result<ResolvedModel, IrisError> {
    let key = op.model_config_key();
    let Some(id) = &ctx.settings.model(op).value else {
        return Err(model_required(ctx, op));
    };
    let resolved = ctx.catalog.resolve(id, None, op)?;
    if !resolved.spec.supports(op) {
        let supported: Vec<&str> = resolved.spec.operations.iter().map(|o| o.as_str()).collect();
        return Err(IrisError::new(
            ErrorCode::UnsupportedOperation,
            format!(
                "the config file's {key} is '{id}', which does not support {op} (supports: {})",
                supported.join(", ")
            ),
        )
        .with_hint(format!(
            "pass -m <MODEL> for {op} (`iris models list --operation {op}`), or set {key} to a model that \
             supports it"
        ))
        .with_detail("config_key", key)
        .with_detail("operation", op.as_str())
        .with_detail("candidates", ctx.catalog.candidates(Some(op))));
    }
    Ok(resolved)
}

/// `model_required`: neither `-m/--model` nor the config file names a model for
/// `op`. Nothing was sent.
fn model_required(ctx: &AppContext, op: Operation) -> IrisError {
    let key = op.model_config_key();
    let table = key.split_once('.').map_or(key, |(table, _)| table);
    let config_file = ctx.settings.config_file.value.display().to_string();
    IrisError::new(
        ErrorCode::ModelRequired,
        format!("{op} needs a model: pass -m/--model, or set model in the [{table}] table of the config file"),
    )
    .with_hint(format!(
        "run `iris models list --operation {op}` and pass -m <MODEL>, or set model under [{table}] in {config_file}"
    ))
    .with_detail("operation", op.as_str())
    .with_detail("config_key", key)
    .with_detail("config_file", config_file)
    .with_detail("candidates", ctx.catalog.candidates(Some(op)))
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
    let path = paths::absolute(output)?.to_string_lossy().into_owned();
    Err(IrisError::invalid(format!("-o/--output extension '.{ext}' contradicts {given}"))
        .with_hint("make the -o extension and the requested format agree, or give only one of them")
        .with_detail("option", "format")
        .with_detail("path", path))
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

/// The model's cost estimate for this request, or why there is none and how to get
/// one (the model's own estimator says which options to pass), as the
/// `cost_estimate_unavailable` warning reports it ([`cost_unavailable`]). A model
/// resolved with `--capabilities-from` has none: it borrows the template's
/// capabilities, not its prices.
pub(crate) fn estimate(
    model: &ResolvedModel,
    op: Operation,
    opts: &ResolvedOptions,
    count: u32,
) -> Result<CostEstimate, String> {
    if let CapabilitySource::Borrowed { from } = model.source {
        return Err(format!(
            "the model was given the capabilities of '{from}' with --capabilities-from, and that model's prices \
             are not assumed to apply to it; check the provider's published prices"
        ));
    }
    let spec = model.spec;
    let Some(estimator) = spec.estimate else {
        return Err(format!(
            "Iris cannot estimate the cost of {} before the call; `iris models show {}` lists its published prices",
            spec.id, spec.id
        ));
    };
    (estimator.estimate)(spec, &EstimateInput { operation: op, options: opts, count })
}

/// The cost estimate from the usage a provider reported for a completed call (it
/// covers every output of the response), if the model supports one. Like
/// [`estimate`], never for a model resolved with `--capabilities-from`.
pub(crate) fn estimate_from_usage(model: &ResolvedModel, usage: &Usage) -> Option<CostEstimate> {
    let spec = priced(model)?;
    spec.estimate_usage.and_then(|f| f(spec, usage))
}

/// The spec whose prices apply to `model`: its own for a catalog model, none for a
/// model that borrowed another's capabilities.
fn priced(model: &ResolvedModel) -> Option<&'static ModelSpec> {
    match model.source {
        CapabilitySource::Catalog => Some(model.spec),
        CapabilitySource::Borrowed { .. } => None,
    }
}

/// Warning `cost_estimate_unavailable`, with the reason [`estimate`] gave.
pub(crate) fn cost_unavailable(reason: &str) -> Warning {
    Warning::new(WarningCode::CostEstimateUnavailable, format!("no cost estimate: {reason}"))
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
