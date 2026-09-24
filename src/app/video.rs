//! `video generate`: a provider-native asynchronous job (C-04).
//!
//! The job record is written (`submitting`) BEFORE the paid submission, so a crash
//! in the uncertainty window is detectable later (`submission_unknown`). Outcomes:
//! accepted → `running`; definite rejection → `failed`; ambiguous (sent, no usable
//! answer) → `submission_unknown`, error `submission_uncertain`, exit 5, never
//! resubmitted.
//!
//! Ctrl-C during the submission is deferred once, until the provider answers, so
//! the operation id gets recorded (then exit 130 with the job `running`); a second
//! Ctrl-C exits at once and leaves the record `submitting` (later reported as
//! `submission_unknown`). With `--detach` the command returns after submission;
//! otherwise it waits and downloads exactly like `jobs wait`.

use std::path::PathBuf;

use crate::artifacts::{self, Naming, PathRequest, media};
use crate::catalog::{self, InputCounts};
use crate::domain::{JobStatus, Operation, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::jobs::{self, JobId, JobRecord, NewJob, OutputPlan, PromptRecord};
use crate::output::results::{JobResult, PlanResult};
use crate::providers::{InputRole, VideoRequest};

use super::context::AppContext;
use super::jobs::{Target, WaitArgs, job_result, wait_parsed, with_job_context};
use super::request::{self, GenerationArgs, GenerationOutcome};

/// Arguments of `video generate`.
#[derive(Debug, Clone, Default)]
pub struct VideoArgs {
    pub common: GenerationArgs,
    /// `--image`: first frame (image-to-video).
    pub first_frame: Option<PathBuf>,
    /// `--last-frame`.
    pub last_frame: Option<PathBuf>,
    /// `--ref` reference images.
    pub references: Vec<PathBuf>,
    /// `--detach`: submit, record, and return.
    pub detach: bool,
}

/// Run `video generate`.
pub async fn run(
    ctx: &AppContext,
    args: VideoArgs,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<JobResult>, IrisError> {
    let op = Operation::VideoGenerate;
    let common = &args.common;
    let resolved = request::resolve_model(ctx, op, common, warnings)?;
    let spec = resolved.spec;
    let provider = spec.provider;

    let counts = InputCounts {
        first_frame: args.first_frame.is_some(),
        last_frame: args.last_frame.is_some(),
        references: args.references.len(),
        ..InputCounts::default()
    };
    let opts = catalog::validate_request(spec, op, &common.options, counts)?;
    request::check_prompt(spec, &common.prompt)?;
    let first_frame = args
        .first_frame
        .as_deref()
        .map(|p| artifacts::read_input_image(p, InputRole::FirstFrame, &spec.inputs))
        .transpose()?;
    let last_frame = args
        .last_frame
        .as_deref()
        .map(|p| artifacts::read_input_image(p, InputRole::LastFrame, &spec.inputs))
        .transpose()?;
    let references = args
        .references
        .iter()
        .map(|p| artifacts::read_input_image(p, InputRole::Reference, &spec.inputs))
        .collect::<Result<Vec<_>, _>>()?;

    // Output planning: validates -o now; when this run downloads, the paths are
    // preflighted before anything is sent.
    let count = request::effective_count(spec, op, &opts);
    let job_id = JobId::generate();
    let out_dir = ctx.settings.output_dir.value.clone();
    let plan = artifacts::plan_outputs(&PathRequest {
        naming: Naming::Video { job_id: job_id.as_str() },
        count,
        output: common.output.as_deref(),
        dir: &out_dir,
        format: None,
        media_types: spec.outputs.media_types,
    })?;
    warnings.extend(plan.warnings.iter().cloned());
    if !args.detach {
        artifacts::preflight(&plan.paths, common.overwrite)?;
        artifacts::preflight_dirs(&plan.paths, !common.dry_run)?;
    }

    let video = ctx
        .provider(provider)?
        .video()
        .ok_or_else(|| IrisError::internal(format!("provider '{provider}' has no video adapter")))?;
    let estimate = request::estimate(spec, op, &opts, count);
    if estimate.is_none() {
        warnings.push(request::cost_unavailable(spec));
    }
    let store_prompts = ctx.settings.store_prompts.value;

    if common.dry_run {
        let inputs = first_frame
            .iter()
            .chain(last_frame.iter())
            .chain(references.iter())
            .map(request::plan_input)
            .collect();
        let outputs = if common.output.is_some() {
            plan.paths.iter().map(|p| p.display().to_string()).collect()
        } else {
            placeholder_outputs(&out_dir, plan.media_type, count)
        };
        return Ok(GenerationOutcome::Planned(PlanResult {
            dry_run: true,
            provider,
            model: resolved.id.clone(),
            operation: op,
            async_job: true,
            options: request::options_view(spec, &opts, store_prompts),
            inputs,
            outputs,
            credential_present: ctx.settings.credential_present(provider),
            cost_estimate: estimate,
        }));
    }

    let pctx = ctx.provider_context(provider)?;

    // Persist the record BEFORE the paid request.
    let (plan_dir, plan_path) = match &common.output {
        Some(path) => (None, Some(path.as_path())),
        None => (Some(out_dir.as_path()), None),
    };
    let record = JobRecord::with_id(
        job_id.clone(),
        NewJob {
            provider,
            model: resolved.id.clone(),
            operation: op,
            request: jobs::request_metadata(spec, &opts, &counts, store_prompts),
            prompt: PromptRecord::new(&common.prompt, store_prompts),
            output_plan: OutputPlan::new(plan_dir, plan_path, common.overwrite)?,
            cost_estimate: estimate,
        },
        ctx.now(),
    )?;
    ctx.store.create(&record)?;

    let req = VideoRequest {
        model: resolved.id.clone(),
        prompt: common.prompt.clone(),
        first_frame,
        last_frame,
        references,
        options: opts,
    };
    ctx.progress
        .line(format!("Submitting job {job_id} to {provider} ({}); this is a paid request", resolved.id));
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let mut deferred = false;
    let submission = {
        let submit = video.submit(&req, &pctx);
        tokio::pin!(submit);
        loop {
            tokio::select! {
                biased;
                result = &mut submit => break result,
                () = ctx.interrupt.after(seen), if !deferred => {
                    deferred = true;
                    ctx.progress.line(format!(
                        "Interrupt received: waiting for the provider to acknowledge job {job_id} so it can be \
                         recorded; press Ctrl-C again to stop immediately (the job may then be unrecoverable)"
                    ));
                }
                () = ctx.interrupt.after(seen + 1), if deferred => {
                    return Err(IrisError::new(
                        ErrorCode::Interrupted,
                        format!("interrupted while submitting job {job_id}; the provider's answer was not recorded"),
                    )
                    .with_job(job_id.to_string(), Some(JobStatus::Submitting))
                    .with_provider(provider)
                    .with_hint(format!(
                        "the provider may have accepted (and will bill) this request; `iris jobs status {job_id}` \
                         will report it as submission_unknown; check the provider console before resubmitting"
                    )));
                }
            }
        }
    };

    let now = ctx.now();
    match submission {
        Ok(operation) => {
            let (record, ()) = ctx.store.update(&job_id, |r| r.mark_submitted(&operation, now)).map_err(|e| {
                e.with_job(job_id.to_string(), Some(JobStatus::Submitting))
                    .with_remote_operation(operation.remote_id.clone())
                    .with_hint(
                        "the provider accepted the job but Iris could not record it; keep the remote operation id \
                         shown here, and do not resubmit",
                    )
            })?;
            ctx.progress.line(format!("Job {job_id} accepted by {provider}"));
            if deferred {
                return Err(with_job_context(
                    IrisError::new(
                        ErrorCode::Interrupted,
                        format!("interrupted; job {job_id} was submitted and continues remotely"),
                    )
                    .with_hint(format!("resume with `iris jobs wait {job_id}`")),
                    &record,
                ));
            }
            if args.detach {
                return Ok(GenerationOutcome::Completed(job_result(&record)));
            }
            let wait = WaitArgs { download: true, target: Target::default() };
            Ok(GenerationOutcome::Completed(wait_parsed(ctx, &job_id, &wait, warnings).await?))
        }
        Err(e) if is_uncertain(&e) => {
            let e = as_uncertain(e);
            ctx.store.update(&job_id, |r| r.mark_submission_unknown(&e, now))?;
            Err(e.with_job(job_id.to_string(), Some(JobStatus::SubmissionUnknown)))
        }
        Err(e) => {
            ctx.store.update(&job_id, |r| r.mark_rejected(&e, now))?;
            Err(e.with_job(job_id.to_string(), Some(JobStatus::Failed)))
        }
    }
}

/// A submission error that leaves open whether the provider accepted the job:
/// `submission_uncertain`, or any error flagged `charge_possible` (a transport
/// failure after the request was sent).
fn is_uncertain(e: &IrisError) -> bool {
    e.code == ErrorCode::SubmissionUncertain
        || e.details.get("charge_possible").and_then(serde_json::Value::as_bool) == Some(true)
}

fn as_uncertain(e: IrisError) -> IrisError {
    if e.code == ErrorCode::SubmissionUncertain {
        return e;
    }
    let mut uncertain = IrisError::new(
        ErrorCode::SubmissionUncertain,
        format!("the provider may have accepted this job: {}", e.message),
    );
    uncertain.provider = e.provider;
    uncertain.provider_status = e.provider_status;
    uncertain.provider_request_id = e.provider_request_id.clone();
    uncertain.details = e.details.clone();
    uncertain.with_hint(
        "the provider may have accepted this paid request; check usage/billing in the provider console before \
         resubmitting; Iris will not resubmit automatically",
    )
}

/// Planned output paths for a dry run, where the job id does not exist yet.
fn placeholder_outputs(dir: &std::path::Path, media_type: &str, count: u32) -> Vec<String> {
    let ext = media::extension_for(media_type).unwrap_or("bin");
    let name = |suffix: String| dir.join(format!("<job_id>{suffix}.{ext}")).display().to_string();
    if count == 1 { vec![name(String::new())] } else { (1..=count).map(|i| name(format!("-{i}"))).collect() }
}
