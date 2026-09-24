//! The versioned job record (see docs/jobs.md) and its status transitions.
//!
//! Lifecycle fields are private: the only way to change a job's status is through
//! the transition methods below, which implement exactly the arrows documented in docs/jobs.md:
//!
//! ```text
//! (create: submitting) --2xx with operation id--> running
//! submitting --definite rejection--> failed
//! submitting --ambiguous--> submission_unknown        (also: stale submitting, see SUBMIT_GRACE)
//! running --poll done+ok--> succeeded   running --poll done+error--> failed
//! running --poll "not found" after submitted_at + retention--> expired
//! succeeded: outputs[i].download_state pending -> downloaded | failed | expired
//! ```
//!
//! There is deliberately no method that turns a `running` or `succeeded` job into
//! `failed` because of something local (Ctrl-C, wait timeout, poll network error,
//! download failure). A `downloaded` output stays `downloaded` when a later attempt
//! fails (only its `last_error` changes), so a job never loses track of a saved file.
//! Every transition into a terminal status (`succeeded`, `failed`, `expired`,
//! `submission_unknown`) sets `completed_at`.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::JobId;
use crate::artifacts::RecordedFile;
use crate::catalog::{InputCounts, ModelSpec, OptionKind, ResolvedOptions};
use crate::domain::{
    Artifact, CostEstimate, DownloadState, JobStatus, Operation, ProviderId, Usage, Warning,
};
use crate::error::{ErrorCode, IrisError};
use crate::output::ErrorBody;
use crate::output::results::{JobOutputView, JobView};
use crate::providers::{RemoteStatus, SubmittedOperation};
use crate::redact;

/// Version of the persisted record format written by this binary. Readers accept
/// `schema_version <= JOB_RECORD_VERSION` and reject newer records (`state_invalid`).
pub const JOB_RECORD_VERSION: u32 = 1;

/// Grace period added to the submit budget (see
/// [`paid_submit_budget`](super::paid_submit_budget)) before a record still in
/// `submitting` is considered abandoned (the process died in the uncertainty
/// window) and is reported as `submission_unknown`.
pub const SUBMIT_GRACE: Duration = Duration::from_secs(60);

/// A nested object of the record (a persisted error body, usage, or cost
/// estimate): the value as this version understands it, plus the object exactly
/// as it was read from disk.
///
/// Reading is tolerant (unknown fields are ignored, unknown error codes read as
/// `internal_error`), but a rewrite writes the object back as it was read, so
/// fields and error codes added by a newer Iris survive every rewrite of a record
/// this version did not otherwise change. A value set by this version is written
/// as this version serializes it. `Deref` gives the understood value.
#[derive(Clone)]
pub struct Preserved<T> {
    value: T,
    /// The object as read from disk; `None` for a value set by this version.
    raw: Option<Value>,
}

impl<T> Preserved<T> {
    /// A value set by this version.
    pub fn new(value: T) -> Preserved<T> {
        Preserved { value, raw: None }
    }

    /// The object as read from disk, if this value was read (not set by this version).
    pub fn raw(&self) -> Option<&Value> {
        self.raw.as_ref()
    }
}

impl<T> std::ops::Deref for Preserved<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: fmt::Debug> fmt::Debug for Preserved<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

impl<T: Serialize> Serialize for Preserved<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.raw {
            Some(raw) => raw.serialize(serializer),
            None => self.value.serialize(serializer),
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Preserved<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        let value = T::deserialize(&raw).map_err(serde::de::Error::custom)?;
        Ok(Preserved { value, raw: Some(raw) })
    }
}

impl Preserved<ErrorBody> {
    /// The error as the public view shows it. A code this version does not know
    /// (written by a newer Iris) reads as `internal_error`, and the code as written
    /// is kept in `details.recorded_code`. The category always follows the code, so
    /// code and category agree with the published table.
    pub fn view(&self) -> ErrorBody {
        let mut body = self.value.clone();
        body.category = body.code.category();
        let written = self.raw.as_ref().and_then(|r| r.get("code")).and_then(Value::as_str);
        if let Some(code) = written.filter(|c| *c != body.code.as_str()) {
            body.details.get_or_insert_with(Map::new).insert("recorded_code".to_string(), json!(code));
        }
        body
    }
}

/// What Iris remembers about the prompt: always a SHA-256 and a character count;
/// the text itself only when `jobs.store_prompts = true`.
///
/// `Debug` never shows the text (only its length), so records can be logged.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptRecord {
    /// Lowercase hex SHA-256 of the UTF-8 prompt.
    pub sha256: String,
    /// Length in Unicode scalar values.
    pub chars: u64,
    /// The prompt text, or `None` unless prompt storage is enabled.
    pub text: Option<String>,
    /// Fields written by newer versions; preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl fmt::Debug for PromptRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptRecord")
            .field("sha256", &self.sha256)
            .field("chars", &self.chars)
            .field("text", &TextLen(self.text.as_deref()))
            .field("extra", &KeysOnly(&self.extra))
            .finish()
    }
}

impl PromptRecord {
    /// Build the record for `prompt`. `store_text` is config `jobs.store_prompts`.
    pub fn new(prompt: &str, store_text: bool) -> PromptRecord {
        PromptRecord {
            sha256: sha256_hex(prompt.as_bytes()),
            chars: prompt.chars().count() as u64,
            text: store_text.then(|| prompt.to_string()),
            extra: Map::new(),
        }
    }
}

/// Where the outputs of the job should be saved, as decided at submission time.
/// Used by `jobs wait`/`jobs download` when no `-o`/`-d` is given then.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutputPlan {
    /// Absolute output directory, if one was resolved at submission.
    pub dir: Option<PathBuf>,
    /// Absolute exact output path (`-o`), if one was given.
    pub path: Option<PathBuf>,
    /// Whether `--overwrite` was given at submission.
    pub overwrite: bool,
    /// Fields written by newer versions; preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl OutputPlan {
    /// Build a plan, making paths absolute. Paths must be valid UTF-8 (they are
    /// reported in JSON output), otherwise `invalid_argument`.
    pub fn new(dir: Option<&Path>, path: Option<&Path>, overwrite: bool) -> Result<OutputPlan, IrisError> {
        let abs = |p: &Path| -> Result<PathBuf, IrisError> {
            let abs = std::path::absolute(p)
                .map_err(|e| IrisError::io(format_args!("cannot resolve path {}", p.display()), &e))?;
            if abs.to_str().is_none() {
                return Err(IrisError::invalid(format!(
                    "output path {} is not valid UTF-8",
                    abs.to_string_lossy()
                )));
            }
            Ok(abs)
        };
        Ok(OutputPlan {
            dir: dir.map(abs).transpose()?,
            path: path.map(abs).transpose()?,
            overwrite,
            extra: Map::new(),
        })
    }
}

/// One remote output of a succeeded job and its local download state.
///
/// Obtained read-only through [`JobRecord::outputs`]; changed only through the
/// `mark_output_*` methods of [`JobRecord`]. `Debug` shows `remote_uri` redacted.
#[derive(Clone, Serialize, Deserialize)]
pub struct JobOutput {
    pub index: u32,
    /// Provider download URI. Stored as-is in the 0600 record; never shown in
    /// views and always redacted when printed.
    pub remote_uri: String,
    /// Media type reported by the provider, replaced by the sniffed type once downloaded.
    pub media_type: Option<String>,
    pub download_state: DownloadState,
    /// Absolute path of the downloaded file.
    pub local_path: Option<PathBuf>,
    pub bytes: Option<u64>,
    /// Lowercase hex SHA-256 of the downloaded file.
    pub sha256: Option<String>,
    pub downloaded_at: Option<Timestamp>,
    /// Last download error (scrubbed), if the last attempt failed or found the output expired.
    pub last_error: Option<Preserved<ErrorBody>>,
    /// Image width of the downloaded file, if applicable (Iris addition to the v1 schema).
    #[serde(default)]
    pub width: Option<u32>,
    /// Image height of the downloaded file, if applicable (Iris addition to the v1 schema).
    #[serde(default)]
    pub height: Option<u32>,
    /// Video duration of the downloaded file, if parseable (Iris addition to the v1 schema).
    #[serde(default)]
    pub duration_seconds: Option<f64>,
    /// Fields written by newer versions; preserved on rewrite.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl fmt::Debug for JobOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobOutput")
            .field("index", &self.index)
            .field("remote_uri", &redact::redact_url(&self.remote_uri))
            .field("media_type", &self.media_type)
            .field("download_state", &self.download_state)
            .field("local_path", &self.local_path)
            .field("bytes", &self.bytes)
            .field("sha256", &self.sha256)
            .field("downloaded_at", &self.downloaded_at)
            .field("last_error", &self.last_error)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("duration_seconds", &self.duration_seconds)
            .field("extra", &KeysOnly(&self.extra))
            .finish()
    }
}

impl JobOutput {
    fn pending(index: u32, remote_uri: String, media_type: Option<String>) -> JobOutput {
        JobOutput {
            index,
            remote_uri,
            media_type,
            download_state: DownloadState::Pending,
            local_path: None,
            bytes: None,
            sha256: None,
            downloaded_at: None,
            last_error: None,
            width: None,
            height: None,
            duration_seconds: None,
            extra: Map::new(),
        }
    }

    /// The recorded saved file of a `downloaded` output, as
    /// [`decide_download`](crate::artifacts::decide_download) needs it (`None` if
    /// the output is not downloaded or its record is incomplete).
    pub fn recorded_file(&self) -> Option<RecordedFile<'_>> {
        if self.download_state != DownloadState::Downloaded {
            return None;
        }
        Some(RecordedFile {
            path: self.local_path.as_deref()?,
            bytes: self.bytes?,
            sha256: self.sha256.as_deref()?,
            media_type: self.media_type.as_deref(),
        })
    }

    /// The saved artifact, if this output is downloaded.
    pub fn artifact(&self) -> Option<Artifact> {
        if self.download_state != DownloadState::Downloaded {
            return None;
        }
        let path = self.local_path.as_ref()?;
        Some(Artifact {
            index: self.index,
            path: path.to_string_lossy().into_owned(),
            media_type: self.media_type.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
            bytes: self.bytes?,
            sha256: self.sha256.clone()?,
            width: self.width,
            height: self.height,
            duration_seconds: self.duration_seconds,
        })
    }
}

/// Everything needed to create a record for a job about to be submitted.
/// `Debug` lists the `request` keys only (free-text options may hold prompt text).
#[derive(Clone)]
pub struct NewJob {
    pub provider: ProviderId,
    /// Model id sent to the provider.
    pub model: String,
    /// Must be a provider-native async operation (`video.generate`).
    pub operation: Operation,
    /// Non-secret request metadata; build it with [`request_metadata`].
    pub request: Map<String, Value>,
    pub prompt: PromptRecord,
    pub output_plan: OutputPlan,
    pub cost_estimate: Option<CostEstimate>,
}

impl fmt::Debug for NewJob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewJob")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("operation", &self.operation)
            .field("request", &KeysOnly(&self.request))
            .field("prompt", &self.prompt)
            .field("output_plan", &self.output_plan)
            .field("cost_estimate", &self.cost_estimate)
            .finish()
    }
}

/// What [`JobRecord::apply_poll`] changed.
#[derive(Debug, Clone, PartialEq)]
pub enum PollApplied {
    /// Still running; `last_checked_at` updated.
    Running { progress: Option<f32> },
    /// Now `succeeded`, outputs recorded as `pending` downloads. Carries the
    /// provider's warnings for the caller to report (they are not persisted).
    Succeeded { warnings: Vec<Warning> },
    /// Now `failed` with the provider's error recorded.
    Failed,
    /// Now `expired`: the provider no longer knows the operation.
    Expired,
    /// The record already had a terminal status (e.g. another process finished it
    /// first); nothing was changed.
    AlreadyTerminal,
}

/// Top-level field names of the v1 record (extension fields may not reuse them).
const RECORD_FIELDS: &[&str] = &[
    "schema_version",
    "job_id",
    "provider",
    "model",
    "operation",
    "status",
    "created_at",
    "updated_at",
    "submitted_at",
    "completed_at",
    "last_checked_at",
    "remote_operation_id",
    "provider_request_id",
    "remote_expires_at",
    "request",
    "prompt",
    "output_plan",
    "outputs",
    "error",
    "usage",
    "cost_estimate",
];

/// A persisted provider-native job (see docs/jobs.md; record schema v1).
///
/// Unknown fields (written by newer Iris versions with the same major record
/// version) are kept in `extra` and written back unchanged.
///
/// `Debug` is safe to log: remote URIs are redacted, prompt text is replaced by
/// its length, and `request`/`extra` are shown as key lists (free-text options
/// are stored as text when `jobs.store_prompts` is on).
#[derive(Clone, Serialize, Deserialize)]
pub struct JobRecord {
    schema_version: u32,
    job_id: JobId,
    provider: ProviderId,
    model: String,
    operation: Operation,
    status: JobStatus,
    created_at: Timestamp,
    updated_at: Timestamp,
    submitted_at: Option<Timestamp>,
    completed_at: Option<Timestamp>,
    last_checked_at: Option<Timestamp>,
    remote_operation_id: Option<String>,
    provider_request_id: Option<String>,
    remote_expires_at: Option<Timestamp>,
    request: Map<String, Value>,
    prompt: PromptRecord,
    output_plan: OutputPlan,
    outputs: Vec<JobOutput>,
    error: Option<Preserved<ErrorBody>>,
    usage: Option<Preserved<Usage>>,
    cost_estimate: Option<Preserved<CostEstimate>>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl fmt::Debug for JobRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobRecord")
            .field("schema_version", &self.schema_version)
            .field("job_id", &self.job_id)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("operation", &self.operation)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("submitted_at", &self.submitted_at)
            .field("completed_at", &self.completed_at)
            .field("last_checked_at", &self.last_checked_at)
            .field("remote_operation_id", &self.remote_operation_id)
            .field("provider_request_id", &self.provider_request_id)
            .field("remote_expires_at", &self.remote_expires_at)
            .field("request", &KeysOnly(&self.request))
            .field("prompt", &self.prompt)
            .field("output_plan", &self.output_plan)
            .field("outputs", &self.outputs)
            .field("error", &self.error)
            .field("usage", &self.usage)
            .field("cost_estimate", &self.cost_estimate)
            .field("extra", &KeysOnly(&self.extra))
            .finish()
    }
}

impl JobRecord {
    /// A new record in `submitting` with a freshly generated id. Write it with
    /// [`JobStore::create`](super::JobStore::create) *before* sending the paid request.
    pub fn new(new: NewJob, now: Timestamp) -> Result<JobRecord, IrisError> {
        Self::with_id(JobId::generate(), new, now)
    }

    /// Like [`JobRecord::new`] with a caller-chosen id.
    pub fn with_id(job_id: JobId, new: NewJob, now: Timestamp) -> Result<JobRecord, IrisError> {
        if !new.operation.is_async_job() {
            return Err(IrisError::internal(format!(
                "{} is synchronous and never creates a job record",
                new.operation
            )));
        }
        Ok(JobRecord {
            schema_version: JOB_RECORD_VERSION,
            job_id,
            provider: new.provider,
            model: new.model,
            operation: new.operation,
            status: JobStatus::Submitting,
            created_at: now,
            updated_at: now,
            submitted_at: None,
            completed_at: None,
            last_checked_at: None,
            remote_operation_id: None,
            provider_request_id: None,
            remote_expires_at: None,
            request: new.request,
            prompt: new.prompt,
            output_plan: new.output_plan,
            outputs: Vec::new(),
            error: None,
            usage: None,
            cost_estimate: new.cost_estimate.map(Preserved::new),
            extra: Map::new(),
        })
    }

    // ----- read access -------------------------------------------------------

    /// Record format version as read (always [`JOB_RECORD_VERSION`] when written).
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    pub fn job_id(&self) -> &JobId {
        &self.job_id
    }
    pub fn provider(&self) -> ProviderId {
        self.provider
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
    pub fn status(&self) -> JobStatus {
        self.status
    }
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }
    pub fn updated_at(&self) -> Timestamp {
        self.updated_at
    }
    pub fn submitted_at(&self) -> Option<Timestamp> {
        self.submitted_at
    }
    /// When Iris observed that the job reached a terminal status (`succeeded`,
    /// `failed`, `expired`, `submission_unknown`), not when the provider finished it;
    /// `None` while `submitting` or `running`.
    pub fn completed_at(&self) -> Option<Timestamp> {
        self.completed_at
    }
    pub fn last_checked_at(&self) -> Option<Timestamp> {
        self.last_checked_at
    }
    /// Provider operation id used for polling (e.g. `models/…/operations/…`).
    pub fn remote_operation_id(&self) -> Option<&str> {
        self.remote_operation_id.as_deref()
    }
    pub fn provider_request_id(&self) -> Option<&str> {
        self.provider_request_id.as_deref()
    }
    /// Earliest time the provider may stop serving the outputs: submission time plus
    /// the provider's documented retention (the provider may keep them longer).
    pub fn remote_expires_at(&self) -> Option<Timestamp> {
        self.remote_expires_at
    }
    /// Non-secret request metadata (resolved options and input counts).
    pub fn request(&self) -> &Map<String, Value> {
        &self.request
    }
    pub fn prompt(&self) -> &PromptRecord {
        &self.prompt
    }
    pub fn output_plan(&self) -> &OutputPlan {
        &self.output_plan
    }
    pub fn outputs(&self) -> &[JobOutput] {
        &self.outputs
    }
    /// The job's error as this version reads it (an unknown code reads as
    /// `internal_error`); [`JobRecord::error_view`] is what the public view shows.
    pub fn error(&self) -> Option<&ErrorBody> {
        self.error.as_deref()
    }
    /// The job's error as the public view shows it (see [`Preserved::view`]).
    pub fn error_view(&self) -> Option<ErrorBody> {
        self.error.as_ref().map(Preserved::view)
    }
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_deref()
    }
    pub fn cost_estimate(&self) -> Option<&CostEstimate> {
        self.cost_estimate.as_deref()
    }
    /// Top-level fields this version does not understand (preserved on rewrite).
    pub fn extra(&self) -> &Map<String, Value> {
        &self.extra
    }
    /// Set an extension field (kept alongside the v1 fields and preserved by every
    /// rewrite). Names of v1 fields are refused (`internal_error`): an extension
    /// must never shadow `status` or any other lifecycle field.
    pub fn set_extra(&mut self, key: &str, value: Value) -> Result<(), IrisError> {
        if RECORD_FIELDS.contains(&key) {
            return Err(IrisError::internal(format!(
                "'{key}' is a job record field, not an extension field"
            )));
        }
        self.extra.insert(key.to_string(), value);
        Ok(())
    }

    /// `submitting` or `running`: the remote side may still be working, so deleting
    /// the record would make the job unrecoverable.
    pub fn is_active(&self) -> bool {
        matches!(self.status, JobStatus::Submitting | JobStatus::Running)
    }

    /// True once `now` has reached `remote_expires_at` (the provider may have
    /// deleted the outputs). Unknown retention is never "expired".
    pub fn remote_expired(&self, now: Timestamp) -> bool {
        self.remote_expires_at.is_some_and(|t| now >= t)
    }

    // ----- submission --------------------------------------------------------

    /// `submitting --2xx with operation id--> running`.
    ///
    /// Also accepted from `submission_unknown` while no operation id is recorded:
    /// that status may have been assigned by the stale-`submitting` rule while this
    /// process was still waiting for the answer, and a definite operation id
    /// resolves the uncertainty (nothing is resubmitted).
    pub fn mark_submitted(&mut self, op: &SubmittedOperation, now: Timestamp) -> Result<(), IrisError> {
        self.require_awaiting_submission("record the submission")?;
        if op.remote_id.trim().is_empty() {
            return Err(IrisError::internal("provider returned an empty operation id"));
        }
        self.status = JobStatus::Running;
        self.remote_operation_id = Some(op.remote_id.clone());
        self.provider_request_id = op.provider_request_id.clone();
        self.submitted_at = Some(now);
        // Set if the stale rule had already declared the submission unknown.
        self.completed_at = None;
        self.error = None;
        self.updated_at = now;
        Ok(())
    }

    /// `submitting --definite rejection (HTTP error response, or connection failed
    /// before send)--> failed`. `error` is the mapped provider/transport error.
    pub fn mark_rejected(&mut self, error: &IrisError, now: Timestamp) -> Result<(), IrisError> {
        self.require_awaiting_submission("record the rejection")?;
        self.status = JobStatus::Failed;
        self.error = Some(self.error_body(error));
        self.completed_at = Some(now);
        self.updated_at = now;
        Ok(())
    }

    /// `submitting --ambiguous (timeout/reset after send, unparseable response)-->
    /// submission_unknown`. Terminal: Iris never resubmits automatically.
    pub fn mark_submission_unknown(&mut self, error: &IrisError, now: Timestamp) -> Result<(), IrisError> {
        self.require_awaiting_submission("record the uncertain submission")?;
        self.status = JobStatus::SubmissionUnknown;
        self.error = Some(self.error_body(error));
        self.completed_at = Some(now);
        self.updated_at = now;
        Ok(())
    }

    /// True if this record is still `submitting` although `created_at` is older than
    /// `submit_budget + SUBMIT_GRACE`: the submitting process must have died in the
    /// uncertainty window. `submit_budget` is the worst-case duration of the submit
    /// call ([`paid_submit_budget`](super::paid_submit_budget)), not the bare
    /// per-request timeout.
    pub fn is_stale_submitting(&self, now: Timestamp, submit_budget: Duration) -> bool {
        if self.status != JobStatus::Submitting {
            return false;
        }
        let limit = submit_budget.saturating_add(SUBMIT_GRACE).as_secs().min(i64::MAX as u64) as i64;
        now.as_second().saturating_sub(self.created_at.as_second()) > limit
    }

    /// Apply the stale-`submitting` rule: rewrite such a record as
    /// `submission_unknown` with a `submission_uncertain` error. Returns whether the
    /// record changed. [`JobStore`](super::JobStore) applies this on every load,
    /// list, and locked update.
    ///
    /// `completed_at` is set to the moment the record became stale
    /// (`created_at + submit_budget + SUBMIT_GRACE`), not to `now`, so repeated
    /// in-memory reports of the same unpersisted record agree.
    pub fn resolve_stale_submitting(&mut self, now: Timestamp, submit_budget: Duration) -> bool {
        if !self.is_stale_submitting(now, submit_budget) {
            return false;
        }
        let deadline = submit_budget.saturating_add(SUBMIT_GRACE);
        let became_stale = self.created_at.checked_add(deadline).ok().filter(|t| *t <= now).unwrap_or(now);
        let error = IrisError::new(
            ErrorCode::SubmissionUncertain,
            "Iris stopped while submitting this job, before the provider's operation id was recorded; \
             the provider may or may not have accepted (and billed) the request",
        )
        .with_hint(
            "check the provider console for the request before resubmitting; Iris never resubmits automatically",
        );
        self.status = JobStatus::SubmissionUnknown;
        self.error = Some(self.error_body(&error));
        self.completed_at = Some(became_stale);
        self.updated_at = now;
        true
    }

    // ----- polling -----------------------------------------------------------

    /// Apply one poll result to a `running` job.
    ///
    /// * `Running` → stays `running`, `last_checked_at = now`.
    /// * `Succeeded` → `succeeded`, outputs recorded as pending downloads, usage
    ///   recorded, `remote_expires_at = submitted_at + retention` when the provider
    ///   documents a retention period (the earliest time the provider may delete
    ///   the outputs; the observed completion time if `submitted_at` is missing).
    /// * `Failed` → `failed` with the provider's error.
    /// * `Gone` → `expired`, but only once `now` has reached `submitted_at +
    ///   retention` (or the retention is unknown). Earlier, the provider's "not
    ///   found" cannot mean the job is gone: nothing changes and the provider's
    ///   error is returned (not retryable as is), carrying this job's identifiers.
    ///
    /// A record that is already terminal (another process got there first) is left
    /// unchanged ([`PollApplied::AlreadyTerminal`]). A record without an operation
    /// id cannot have been polled: `internal_error`.
    pub fn apply_poll(
        &mut self,
        status: RemoteStatus,
        retention: Option<Duration>,
        now: Timestamp,
    ) -> Result<PollApplied, IrisError> {
        match self.status {
            JobStatus::Running => {}
            JobStatus::Succeeded | JobStatus::Failed | JobStatus::Expired => {
                return Ok(PollApplied::AlreadyTerminal);
            }
            JobStatus::Submitting | JobStatus::SubmissionUnknown => {
                return Err(IrisError::internal(format!(
                    "job {} has no provider operation id (status {}); it cannot be polled",
                    self.job_id, self.status
                )));
            }
        }
        let retained_until =
            retention.and_then(|r| self.submitted_at.unwrap_or(self.created_at).checked_add(r).ok());
        if let RemoteStatus::Gone { error } = &status
            && let Some(until) = retained_until.filter(|until| now < *until)
        {
            let mut e = error.clone().with_job(self.job_id.to_string(), Some(self.status));
            e.message = format!(
                "{}; the provider keeps this job at least until about {until}, so Iris does not treat it as \
                 expired and leaves it {}",
                e.message, self.status
            );
            if e.remote_operation_id.is_none()
                && let Some(remote) = &self.remote_operation_id
            {
                e = e.with_remote_operation(remote.clone());
            }
            return Err(e.with_retryable(Some(false)));
        }
        self.last_checked_at = Some(now);
        self.updated_at = now;
        Ok(match status {
            RemoteStatus::Running { progress } => PollApplied::Running { progress },
            RemoteStatus::Succeeded { outputs, usage, warnings } => {
                self.status = JobStatus::Succeeded;
                self.completed_at = Some(now);
                self.outputs = outputs
                    .into_iter()
                    .enumerate()
                    .map(|(i, o)| JobOutput::pending(i as u32, o.uri, o.media_type))
                    .collect();
                self.usage = usage.map(Preserved::new);
                self.remote_expires_at =
                    retention.and_then(|r| self.submitted_at.unwrap_or(now).checked_add(r).ok());
                self.error = None;
                PollApplied::Succeeded { warnings }
            }
            RemoteStatus::Failed { error } => {
                self.status = JobStatus::Failed;
                self.completed_at = Some(now);
                self.error = Some(self.error_body(&error));
                PollApplied::Failed
            }
            RemoteStatus::Gone { error: evidence } => {
                self.status = JobStatus::Expired;
                self.completed_at = Some(now);
                let message = match retained_until {
                    Some(until) => format!(
                        "the provider no longer has this operation, and its retention period (until about \
                         {until}) has passed; the outputs cannot be retrieved"
                    ),
                    None => "the provider no longer has this operation; the outputs cannot be retrieved"
                        .to_string(),
                };
                let mut error =
                    IrisError::new(ErrorCode::ArtifactExpired, message).with_retryable(Some(false));
                // Keep the provider's evidence (status, code, request id, message).
                error.provider = evidence.provider;
                error.provider_status = evidence.provider_status;
                error.provider_code = evidence.provider_code.clone();
                error.provider_request_id = evidence.provider_request_id.clone();
                if let Some(message) = evidence.details.get("provider_message") {
                    error = error.with_detail("provider_message", message.clone());
                }
                self.error = Some(self.error_body(&error));
                PollApplied::Expired
            }
        })
    }

    // ----- downloads ---------------------------------------------------------

    /// Record a successful download (or local copy) of output `index`.
    /// Only valid on a `succeeded` job; the job status does not change.
    pub fn mark_output_downloaded(
        &mut self,
        index: u32,
        artifact: &Artifact,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        let out = self.output_mut(index)?;
        out.download_state = DownloadState::Downloaded;
        out.local_path = Some(PathBuf::from(&artifact.path));
        out.media_type = Some(artifact.media_type.clone());
        out.bytes = Some(artifact.bytes);
        out.sha256 = Some(artifact.sha256.clone());
        out.width = artifact.width;
        out.height = artifact.height;
        out.duration_seconds = artifact.duration_seconds;
        out.downloaded_at = Some(now);
        out.last_error = None;
        self.updated_at = now;
        Ok(())
    }

    /// Record a failed (retryable) fetch of output `index`: `pending`/`failed`/
    /// `expired` → `failed` with `last_error`. The job status is unchanged: a
    /// download failure never turns a succeeded job into a failed one.
    ///
    /// An output that is already `downloaded` stays `downloaded` (only `last_error`
    /// is set): its recorded file (path, size, SHA-256) remains the job's artifact.
    /// Whether that file is still intact is decided by
    /// [`decide_download`](crate::artifacts::decide_download) when it matters, never
    /// by a later failed attempt. Record only failures of a remote fetch here; purely
    /// local failures (`output_exists`, `invalid_argument`, a failed local copy) are
    /// returned to the user without touching the record.
    pub fn mark_output_failed(
        &mut self,
        index: u32,
        error: &IrisError,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        self.record_output_problem(index, error, DownloadState::Failed, now)
    }

    /// Record that output `index` is gone at the provider: the file host answered
    /// 410, or 403/404 once `remote_expires_at` has passed (see `refused_or_gone` in
    /// `app::jobs`): `pending`/`failed` → `expired` with `last_error`. The retention
    /// estimate alone never expires an output. The job status is unchanged. As with
    /// [`mark_output_failed`](Self::mark_output_failed), a `downloaded` output stays
    /// `downloaded` and only gets `last_error`.
    pub fn mark_output_expired(
        &mut self,
        index: u32,
        error: &IrisError,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        self.record_output_problem(index, error, DownloadState::Expired, now)
    }

    // ----- views -------------------------------------------------------------

    /// The public view (docs/json-contract.md `Job`). Remote download URIs are never included;
    /// `artifacts` are built from downloaded outputs only.
    pub fn to_view(&self) -> JobView {
        let outputs: Vec<JobOutputView> = self
            .outputs
            .iter()
            .map(|o| JobOutputView {
                index: o.index,
                media_type: o.media_type.clone(),
                download_state: o.download_state,
                artifact: o.artifact(),
                last_error: o.last_error.as_ref().map(Preserved::view),
            })
            .collect();
        let artifacts = outputs.iter().filter_map(|o| o.artifact.clone()).collect();
        JobView {
            job_id: self.job_id.to_string(),
            remote_operation_id: self.remote_operation_id.clone(),
            provider: self.provider,
            model: self.model.clone(),
            operation: self.operation,
            status: self.status,
            created_at: self.created_at.to_string(),
            submitted_at: self.submitted_at.map(|t| t.to_string()),
            updated_at: self.updated_at.to_string(),
            completed_at: self.completed_at.map(|t| t.to_string()),
            last_checked_at: self.last_checked_at.map(|t| t.to_string()),
            remote_expires_at: self.remote_expires_at.map(|t| t.to_string()),
            outputs,
            artifacts,
            error: self.error_view(),
            usage: self.usage().cloned(),
            cost_estimate: self.cost_estimate().cloned(),
            request: self.request.clone(),
        }
    }

    // ----- internals ---------------------------------------------------------

    fn require_awaiting_submission(&self, action: &str) -> Result<(), IrisError> {
        let awaiting = match self.status {
            JobStatus::Submitting => true,
            JobStatus::SubmissionUnknown => self.remote_operation_id.is_none(),
            _ => false,
        };
        if awaiting {
            Ok(())
        } else {
            Err(IrisError::internal(format!(
                "cannot {action} for job {}: it is already {}",
                self.job_id, self.status
            )))
        }
    }

    fn record_output_problem(
        &mut self,
        index: u32,
        error: &IrisError,
        state: DownloadState,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        let body = self.error_body(error);
        let out = self.output_mut(index)?;
        if out.download_state != DownloadState::Downloaded {
            out.download_state = state;
        }
        out.last_error = Some(body);
        self.updated_at = now;
        Ok(())
    }

    fn output_mut(&mut self, index: u32) -> Result<&mut JobOutput, IrisError> {
        if self.status != JobStatus::Succeeded {
            return Err(IrisError::internal(format!(
                "job {} is {}; only succeeded jobs have downloadable outputs",
                self.job_id, self.status
            )));
        }
        let job_id = self.job_id.clone();
        self.outputs
            .iter_mut()
            .find(|o| o.index == index)
            .ok_or_else(|| IrisError::internal(format!("job {job_id} has no output with index {index}")))
    }

    /// Scrubbed error body carrying this job's identifiers.
    fn error_body(&self, error: &IrisError) -> Preserved<ErrorBody> {
        let mut e = error.clone().with_job(self.job_id.to_string(), None);
        if e.remote_operation_id.is_none()
            && let Some(remote) = &self.remote_operation_id
        {
            e = e.with_remote_operation(remote.clone());
        }
        Preserved::new(ErrorBody::from(&e))
    }
}

/// Build the non-secret `request` metadata of a job record: the explicitly set,
/// validated options plus `input_counts` (`first_frame`, `last_frame`, `reference`).
///
/// Free-text options (catalog kind `Text`, e.g. a negative prompt) are prompt-like:
/// unless `store_prompts` is true they are recorded as `{"sha256", "chars"}` instead
/// of their text. Input image paths and contents are never recorded.
pub fn request_metadata(
    spec: &ModelSpec,
    options: &ResolvedOptions,
    inputs: &InputCounts,
    store_prompts: bool,
) -> Map<String, Value> {
    let mut map = Map::new();
    for (name, value) in options.iter() {
        let free_text = spec.option(name).is_none_or(|o| matches!(o.kind, OptionKind::Text { .. }));
        let stored = if free_text && !store_prompts {
            let text = value.to_string();
            json!({ "sha256": sha256_hex(text.as_bytes()), "chars": text.chars().count() })
        } else {
            serde_json::to_value(value).unwrap_or(Value::Null)
        };
        map.insert(name.clone(), stored);
    }
    map.insert(
        "input_counts".to_string(),
        json!({
            "first_frame": u32::from(inputs.first_frame),
            "last_frame": u32::from(inputs.last_frame),
            "reference": inputs.references,
        }),
    );
    map
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// `Debug` of a JSON object that lists its keys but never its values.
struct KeysOnly<'a>(&'a Map<String, Value>);

impl fmt::Debug for KeysOnly<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// `Debug` of optional text that shows only its length: `Some(<N chars>)`.
struct TextLen<'a>(Option<&'a str>);

impl fmt::Debug for TextLen<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Some(text) => write!(f, "Some(<{} chars>)", text.chars().count()),
            None => f.write_str("None"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_fields_list_matches_serialized_fields() {
        let new = NewJob {
            provider: ProviderId::Gemini,
            model: "m".into(),
            operation: Operation::VideoGenerate,
            request: Map::new(),
            prompt: PromptRecord::new("p", false),
            output_plan: OutputPlan::default(),
            cost_estimate: None,
        };
        let rec = JobRecord::new(new, super::super::now()).unwrap();
        let value = serde_json::to_value(&rec).unwrap();
        let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        let mut fields = RECORD_FIELDS.to_vec();
        keys.sort_unstable();
        fields.sort_unstable();
        assert_eq!(keys, fields);
    }
}
