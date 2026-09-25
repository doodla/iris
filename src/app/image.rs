//! `image generate` / `image edit`: synchronous provider calls. No job record is
//! created; the paid output is validated and saved before the command returns.
//!
//! Order of checks (everything local happens before the paid request):
//! model resolution → option/input validation against the catalog → prompt
//! length → input files → output planning and preflight (`output_exists`, output
//! directory checked but not created) → `--dry-run` plan → credential → output
//! directories created and proven writable → provider call → save every image
//! (never discarding paid output: a file that appeared meanwhile makes the image go
//! to `<stem>.<n>.<ext>`, and an image that cannot be saved there at all goes to
//! `<state_dir>/unsaved/`).

use std::path::PathBuf;

use crate::artifacts::{self, FinalizeMode, Naming, PathRequest};
use crate::catalog::{self, InputCounts, OptionSource, RawOption};
use crate::domain::{Artifact, JobStatus, Operation, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{ImageResult, PlanResult};
use crate::providers::{ImageOutput, ImageRequest, InputRole};

use super::context::AppContext;
use super::request::{self, GenerationArgs, GenerationOutcome};

/// Warning: a paid image could not be saved where it was requested and was saved
/// in `<state_dir>/unsaved/` instead. Listed in docs/json-contract.md's warning codes.
const WARNING_SAVED_ELSEWHERE: &str = "output_saved_elsewhere";

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
    run_checked(ctx, op, args, warnings).await.map_err(|mut e| {
        let charged = e.details.get("charge_possible").and_then(serde_json::Value::as_bool) == Some(true);
        if charged && e.retryable == Some(true) {
            e.retryable = Some(false);
        }
        e
    })
}

async fn run_checked(
    ctx: &AppContext,
    op: Operation,
    args: ImageArgs,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<ImageResult>, IrisError> {
    if !matches!(op, Operation::ImageGenerate | Operation::ImageEdit) {
        return Err(IrisError::internal(format!("{op} is not an image operation")));
    }
    let common = &args.common;
    let resolved = request::resolve_model(ctx, op, common, warnings)?;
    let spec = resolved.spec;
    let provider = spec.provider;

    let counts =
        InputCounts { images: args.images.len(), mask: args.mask.is_some(), ..InputCounts::default() };
    let mut raw = common.options.clone();
    let mut opts = catalog::validate_request(spec, op, &raw, counts)?;
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
        opts = catalog::validate_request(spec, op, &raw, counts)?;
    }
    warnings.extend(plan.warnings.iter().cloned());
    artifacts::preflight(&plan.paths, common.overwrite)?;
    // Check the output directories without creating anything yet: a real run
    // creates them only once the credential is known to be present.
    artifacts::preflight_dirs(&plan.paths, false)?;

    let adapter = ctx
        .provider(provider)?
        .image()
        .ok_or_else(|| IrisError::internal(format!("provider '{provider}' has no image adapter")))?;
    let pre_estimate = request::estimate(spec, op, &opts, count);

    if common.dry_run {
        if pre_estimate.is_none() {
            warnings.push(request::cost_unavailable(spec));
        }
        let inputs = images.iter().chain(mask.iter()).map(request::plan_input).collect();
        return Ok(GenerationOutcome::Planned(PlanResult {
            dry_run: true,
            provider,
            model: resolved.id.clone(),
            operation: op,
            async_job: false,
            options: request::options_view(spec, op, &opts, ctx.settings.store_prompts.value),
            inputs,
            outputs: plan.paths.iter().map(|p| p.display().to_string()).collect(),
            credential_present: ctx.settings.credential_present(provider),
            cost_estimate: pre_estimate,
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
        "Requesting {count} image{} from {provider} ({}); this is a paid request",
        if count == 1 { "" } else { "s" },
        resolved.id
    ));
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let call = async {
        match op {
            Operation::ImageEdit => adapter.edit(&req, &pctx).await,
            _ => adapter.generate(&req, &pctx).await,
        }
    };
    let output: ImageOutput = tokio::select! {
        result = call => result?,
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
    if output.images.is_empty() {
        return Err(IrisError::new(
            ErrorCode::ProviderBadResponse,
            "the provider reported success but returned no image",
        )
        .with_provider(provider)
        .with_provider_request_id(output.provider_request_id.clone())
        .with_detail("charge_possible", true)
        .with_hint(
            "the request completed, so the provider may have billed it; Iris did not retry automatically",
        ));
    }
    warnings.extend(output.warnings.iter().cloned());

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
    let saving = save_all(ctx, &output, &planned, warnings);
    if let Some(e) = saving.failure {
        let paths = |artifacts: &[Artifact]| artifacts.iter().map(|a| a.path.clone()).collect::<Vec<_>>();
        return Err(e
            .with_provider(provider)
            .with_provider_request_id(output.provider_request_id.clone())
            .with_detail("saved", paths(&saving.saved))
            .with_detail("fallback_paths", saving.elsewhere)
            .with_detail("charge_possible", true)
            .with_hint(
                "the provider completed this request and may have billed it; Iris did not retry \
                 automatically. Every image that could be saved is listed in details.saved \
                 (details.fallback_paths lists those saved in Iris's state directory instead of where \
                 they were requested)",
            ));
    }
    let saved = saving.saved;

    // Prefer the provider-reported usage (covers every returned image); fall back to
    // the pre-call estimate for the number of images actually returned.
    let from_usage = match (spec.estimate_usage, output.usage.as_ref()) {
        (Some(estimate_usage), Some(usage)) => estimate_usage(spec, usage),
        _ => None,
    };
    let cost_estimate = from_usage.or_else(|| {
        if returned == count { pre_estimate } else { request::estimate(spec, op, &opts, returned) }
    });
    if cost_estimate.is_none() {
        warnings.push(request::cost_unavailable(spec));
    }
    Ok(GenerationOutcome::Completed(ImageResult {
        provider,
        model: resolved.id,
        operation: op,
        status: JobStatus::Succeeded,
        created_at: created_at.to_string(),
        completed_at: ctx.now().to_string(),
        provider_request_id: output.provider_request_id,
        artifacts: saved,
        text: output.text,
        usage: output.usage,
        cost_estimate,
    }))
}

/// What happened to the images of one paid response.
struct Saving {
    /// Every saved image, at the requested location or in the fallback directory.
    saved: Vec<Artifact>,
    /// Paths of the images saved in the fallback directory instead.
    elsewhere: Vec<String>,
    /// The first image that could not be saved anywhere.
    failure: Option<IrisError>,
}

/// Save every returned image at its planned path (`planned`: paths and mode, or the
/// error that prevented planning them). A valid image that cannot be saved there
/// (an I/O failure after preflight) goes to `<state_dir>/unsaved/` with warning
/// `output_saved_elsewhere`: paid output is never discarded. Content that is not a
/// valid image stays `invalid_media` and is not saved.
fn save_all(
    ctx: &AppContext,
    output: &ImageOutput,
    planned: &Result<(Vec<PathBuf>, FinalizeMode), IrisError>,
    warnings: &mut Vec<Warning>,
) -> Saving {
    let run_id = ulid::Ulid::generate().to_string().to_ascii_lowercase();
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
            Err(e) if e.code == ErrorCode::InvalidMedia => e,
            Err(e) => {
                match artifacts::save_unsaved(&ctx.settings.state_dir.value, &run_id, index, &image.bytes) {
                    Ok(artifact) => {
                        let wanted = requested
                            .map_or("the requested location".to_string(), |p| p.display().to_string());
                        let message = format!(
                            "image {index} could not be saved to {wanted} ({}); it was saved to {} \
                             instead so the paid output is not lost",
                            e.message, artifact.path
                        );
                        warnings.push(Warning::new(WARNING_SAVED_ELSEWHERE, message));
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
