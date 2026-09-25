//! `image generate` / `image edit`: synchronous provider calls. No job record is
//! created; the paid output is validated and saved before the command returns.
//!
//! Order of checks (everything local happens before the paid request):
//! model resolution → option/input validation against the catalog → prompt
//! length → input files → output planning and preflight (`output_exists`, output
//! directory checked but not created) → `--dry-run` plan → credential → output
//! directories created and proven writable → provider call → save every image
//! (never discarding paid output: a file that appeared meanwhile makes the image go
//! to `<stem>.<n>.<ext>`, an image that cannot be saved there at all goes to
//! `<state_dir>/unsaved/`, and returned content that is not a valid image is kept
//! there as received, as `.bin`).

use std::path::PathBuf;

use crate::artifacts::{self, FinalizeMode, Naming, PathRequest};
use crate::catalog::{self, InputCounts, OptionSource, RawOption, ResolvedModel};
use crate::domain::{Artifact, JobStatus, ModelSource, Operation, Usage, Warning, WarningCode};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{ImageResult, PlanResult, PromptFingerprint};
use crate::providers::{ImageFailure, ImageOutput, ImageRequest, InputRole, UnusableOutput};

use super::context::AppContext;
use super::jobs::Commands;
use super::request::{self, GenerationArgs, GenerationOutcome};

/// Arguments of `image generate` / `image edit`.
#[derive(Debug, Clone, Default)]
pub struct ImageArgs {
    pub common: GenerationArgs,
    /// `-i, --image` inputs (edit only; at least one).
    pub images: Vec<PathBuf>,
    /// `--mask` (edit only, if the model declares mask support).
    pub mask: Option<PathBuf>,
}

/// Run `image generate` (`op = image.generate`) or `image edit` (`image.edit`).
/// Hints and warnings that name `iris` commands name the config file too when it
/// was chosen explicitly (see `Commands`).
///
/// No error it returns says both `details.charge_possible: true` and
/// `retryable: true`: a request that may already have been billed is never
/// presented as safe to run again.
pub async fn run(
    ctx: &AppContext,
    op: Operation,
    args: ImageArgs,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<ImageResult>, IrisError> {
    let start = warnings.len();
    let result = resolved_run(ctx, op, args, warnings).await.map_err(|mut e| {
        let charged = e.details.get("charge_possible").and_then(serde_json::Value::as_bool) == Some(true);
        if charged && e.retryable == Some(true) {
            e.retryable = Some(false);
        }
        e
    });
    Commands::of(ctx).finish(result, warnings, start)
}

/// Resolve the model, then run; every later error that names the model says where
/// it came from ([`request::with_model_source`]).
async fn resolved_run(
    ctx: &AppContext,
    op: Operation,
    args: ImageArgs,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<ImageResult>, IrisError> {
    if !matches!(op, Operation::ImageGenerate | Operation::ImageEdit) {
        return Err(IrisError::internal(format!("{op} is not an image operation")));
    }
    let (resolved, model_source) = request::resolve_model(ctx, op, &args.common, warnings)?;
    run_checked(ctx, op, args, resolved.clone(), model_source, warnings)
        .await
        .map_err(|e| request::with_model_source(e, &resolved, model_source, op))
}

async fn run_checked(
    ctx: &AppContext,
    op: Operation,
    args: ImageArgs,
    resolved: ResolvedModel,
    model_source: ModelSource,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<ImageResult>, IrisError> {
    let common = &args.common;
    let spec = resolved.spec;
    let provider = spec.provider;

    let counts =
        InputCounts { images: args.images.len(), mask: args.mask.is_some(), ..InputCounts::default() };
    let mut raw = common.options.clone();
    let mut opts = catalog::validate_request(spec, op, &raw, counts, &ctx.catalog.models())?;
    request::check_prompt(spec, &common.prompt)?;

    let images = args
        .images
        .iter()
        .map(|p| artifacts::read_input_image(p, InputRole::Image, &spec.inputs))
        .collect::<Result<Vec<_>, _>>()?;
    let mask = args
        .mask
        .as_deref()
        .map(|p| artifacts::read_input_image(p, InputRole::Mask, &spec.inputs))
        .transpose()?;
    request::check_request(spec, common, &opts, images.iter().chain(&mask))?;

    // Output planning. With no explicit format, a declared `format` option follows
    // the -o extension.
    let count = request::effective_count(spec, op, &opts);
    let format = opts.get("format").and_then(|v| v.as_str()).map(str::to_string);
    let out_dir = ctx.settings.output_dir.value.clone();
    fn path_request<'a>(
        count: u32,
        format: Option<&'a str>,
        output: Option<&'a std::path::Path>,
        dir: &'a std::path::Path,
        media_types: &'a [&'a str],
    ) -> PathRequest<'a> {
        PathRequest { naming: Naming::Image, count, output, dir, format, media_types }
    }
    let output_path = common.output.as_deref();
    let media_types = spec.outputs.media_types;
    let plan =
        artifacts::plan_outputs(&path_request(count, format.as_deref(), output_path, &out_dir, media_types))?;
    if format.is_none()
        && let Some(implied) = plan.implied_format
        && spec.options_for(op).any(|o| o.name == "format")
    {
        raw.push(RawOption {
            name: "format".to_string(),
            value: implied.to_string(),
            source: OptionSource::Flag("-o/--output extension"),
        });
        opts = catalog::validate_request(spec, op, &raw, counts, &ctx.catalog.models())?;
    }
    warnings.extend(plan.warnings.iter().cloned());
    // Without a format option the provider chooses the type, so the image may be
    // saved under another extension than planned.
    let provider_chooses_type = !spec.options_for(op).any(|o| o.name == "format") && media_types.len() > 1;
    if let Some(output) = output_path
        && provider_chooses_type
    {
        let instead = match plan.paths.as_slice() {
            [one] => {
                let others: Vec<String> = media_types
                    .iter()
                    .filter_map(|t| match artifacts::adjust_extension(one, t) {
                        (other, Some(_)) => Some(other.display().to_string()),
                        (_, None) => None,
                    })
                    .collect();
                format!("{} may be saved as {} instead", one.display(), others.join(" or "))
            }
            _ => format!(
                "each path planned from {} may be saved with the extension of another of those types",
                output.display()
            ),
        };
        warnings.push(Warning::new(
            WarningCode::OutputExtensionMayChange,
            format!(
                "{} takes no output format: the provider chooses the image type ({}), so {instead} (reported \
                 with output_extension_adjusted); without --overwrite, a file already there stops the run \
                 (output_exists)",
                resolved.id,
                media_types.join(", "),
            ),
        ));
    }
    artifacts::preflight(&plan.paths, common.overwrite)?;
    if provider_chooses_type {
        artifacts::preflight_other_types(&plan.paths, media_types, common.overwrite)?;
    }
    // Check the output directories without creating them (a dry run stops after
    // this): a real run creates them only once the credential is known to be present.
    artifacts::preflight_dirs(&plan.paths, false)?;

    let adapter = ctx
        .provider(provider)?
        .image()
        .ok_or_else(|| IrisError::internal(format!("provider '{provider}' has no image adapter")))?;
    let pre_estimate = request::estimate(&resolved, op, &opts, count);

    if common.dry_run {
        if let Err(reason) = &pre_estimate {
            warnings.push(request::cost_unavailable(reason));
        }
        let inputs = images.iter().chain(mask.iter()).map(request::plan_input).collect();
        return Ok(GenerationOutcome::Planned(PlanResult {
            dry_run: true,
            provider,
            model: resolved.id.clone(),
            model_source,
            operation: op,
            async_job: false,
            detach: false,
            wait: None,
            billing: spec.billing,
            options: request::options_view(spec, op, &opts, ctx.settings.store_prompts.value),
            inputs,
            outputs: plan.shown.clone(),
            credential_present: ctx.settings.credential_present(provider),
            cost_estimate: pre_estimate.ok(),
            prompt_fingerprint: PromptFingerprint::of(&common.prompt),
        }));
    }

    // The credential is checked after every local check but before any output
    // directory is created, so a missing key leaves nothing on disk.
    let pctx = ctx.provider_context(provider)?;
    ctx.settings.warn_non_default_base_url(provider, warnings);
    artifacts::preflight_dirs(&plan.paths, true)?;
    let req = ImageRequest {
        model: resolved.id.clone(),
        prompt: common.prompt.clone(),
        images,
        mask,
        options: opts.clone(),
    };
    let created_at = ctx.now();
    ctx.progress.line(format!(
        "Requesting {count} image{} from {provider} ({}); this is a {} request",
        if count == 1 { "" } else { "s" },
        request::progress_model(&resolved, model_source, op),
        spec.billing
    ));
    // Names every file this command may keep in the state directory.
    let run_id = ulid::Ulid::generate().to_string().to_ascii_lowercase();
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let call = async {
        match op {
            Operation::ImageEdit => adapter.edit(&req, &pctx).await,
            _ => adapter.generate(&req, &pctx).await,
        }
    };
    let output: ImageOutput = tokio::select! {
        result = call => match result {
            Ok(output) => output,
            Err(failure) => return Err(failed_call(ctx, &run_id, failure, &resolved, warnings)),
        },
        () = ctx.interrupt.after(seen) => {
            // Interrupted, but the outcome is as uncertain as a lost connection: not retryable.
            return Err(IrisError::new(
                ErrorCode::Interrupted,
                "interrupted while waiting for the provider's answer; no image was saved",
            )
            .with_provider(provider)
            .with_retryable(Some(false))
            .with_detail("charge_possible", true)
            .with_hint(
                "the provider may still have processed (and billed) the request; Iris did not retry it. \
                 Check your provider usage before running the command again",
            ));
        }
    };
    warnings.extend(output.warnings.iter().cloned());
    let kept = keep_unusable(ctx, &run_id, &output.unusable, warnings);
    if output.images.is_empty() {
        return Err(kept.report(
            IrisError::new(
                ErrorCode::ProviderBadResponse,
                "the provider reported success but returned no image",
            )
            .with_provider(provider)
            .with_provider_request_id(output.provider_request_id.clone())
            .with_detail("charge_possible", true)
            .with_hint(
                "the request completed, so the provider may have billed it; Iris did not retry automatically",
            ),
            Vec::new(),
        ));
    }

    // Save every returned image. If the provider returned another number of images
    // than planned, plan names for what arrived; those paths were not preflighted,
    // so they never overwrite anything.
    let returned = output.images.len() as u32;
    let planned = if returned == count {
        Ok((plan.paths.clone(), FinalizeMode::for_generated(common.overwrite)))
    } else {
        let format = opts.get("format").and_then(|v| v.as_str()).map(str::to_string);
        artifacts::plan_outputs(&path_request(
            returned,
            format.as_deref(),
            output_path,
            &out_dir,
            media_types,
        ))
        .map(|replanned| (replanned.paths, FinalizeMode::RenameOnConflict))
    };
    let saving = save_all(ctx, &run_id, &output, &planned, warnings);
    if let Some(e) = saving.failure {
        let paths = |artifacts: &[Artifact]| artifacts.iter().map(|a| a.path.clone()).collect::<Vec<_>>();
        let e = kept.report(e, saving.elsewhere);
        return Err(e
            .with_provider(provider)
            .with_provider_request_id(output.provider_request_id.clone())
            .with_detail("saved", paths(&saving.saved))
            .with_detail("charge_possible", true)
            .with_hint(
                "the provider completed this request and may have billed it; Iris did not retry \
                 automatically. Every image that could be saved is listed in details.saved; \
                 details.fallback_paths lists the files kept in Iris's state directory instead of where \
                 they were requested (content that is not a valid image is kept as received, as .bin)",
            ));
    }
    let saved = saving.saved;

    // Prefer the provider-reported usage (covers every returned image); fall back to
    // the pre-call estimate for the number of images actually returned.
    let from_usage = output.usage.as_ref().and_then(|usage| request::estimate_from_usage(&resolved, usage));
    let cost_estimate = match from_usage {
        Some(estimate) => Ok(estimate),
        None if returned == count => pre_estimate,
        None => request::estimate(&resolved, op, &opts, returned),
    };
    if let Err(reason) = &cost_estimate {
        warnings.push(request::cost_unavailable(reason));
    }
    Ok(GenerationOutcome::Completed(ImageResult {
        provider,
        model: resolved.id,
        model_source,
        operation: op,
        status: JobStatus::Succeeded,
        created_at: created_at.to_string(),
        completed_at: ctx.now().to_string(),
        provider_request_id: output.provider_request_id,
        artifacts: saved,
        text: output.text,
        usage: output.usage,
        cost_estimate: cost_estimate.ok(),
    }))
}

/// The error of a failed image call, once any paid content that came with it (items
/// of a completed response, none of them a usable image) is kept in the state
/// directory (`details.fallback_paths`, and a warning per file), and a cost
/// estimate is added from the usage the answer reported.
fn failed_call(
    ctx: &AppContext,
    run_id: &str,
    failure: ImageFailure,
    model: &ResolvedModel,
    warnings: &mut Vec<Warning>,
) -> IrisError {
    let ImageFailure { error, unusable } = failure;
    let error = with_usage_estimate(error, model);
    if unusable.is_empty() {
        return error;
    }
    keep_unusable(ctx, run_id, &unusable, warnings).report(error, Vec::new())
}

/// Where the content of returned items that are not usable images was kept.
struct Kept {
    /// Files written in the state directory.
    paths: Vec<String>,
    /// The first failure to keep an item's content.
    failure: Option<String>,
}

impl Kept {
    /// `error` with every file kept in the state directory in
    /// `details.fallback_paths` (these, then `more`), and the first failure to keep
    /// one in `details.fallback_error` unless the error already gives one.
    fn report(self, error: IrisError, more: Vec<String>) -> IrisError {
        let mut paths = self.paths;
        paths.extend(more);
        let error = error.with_detail("fallback_paths", paths);
        match self.failure {
            Some(failure) if !error.details.contains_key("fallback_error") => {
                error.with_detail("fallback_error", failure)
            }
            _ => error,
        }
    }
}

/// Keep the content of returned items that are not usable images (see
/// [`keep_raw`]). An item whose content cannot be kept either gets an
/// `output_item_unusable` warning saying so.
fn keep_unusable(
    ctx: &AppContext,
    run_id: &str,
    unusable: &[UnusableOutput],
    warnings: &mut Vec<Warning>,
) -> Kept {
    let mut kept = Kept { paths: Vec::new(), failure: None };
    for u in unusable {
        match keep_raw(ctx, run_id, u.item, &u.bytes, "is not a usable image", warnings) {
            Ok(path) => kept.paths.push(path),
            Err(e) => {
                warnings.push(Warning::new(
                    WarningCode::OutputItemUnusable,
                    format!(
                        "the content of response item {} could not be kept either: {}",
                        u.item, e.message
                    ),
                ));
                kept.failure.get_or_insert(e.message.clone());
            }
        }
    }
    kept
}

/// Save paid content that is not a valid image exactly as received, to
/// `<state_dir>/unsaved/<run_id>-<item>.bin` (`item`: its position in the response,
/// so names never collide within a command), with an `output_saved_elsewhere`
/// warning naming the file: paid output is never discarded. These files are not
/// artifacts, since they hold no valid image. Returns the file's path.
fn keep_raw(
    ctx: &AppContext,
    run_id: &str,
    item: usize,
    bytes: &[u8],
    what: &str,
    warnings: &mut Vec<Warning>,
) -> Result<String, IrisError> {
    let n = u32::try_from(item).unwrap_or(u32::MAX);
    let path = artifacts::save_unsaved_raw(&ctx.settings.state_dir.value, run_id, n, bytes)?;
    let path = path.display().to_string();
    warnings.push(Warning::new(
        WarningCode::OutputSavedElsewhere,
        format!(
            "response item {item} {what}; its content was saved as received to {path} so the paid output \
             is not lost"
        ),
    ));
    Ok(path)
}

/// An error built from a completed answer carries the usage that answer reported in
/// `details.usage` (billed for sure when `details.charged`, possibly when
/// `details.charge_possible`); add the cost estimate computed from it
/// (`details.cost_estimate`) when the model has one.
fn with_usage_estimate(e: IrisError, model: &ResolvedModel) -> IrisError {
    let usage = e.details.get("usage").cloned().and_then(|u| serde_json::from_value::<Usage>(u).ok());
    match usage.and_then(|usage| request::estimate_from_usage(model, &usage)) {
        Some(estimate) => match serde_json::to_value(estimate) {
            Ok(estimate) => e.with_detail("cost_estimate", estimate),
            Err(_) => e,
        },
        None => e,
    }
}

/// What happened to the images of one paid response.
struct Saving {
    /// Every saved image, at the requested location or in the fallback directory.
    saved: Vec<Artifact>,
    /// Paths of the files saved in the fallback directory instead: images, and the
    /// raw bytes of content that is not a valid image.
    elsewhere: Vec<String>,
    /// The first image that could not be saved anywhere.
    failure: Option<IrisError>,
}

/// Save every returned image at its planned path (`planned`: paths and mode, or the
/// error that prevented planning them). A valid image that cannot be saved there
/// (an I/O failure after preflight) goes to `<state_dir>/unsaved/` with warning
/// `output_saved_elsewhere`: paid output is never discarded. Content that is not a
/// valid image stays `invalid_media`, and its bytes are kept as received (see
/// [`keep_raw`]; listed with the fallback paths).
fn save_all(
    ctx: &AppContext,
    run_id: &str,
    output: &ImageOutput,
    planned: &Result<(Vec<PathBuf>, FinalizeMode), IrisError>,
    warnings: &mut Vec<Warning>,
) -> Saving {
    let mut saving = Saving { saved: Vec::new(), elsewhere: Vec::new(), failure: None };
    for (index, image) in output.images.iter().enumerate() {
        let index = index as u32;
        let requested = planned.as_ref().ok().and_then(|(paths, _)| paths.get(index as usize));
        let attempt = match (planned, requested) {
            (Ok((_, mode)), Some(path)) => artifacts::save_image(&image.bytes, path, index, *mode),
            (Err(e), _) => Err(e.clone()),
            (Ok(_), None) => {
                Err(IrisError::internal(format!("no output path was planned for image {index}")))
            }
        };
        let error = match attempt {
            Ok(artifact) => {
                warnings.extend(artifact.warnings);
                saving.saved.push(artifact.artifact);
                continue;
            }
            Err(e) if e.code == ErrorCode::InvalidMedia => {
                let what = format!("(image {index}) cannot be saved as an image ({})", e.message);
                match keep_raw(ctx, run_id, image.item, &image.bytes, &what, warnings) {
                    Ok(path) => {
                        saving.elsewhere.push(path);
                        e
                    }
                    Err(fallback) => e.with_detail("fallback_error", fallback.message.clone()),
                }
            }
            Err(e) => {
                match artifacts::save_unsaved(&ctx.settings.state_dir.value, run_id, index, &image.bytes) {
                    Ok(artifact) => {
                        let wanted = requested
                            .map_or("the requested location".to_string(), |p| p.display().to_string());
                        let message = format!(
                            "image {index} could not be saved to {wanted} ({}); it was saved to {} \
                             instead so the paid output is not lost",
                            e.message, artifact.path
                        );
                        warnings.push(Warning::new(WarningCode::OutputSavedElsewhere, message));
                        saving.elsewhere.push(artifact.path.clone());
                        saving.saved.push(artifact);
                        continue;
                    }
                    Err(fallback) => e.with_detail("fallback_error", fallback.message.clone()),
                }
            }
        };
        if saving.failure.is_none() {
            saving.failure = Some(error.with_detail("index", index));
        }
    }
    saving
}
