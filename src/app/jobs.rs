//! `jobs list/status/wait/download/delete`, and the wait-and-download phase that
//! `video generate` shares with `jobs wait` (see `iris --help` and docs/jobs.md).
//!
//! Invariants:
//! * Ctrl-C, wait limits, poll failures, and download failures never change a
//!   `running` or `succeeded` job to `failed`; only provider answers do.
//! * Downloads never resubmit anything, and are safe to repeat: an intact, valid
//!   file at the target is reported as `already_downloaded`, one elsewhere is
//!   copied locally (and fetched again if it changes while being copied). A
//!   recorded file that no longer validates as media, or any file with
//!   `--overwrite`, is fetched again and atomically replaced.
//! * Deletion is local only; remote jobs and downloaded media are never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use url::Url;

use crate::artifacts::{
    self, DownloadDecision, FinalizeMode, Naming, PartFile, PathRequest, RecordedFile, SavedArtifact,
};
use crate::config::SettingSource;
use crate::domain::{JobStatus, Operation, ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::{self, AuthHeader, DownloadError, DownloadRequest};
use crate::jobs::{self, DeleteRefusal, JobId, JobOutput, JobRecord, PollApplied, RefusalKind};
use crate::output::ErrorBody;
use crate::output::results::{JobDeleteResult, JobListResult, JobResult, JobView};
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

/// How a file already at an output's target is handled when the output is saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SaveMode {
    /// `jobs wait` / `jobs download` (docs/jobs.md "Downloads" step 4): without
    /// `--overwrite` a different file at the target is `output_exists`. Nothing
    /// is regenerated, so the caller can simply choose another target.
    Download,
    /// The wait of `video generate` itself: the caller just paid for this output
    /// and the target passed the preflight, so a file that appeared since then
    /// never blocks the save; the output goes to `<stem>.<n>.<ext>` with warning
    /// `output_renamed` (see docs/json-contract.md "Warning codes", as for images).
    Generated,
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
    let commands = Commands::of(ctx);
    let start = warnings.len();
    let result = ctx.store.list().map(|listing| {
        warnings.extend(listing.warnings);
        let jobs = listing
            .records
            .iter()
            .filter(|r| filter.status.is_none_or(|s| r.status() == s))
            .filter(|r| filter.provider.is_none_or(|p| r.provider() == p))
            .take(filter.limit.unwrap_or(usize::MAX))
            .map(|r| commands.view(r.to_view()))
            .collect();
        JobListResult { jobs }
    });
    commands.finish(result, warnings, start)
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
    let start = warnings.len();
    let result = async {
        let id = JobId::parse(job_id)?;
        let mut rec = ctx.store.load(&id)?;
        if refresh {
            rec = refresh_running(ctx, rec, warnings).await?;
        }
        warnings.extend(retention_warning(&rec, ctx.now()));
        Ok(job_result(ctx, &rec))
    }
    .await;
    Commands::of(ctx).finish(result, warnings, start)
}

/// Refresh a `running` record once from the provider (a free status read) and
/// persist the answer; other records are returned as they are. A failed refresh
/// is reported as warning `status_refresh_failed` and the last known record is
/// returned; only an interrupt is an error.
async fn refresh_running(
    ctx: &AppContext,
    rec: JobRecord,
    warnings: &mut Vec<Warning>,
) -> Result<JobRecord, IrisError> {
    match refresh_once(ctx, &rec, warnings).await {
        Ok(Some(updated)) => Ok(updated),
        Ok(None) => Ok(rec),
        Err(e) if e.code == ErrorCode::Interrupted => Err(e),
        Err(e) => {
            warnings.push(Warning::new(
                "status_refresh_failed",
                format!("could not refresh the remote status ({}); showing the last known status", e.message),
            ));
            Ok(rec)
        }
    }
}

/// Poll a `running` record once and persist the answer. `Ok(None)` if nothing was
/// polled (the record is not `running`, or has no operation id). Errors carry the
/// job's context.
async fn refresh_once(
    ctx: &AppContext,
    rec: &JobRecord,
    warnings: &mut Vec<Warning>,
) -> Result<Option<JobRecord>, IrisError> {
    if rec.status() != JobStatus::Running {
        return Ok(None);
    }
    ctx.interrupt.arm();
    let seen = ctx.interrupt.count();
    poll_once(ctx, rec, seen, warnings).await
}

/// `jobs wait`: poll until the job is terminal, then download its outputs
/// (unless `download` is false).
pub async fn wait(
    ctx: &AppContext,
    job_id: &str,
    args: &WaitArgs,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let start = warnings.len();
    let result = match JobId::parse(job_id) {
        Ok(id) => wait_parsed(ctx, &id, args, SaveMode::Download, warnings).await,
        Err(e) => Err(e),
    };
    Commands::of(ctx).finish(result, warnings, start)
}

/// `jobs download`: download the outputs of a succeeded job. Never resubmits.
/// A record that still says `running` may be stale (the job may have finished
/// since the last check), so it is refreshed once first.
///
/// Unlike `jobs status`, which always shows the last known status, a download
/// fails anyway when the job is not finished, so only a transient refresh failure
/// (network, timeout, rate limit, a provider 5xx: `retryable: true`) falls back to
/// the last known status: `job_not_ready` with `details.status_checked: false`
/// and warning `status_refresh_failed`. Any other failure (a missing key,
/// rejected credentials, no access, quota, configuration) is the command's error,
/// with the job's context, as in `jobs wait`.
pub async fn download(
    ctx: &AppContext,
    job_id: &str,
    target: &Target,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let start = warnings.len();
    let result = download_parsed(ctx, job_id, target, warnings).await;
    Commands::of(ctx).finish(result, warnings, start)
}

async fn download_parsed(
    ctx: &AppContext,
    job_id: &str,
    target: &Target,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let id = JobId::parse(job_id)?;
    let rec = ctx.store.load(&id)?;
    let mut status_checked = true;
    match refresh_once(ctx, &rec, warnings).await {
        Ok(_) => {}
        Err(e) if e.code != ErrorCode::Interrupted && e.retryable == Some(true) => {
            status_checked = false;
            warnings.push(Warning::new(
                "status_refresh_failed",
                format!("could not check the remote status ({}); going by the last known status", e.message),
            ));
        }
        Err(e) => return Err(e),
    }
    match download_outputs(ctx, &id, target, SaveMode::Download, warnings).await {
        Ok(rec) => Ok(job_result(ctx, &rec)),
        Err(e) if e.code == ErrorCode::JobNotReady && !status_checked => {
            let status = e.job_status.map_or_else(|| "running".to_string(), |s| s.to_string());
            let checked = rec.last_checked_at().map(|t| format!(" (last checked {t})")).unwrap_or_default();
            let mut e = e.with_detail("status_checked", false).with_hint(format!(
                "check again with `iris jobs download {id}`, or wait for it with `iris jobs wait {id}`; the \
                 status_refresh_failed warning says why the check failed"
            ));
            e.message = format!(
                "job {id} was last known to be {status}{checked}; its status could not be checked now, so its \
                 outputs are not known to be ready"
            );
            Err(e)
        }
        Err(e) => Err(e),
    }
}

/// `jobs delete`: delete local records only, all or nothing.
///
/// Every named record (with `all`: every record in the jobs directory) is checked
/// with the store's own rule ([`JobStore::check_delete`](crate::jobs::JobStore::check_delete),
/// the status on disk) before anything is deleted; if any is refused, nothing is
/// deleted and the error lists every refusal, with `details.deleted: []`. Without
/// `force`, active jobs (`submitting`, `running`), succeeded jobs with outputs not
/// yet downloaded that the provider still keeps, and unreadable records are
/// refused, and `all` skips unreadable records with a warning. With `all` and
/// `force`, unreadable record files (regular files named `<job_id>.json` in the
/// jobs directory, nothing else) are deleted too, listed in `deleted` and named in
/// the note.
///
/// A record that changes between the check and its deletion can still make a
/// later one fail; the error then lists what was deleted in `details.deleted`.
pub fn delete(
    ctx: &AppContext,
    job_ids: &[String],
    all: bool,
    force: bool,
    warnings: &mut Vec<Warning>,
) -> Result<JobDeleteResult, IrisError> {
    let start = warnings.len();
    let result = delete_records(ctx, job_ids, all, force, warnings);
    Commands::of(ctx).finish(result, warnings, start)
}

fn delete_records(
    ctx: &AppContext,
    job_ids: &[String],
    all: bool,
    force: bool,
    warnings: &mut Vec<Warning>,
) -> Result<JobDeleteResult, IrisError> {
    let nothing_deleted = |e: IrisError| e.with_detail("deleted", Vec::<String>::new());
    let mut unreadable: Vec<JobId> = Vec::new();
    let ids: Vec<JobId> = if all {
        let listing = ctx.store.list().map_err(nothing_deleted)?;
        let mut ids: Vec<JobId> = listing.records.iter().map(|r| r.job_id().clone()).collect();
        if force {
            // These are deleted, not skipped: their "skipped" warnings would be wrong.
            let deleted_warnings: Vec<&Warning> = listing.unreadable.iter().map(|u| &u.warning).collect();
            warnings.extend(listing.warnings.iter().filter(|w| !deleted_warnings.contains(w)).cloned());
            unreadable = listing.unreadable.iter().map(|u| u.id.clone()).collect();
            ids.extend(unreadable.iter().cloned());
        } else {
            warnings.extend(listing.warnings);
        }
        ids
    } else {
        let mut ids: Vec<JobId> = Vec::new();
        for raw in job_ids {
            let id = JobId::parse(raw).map_err(nothing_deleted)?;
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    };

    // Check everything first: all or nothing.
    let refusals: Vec<(JobId, DeleteRefusal)> = ids
        .iter()
        .filter_map(|id| ctx.store.check_delete(id, force).err().map(|r| (id.clone(), r)))
        // With --all, a record deleted meanwhile (by another process) is simply gone.
        .filter(|(_, r)| !(all && r.kind == RefusalKind::NotFound))
        .collect();
    if !refusals.is_empty() {
        return Err(nothing_deleted(refused_deletion(refusals, ids.len(), all)));
    }

    let mut deleted: Vec<String> = Vec::new();
    for id in &ids {
        match ctx.store.delete(id, force) {
            Ok(()) => deleted.push(id.to_string()),
            Err(e) if all && e.code == ErrorCode::JobNotFound => {}
            Err(e) => return Err(e.with_detail("deleted", deleted)),
        }
    }
    let mut note = "Local records only; remote jobs and downloaded files are untouched.".to_string();
    let removed_unreadable: Vec<String> =
        unreadable.iter().map(JobId::to_string).filter(|id| deleted.contains(id)).collect();
    if !removed_unreadable.is_empty() {
        note.push_str(&format!(
            " Also deleted {} record(s) that could not be read: {}.",
            removed_unreadable.len(),
            removed_unreadable.join(", ")
        ));
    }
    Ok(JobDeleteResult { deleted, remote_effect: "none".to_string(), note })
}

/// The error of a `jobs delete` that refused some of `requested` records. A single
/// named record keeps the store's own error (its code, message, and hint); several,
/// or any with `--all`, become one `invalid_argument` that names each job and why.
fn refused_deletion(mut refusals: Vec<(JobId, DeleteRefusal)>, requested: usize, all: bool) -> IrisError {
    if !all && refusals.len() == 1 {
        let (_, refusal) = refusals.remove(0);
        let mut e = refusal.error;
        if requested > 1 {
            e.message = format!("{}; nothing was deleted", e.message);
        }
        return e;
    }
    let kinds: Vec<RefusalKind> = refusals.iter().map(|(_, r)| r.kind).collect();
    let listed: Vec<String> = refusals.iter().map(|(id, r)| format!("{id} ({})", r.summary)).collect();
    let mut steps: Vec<&str> = Vec::new();
    if kinds.contains(&RefusalKind::Active) {
        steps.push("wait for active jobs to finish (`iris jobs wait <id>`)");
    }
    if kinds.contains(&RefusalKind::NotDownloaded) {
        steps.push("download the outputs of succeeded jobs first (`iris jobs download <id>`)");
    }
    if kinds.contains(&RefusalKind::NotFound) {
        steps.push("check the ids with `iris jobs list`");
    }
    if all {
        steps.push("delete the other jobs by id");
    }
    let force = kinds.iter().any(|k| *k != RefusalKind::NotFound).then_some(
        "pass --force to delete the local records anyway (remote jobs are not cancelled, and outputs not \
         downloaded can no longer be fetched; for a job still submitting, check the provider console first)",
    );
    let hint = match (steps.is_empty(), force) {
        (false, Some(force)) => format!("{}, or {force}", steps.join(", ")),
        (false, None) => steps.join(", "),
        (true, Some(force)) => force.to_string(),
        (true, None) => String::new(),
    };
    let e = IrisError::invalid(format!(
        "{} of the {requested} job(s) cannot be deleted: {}; nothing was deleted",
        refusals.len(),
        listed.join(", ")
    ))
    .with_detail("refused", refusals.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>());
    if hint.is_empty() { e } else { e.with_hint(hint) }
}

/// Wait for a job, then finish per `args` (shared with `video generate`).
pub(crate) async fn wait_parsed(
    ctx: &AppContext,
    id: &JobId,
    args: &WaitArgs,
    save: SaveMode,
    warnings: &mut Vec<Warning>,
) -> Result<JobResult, IrisError> {
    let rec = wait_until_terminal(ctx, id, warnings).await?;
    match rec.status() {
        JobStatus::Succeeded if args.download => {
            let rec = download_outputs(ctx, id, &args.target, save, warnings).await?;
            Ok(job_result(ctx, &rec))
        }
        JobStatus::Succeeded => {
            warnings.extend(retention_warning(&rec, ctx.now()));
            Ok(job_result(ctx, &rec))
        }
        _ => Err(job_error(&rec)),
    }
}

/// The `{job, next_steps}` result for a record.
pub(crate) fn job_result(ctx: &AppContext, rec: &JobRecord) -> JobResult {
    let commands = Commands::of(ctx);
    JobResult { job: commands.view(rec.to_view()), next_steps: next_steps(&commands, rec) }
}

/// Suggested follow-up commands for a job in its current state.
fn next_steps(commands: &Commands, rec: &JobRecord) -> Vec<String> {
    let id = rec.job_id();
    match rec.status() {
        JobStatus::Submitting | JobStatus::Running => {
            vec![
                commands.line(format_args!("jobs status {id}")),
                commands.line(format_args!("jobs wait {id}")),
            ]
        }
        JobStatus::Succeeded if has_downloadable(rec) => {
            vec![commands.line(format_args!("jobs download {id}"))]
        }
        _ => Vec::new(),
    }
}

/// How the hints, warnings, and next steps of this invocation name `iris`
/// commands. They assume the environment of this invocation (the same state
/// directory variables, for one), with one exception: when the config file was
/// chosen explicitly (`--config`, or `IRIS_CONFIG`), every command names it with
/// `--config <absolute path>`, since the config file can decide where the jobs
/// live (`state_dir`) and which base URL is used. Nothing else is added.
pub(crate) struct Commands {
    /// `--config <shell-quoted absolute path>`, if needed.
    config: Option<String>,
}

impl Commands {
    pub(crate) fn of(ctx: &AppContext) -> Commands {
        let file = &ctx.settings.config_file;
        let config = (file.source != SettingSource::Default)
            .then(|| format!("--config {}", shell_quote(&file.value.to_string_lossy())));
        Commands { config }
    }

    /// `iris <args>` as a command line.
    fn line(&self, args: std::fmt::Arguments<'_>) -> String {
        match &self.config {
            Some(config) => format!("iris {config} {args}"),
            None => format!("iris {args}"),
        }
    }

    /// `text` with every command it names (`` `iris …` ``) naming the config file.
    fn text(&self, text: &str) -> String {
        match &self.config {
            Some(config) => with_config_arg(text, config),
            None => text.to_string(),
        }
    }

    fn body(&self, body: &mut ErrorBody) {
        body.message = self.text(&body.message);
        body.hint = body.hint.as_deref().map(|h| self.text(h));
    }

    /// A job view whose recorded errors name commands as this invocation must.
    fn view(&self, mut view: JobView) -> JobView {
        if self.config.is_some() {
            if let Some(error) = &mut view.error {
                self.body(error);
            }
            for error in view.outputs.iter_mut().filter_map(|o| o.last_error.as_mut()) {
                self.body(error);
            }
        }
        view
    }

    /// `result` and the warnings added since `start`, naming commands as this
    /// invocation must.
    pub(crate) fn finish<T>(
        &self,
        result: Result<T, IrisError>,
        warnings: &mut [Warning],
        start: usize,
    ) -> Result<T, IrisError> {
        if self.config.is_none() {
            return result;
        }
        for warning in warnings.iter_mut().skip(start) {
            warning.message = self.text(&warning.message);
        }
        result.map_err(|mut e| {
            e.message = self.text(&e.message);
            e.hint = e.hint.as_deref().map(|h| self.text(h));
            e
        })
    }
}

/// `text` with `config` (e.g. `--config /x.toml`) inserted after the `iris` of
/// every `` `iris …` `` command that does not name a config file yet.
fn with_config_arg(text: &str, config: &str) -> String {
    const COMMAND: &str = "`iris ";
    let mut out = String::with_capacity(text.len() + config.len());
    let mut rest = text;
    while let Some(at) = rest.find(COMMAND) {
        let (before, after) = rest.split_at(at + COMMAND.len());
        out.push_str(before);
        if !after.starts_with("--config ") {
            out.push_str(config);
            out.push(' ');
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// `value` as one POSIX shell word: as is when it holds only characters no shell
/// treats specially, otherwise single-quoted (a `'` becomes `'\''`). Hand-rolled:
/// the rule is two lines, and the crate that implements it is not a dependency.
fn shell_quote(value: &str) -> String {
    let plain =
        !value.is_empty() && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-+,:@%=".contains(&b));
    if plain { value.to_string() } else { format!("'{}'", value.replace('\'', "'\\''")) }
}

/// Outputs a download may still get (`pending` or `failed`, with a usable URI).
fn has_downloadable(rec: &JobRecord) -> bool {
    rec.outputs().iter().any(JobOutput::awaits_download)
}

/// Warning `retention_limited` for a succeeded job with outputs still to download.
/// `remote_expires_at` is the earliest time the provider may delete them
/// (submission time plus the documented retention), hence "at least until".
fn retention_warning(rec: &JobRecord, now: Timestamp) -> Option<Warning> {
    let until = rec.remote_expires_at()?;
    (rec.status() == JobStatus::Succeeded && has_downloadable(rec)).then(|| {
        let id = rec.job_id();
        let message = if rec.remote_expired(now) {
            format!(
                "the provider's documented retention for the outputs of {id} ended at about {until}; they may \
                 already be deleted, but `iris jobs download {id}` still tries"
            )
        } else {
            format!(
                "the provider keeps the outputs of {id} at least until about {until}; download them before then \
                 with `iris jobs download {id}`"
            )
        };
        Warning::new("retention_limited", message)
    })
}

/// Attach the job's identifiers, status, and provider to an error.
pub(crate) fn with_job_context(e: IrisError, rec: &JobRecord) -> IrisError {
    let mut e = e.with_job(rec.job_id().to_string(), Some(rec.status()));
    if e.remote_operation_id.is_none()
        && let Some(remote) = rec.remote_operation_id()
    {
        e = e.with_remote_operation(remote);
    }
    if e.provider.is_none() {
        e = e.with_provider(rec.provider());
    }
    e
}

/// When a record that is still `submitting` will be reported as
/// `submission_unknown` (the stale-`submitting` rule of docs/jobs.md): its creation time
/// plus the store's paid-submit budget and grace period.
pub(crate) fn submission_unknown_at(ctx: &AppContext, rec: &JobRecord) -> Option<Timestamp> {
    rec.created_at().checked_add(ctx.store.submit_budget().saturating_add(jobs::SUBMIT_GRACE)).ok()
}

/// "at about <time>" for [`submission_unknown_at`], or "later" if unknown.
fn at_about(when: Option<Timestamp>) -> String {
    when.map(|t| format!("at about {t}")).unwrap_or_else(|| "later".to_string())
}

/// What happens to a job the caller stops waiting for.
fn still_pending(ctx: &AppContext, rec: &JobRecord) -> String {
    if rec.status() == JobStatus::Submitting {
        format!(
            "it has no operation id yet (its submission was not recorded); unless one is recorded, it becomes \
             submission_unknown {}",
            at_about(submission_unknown_at(ctx, rec))
        )
    } else {
        "it continues remotely".to_string()
    }
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
    let base = match rec.error_view() {
        Some(body) => error_from_body(&body),
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

fn wait_timeout(ctx: &AppContext, rec: &JobRecord, waited: Duration) -> IrisError {
    let id = rec.job_id();
    with_job_context(
        IrisError::new(
            ErrorCode::WaitTimeout,
            format!(
                "job {id} did not finish within {}; {}",
                humantime::format_duration(waited),
                still_pending(ctx, rec)
            ),
        )
        .with_hint(format!("resume with `iris jobs wait {id}` (or check with `iris jobs status {id}`)")),
        rec,
    )
}

fn interrupted_wait(ctx: &AppContext, rec: &JobRecord) -> IrisError {
    let id = rec.job_id();
    with_job_context(
        IrisError::new(
            ErrorCode::Interrupted,
            format!("stopped waiting for job {id}; {}", still_pending(ctx, rec)),
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
    let video = video_adapter(ctx, provider).map_err(|e| with_job_context(e, rec))?;
    let pctx = ctx.provider_context(provider).map_err(|e| with_job_context(e, rec))?;
    ctx.settings.warn_non_default_base_url(provider, warnings);
    let status = tokio::select! {
        result = video.poll(remote, &pctx) => result.map_err(|e| with_job_context(e, rec))?,
        () = ctx.interrupt.after(seen) => return Err(interrupted_wait(ctx, rec)),
    };
    let (updated, applied) = ctx
        .store
        .update(rec.job_id(), |r| r.apply_poll(status, video.output_retention(), ctx.now()))
        .map_err(|e| with_job_context(e, rec))?;
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
                        "Job {id} has no operation id yet: another iris process is still submitting it, or the \
                         process that submitted it was interrupted. Waiting; unless an operation id is recorded, \
                         it becomes submission_unknown {}",
                        at_about(submission_unknown_at(ctx, &rec))
                    ));
                }
            }
            _ => {
                let provider = rec.provider();
                let video = video_adapter(ctx, provider)?;
                let pctx = ctx.provider_context(provider).map_err(|e| with_job_context(e, &rec))?;
                ctx.settings.warn_non_default_base_url(provider, warnings);
                let Some(remote) = rec.remote_operation_id().map(str::to_string) else {
                    return Err(with_job_context(
                        IrisError::internal(format!("job {id} is running but has no operation id")),
                        &rec,
                    ));
                };
                let polled = tokio::select! {
                    result = video.poll(&remote, &pctx) => result,
                    () = ctx.interrupt.after(seen) => return Err(interrupted_wait(ctx, &rec)),
                    () = tokio::time::sleep_until(deadline) => return Err(wait_timeout(ctx, &rec, limit)),
                };
                match polled {
                    Ok(status) => {
                        let retention = video.output_retention();
                        let (updated, applied) = ctx
                            .store
                            .update(id, |r| r.apply_poll(status, retention, ctx.now()))
                            .map_err(|e| with_job_context(e, &rec))?;
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
            () = ctx.interrupt.after(seen) => return Err(interrupted_wait(ctx, &rec)),
            () = tokio::time::sleep_until(deadline) => return Err(wait_timeout(ctx, &rec, limit)),
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

/// docs/jobs.md "Downloads" steps 1–4 for every output of a job. `save` decides what a
/// file already at a target means (see [`SaveMode`]).
async fn download_outputs(
    ctx: &AppContext,
    id: &JobId,
    target: &Target,
    save: SaveMode,
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
                    let e = IrisError::new(ErrorCode::Interrupted, format!("stopped waiting to download job {id}"))
                        .with_job(id.to_string(), None)
                        .with_hint(format!("download later with `iris jobs download {id}`"));
                    return Err(match ctx.store.load(id) {
                        Ok(rec) => with_job_context(e, &rec),
                        Err(_) => e,
                    });
                }
            }
        }
    };
    let rec = ctx.store.load(id)?;
    match rec.status() {
        JobStatus::Succeeded => {}
        JobStatus::Submitting | JobStatus::Running => {
            let checked = rec.last_checked_at().map(|t| format!(" (last checked {t})")).unwrap_or_default();
            return Err(with_job_context(
                IrisError::new(
                    ErrorCode::JobNotReady,
                    format!("job {id} is still {}{checked}; its outputs are not ready", rec.status()),
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

    let provider = rec.provider();
    let video = video_adapter(ctx, provider).map_err(|e| with_job_context(e, &rec))?;
    let base_url = ctx.settings.provider(provider).base_url.value.clone();
    // `--overwrite` given to this command asks for a fresh copy of every output,
    // so a file saved earlier is never reused as is (the recorded plan's
    // overwrite only decides how a file in the way is treated).
    let refetch = target.overwrite;
    let decisions: Vec<DownloadDecision> = rec
        .outputs()
        .iter()
        .zip(&plan.paths)
        .map(|(out, path)| artifacts::decide_download(out.recorded_file(), path, refetch))
        .collect();
    // A fetch from the provider's own origin needs the credential: check that it
    // is present before any output directory is created.
    let needs_credential = rec.outputs().iter().zip(&decisions).any(|(out, decision)| {
        matches!(decision, DownloadDecision::Fetch | DownloadDecision::Refetch)
            && out.unusable_reason().is_none()
            && Url::parse(&out.remote_uri).is_ok_and(|u| http::same_origin(&u, &base_url))
    });
    if needs_credential {
        ctx.settings.require_credential(provider).map_err(|e| with_job_context(e, &rec))?;
        ctx.settings.warn_non_default_base_url(provider, warnings);
    }
    artifacts::preflight_dirs(&plan.paths, true).map_err(|e| with_job_context(e, &rec))?;
    let access = Access {
        provider,
        base_url: &base_url,
        idle_timeout: ctx.settings.timeouts(provider).download_idle,
        media_types,
        mode: match save {
            SaveMode::Download => FinalizeMode::for_download(overwrite),
            SaveMode::Generated => FinalizeMode::for_generated(overwrite),
        },
        seen,
    };

    let mut remote_failure: Option<IrisError> = None;
    for ((out, planned), &decision) in rec.outputs().iter().zip(&plan.paths).zip(&decisions) {
        // The provider gave no usable URI for this output (recorded `failed` when
        // the job finished): nothing can fetch it, and the others are unaffected.
        // One warning per output and command: when this command's own poll saw
        // the job finish (`jobs wait`, `video generate`, or the refresh of `jobs
        // download`), that poll has reported it already.
        if let Some(warning) = out.unusable_warning(id) {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
            continue;
        }
        let recorded = out.recorded_file();
        // Where a fetch saves the output: the planned target, or, to replace the
        // file saved earlier, its recorded path (which may carry an adjusted
        // extension).
        let (path, mode) = match (decision, recorded) {
            (DownloadDecision::Refetch, Some(file)) => (file.path.to_path_buf(), FinalizeMode::Overwrite),
            _ => (planned.clone(), access.mode),
        };
        let path = path.as_path();
        if decision != DownloadDecision::AlreadyDownloaded {
            // Partial files of this target that no running process is writing:
            // left by a run that was killed or crashed. The job's download lock
            // does not cover them (another job may be saved to the same target),
            // but every live writer holds a lock on its own partial file, and
            // those are left alone.
            for stale in PartFile::remove_stale(path) {
                ctx.progress.line(format!(
                    "Removed a partial download that no running iris process was writing (left by an \
                     interrupted earlier run): {}",
                    stale.display()
                ));
            }
        }
        match (decision, recorded) {
            (DownloadDecision::AlreadyDownloaded, Some(file)) => {
                warnings.push(artifacts::already_present_warning(file.path));
                continue;
            }
            (DownloadDecision::CopyLocal, Some(file)) => {
                ctx.progress.line(format!("Copying output {} of job {id} to {}", out.index, path.display()));
                match artifacts::copy_local(file, path, out.index, media_types, access.mode) {
                    Ok(saved) => {
                        record_saved(ctx, id, out.index, saved, warnings)?;
                        continue;
                    }
                    // The recorded file changed (or vanished) after it was checked:
                    // it is no copy of the output any more, so fetch the output.
                    Err(_) if !artifacts::is_intact(&file) => ctx.progress.line(format!(
                        "{} changed since it was downloaded; downloading output {} of job {id} again",
                        file.path.display(),
                        out.index
                    )),
                    Err(e) => return Err(with_job_context(e, &rec)),
                }
            }
            (DownloadDecision::Refetch, Some(file)) if !refetch => ctx.progress.line(format!(
                "{} is not a complete, valid media file; downloading output {} of job {id} again to replace it",
                file.path.display(),
                out.index
            )),
            _ => {}
        }

        // Fetch. No local short-circuit on the retention estimate: the provider may
        // keep outputs longer, so the file host's answer decides.
        // Download trust is decided now, against the base URL configured now: a
        // refusal fails this output only, never the job, and a later download with
        // another configuration checks again.
        if let Err(e) = video.check_output_uri(&out.remote_uri, &base_url) {
            let e = match e.hint.clone() {
                Some(hint) => e.with_hint(format!("{hint} with `iris jobs download {id}`")),
                None => e,
            };
            let now = ctx.now();
            ctx.store.update(id, |r| r.mark_output_failed(out.index, &e, now))?;
            remote_failure.get_or_insert(e);
            continue;
        }
        ctx.progress.line(format!("Downloading output {} of job {id}", out.index));
        match fetch(ctx, out, path, mode, &access).await {
            Ok(saved) => record_saved(ctx, id, out.index, saved, warnings)?,
            Err(FetchFailure::Interrupted) => return Err(interrupted_download(&rec)),
            Err(FetchFailure::Local(e)) => return Err(with_job_context(e, &rec)),
            Err(FetchFailure::Remote(e)) => {
                let now = ctx.now();
                // Content that is not the expected media (an error page served as
                // a video, a truncated file) is worth downloading again.
                let e = if e.code == ErrorCode::InvalidMedia { e.with_retryable(Some(true)) } else { e };
                let e = refused_or_gone(e, &rec, now);
                let e = if e.code == ErrorCode::ArtifactExpired || e.hint.is_some() {
                    e
                } else {
                    e.with_hint(format!(
                        "the job itself succeeded; retry the download with `iris jobs download {id}` (nothing \
                         is regenerated)"
                    ))
                };
                // A file saved earlier stays the output's artifact (see
                // `JobRecord::mark_output_failed`); say what became of it.
                let e = match recorded {
                    Some(file) => {
                        let note = earlier_file_note(&file);
                        let hint = e.hint.clone();
                        e.with_hint(hint.map_or_else(|| note.clone(), |h| format!("{h}; {note}")))
                    }
                    None => e,
                };
                if e.code == ErrorCode::ArtifactExpired {
                    ctx.store.update(id, |r| r.mark_output_expired(out.index, &e, now))?;
                } else {
                    ctx.store.update(id, |r| r.mark_output_failed(out.index, &e, now))?;
                }
                remote_failure.get_or_insert(e);
            }
        }
    }
    let rec = ctx.store.load(id)?;
    match remote_failure {
        Some(e) => Err(with_job_context(e, &rec)),
        None => Ok(rec),
    }
}

/// For the error of a failed fetch: the state of the file recorded for the output
/// earlier. The fetch never touches it and the record keeps naming it, but the
/// file may have been deleted or edited since it was downloaded (that is one
/// reason to fetch the output again), so it is called unchanged only while it is
/// intact.
fn earlier_file_note(file: &RecordedFile<'_>) -> String {
    let path = file.path.display();
    if artifacts::is_intact(file) {
        format!("the file saved earlier, {path}, is unchanged")
    } else if std::fs::symlink_metadata(file.path).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        format!("the file saved earlier, {path}, no longer exists")
    } else {
        format!("the file saved earlier, {path}, no longer matches the downloaded output")
    }
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

/// A file host's 403/404 (reported by the downloader as retryable
/// `download_failed`) means the output is gone only once the provider's retention
/// period has passed (`remote_expires_at`): then it is `artifact_expired`. Before
/// that it stays a retryable `download_failed` (the output is re-downloadable),
/// with a hint on what to check. 410 is already `artifact_expired`.
fn refused_or_gone(e: IrisError, rec: &JobRecord, now: Timestamp) -> IrisError {
    let status = e.provider_status;
    if e.code != ErrorCode::DownloadFailed || !matches!(status, Some(403 | 404)) {
        return e;
    }
    let id = rec.job_id();
    let status = status.unwrap_or_default();
    match rec.remote_expires_at() {
        Some(until) if rec.remote_expired(now) => {
            let mut gone = IrisError::new(
                ErrorCode::ArtifactExpired,
                format!(
                    "the file host no longer serves this output (HTTP {status}), and the provider's retention \
                     period for the outputs of {id} ended at about {until}"
                ),
            )
            .with_retryable(Some(false))
            .with_hint(
                "the output can no longer be downloaded; getting it again means submitting (and paying for) a \
                 new job",
            );
            gone.provider = e.provider;
            gone.provider_status = e.provider_status;
            gone.details = e.details.clone();
            gone
        }
        until => {
            let why = match until {
                Some(t) => format!(
                    "the provider's retention period has not passed (it lasts at least until about {t}), so the \
                     output should still exist"
                ),
                None => {
                    "the provider documents no retention period, so the output may still exist".to_string()
                }
            };
            let mut e = e.with_retryable(Some(true)).with_hint(format!(
                "{why}; retry with `iris jobs download {id}` (nothing is regenerated); if it keeps failing, \
                 check the API key and the base URL"
            ));
            e.message = format!("{}; the output is not treated as expired", e.message);
            e
        }
    }
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

/// Stream one output into a temp file next to `path`, validate, and finalize with
/// `mode`.
async fn fetch(
    ctx: &AppContext,
    out: &JobOutput,
    path: &Path,
    mode: FinalizeMode,
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
        Ok(_) => artifacts::finalize_download(part, out.index, access.media_types, mode).map_err(|e| {
            if e.code == ErrorCode::InvalidMedia { FetchFailure::Remote(e) } else { FetchFailure::Local(e) }
        }),
        Err(e @ (DownloadError::Io { .. } | DownloadError::Internal { .. })) => {
            Err(FetchFailure::Local(e.into_iris()))
        }
        Err(e) => Err(FetchFailure::Remote(e.into_iris())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_in_text_name_the_config_file_once() {
        let config = "--config '/tmp/my config.toml'";
        assert_eq!(
            with_config_arg(
                "resume with `iris jobs wait job_x` (or check with `iris jobs status job_x`)",
                config
            ),
            "resume with `iris --config '/tmp/my config.toml' jobs wait job_x` (or check with `iris --config \
             '/tmp/my config.toml' jobs status job_x`)"
        );
        // Already named (e.g. a hint rebuilt from a recorded one): unchanged.
        let named = "retry with `iris --config /a.toml jobs download job_x`";
        assert_eq!(with_config_arg(named, config), named);
        // Text that names no command, or names iris outside a command, is unchanged.
        assert_eq!(with_config_arg("the iris job is running", config), "the iris job is running");
    }

    #[test]
    fn paths_are_quoted_for_the_shell_only_when_needed() {
        assert_eq!(shell_quote("/home/you/.config/iris/work.toml"), "/home/you/.config/iris/work.toml");
        assert_eq!(shell_quote("/tmp/my config.toml"), "'/tmp/my config.toml'");
        assert_eq!(shell_quote("/tmp/it's.toml"), "'/tmp/it'\\''s.toml'");
        assert_eq!(shell_quote("/tmp/$HOME;rm"), "'/tmp/$HOME;rm'");
        assert_eq!(shell_quote(""), "''");
    }
}
