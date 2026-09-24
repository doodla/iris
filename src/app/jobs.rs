//! `jobs list/status/wait/download/delete`, and the wait-and-download phase that
//! `video generate` shares with `jobs wait` (C-02, C-04).
//!
//! Invariants:
//! * Ctrl-C, wait limits, poll failures, and download failures never change a
//!   `running` or `succeeded` job to `failed`; only provider answers do.
//! * Downloads never resubmit anything, and are safe to repeat: an intact file at
//!   the target is reported as `already_downloaded`, an intact file elsewhere is
//!   copied locally.
//! * Deletion is local only; remote jobs and downloaded media are never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

use crate::artifacts::{self, DownloadDecision, FinalizeMode, Naming, PartFile, PathRequest, SavedArtifact};
use crate::config::SettingSource;
use crate::domain::{DownloadState, JobStatus, Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::{self, AuthHeader, DownloadError, DownloadRequest};
use crate::jobs::{JobId, JobOutput, JobRecord, PollApplied};
use crate::output::ErrorBody;
use crate::output::results::{JobDeleteResult, JobListResult, JobResult};
use crate::providers::VideoProvider;

use super::context::AppContext;

/// Filters of `jobs list`.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub status: Option<JobStatus>,
    pub provider: Option<ProviderId>,
    pub limit: Option<usize>,
}

/// Explicit download target of `jobs wait` / `jobs download`. The directory
/// (`-d`) comes from the settings (flag layer); without `-o`/`-d` the job's
/// recorded output plan applies, then the default output directory.
#[derive(Debug, Clone, Default)]
pub struct Target {
    /// `-o, --output`.
    pub output: Option<PathBuf>,
    /// `--overwrite`.
    pub overwrite: bool,
}

/// Arguments of `jobs wait`.
#[derive(Debug, Clone)]
pub struct WaitArgs {
    /// Download the outputs once the job succeeded (`--no-download` sets false).
    pub download: bool,
    pub target: Target,
}

/// `jobs list`: local records, newest first. Unreadable records are skipped with
/// warning `job_record_unreadable`.
pub fn list(
    ctx: &AppContext,
    filter: &ListFilter,
    warnings: &mut Vec<Warning>,
) -> Result<JobListResult, IrisError> {
    let listing = ctx.store.list()?;
    warnings.extend(listing.warnings);
    let jobs = listing
        .records
        .iter()
        .filter(|r| filter.status.is_none_or(|s| r.status() == s))
        .filter(|r| filter.provider.is_none_or(|p| r.provider() == p))
        .take(filter.limit.unwrap_or(usize::MAX))
        .map(JobRecord::to_view)
        .collect();
    Ok(JobListResult { jobs })
}

/// `jobs status`: the local record, refreshed once from the provider when the job
/// is `running` and `refresh` is set. A failed refresh is reported as warning
/// `status_refresh_failed` with the last known status.
pub async fn status(
    ctx: &AppContext,
    job_id: &str,
    refresh: bool,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let id = JobId::parse(job_id)?;
    let mut rec = ctx.store.load(&id)?;
    if refresh && rec.status() == JobStatus::Running {
        ctx.interrupt.arm();
        let seen = ctx.interrupt.count();
        match poll_once(ctx, &rec, seen, warnings).await {
            Ok(Some(updated)) => rec = updated,
            Ok(None) => {}
            Err(e) if e.code == ErrorCode::Interrupted => return Err(e),
            Err(e) => warnings.push(Warning::new(
                "status_refresh_failed",
                format!("could not refresh the remote status ({}); showing the last known status", e.message),
            )),
        }
    }
    warnings.extend(retention_warning(&rec));
    Ok(job_result(&rec))
}

/// `jobs wait`: poll until the job is terminal, then download its outputs
/// (unless `download` is false).
pub async fn wait(
    ctx: &AppContext,
    job_id: &str,
    args: &WaitArgs,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let id = JobId::parse(job_id)?;
    wait_parsed(ctx, &id, args, warnings).await
}

/// `jobs download`: download the outputs of a succeeded job. Never resubmits.
pub async fn download(
    ctx: &AppContext,
    job_id: &str,
    target: &Target,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let id = JobId::parse(job_id)?;
    let rec = download_outputs(ctx, &id, target, warnings).await?;
    Ok(job_result(&rec))
}

/// `jobs delete`: delete local records only. Active jobs (`submitting`,
/// `running`) are refused unless `force`. With `all`, every readable record.
pub fn delete(
    ctx: &AppContext,
    job_ids: &[String],
    all: bool,
    force: bool,
    warnings: &mut Vec<Warning>,
) -> Result<JobDeleteResult, IrisError> {
    let ids: Vec<JobId> = if all {
        let listing = ctx.store.list()?;
        warnings.extend(listing.warnings);
        if !force {
            let active: Vec<String> = listing
                .records
                .iter()
                .filter(|r| r.is_active())
                .map(|r| format!("{} ({})", r.job_id(), r.status()))
                .collect();
            if !active.is_empty() {
                return Err(IrisError::invalid(format!(
                    "{} job(s) are still active and would become unrecoverable: {}; nothing was deleted",
                    active.len(),
                    active.join(", ")
                ))
                .with_hint(
                    "wait for them (`iris jobs wait <id>`), delete finished jobs by id, or pass --force (remote \
                     jobs are not cancelled)",
                ));
            }
        }
        listing.records.iter().map(|r| r.job_id().clone()).collect()
    } else {
        job_ids.iter().map(|raw| JobId::parse(raw)).collect::<Result<_, _>>()?
    };
    let mut deleted: Vec<String> = Vec::new();
    for id in &ids {
        if let Err(e) = ctx.store.delete(id, force) {
            return Err(e.with_detail("deleted", deleted));
        }
        deleted.push(id.to_string());
    }
    Ok(JobDeleteResult {
        deleted,
        remote_effect: "none".to_string(),
        note: "Local records only; remote jobs and downloaded files are untouched.".to_string(),
    })
}

/// Wait for a job, then finish per `args` (shared with `video generate`).
pub(crate) async fn wait_parsed(
    ctx: &AppContext,
    id: &JobId,
    args: &WaitArgs,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let rec = wait_until_terminal(ctx, id, warnings).await?;
    match rec.status() {
        JobStatus::Succeeded if args.download => {
            let rec = download_outputs(ctx, id, &args.target, warnings).await?;
            Ok(job_result(&rec))
        }
        JobStatus::Succeeded => {
            warnings.extend(retention_warning(&rec));
            Ok(job_result(&rec))
        }
        _ => Err(job_error(&rec)),
    }
}

/// The `{job, next_steps}` result for a record.
pub(crate) fn job_result(rec: &JobRecord) -> JobResult {
    JobResult { job: rec.to_view(), next_steps: next_steps(rec) }
}

/// Suggested follow-up commands for a job in its current state.
pub(crate) fn next_steps(rec: &JobRecord) -> Vec<String> {
    let id = rec.job_id();
    match rec.status() {
        JobStatus::Submitting | JobStatus::Running => {
            vec![format!("iris jobs status {id}"), format!("iris jobs wait {id}")]
        }
        JobStatus::Succeeded if has_downloadable(rec) => vec![format!("iris jobs download {id}")],
        _ => Vec::new(),
    }
}

fn has_downloadable(rec: &JobRecord) -> bool {
    rec.outputs().iter().any(|o| matches!(o.download_state, DownloadState::Pending | DownloadState::Failed))
}

/// Warning `retention_limited` for a succeeded job with outputs still to download.
fn retention_warning(rec: &JobRecord) -> Option<Warning> {
    let until = rec.remote_expires_at()?;
    (rec.status() == JobStatus::Succeeded && has_downloadable(rec)).then(|| {
        let id = rec.job_id();
        Warning::new(
            "retention_limited",
            format!(
                "the provider keeps the outputs of {id} only until about {until}; download them before then with \
                 `iris jobs download {id}`"
            ),
        )
    })
}

/// Attach the job's identifiers and status to an error.
pub(crate) fn with_job_context(e: IrisError, rec: &JobRecord) -> IrisError {
    let mut e = e.with_job(rec.job_id().to_string(), Some(rec.status()));
    if e.remote_operation_id.is_none()
        && let Some(remote) = rec.remote_operation_id()
    {
        e = e.with_remote_operation(remote);
    }
    e
}

/// Rebuild an error from a persisted error body.
pub(crate) fn error_from_body(body: &ErrorBody) -> IrisError {
    let mut e = IrisError::new(body.code, body.message.clone()).with_retryable(body.retryable);
    e.hint = body.hint.clone();
    e.retry_after = body.retry_after_seconds.map(Duration::from_secs);
    e.provider = body.provider;
    e.provider_status = body.provider_status;
    e.provider_code = body.provider_code.clone();
    e.provider_request_id = body.provider_request_id.clone();
    e.job_id = body.job_id.clone();
    e.remote_operation_id = body.remote_operation_id.clone();
    e.job_status = body.job_status;
    e.details = body.details.clone().unwrap_or_default();
    e
}

/// The error a terminal, unsuccessful job reports (its recorded error when present).
pub(crate) fn job_error(rec: &JobRecord) -> IrisError {
    let id = rec.job_id();
    let base = match rec.error() {
        Some(body) => error_from_body(body),
        None => match rec.status() {
            JobStatus::Failed => IrisError::new(
                ErrorCode::RemoteJobFailed,
                format!("the provider reported that job {id} failed"),
            ),
            JobStatus::Expired => IrisError::new(
                ErrorCode::ArtifactExpired,
                format!("the provider no longer has job {id} or its outputs"),
            ),
            JobStatus::SubmissionUnknown => IrisError::new(
                ErrorCode::SubmissionUncertain,
                format!("it is unknown whether the provider accepted job {id}"),
            )
            .with_hint("check the provider console before resubmitting; Iris never resubmits automatically"),
            status => IrisError::internal(format!("job {id} is {status}")),
        },
    };
    with_job_context(base, rec)
}

fn video_adapter(ctx: &AppContext, provider: ProviderId) -> Result<&dyn VideoProvider, IrisError> {
    ctx.provider(provider)?
        .video()
        .ok_or_else(|| IrisError::internal(format!("provider '{provider}' has no video adapter")))
}

fn wait_timeout(rec: &JobRecord, waited: Duration) -> IrisError {
    let id = rec.job_id();
    with_job_context(
        IrisError::new(
            ErrorCode::WaitTimeout,
            format!(
                "job {id} did not finish within {}; it continues remotely",
                humantime::format_duration(waited)
            ),
        )
        .with_hint(format!("resume with `iris jobs wait {id}` (or check with `iris jobs status {id}`)")),
        rec,
    )
}

fn interrupted_wait(rec: &JobRecord) -> IrisError {
    let id = rec.job_id();
    with_job_context(
        IrisError::new(
            ErrorCode::Interrupted,
            format!("stopped waiting for job {id}; it continues remotely"),
        )
        .with_hint(format!("resume with `iris jobs wait {id}`")),
        rec,
    )
}

fn interrupted_download(rec: &JobRecord) -> IrisError {
    let id = rec.job_id();
    with_job_context(
        IrisError::new(
            ErrorCode::Interrupted,
            format!("download of job {id} interrupted; nothing partial was kept"),
        )
        .with_hint(format!("download again with `iris jobs download {id}`")),
        rec,
    )
}

/// Poll a running job once and persist the answer. `Ok(None)` if the record
/// cannot be polled (no operation id yet).
async fn poll_once(
    ctx: &AppContext,
    rec: &JobRecord,
    seen: u64,
    warnings: &mut Vec<Warning>,
) -> Result<Option<JobRecord>, IrisError> {
    let Some(remote) = rec.remote_operation_id() else {
        return Ok(None);
    };
    let provider = rec.provider();
    let video = video_adapter(ctx, provider)?;
    let pctx = ctx.provider_context(provider)?;
    let status = tokio::select! {
        result = video.poll(remote, &pctx) => result?,
        () = ctx.interrupt.after(seen) => return Err(interrupted_wait(rec)),
    };
    let (updated, applied) =
        ctx.store.update(rec.job_id(), |r| r.apply_poll(status, video.output_retention(), ctx.now()))?;
    report_poll(ctx, &updated, &applied, warnings);
    Ok(Some(updated))
}

fn report_poll(ctx: &AppContext, rec: &JobRecord, applied: &PollApplied, warnings: &mut Vec<Warning>) {
    let id = rec.job_id();
    match applied {
        PollApplied::Running { progress: Some(p) } => {
            ctx.progress.line(format!("Job {id} is running ({:.0}% done)", p.clamp(0.0, 100.0)));
        }
        PollApplied::Running { progress: None } => ctx.progress.line(format!("Job {id} is running")),
        PollApplied::Succeeded { warnings: w } => {
            warnings.extend(w.iter().cloned());
            ctx.progress.line(format!("Job {id} succeeded"));
        }
        PollApplied::Failed => ctx.progress.line(format!("Job {id} failed")),
        PollApplied::Expired => ctx.progress.line(format!("Job {id} expired at the provider")),
        PollApplied::AlreadyTerminal => {}
    }
}

/// Poll until the job is terminal, the wait limit passes (`wait_timeout`, exit 4),
/// or Ctrl-C (`interrupted`, exit 130). Each poll answer is persisted. Retryable
/// poll failures are reported and retried; they never change the job.
async fn wait_until_terminal(
    ctx: &AppContext,
    id: &JobId,
    warnings: &mut Vec<Warning>,
) -> Result<JobRecord, IrisError> {
    let limit = ctx.settings.wait_timeout.value;
    let interval = ctx.settings.poll_interval.value;
    let mut rec = ctx.store.load(id)?;
    if rec.status().is_terminal() {
        return Ok(rec);
    }
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    let deadline = tokio::time::Instant::now() + limit;
    let mut announced_submitting = false;
    loop {
        let mut retry_after = Duration::ZERO;
        match rec.status() {
            status if status.is_terminal() => return Ok(rec),
            JobStatus::Submitting => {
                if !announced_submitting {
                    announced_submitting = true;
                    ctx.progress.line(format!(
                        "Job {id} is still being submitted by another iris process; waiting for its operation id"
                    ));
                }
            }
            _ => {
                let provider = rec.provider();
                let video = video_adapter(ctx, provider)?;
                let pctx = ctx.provider_context(provider).map_err(|e| with_job_context(e, &rec))?;
                let Some(remote) = rec.remote_operation_id().map(str::to_string) else {
                    return Err(with_job_context(
                        IrisError::internal(format!("job {id} is running but has no operation id")),
                        &rec,
                    ));
                };
                let polled = tokio::select! {
                    result = video.poll(&remote, &pctx) => result,
                    () = ctx.interrupt.after(seen) => return Err(interrupted_wait(&rec)),
                    () = tokio::time::sleep_until(deadline) => return Err(wait_timeout(&rec, limit)),
                };
                match polled {
                    Ok(status) => {
                        let retention = video.output_retention();
                        let (updated, applied) =
                            ctx.store.update(id, |r| r.apply_poll(status, retention, ctx.now()))?;
                        rec = updated;
                        report_poll(ctx, &rec, &applied, warnings);
                        if rec.status().is_terminal() {
                            return Ok(rec);
                        }
                    }
                    Err(e) if e.retryable == Some(true) => {
                        ctx.progress.line(format!(
                            "Could not check job {id} ({}); will retry. The job itself is unaffected.",
                            e.message
                        ));
                        retry_after = e.retry_after.unwrap_or_default();
                    }
                    Err(e) => return Err(with_job_context(e, &rec)),
                }
            }
        }
        let delay = jittered(interval).max(retry_after);
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = ctx.interrupt.after(seen) => return Err(interrupted_wait(&rec)),
            () = tokio::time::sleep_until(deadline) => return Err(wait_timeout(&rec, limit)),
        }
        if rec.status() == JobStatus::Submitting {
            rec = ctx.store.load(id)?;
        }
    }
}

/// `interval` ± 10%.
fn jittered(interval: Duration) -> Duration {
    interval.mul_f64(0.9 + fastrand::f64() * 0.2)
}

/// Why fetching one output failed.
enum FetchFailure {
    /// Local problem (target exists, disk, setup): returned without touching the record.
    Local(IrisError),
    /// Remote fetch problem: recorded on the output (`failed` / `expired`).
    Remote(IrisError),
    Interrupted,
}

/// Where and how outputs of one job are fetched.
struct Access<'a> {
    provider: ProviderId,
    base_url: &'a Url,
    idle_timeout: Duration,
    media_types: &'a [&'a str],
    mode: FinalizeMode,
    seen: u64,
}

/// C-04 "Downloads" steps 1–6 for every output of a job.
async fn download_outputs(
    ctx: &AppContext,
    id: &JobId,
    target: &Target,
    warnings: &mut Vec<Warning>,
) -> Result<JobRecord, IrisError> {
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();

    // 1. The job's download lock (without blocking the runtime), then re-read.
    let _lock = match ctx.store.try_download_lock(id)? {
        Some(lock) => lock,
        None => {
            ctx.progress.line(format!("Waiting for another iris process that is downloading job {id}"));
            tokio::select! {
                lock = ctx.store.download_lock_async(id, Duration::from_millis(250)) => lock?,
                () = ctx.interrupt.after(seen) => {
                    return Err(IrisError::new(ErrorCode::Interrupted, format!("stopped waiting to download job {id}"))
                        .with_job(id.to_string(), None)
                        .with_hint(format!("download later with `iris jobs download {id}`")));
                }
            }
        }
    };
    let rec = ctx.store.load(id)?;
    match rec.status() {
        JobStatus::Succeeded => {}
        JobStatus::Submitting | JobStatus::Running => {
            return Err(with_job_context(
                IrisError::new(
                    ErrorCode::JobNotReady,
                    format!("job {id} is still {}; its outputs are not ready", rec.status()),
                )
                .with_hint(format!("wait for it with `iris jobs wait {id}`")),
                &rec,
            ));
        }
        _ => return Err(job_error(&rec)),
    }
    if rec.outputs().is_empty() {
        return Ok(rec);
    }

    // 2. Targets: explicit -o/-d > the recorded output plan > default dir + name.
    let media_types = output_media_types(ctx, &rec);
    let recorded_plan = rec.output_plan();
    let explicit_dir = (ctx.settings.output_dir.source == SettingSource::Flag)
        .then(|| ctx.settings.output_dir.value.clone());
    let (output, dir, overwrite) = if let Some(o) = &target.output {
        (Some(o.clone()), ctx.settings.output_dir.value.clone(), target.overwrite)
    } else if let Some(d) = explicit_dir {
        (None, d, target.overwrite)
    } else {
        (
            recorded_plan.path.clone(),
            recorded_plan.dir.clone().unwrap_or_else(|| ctx.settings.output_dir.value.clone()),
            target.overwrite || recorded_plan.overwrite,
        )
    };
    let plan = artifacts::plan_outputs(&PathRequest {
        naming: Naming::Video { job_id: id.as_str() },
        count: rec.outputs().len() as u32,
        output: output.as_deref(),
        dir: &dir,
        format: None,
        media_types,
    })
    .map_err(|e| with_job_context(e, &rec))?;
    warnings.extend(plan.warnings.iter().cloned());
    artifacts::preflight_dirs(&plan.paths, true).map_err(|e| with_job_context(e, &rec))?;

    let provider = rec.provider();
    let base_url = ctx.settings.provider(provider).base_url.value.clone();
    let access = Access {
        provider,
        base_url: &base_url,
        idle_timeout: ctx.settings.timeouts(provider).download_idle,
        media_types,
        mode: FinalizeMode::for_download(overwrite),
        seen,
    };

    let mut remote_failure: Option<IrisError> = None;
    for (out, path) in rec.outputs().iter().zip(&plan.paths) {
        let recorded = out.recorded_file();
        match artifacts::decide_download(recorded, path) {
            DownloadDecision::AlreadyDownloaded => {
                if let Some(file) = recorded {
                    warnings.push(artifacts::already_present_warning(file.path));
                }
            }
            DownloadDecision::CopyLocal => {
                let Some(file) = recorded else { continue };
                ctx.progress.line(format!("Copying output {} of job {id} to {}", out.index, path.display()));
                let saved = artifacts::copy_local(file, path, out.index, media_types, access.mode)
                    .map_err(|e| with_job_context(e, &rec))?;
                record_saved(ctx, id, out.index, saved, warnings)?;
            }
            DownloadDecision::Fetch => {
                let now = ctx.now();
                if rec.remote_expired(now) {
                    let e = retention_passed(&rec);
                    ctx.store.update(id, |r| r.mark_output_expired(out.index, &e, now))?;
                    remote_failure.get_or_insert(e);
                    continue;
                }
                ctx.progress.line(format!("Downloading output {} of job {id}", out.index));
                match fetch(ctx, out, path, &access).await {
                    Ok(saved) => record_saved(ctx, id, out.index, saved, warnings)?,
                    Err(FetchFailure::Interrupted) => return Err(interrupted_download(&rec)),
                    Err(FetchFailure::Local(e)) => return Err(with_job_context(e, &rec)),
                    Err(FetchFailure::Remote(e)) => {
                        let now = ctx.now();
                        if e.code == ErrorCode::ArtifactExpired {
                            ctx.store.update(id, |r| r.mark_output_expired(out.index, &e, now))?;
                        } else {
                            ctx.store.update(id, |r| r.mark_output_failed(out.index, &e, now))?;
                        }
                        remote_failure.get_or_insert(e);
                    }
                }
            }
        }
    }
    let rec = ctx.store.load(id)?;
    if let Some(e) = remote_failure {
        let e = if e.code == ErrorCode::ArtifactExpired || e.hint.is_some() {
            e
        } else {
            e.with_hint(format!(
                "the job itself succeeded; retry the download with `iris jobs download {id}` (nothing is \
                 regenerated)"
            ))
        };
        return Err(with_job_context(e, &rec));
    }
    Ok(rec)
}

fn record_saved(
    ctx: &AppContext,
    id: &JobId,
    index: u32,
    saved: SavedArtifact,
    warnings: &mut Vec<Warning>,
) -> Result<(), IrisError> {
    warnings.extend(saved.warnings);
    let now = ctx.now();
    ctx.store.update(id, |r| r.mark_output_downloaded(index, &saved.artifact, now))?;
    Ok(())
}

fn retention_passed(rec: &JobRecord) -> IrisError {
    let until = rec.remote_expires_at().map(|t| t.to_string()).unwrap_or_default();
    IrisError::new(
        ErrorCode::ArtifactExpired,
        format!(
            "the provider's retention period for the outputs of {} ended at {until}; they can no longer be \
             downloaded",
            rec.job_id()
        ),
    )
    .with_retryable(Some(false))
}

/// Declared output types of the job's model, or the operation's default.
fn output_media_types(ctx: &AppContext, rec: &JobRecord) -> &'static [&'static str] {
    if let Some(spec) = ctx.catalog.find(rec.model()) {
        return spec.outputs.media_types;
    }
    match rec.operation() {
        Operation::VideoGenerate => &["video/mp4"],
        Operation::ImageGenerate | Operation::ImageEdit => &["image/png"],
    }
}

/// The credential header for `uri`, only when it has the provider's origin.
fn auth_for(
    ctx: &AppContext,
    provider: ProviderId,
    base_url: &Url,
    uri: &str,
) -> Result<Option<AuthHeader>, IrisError> {
    let Ok(url) = Url::parse(uri) else {
        return Ok(None);
    };
    if !http::same_origin(&url, base_url) {
        return Ok(None);
    }
    let secret = ctx.settings.require_credential(provider)?;
    let header = ctx.provider(provider)?.credential_header();
    Ok(Some(AuthHeader::new(header.name, header.prefix, &secret)?))
}

/// Stream one output into a temp file next to `path`, validate, and finalize.
async fn fetch(
    ctx: &AppContext,
    out: &JobOutput,
    path: &Path,
    access: &Access<'_>,
) -> Result<SavedArtifact, FetchFailure> {
    let auth =
        auth_for(ctx, access.provider, access.base_url, &out.remote_uri).map_err(FetchFailure::Local)?;
    let client = ctx.http().map_err(FetchFailure::Local)?;
    let mut part = PartFile::create_for(path).map_err(FetchFailure::Local)?;
    let result = {
        let request = DownloadRequest {
            url: &out.remote_uri,
            dest: &*part.file_mut(),
            base_url: access.base_url,
            auth: auth.as_ref(),
            idle_timeout: access.idle_timeout,
            provider: Some(access.provider),
        };
        tokio::select! {
            result = http::download(client, &request) => result,
            () = ctx.interrupt.after(access.seen) => return Err(FetchFailure::Interrupted),
        }
    };
    match result {
        Ok(_) => {
            artifacts::finalize_download(part, out.index, access.media_types, access.mode).map_err(|e| {
                if e.code == ErrorCode::InvalidMedia {
                    FetchFailure::Remote(e)
                } else {
                    FetchFailure::Local(e)
                }
            })
        }
        Err(e @ (DownloadError::Io { .. } | DownloadError::Internal { .. })) => {
            Err(FetchFailure::Local(e.into_iris()))
        }
        Err(e) => Err(FetchFailure::Remote(e.into_iris())),
    }
}
