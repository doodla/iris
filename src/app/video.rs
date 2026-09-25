//! `video generate`: a provider-native asynchronous job (see docs/jobs.md).
//!
//! The job record is written (`submitting`) BEFORE the paid submission, so a crash
//! in the uncertainty window is detectable later (`submission_unknown`). Outcomes:
//! accepted → `running`; definite rejection → `failed`; ambiguous (sent, no usable
//! answer) → `submission_unknown`, error `submission_uncertain`, exit 5, never
//! resubmitted.
//!
//! An interrupt (Ctrl-C, SIGTERM, SIGHUP) during the submission is deferred once,
//! until the provider answers, so the operation id gets recorded (then exit 130
//! with the job `running`); a second one exits at once and leaves the record
//! `submitting` (reported as `submission_unknown` once the paid-submit budget has
//! passed). Both report `retryable: false` and `details.charge_possible: true`:
//! running the command again would submit another paid job. With `--detach`
//! the command returns after submission; otherwise it waits and downloads like
//! `jobs wait`, except that a file which appeared at the target since the
//! preflight never blocks saving the paid output (`<stem>.<n>.<ext>`,
//! `output_renamed`).
//!
//! Once the provider has been contacted, no outcome is reported with exit 2
//! ("nothing was sent"), and failing to update the local record never hides
//! whether the provider accepted the job.

use std::path::PathBuf;

use serde_json::json;

use crate::artifacts::{self, Naming, PathRequest, media};
use crate::catalog::{self, InputCounts};
use crate::domain::{JobStatus, Operation, ProviderId, Warning};
use crate::error::{ErrorCategory, ErrorCode, IrisError, exit};
use crate::jobs::{self, JobId, JobRecord, NewJob, OutputPlan, PromptRecord};
use crate::output::results::{JobResult, PlanResult};
use crate::providers::{InputRole, VideoRequest};

use super::context::AppContext;
use super::jobs::{
    Commands, SaveMode, Target, WaitArgs, job_result, submission_unknown_at, wait_parsed, with_job_context,
};
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

/// Run `video generate`. Hints and next steps that name `iris` commands name the
/// config file too when it was chosen explicitly (see `Commands`).
pub async fn run(
    ctx: &AppContext,
    args: VideoArgs,
    warnings: &mut Vec<Warning>,
) -> Result<GenerationOutcome<JobResult>, IrisError> {
    let start = warnings.len();
    let result = generate(ctx, args, warnings).await;
    Commands::of(ctx).finish(result, warnings, start)
}

async fn generate(
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
    request::check_request(spec, common, &opts, first_frame.iter().chain(&last_frame).chain(&references))?;

    // Output planning and preflight (see `iris --help`): every planned path is checked before
    // anything is sent, with --detach too, since the recorded plan is where
    // `iris jobs wait/download` will save the output later.
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
    artifacts::preflight(&plan.paths, common.overwrite)?;
    // Only checked here; directories are created once the credential is known to
    // be present (below), so a run that cannot be sent leaves nothing behind.
    artifacts::preflight_dirs(&plan.paths, false)?;

    let video = ctx
        .provider(provider)?
        .video()
        .ok_or_else(|| IrisError::internal(format!("provider '{provider}' has no video adapter")))?;
    let estimate = request::estimate(&resolved, op, &opts, count);
    if estimate.is_none() {
        warnings.push(request::cost_unavailable(&resolved));
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
            options: request::options_view(spec, op, &opts, store_prompts),
            inputs,
            outputs,
            credential_present: ctx.settings.credential_present(provider),
            cost_estimate: estimate,
        }));
    }

    let req = VideoRequest {
        model: resolved.id.clone(),
        prompt: common.prompt.clone(),
        first_frame,
        last_frame,
        references,
        options: opts.clone(),
    };
    // Everything the adapter would refuse before sending is refused now, before a
    // job record exists.
    video.validate(&req)?;
    let pctx = ctx.provider_context(provider)?;
    ctx.settings.warn_non_default_base_url(provider, warnings);
    artifacts::preflight_dirs(&plan.paths, true)?;

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
            request: jobs::request_metadata(
                spec,
                &request::effective_options(spec, op, &opts),
                &counts,
                store_prompts,
            ),
            prompt: PromptRecord::new(&common.prompt, store_prompts),
            output_plan: OutputPlan::new(plan_dir, plan_path, common.overwrite)?,
            cost_estimate: estimate,
        },
        ctx.now(),
    )?;
    // Interrupts (Ctrl-C, SIGTERM, SIGHUP) are handled from before the record
    // exists, so none can end the process by default between recording the job
    // and recording the provider's answer.
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let delivered = ctx.interrupt.delivered();
    ctx.store.create(&record)?;

    ctx.progress
        .line(format!("Submitting job {job_id} to {provider} ({}); this is a paid request", resolved.id));
    // One that arrived before the request is sent (while the record was written or
    // the line printed) stops here, without sending it. `count()` would miss it: the
    // runtime has not been polled since, so the task forwarding a signal has not run.
    // `delivered()` is noted by the OS signal handler itself.
    if ctx.interrupt.delivered() > delivered {
        let e = IrisError::new(
            ErrorCode::Interrupted,
            format!("interrupted before job {job_id} was sent to {provider}; nothing was submitted"),
        )
        .with_retryable(Some(true))
        .with_provider(provider)
        .with_hint("nothing was sent or billed; run the same command again to submit the request");
        return Err(discard_unsent(ctx, &job_id, e, ctx.now()));
    }
    // From here on the request may be in flight: the first interrupt is deferred
    // until the provider answers.
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
                         recorded; interrupt again (Ctrl-C) to stop immediately (the job may then be \
                         unrecoverable)"
                    ));
                }
                () = ctx.interrupt.after(seen + 1), if deferred => {
                    let unknown_at = submission_unknown_at(ctx, &record)
                        .map(|t| format!("until about {t}"))
                        .unwrap_or_else(|| "for a while".to_string());
                    // The request may have reached the provider: running the command
                    // again would submit (and bill) another job.
                    return Err(IrisError::new(
                        ErrorCode::Interrupted,
                        format!("interrupted while submitting job {job_id}; the provider's answer was not recorded"),
                    )
                    .with_retryable(Some(false))
                    .with_detail("charge_possible", true)
                    .with_job(job_id.to_string(), Some(JobStatus::Submitting))
                    .with_provider(provider)
                    .with_hint(format!(
                        "the provider may have accepted (and will bill) this request, but Iris cannot follow it \
                         without an operation id; `iris jobs status {job_id}` shows it as submitting {unknown_at} \
                         and as submission_unknown after that; check the provider console before resubmitting"
                    )));
                }
            }
        }
    };

    let now = ctx.now();
    match submission {
        Ok(operation) => {
            let record = match ctx.store.update(&job_id, |r| r.mark_submitted(&operation, now)) {
                Ok((record, ())) => record,
                Err(store_error) => {
                    return Err(accepted_but_unrecorded(
                        &job_id,
                        provider,
                        &operation.remote_id,
                        &store_error,
                    ));
                }
            };
            ctx.progress.line(format!("Job {job_id} accepted by {provider}"));
            if deferred {
                // The job exists and is billed; running the command again would
                // submit another one, so this is not retryable as is.
                return Err(with_job_context(
                    IrisError::new(
                        ErrorCode::Interrupted,
                        format!("interrupted; job {job_id} was submitted and continues remotely"),
                    )
                    .with_retryable(Some(false))
                    .with_detail("charge_possible", true)
                    .with_hint(format!(
                        "resume with `iris jobs wait {job_id}`; do not re-run `iris video generate`, which would \
                         submit and bill a new job"
                    )),
                    &record,
                ));
            }
            if args.detach {
                return Ok(GenerationOutcome::Completed(job_result(ctx, &record)));
            }
            // Save where this command was asked to (its own -o/-d/--overwrite).
            let wait = WaitArgs {
                download: true,
                target: Target { output: common.output.clone(), overwrite: common.overwrite },
            };
            match wait_parsed(ctx, &job_id, &wait, SaveMode::Generated, warnings).await {
                Ok(result) => Ok(GenerationOutcome::Completed(result)),
                Err(e) => Err(after_acceptance(ctx, e, &record)),
            }
        }
        Err(e) if is_uncertain(&e) => {
            let e = as_uncertain(e).with_job(job_id.to_string(), Some(JobStatus::SubmissionUnknown));
            match ctx.store.update(&job_id, |r| r.mark_submission_unknown(&e, now)) {
                Ok(_) => Err(e),
                // The uncertainty is the outcome that matters (exit 5, do not resubmit).
                Err(store_error) => Err(e.with_detail("record_error", record_error(&store_error))),
            }
        }
        // A local refusal inside the adapter (exit 2 without any provider status):
        // nothing was sent, so no job exists and its record is dropped.
        Err(e) if e.exit_code() == exit::USAGE && e.provider_status.is_none() => {
            Err(discard_unsent(ctx, &job_id, e, now))
        }
        Err(e) => {
            let e = e.with_job(job_id.to_string(), Some(JobStatus::Failed));
            match ctx.store.update(&job_id, |r| r.mark_rejected(&e, now)) {
                Ok(_) => Err(e),
                Err(store_error) => Err(e.with_detail("record_error", record_error(&store_error))),
            }
        }
    }
}

/// `e` for a job whose request was never sent: its just-created record is deleted
/// (a record would claim a submission that never happened), and `e` names no job.
/// If the record cannot be deleted, it is marked `failed` with `e` (still true:
/// nothing reached the provider) and `e` names the job.
fn discard_unsent(ctx: &AppContext, job_id: &JobId, e: IrisError, now: jiff::Timestamp) -> IrisError {
    match ctx.store.delete(job_id, true) {
        Ok(()) => e,
        Err(delete_error) => {
            let e = e.with_job(job_id.to_string(), Some(JobStatus::Failed));
            let recorded = ctx.store.update(job_id, |r| r.mark_rejected(&e, now)).map(|_| ());
            e.with_detail("record_error", record_error(recorded.as_ref().err().unwrap_or(&delete_error)))
        }
    }
}

/// `{code, message}` of a failed job-store update, for `details.record_error`.
fn record_error(e: &IrisError) -> serde_json::Value {
    json!({ "code": e.code.as_str(), "message": e.message })
}

/// The provider accepted job `id` as `remote_id`, but Iris could not record (or
/// no longer has) the job's local record: exit 5 with the remote operation id,
/// never "nothing was sent", and never a suggestion to resubmit.
fn accepted_but_unrecorded(
    id: &JobId,
    provider: ProviderId,
    remote_id: &str,
    cause: &IrisError,
) -> IrisError {
    IrisError::new(
        ErrorCode::SubmissionUncertain,
        format!(
            "the provider accepted job {id} (remote operation {remote_id}), but Iris could not keep its local \
             record: {}",
            cause.message
        ),
    )
    .with_provider(provider)
    .with_job(id.to_string(), None)
    .with_remote_operation(remote_id)
    .with_detail("provider_accepted", true)
    .with_detail("record_error", record_error(cause))
    .with_hint(
        "the job continues remotely and is billed by the provider, but Iris cannot wait for or download it \
         without its record; do not resubmit; keep the remote operation id and check it in the provider console",
    )
}

/// Errors after the provider accepted the job and it was recorded. The paid job
/// exists, so nothing may be reported as exit 2 ("nothing was sent"), and running
/// `video generate` again is never the fix:
/// * the local record disappeared: exit 5 with the remote operation id;
/// * the job succeeded but its output could not be saved locally (a file or
///   directory in the way, disk errors): `download_failed` naming
///   `iris jobs download`;
/// * any other exit-2 answer while waiting (e.g. a provider 400 on a status
///   check): `provider_error`; the job continues remotely.
///
/// A replaced code is kept in `details.cause_code`. Remote, pending, and
/// interrupt outcomes (`download_failed`, `wait_timeout`, `interrupted`, ...)
/// pass through unchanged.
fn after_acceptance(ctx: &AppContext, e: IrisError, record: &JobRecord) -> IrisError {
    let id = record.job_id();
    let local =
        e.exit_code() == exit::USAGE || matches!(e.category(), ErrorCategory::Io | ErrorCategory::Internal);
    if !local {
        return e;
    }
    if e.code == ErrorCode::JobNotFound {
        let remote = record.remote_operation_id().unwrap_or("unknown");
        return accepted_but_unrecorded(id, record.provider(), remote, &e);
    }
    // Store and lock errors carry no job context: take it from the current record.
    let e = if e.job_id.is_some() {
        e
    } else {
        match ctx.store.load(id) {
            Ok(current) => with_job_context(e, &current),
            Err(_) => with_job_context(e, record),
        }
    };
    let dont_resubmit = "do not re-run `iris video generate`, which would submit and bill a new job";
    if e.job_status == Some(JobStatus::Succeeded) {
        return recode(
            e,
            ErrorCode::DownloadFailed,
            |m| format!("job {id} succeeded, but its output could not be saved: {m}"),
            format!(
                "the job succeeded and is recorded; save its output with `iris jobs download {id}` (choose \
                 another location with -o PATH or -d DIR, or replace an existing file with --overwrite); \
                 {dont_resubmit}"
            ),
        );
    }
    let resume = format!(
        "the job was submitted and continues remotely; check it with `iris jobs status {id}` or resume with \
         `iris jobs wait {id}`; {dont_resubmit}"
    );
    if e.exit_code() == exit::USAGE {
        return recode(e, ErrorCode::ProviderError, |m| format!("could not check job {id}: {m}"), resume);
    }
    if e.hint.is_none() { e.with_hint(resume) } else { e }
}

/// `e` under another code (with that code's default retryability), keeping its
/// provider and job context; the original code goes to `details.cause_code`.
fn recode(e: IrisError, code: ErrorCode, message: impl FnOnce(&str) -> String, hint: String) -> IrisError {
    let mut out = IrisError::new(code, message(&e.message)).with_hint(hint);
    out.retry_after = e.retry_after;
    out.provider = e.provider;
    out.provider_status = e.provider_status;
    out.provider_code = e.provider_code.clone();
    out.provider_request_id = e.provider_request_id.clone();
    out.job_id = e.job_id.clone();
    out.remote_operation_id = e.remote_operation_id.clone();
    out.job_status = e.job_status;
    out.details = e.details.clone();
    out.with_detail("cause_code", e.code.as_str())
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
/// Lexically normalized like every other planned path (`-d a/../out` plans
/// `<cwd>/out/<job_id>.mp4`), which is the form the real run writes to.
fn placeholder_outputs(dir: &std::path::Path, media_type: &str, count: u32) -> Vec<String> {
    let dir = artifacts::paths::normalize_lexically(dir);
    let ext = media::extension_for(media_type).unwrap_or("bin");
    let name = |suffix: String| dir.join(format!("<job_id>{suffix}.{ext}")).display().to_string();
    if count == 1 { vec![name(String::new())] } else { (1..=count).map(|i| name(format!("-{i}"))).collect() }
}
