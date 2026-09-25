//! The job store over `<state_dir>/jobs/` (see docs/jobs.md).
//!
//! Layout: `<job_id>.json` (record), `<job_id>.lock` (exclusive advisory lock held
//! only for a read-modify-write), `<job_id>.download.lock` (exclusive lock held for
//! a whole download). Directories are created 0700 and files 0600.
//!
//! Records are replaced atomically (temp file in the same directory → `sync_all` →
//! rename → best-effort directory fsync), so lock-free readers never see a partial
//! record. Locks use `std::fs::File::lock` (flock on Linux and macOS): they are per
//! open file description, so threads of one process exclude each other as well as
//! separate processes.
//!
//! The blocking lock calls ([`JobStore::update`], [`JobStore::delete`],
//! [`JobStore::download_lock`]) park the calling thread in `flock`. Record locks are
//! held only for one short read-modify-write, but a download lock is held for a
//! whole download: async code waits for it with [`JobStore::download_lock_async`],
//! which never blocks the runtime (so Ctrl-C handling keeps working).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use super::record::{JOB_RECORD_VERSION, JobRecord};
use super::{JobId, now};
use crate::domain::{JobStatus, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::Timeouts;

/// Name of the jobs directory inside the state directory.
const JOBS_DIR: &str = "jobs";

/// Attempts of the `PaidSubmit` retry class (see docs/architecture.md "Where
/// invariants live").
const PAID_SUBMIT_ATTEMPTS: u32 = 3;
/// Longest wait between two `PaidSubmit` attempts: `Retry-After` is honored up to
/// 60s; the exponential backoff cap (30s) is lower.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(60);

/// Worst-case wall-clock time of one `PaidSubmit` call with `timeouts`:
/// `attempts × (connect + longest submit attempt) + (attempts − 1) × longest retry
/// wait`, where the longest attempt is the submit timeout plus the largest upload
/// allowance ([`Timeouts::max_submit_attempt`]). With the default timeouts this is
/// 3 × (15s + 60s + 600s) + 2 × 60s = 2145s.
///
/// This is the "submit timeout" of the stale-`submitting` rule: a record is only
/// declared abandoned once no live submitter can still be waiting for its answer.
pub fn paid_submit_budget(timeouts: &Timeouts) -> Duration {
    let per_attempt = timeouts.connect.saturating_add(timeouts.max_submit_attempt());
    per_attempt
        .saturating_mul(PAID_SUBMIT_ATTEMPTS)
        .saturating_add(MAX_RETRY_WAIT.saturating_mul(PAID_SUBMIT_ATTEMPTS - 1))
}

/// Records returned by [`JobStore::list`], newest first, plus one
/// `job_record_unreadable` warning per skipped record.
#[derive(Debug, Clone, Default)]
pub struct JobListing {
    pub records: Vec<JobRecord>,
    pub warnings: Vec<Warning>,
    /// The skipped records that are regular files named `<job_id>.json` in the
    /// jobs directory, with the warning each produced (also in `warnings`): what
    /// `jobs delete --all --force` removes along with the readable records.
    pub unreadable: Vec<UnreadableRecord>,
}

/// A record file that [`JobStore::list`] could not read.
#[derive(Debug, Clone)]
pub struct UnreadableRecord {
    pub id: JobId,
    /// Its `job_record_unreadable` warning.
    pub warning: Warning,
}

/// Why [`JobStore::delete`] refuses a record without `force` (or at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// There is no local record with this id (`job_not_found`; `force` does not help).
    NotFound,
    /// The record cannot be read (corrupt, written by a newer Iris, or an I/O error).
    Unreadable,
    /// `running`, or `submitting` while its submitter may still be waiting for the
    /// provider's answer.
    Active,
    /// Still `submitting` past the stale threshold (reported as
    /// `submission_unknown`): the submitter has probably stopped, but a live one
    /// could still record the operation id.
    Abandoned,
    /// `succeeded`, with outputs not downloaded yet (`pending` or `failed`) while
    /// the provider still keeps them: the record is the only reference to paid
    /// outputs that can still be saved.
    NotDownloaded,
}

/// A refused deletion: its kind, a short description for lists, and the error
/// [`JobStore::delete`] returns (with the job's id and on-disk status).
#[derive(Debug, Clone)]
pub struct DeleteRefusal {
    pub kind: RefusalKind,
    pub summary: String,
    pub error: IrisError,
}

/// Exclusive lock on `<job_id>.download.lock`, held for the whole download of a
/// job's outputs. Released when dropped.
#[derive(Debug)]
pub struct DownloadLock {
    _file: File,
    job_id: JobId,
}

impl DownloadLock {
    pub fn job_id(&self) -> &JobId {
        &self.job_id
    }
}

/// Exclusive lock on `<job_id>.lock` for one read-modify-write. Released when dropped.
struct RecordLock {
    _file: File,
}

/// Persistent store of job records under `<state_dir>/jobs/`.
///
/// Constructing a store touches nothing on disk; directories are created on the
/// first write. Every method takes a validated [`JobId`], so paths cannot escape
/// the jobs directory.
#[derive(Debug, Clone)]
pub struct JobStore {
    dir: PathBuf,
    submit_budget: Duration,
}

impl JobStore {
    /// A store for `<state_dir>/jobs/`. The stale-`submitting` rule uses the
    /// worst-case `PaidSubmit` duration with default timeouts
    /// ([`paid_submit_budget`]`(&Timeouts::default())`, 2145s); use
    /// [`JobStore::with_submit_budget`] when timeouts are configured.
    pub fn new(state_dir: impl AsRef<Path>) -> JobStore {
        JobStore {
            dir: state_dir.as_ref().join(JOBS_DIR),
            submit_budget: paid_submit_budget(&Timeouts::default()),
        }
    }

    /// Set the submit budget of the stale-`submitting` rule: a `submitting` record
    /// older than `submit_budget + SUBMIT_GRACE` is reported and rewritten as
    /// `submission_unknown`. Pass [`paid_submit_budget`] of the configured
    /// timeouts, never the bare submit timeout: a slower threshold only
    /// delays the report, a faster one relabels live submissions.
    pub fn with_submit_budget(mut self, submit_budget: Duration) -> JobStore {
        self.submit_budget = submit_budget;
        self
    }

    /// The submit budget used by the stale-`submitting` rule.
    pub fn submit_budget(&self) -> Duration {
        self.submit_budget
    }

    /// The jobs directory (`<state_dir>/jobs`).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of a job's record file.
    pub fn record_path(&self, id: &JobId) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    fn lock_path(&self, id: &JobId) -> PathBuf {
        self.dir.join(format!("{id}.lock"))
    }

    fn download_lock_path(&self, id: &JobId) -> PathBuf {
        self.dir.join(format!("{id}.download.lock"))
    }

    /// Persist a new record. Fails (`internal_error`) if a record with this id
    /// already exists; never overwrites.
    pub fn create(&self, record: &JobRecord) -> Result<(), IrisError> {
        self.ensure_dir()?;
        let path = self.record_path(record.job_id());
        // Later processes judge a stale `submitting` record by at least this
        // process's submit budget (its timeouts may be longer than theirs).
        let mut record = record.clone();
        record.record_submit_budget(self.submit_budget);
        let bytes = encode(&record)?;
        write_atomic(&path, &bytes, Replace::No, &mut |_| Ok(())).map_err(|e| {
            if e.kind() == io::ErrorKind::AlreadyExists {
                IrisError::internal(format!("job record {} already exists", path.display()))
            } else {
                IrisError::io(format_args!("cannot write job record {}", path.display()), &e)
            }
        })
    }

    /// Read one record (without locking). A `submitting` record past the stale
    /// threshold is returned as `submission_unknown` (not persisted here).
    ///
    /// Errors: `job_not_found`, `state_invalid` (corrupt, mismatched id, or written
    /// by a newer iris), `io_error`.
    pub fn load(&self, id: &JobId) -> Result<JobRecord, IrisError> {
        let mut record = self.read(id)?;
        record.resolve_stale_submitting(now(), self.submit_budget);
        Ok(record)
    }

    /// Locked read-modify-write. Takes the job's exclusive record lock (blocking),
    /// re-reads the record, applies the stale-`submitting` rule, runs `f`, and
    /// atomically writes the result. If `f` fails nothing is written.
    ///
    /// Returns the record as written and `f`'s value. Do not call `update` for the
    /// same job from inside `f` (it would wait for itself).
    pub fn update<T>(
        &self,
        id: &JobId,
        f: impl FnOnce(&mut JobRecord) -> Result<T, IrisError>,
    ) -> Result<(JobRecord, T), IrisError> {
        let _lock = self.lock_record(id)?;
        let mut record = self.read(id)?;
        record.resolve_stale_submitting(now(), self.submit_budget);
        let value = f(&mut record)?;
        self.write(&record)?;
        Ok((record, value))
    }

    /// All readable records, newest first (by `created_at`, then id), without
    /// locking. Unreadable records are skipped with a `job_record_unreadable`
    /// warning; temp and lock files are ignored. A missing jobs directory is an
    /// empty list.
    pub fn list(&self) -> Result<JobListing, IrisError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(JobListing::default()),
            Err(e) => {
                return Err(IrisError::io(
                    format_args!("cannot read job directory {}", self.dir.display()),
                    &e,
                ));
            }
        };
        let now = now();
        let mut listing = JobListing::default();
        for entry in entries {
            let entry = entry.map_err(|e| {
                IrisError::io(format_args!("cannot read job directory {}", self.dir.display()), &e)
            })?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".json")).filter(|s| JobId::is_valid(s))
            else {
                continue;
            };
            let Ok(id) = JobId::parse(id) else { continue };
            match self.read(&id) {
                Ok(mut record) => {
                    record.resolve_stale_submitting(now, self.submit_budget);
                    listing.records.push(record);
                }
                // Deleted between read_dir and read: not an error for a listing.
                Err(e) if e.code == ErrorCode::JobNotFound => {}
                Err(e) => {
                    let warning = Warning::new(
                        crate::domain::WarningCode::JobRecordUnreadable,
                        format!("skipped job record {}: {}", entry.path().display(), e.message),
                    );
                    if fs::symlink_metadata(entry.path()).is_ok_and(|m| m.file_type().is_file()) {
                        listing.unreadable.push(UnreadableRecord { id, warning: warning.clone() });
                    }
                    listing.warnings.push(warning);
                }
            }
        }
        listing
            .records
            .sort_by(|a, b| b.created_at().cmp(&a.created_at()).then_with(|| b.job_id().cmp(a.job_id())));
        Ok(listing)
    }

    /// Whether [`JobStore::delete`]`(id, force)` would delete the record, decided
    /// by the same rule without deleting anything and without locking (so a
    /// caller can check every record before deleting any).
    ///
    /// The decision uses the status on disk, without the stale-`submitting` rule: a
    /// record still `submitting` may belong to a live process whose submission is
    /// merely slow, and deleting it would lose the operation id that process is
    /// about to record. Such a record needs `force` even when it is reported as
    /// `submission_unknown` (kind [`RefusalKind::Abandoned`]).
    pub fn check_delete(&self, id: &JobId, force: bool) -> Result<(), DeleteRefusal> {
        let record = match self.read(id) {
            Ok(record) => record,
            Err(e) if e.code == ErrorCode::JobNotFound => {
                return Err(DeleteRefusal {
                    kind: RefusalKind::NotFound,
                    summary: "no local record".into(),
                    error: e,
                });
            }
            Err(_) if force => return Ok(()),
            Err(e) => {
                return Err(DeleteRefusal {
                    kind: RefusalKind::Unreadable,
                    summary: "unreadable record".into(),
                    error: e.with_hint(format!(
                        "pass --force to delete the unreadable local record (`iris jobs delete {id} --force`)"
                    )),
                });
            }
        };
        if force {
            return Ok(());
        }
        let status = record.status();
        let now = now();
        let waiting: Vec<u32> = if status == JobStatus::Succeeded && !record.remote_expired(now) {
            record.outputs().iter().filter(|o| o.awaits_download()).map(|o| o.index).collect()
        } else {
            Vec::new()
        };
        if !waiting.is_empty() {
            let listed = waiting.iter().map(u32::to_string).collect::<Vec<_>>().join(", ");
            let kept = match record.remote_expires_at() {
                Some(until) => format!("the provider keeps them at least until about {until}"),
                None => {
                    "the provider documents no retention period, so they may still be available".to_string()
                }
            };
            let summary = match record.remote_expires_at() {
                Some(until) => {
                    format!("succeeded; output(s) {listed} not downloaded, kept until about {until}")
                }
                None => format!("succeeded; output(s) {listed} not downloaded"),
            };
            let error = IrisError::invalid(format!(
                "job {id} succeeded, but its output(s) {listed} were not downloaded; {kept}, and deleting the \
                 local record would lose the only reference to them"
            ))
            .with_job(id.to_string(), Some(status))
            .with_detail("outputs_not_downloaded", waiting)
            .with_detail("remote_expires_at", record.remote_expires_at().map(|t| t.to_string()))
            .with_hint(format!(
                "download them first with `iris jobs download {id}`, or pass --force to delete the local record \
                 anyway (the outputs stay with the provider until its retention period ends, but Iris can no \
                 longer fetch them)"
            ));
            return Err(DeleteRefusal { kind: RefusalKind::NotDownloaded, summary, error });
        }
        if !record.is_active() {
            return Ok(());
        }
        let refusal = if record.is_stale_submitting(now, self.submit_budget) {
            DeleteRefusal {
                kind: RefusalKind::Abandoned,
                summary: "submitting; probably abandoned, shown as submission_unknown".into(),
                error: IrisError::invalid(format!(
                    "job {id} is recorded as submitting; the submitting process has probably stopped (the job is \
                     reported as submission_unknown), but deleting the local record would lose the provider \
                     operation id if that process is still running"
                ))
                .with_hint(format!(
                    "no command can finish this record; check the provider console for the request, then delete \
                     the local record with `iris jobs delete {id} --force` (a remote job, if one was created, is \
                     not cancelled)"
                )),
            }
        } else {
            DeleteRefusal {
                kind: RefusalKind::Active,
                summary: status.to_string(),
                error: IrisError::invalid(format!(
                    "job {id} is still {status}; deleting its local record would make the job unrecoverable"
                ))
                .with_hint(format!(
                    "wait for the job to finish (`iris jobs wait {id}`), or pass --force to delete the local record \
                     anyway (the remote job is not cancelled)"
                )),
            }
        };
        let DeleteRefusal { kind, summary, error } = refusal;
        Err(DeleteRefusal { kind, summary, error: error.with_job(id.to_string(), Some(status)) })
    }

    /// Delete a job's LOCAL record and its lock files (never downloaded media, never
    /// anything remote), under the job's record lock. Refuses what
    /// [`JobStore::check_delete`] refuses: active jobs (`submitting`/`running`,
    /// which would become unrecoverable), succeeded jobs whose outputs are not all
    /// downloaded while the provider still keeps them (`remote_expires_at` in the
    /// future, or unknown), and unreadable records unless `force`, and a missing
    /// record always (`job_not_found`).
    pub fn delete(&self, id: &JobId, force: bool) -> Result<(), IrisError> {
        let _lock = self.lock_record(id)?;
        self.check_delete(id, force).map_err(|refusal| refusal.error)?;
        let record_path = self.record_path(id);
        fs::remove_file(&record_path).or_else(ignore_not_found).map_err(|e| {
            IrisError::io(format_args!("cannot delete job record {}", record_path.display()), &e)
        })?;
        let _ = fs::remove_file(self.download_lock_path(id));
        let _ = fs::remove_file(self.lock_path(id));
        sync_dir(&self.dir);
        Ok(())
    }

    /// Take the job's exclusive download lock, waiting for any other download of the
    /// same job to finish. Re-read the record after acquiring it.
    ///
    /// This BLOCKS the calling thread (in `flock`) for as long as another process
    /// downloads the job. Do not call it on an async runtime thread; use
    /// [`JobStore::download_lock_async`] there.
    pub fn download_lock(&self, id: &JobId) -> Result<DownloadLock, IrisError> {
        let path = self.download_lock_path(id);
        let file = self.open_lock(id, &path)?;
        file.lock().map_err(|e| IrisError::io(format_args!("cannot lock {}", path.display()), &e))?;
        self.confirm_exists_or_cleanup(id, &path)?;
        Ok(DownloadLock { _file: file, job_id: id.clone() })
    }

    /// Like [`JobStore::download_lock`] but returns `None` instead of waiting when
    /// another process is downloading this job.
    pub fn try_download_lock(&self, id: &JobId) -> Result<Option<DownloadLock>, IrisError> {
        let path = self.download_lock_path(id);
        let file = self.open_lock(id, &path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(e)) => {
                return Err(IrisError::io(format_args!("cannot lock {}", path.display()), &e));
            }
        }
        self.confirm_exists_or_cleanup(id, &path)?;
        Ok(Some(DownloadLock { _file: file, job_id: id.clone() }))
    }

    /// Async form of [`JobStore::download_lock`]: polls
    /// [`JobStore::try_download_lock`] every `poll_interval`, sleeping with
    /// `tokio::time::sleep` in between, so the runtime is never blocked. Waiting
    /// can be cancelled by dropping the future (e.g. in a `tokio::select!` with
    /// Ctrl-C). Callers that want to tell the user they are waiting can call
    /// `try_download_lock` first.
    pub async fn download_lock_async(
        &self,
        id: &JobId,
        poll_interval: Duration,
    ) -> Result<DownloadLock, IrisError> {
        loop {
            if let Some(lock) = self.try_download_lock(id)? {
                return Ok(lock);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    // ----- internals ---------------------------------------------------------

    fn read(&self, id: &JobId) -> Result<JobRecord, IrisError> {
        let path = self.record_path(id);
        let bytes = fs::read(&path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                not_found(id, &self.dir)
            } else {
                IrisError::io(format_args!("cannot read job record {}", path.display()), &e)
            }
        })?;
        parse_record(&bytes, &path, id)
    }

    fn write(&self, record: &JobRecord) -> Result<(), IrisError> {
        let path = self.record_path(record.job_id());
        let bytes = encode(record)?;
        write_atomic(&path, &bytes, Replace::Yes, &mut |_| Ok(()))
            .map_err(|e| IrisError::io(format_args!("cannot write job record {}", path.display()), &e))
    }

    fn lock_record(&self, id: &JobId) -> Result<RecordLock, IrisError> {
        let path = self.lock_path(id);
        let file = self.open_lock(id, &path)?;
        file.lock().map_err(|e| IrisError::io(format_args!("cannot lock {}", path.display()), &e))?;
        self.confirm_exists_or_cleanup(id, &path)?;
        Ok(RecordLock { _file: file })
    }

    /// Open (creating 0600 if needed) a lock file of an existing job.
    fn open_lock(&self, id: &JobId, path: &Path) -> Result<File, IrisError> {
        // Do not create lock files for jobs that do not exist.
        if !self.record_path(id).exists() {
            return Err(not_found(id, &self.dir));
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .map_err(|e| IrisError::io(format_args!("cannot open lock file {}", path.display()), &e))
    }

    /// After acquiring a lock: if the record was deleted meanwhile, remove the lock
    /// file this call may have re-created and report `job_not_found`. (Job ids are
    /// never reused, so a deleted record cannot come back.)
    fn confirm_exists_or_cleanup(&self, id: &JobId, lock_path: &Path) -> Result<(), IrisError> {
        if self.record_path(id).exists() {
            Ok(())
        } else {
            let _ = fs::remove_file(lock_path);
            Err(not_found(id, &self.dir))
        }
    }

    fn ensure_dir(&self) -> Result<(), IrisError> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.dir).map_err(|e| {
            IrisError::io(format_args!("cannot create job directory {}", self.dir.display()), &e)
        })
    }
}

fn not_found(id: &JobId, dir: &Path) -> IrisError {
    IrisError::new(ErrorCode::JobNotFound, format!("no local job record for {id} in {}", dir.display()))
        .with_job(id.to_string(), None)
        .with_hint("run `iris jobs list` to see local jobs (records live in the state directory)")
}

fn state_invalid(path: &Path, id: &JobId, why: impl std::fmt::Display) -> IrisError {
    IrisError::new(ErrorCode::StateInvalid, format!("job record {} is unreadable: {why}", path.display()))
        .with_job(id.to_string(), None)
        .with_detail("path", path.to_string_lossy().into_owned())
        .with_hint("the file may be corrupt; `iris jobs delete <JOB_ID> --force` removes the local record")
}

/// Parse a record, enforcing the version rule before the structural parse so a
/// newer record gets a precise error even if its shape changed.
fn parse_record(bytes: &[u8], path: &Path, id: &JobId) -> Result<JobRecord, IrisError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|e| state_invalid(path, id, e))?;
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| state_invalid(path, id, "missing or invalid schema_version"))?;
    if version > u64::from(JOB_RECORD_VERSION) {
        return Err(IrisError::new(
            ErrorCode::StateInvalid,
            format!(
                "job record {} was written by a newer iris (record schema_version {version}; this iris reads up \
                 to {JOB_RECORD_VERSION})",
                path.display()
            ),
        )
        .with_job(id.to_string(), None)
        .with_detail("path", path.to_string_lossy().into_owned())
        .with_hint("upgrade iris to read this job"));
    }
    let record: JobRecord = serde_json::from_value(value).map_err(|e| state_invalid(path, id, e))?;
    if record.job_id() != id {
        return Err(state_invalid(
            path,
            id,
            format!("it contains job_id {} but the file name says {id}", record.job_id()),
        ));
    }
    Ok(record)
}

/// Serialize a record as written by this version (pretty JSON, trailing newline).
fn encode(record: &JobRecord) -> Result<Vec<u8>, IrisError> {
    let mut value = serde_json::to_value(record)
        .map_err(|e| IrisError::internal(format!("cannot serialize job record: {e}")))?;
    value["schema_version"] = Value::from(JOB_RECORD_VERSION);
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|e| IrisError::internal(format!("cannot serialize job record: {e}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replace {
    Yes,
    No,
}

/// Atomically place `bytes` at `target`: temp file in the same directory →
/// write → `sync_all` → rename (or no-clobber rename) → best-effort directory fsync.
/// `before_persist` runs after the temp file is durable and before the rename (a
/// test seam for simulated interruptions). On any error the temp file is removed
/// and `target` is untouched.
fn write_atomic(
    target: &Path,
    bytes: &[u8],
    replace: Replace,
    before_persist: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    let dir = target.parent().ok_or_else(|| io::Error::other("record path has no parent directory"))?;
    let file_name = target.file_name().and_then(|n| n.to_str()).unwrap_or("record");
    let prefix = format!(".{file_name}.");
    let mut tmp = tempfile::Builder::new().prefix(&prefix).suffix(".tmp").rand_bytes(8).tempfile_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    before_persist(tmp.path())?;
    match replace {
        Replace::Yes => tmp.persist(target).map_err(|e| e.error)?,
        Replace::No => tmp.persist_noclobber(target).map_err(|e| e.error)?,
    };
    sync_dir(dir);
    Ok(())
}

/// Best-effort fsync of a directory so a rename survives a crash.
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

fn ignore_not_found(e: io::Error) -> io::Result<()> {
    if e.kind() == io::ErrorKind::NotFound { Ok(()) } else { Err(e) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_dir_names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn interrupted_write_leaves_the_previous_record_intact() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("job.json");
        write_atomic(&target, b"{\"v\":1}\n", Replace::Yes, &mut |_| Ok(())).unwrap();

        // Fail after the new content is fully written and synced to the temp file,
        // just before the rename (e.g. the process is interrupted at that point).
        let mut saw_temp = None;
        let err = write_atomic(&target, b"{\"v\":2}\n", Replace::Yes, &mut |tmp| {
            assert_eq!(fs::read(tmp).unwrap(), b"{\"v\":2}\n");
            saw_temp = Some(tmp.to_path_buf());
            Err(io::Error::other("simulated interruption"))
        })
        .unwrap_err();
        assert_eq!(err.to_string(), "simulated interruption");

        assert_eq!(fs::read(&target).unwrap(), b"{\"v\":1}\n");
        let tmp = saw_temp.unwrap();
        assert!(tmp.file_name().unwrap().to_str().unwrap().starts_with(".job.json."));
        assert!(!tmp.exists(), "temp file is cleaned up on failure");
        assert_eq!(read_dir_names(dir.path()), vec!["job.json".to_string()]);
    }

    #[test]
    fn no_clobber_write_refuses_existing_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("job.json");
        write_atomic(&target, b"first", Replace::No, &mut |_| Ok(())).unwrap();
        let err = write_atomic(&target, b"second", Replace::No, &mut |_| Ok(())).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert_eq!(read_dir_names(dir.path()), vec!["job.json".to_string()]);
    }

    #[cfg(unix)]
    #[test]
    fn written_records_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("job.json");
        write_atomic(&target, b"x", Replace::Yes, &mut |_| Ok(())).unwrap();
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
