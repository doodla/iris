//! The versioned job record (C-04 "Record schema (v1)") and its status transitions.
//!
//! Lifecycle fields are private: the only way to change a job's status is through
//! the transition methods below, which implement exactly the arrows of C-04:
//!
//! ```text
//! (create: submitting) --2xx with operation id--> running
//! submitting --definite rejection--> failed
//! submitting --ambiguous--> submission_unknown        (also: stale submitting, see SUBMIT_GRACE)
//! running --poll done+ok--> succeeded   running --poll done+error--> failed
//! running --poll operation gone--> expired
//! succeeded: outputs[i].download_state pending -> downloaded | failed | expired
//! ```
//!
//! There is deliberately no method that turns a `running` or `succeeded` job into
//! `failed` because of something local (Ctrl-C, wait timeout, poll network error,
//! download failure).

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::JobId;
use crate::catalog::{InputCounts, ModelSpec, OptionKind, ResolvedOptions};
use crate::domain::{
    Artifact, CostEstimate, DownloadState, JobStatus, Operation, ProviderId, Usage, Warning,
};
use crate::error::{ErrorCode, IrisError};
use crate::output::ErrorBody;
use crate::output::results::{JobOutputView, JobView};
use crate::providers::{RemoteStatus, SubmittedOperation};

/// Version of the persisted record format written by this binary. Readers accept
/// `schema_version <= JOB_RECORD_VERSION` and reject newer records (`state_invalid`).
pub const JOB_RECORD_VERSION: u32 = 1;

/// Grace period added to the submit timeout before a record still in `submitting`
/// is considered abandoned (the process died in the uncertainty window) and is
/// reported as `submission_unknown`.
pub const SUBMIT_GRACE: Duration = Duration::from_secs(60);

/// What Iris remembers about the prompt: always a SHA-256 and a character count;
/// the text itself only when `jobs.store_prompts = true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
/// `mark_output_*` methods of [`JobRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub last_error: Option<ErrorBody>,
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
#[derive(Debug, Clone)]
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

/// A persisted provider-native job (C-04 record schema v1).
///
/// Unknown fields (written by newer Iris versions with the same major record
/// version) are kept in `extra` and written back unchanged.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    error: Option<ErrorBody>,
    usage: Option<Usage>,
    cost_estimate: Option<CostEstimate>,
    #[serde(flatten)]
    extra: Map<String, Value>,
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
            cost_estimate: new.cost_estimate,
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
    /// Estimated time after which the provider no longer serves the outputs.
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
    pub fn error(&self) -> Option<&ErrorBody> {
        self.error.as_ref()
    }
    pub fn usage(&self) -> Option<&Usage> {
        self.usage.as_ref()
    }
    pub fn cost_estimate(&self) -> Option<&CostEstimate> {
        self.cost_estimate.as_ref()
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

    /// True if the provider's retention window (`remote_expires_at`) has passed.
    pub fn remote_expired(&self, now: Timestamp) -> bool {
        self.remote_expires_at.is_some_and(|t| now > t)
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
        self.updated_at = now;
        Ok(())
    }

    /// True if this record is still `submitting` although `created_at` is older than
    /// `submit_timeout + SUBMIT_GRACE`: the submitting process must have died in the
    /// uncertainty window.
    pub fn is_stale_submitting(&self, now: Timestamp, submit_timeout: Duration) -> bool {
        if self.status != JobStatus::Submitting {
            return false;
        }
        let limit = submit_timeout.saturating_add(SUBMIT_GRACE).as_secs().min(i64::MAX as u64) as i64;
        now.as_second().saturating_sub(self.created_at.as_second()) > limit
    }

    /// Apply the stale-`submitting` rule: rewrite such a record as
    /// `submission_unknown` with a `submission_uncertain` error. Returns whether the
    /// record changed. [`JobStore`](super::JobStore) applies this on every load,
    /// list, and locked update.
    pub fn resolve_stale_submitting(&mut self, now: Timestamp, submit_timeout: Duration) -> bool {
        if !self.is_stale_submitting(now, submit_timeout) {
            return false;
        }
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
        self.updated_at = now;
        true
    }

    // ----- polling -----------------------------------------------------------

    /// Apply one poll result to a `running` job.
    ///
    /// * `Running` → stays `running`, `last_checked_at = now`.
    /// * `Succeeded` → `succeeded`, outputs recorded as pending downloads, usage
    ///   recorded, `remote_expires_at = now + retention` when the provider documents
    ///   a retention period.
    /// * `Failed` → `failed` with the provider's error.
    /// * `Gone` → `expired`.
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
                self.usage = usage;
                self.remote_expires_at = retention.and_then(|r| now.checked_add(r).ok());
                self.error = None;
                PollApplied::Succeeded { warnings }
            }
            RemoteStatus::Failed { error } => {
                self.status = JobStatus::Failed;
                self.completed_at = Some(now);
                self.error = Some(self.error_body(&error));
                PollApplied::Failed
            }
            RemoteStatus::Gone => {
                self.status = JobStatus::Expired;
                let error = IrisError::new(
                    ErrorCode::ArtifactExpired,
                    "the provider no longer has this operation (its retention period has passed); \
                     the outputs cannot be retrieved",
                );
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

    /// Record a failed (retryable) download of output `index`. The job status is
    /// unchanged: a download failure never turns a succeeded job into a failed one.
    pub fn mark_output_failed(
        &mut self,
        index: u32,
        error: &IrisError,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        let body = self.error_body(error);
        let out = self.output_mut(index)?;
        out.download_state = DownloadState::Failed;
        out.last_error = Some(body);
        self.updated_at = now;
        Ok(())
    }

    /// Record that output `index` is no longer available remotely (retention passed,
    /// or the file host answered 403/404/410). The job status is unchanged.
    pub fn mark_output_expired(
        &mut self,
        index: u32,
        error: &IrisError,
        now: Timestamp,
    ) -> Result<(), IrisError> {
        let body = self.error_body(error);
        let out = self.output_mut(index)?;
        out.download_state = DownloadState::Expired;
        out.last_error = Some(body);
        self.updated_at = now;
        Ok(())
    }

    // ----- views -------------------------------------------------------------

    /// The public view (C-03 `Job`). Remote download URIs are never included;
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
                last_error: o.last_error.clone(),
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
            error: self.error.clone(),
            usage: self.usage.clone(),
            cost_estimate: self.cost_estimate.clone(),
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
    fn error_body(&self, error: &IrisError) -> ErrorBody {
        let mut e = error.clone().with_job(self.job_id.to_string(), None);
        if e.remote_operation_id.is_none()
            && let Some(remote) = &self.remote_operation_id
        {
            e = e.with_remote_operation(remote.clone());
        }
        ErrorBody::from(&e)
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
