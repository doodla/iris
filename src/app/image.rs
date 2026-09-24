//! `image generate` / `image edit`: synchronous provider calls. No job record is
//! created; the paid output is validated and saved before the command returns.
//!
//! Order of checks (everything local happens before the paid request):
//! model resolution → option/input validation against the catalog → prompt
//! length → input files → output planning and preflight (`output_exists`, output
//! directory) → `--dry-run` plan → credential → provider call → save every image
//! (never discarding paid output: a file that appeared meanwhile makes the image go
//! to `<stem>.<n>.<ext>`).

use std::path::PathBuf;

use crate::artifacts::{self, FinalizeMode, Naming, PathRequest};
use crate::catalog::{self, InputCounts, OptionSource, RawOption};
use crate::domain::{JobStatus, Operation, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::output::results::{ImageResult, PlanResult};
use crate::providers::{ImageOutput, ImageRequest, InputRole};

use super::context::AppContext;
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
pub async fn run(
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

    // Output planning. With no explicit format, a declared `format` option follows
    // the -o extension (C-02).
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
    artifacts::preflight_dirs(&plan.paths, !common.dry_run)?;

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

    let pctx = ctx.provider_context(provider)?;
    let req = ImageRequest {
        operation: op,
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
            return Err(IrisError::new(
                ErrorCode::Interrupted,
                "interrupted while waiting for the provider's answer; no image was saved",
            )
            .with_provider(provider)
            .with_detail("charge_possible", true)
            .with_hint("the provider may still have processed (and billed) the request; Iris did not retry it"));
        }
    };
    if output.images.is_empty() {
        return Err(IrisError::new(
            ErrorCode::ProviderBadResponse,
            "the provider reported success but returned no image",
        )
        .with_provider(provider)
        .with_provider_request_id(output.provider_request_id.clone()));
    }
    warnings.extend(output.warnings.iter().cloned());

    // Save every returned image. If the provider returned another number of images
    // than planned, plan names for what arrived; those paths were not preflighted,
    // so they never overwrite anything.
    let returned = output.images.len() as u32;
    let (paths, mode) = if returned == count {
        (plan.paths.clone(), FinalizeMode::for_generated(common.overwrite))
    } else {
        let format = opts.get("format").and_then(|v| v.as_str()).map(str::to_string);
        let replanned = artifacts::plan_outputs(&path_request(
            returned,
            format.as_deref(),
            output_path,
            &out_dir,
            media_types,
        ))?;
        (replanned.paths, FinalizeMode::RenameOnConflict)
    };
    let mut saved = Vec::new();
    let mut failure: Option<IrisError> = None;
    for (index, (image, path)) in output.images.iter().zip(&paths).enumerate() {
        match artifacts::save_image(&image.bytes, path, index as u32, mode) {
            Ok(artifact) => {
                warnings.extend(artifact.warnings);
                saved.push(artifact.artifact);
            }
            Err(e) => {
                if failure.is_none() {
                    failure = Some(e.with_detail("index", index as u64));
                }
            }
        }
    }
    if let Some(e) = failure {
        let saved_paths: Vec<String> = saved.iter().map(|a| a.path.clone()).collect();
        return Err(e
            .with_provider(provider)
            .with_provider_request_id(output.provider_request_id.clone())
            .with_detail("saved", saved_paths));
    }

    let cost_estimate =
        if returned == count { pre_estimate } else { request::estimate(spec, op, &opts, returned) };
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
