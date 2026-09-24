//! The versioned JSON envelope (C-03) and the error object.

use schemars::JsonSchema;
use serde::Serialize;

use crate::domain::{JobStatus, ProviderId, Warning};
use crate::error::{ErrorCategory, ErrorCode, IrisError};
use crate::redact;

use super::results::*;

/// Major version of the JSON output contract.
pub const SCHEMA_VERSION: u32 = 1;

/// Command identifiers used in the envelope's `command` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub enum CommandName {
    #[serde(rename = "image.generate")]
    ImageGenerate,
    #[serde(rename = "image.edit")]
    ImageEdit,
    #[serde(rename = "video.generate")]
    VideoGenerate,
    #[serde(rename = "jobs.list")]
    JobsList,
    #[serde(rename = "jobs.status")]
    JobsStatus,
    #[serde(rename = "jobs.wait")]
    JobsWait,
    #[serde(rename = "jobs.download")]
    JobsDownload,
    #[serde(rename = "jobs.delete")]
    JobsDelete,
    #[serde(rename = "models.list")]
    ModelsList,
    #[serde(rename = "models.show")]
    ModelsShow,
    #[serde(rename = "providers.list")]
    ProvidersList,
    #[serde(rename = "config.show")]
    ConfigShow,
    #[serde(rename = "config.path")]
    ConfigPath,
    #[serde(rename = "doctor")]
    Doctor,
    #[serde(rename = "schema")]
    Schema,
    #[serde(rename = "completions")]
    Completions,
    #[serde(rename = "version")]
    Version,
}

/// Every possible `result` payload. Serialized untagged; the envelope's `command`
/// tells which variant applies (see the `$defs` in the published schema).
// Built once per process for printing; variant size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ResultPayload {
    Image(ImageResult),
    Job(JobResult),
    JobList(JobListResult),
    JobDelete(JobDeleteResult),
    ModelList(ModelListResult),
    ModelShow(ModelShowResult),
    ProviderList(ProviderListResult),
    ConfigShow(ConfigShowResult),
    ConfigPath(ConfigPathResult),
    Doctor(DoctorResult),
    Schema(SchemaResult),
    Completions(CompletionsResult),
    Version(VersionResult),
    Help(HelpResult),
    Plan(PlanResult),
}

/// The error object of a failed command.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub category: ErrorCategory,
    pub message: String,
    /// Whether running the same command again may succeed; null if unknown.
    pub retryable: Option<bool>,
    pub retry_after_seconds: Option<u64>,
    pub hint: Option<String>,
    pub provider: Option<ProviderId>,
    pub provider_status: Option<u16>,
    /// Provider's own code (informational, unstable).
    pub provider_code: Option<String>,
    pub provider_request_id: Option<String>,
    pub job_id: Option<String>,
    pub remote_operation_id: Option<String>,
    pub job_status: Option<JobStatus>,
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
}

impl From<&IrisError> for ErrorBody {
    fn from(e: &IrisError) -> Self {
        let s = |t: &str| redact::scrub(t).into_owned();
        let details = if e.details.is_empty() {
            None
        } else {
            let mut v = serde_json::Value::Object(e.details.clone());
            redact::scrub_json(&mut v);
            match v {
                serde_json::Value::Object(m) => Some(m),
                _ => None,
            }
        };
        ErrorBody {
            code: e.code,
            category: e.code.category(),
            message: s(&e.message),
            retryable: e.retryable,
            retry_after_seconds: e.retry_after.map(|d| d.as_secs().max(1)),
            hint: e.hint.as_deref().map(s),
            provider: e.provider,
            provider_status: e.provider_status,
            provider_code: e.provider_code.as_deref().map(s),
            provider_request_id: e.provider_request_id.as_deref().map(s),
            job_id: e.job_id.clone(),
            remote_operation_id: e.remote_operation_id.as_deref().map(s),
            job_status: e.job_status,
            details,
        }
    }
}

/// The single JSON document printed on stdout in `--json` mode.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Envelope {
    pub schema_version: u32,
    pub ok: bool,
    pub command: Option<CommandName>,
    pub result: Option<ResultPayload>,
    pub error: Option<ErrorBody>,
    pub warnings: Vec<Warning>,
}

impl Envelope {
    pub fn success(command: CommandName, result: ResultPayload, warnings: Vec<Warning>) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            ok: true,
            command: Some(command),
            result: Some(result),
            error: None,
            warnings,
        }
    }

    pub fn failure(command: Option<CommandName>, error: &IrisError, warnings: Vec<Warning>) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            ok: false,
            command,
            result: None,
            error: Some(ErrorBody::from(error)),
            warnings,
        }
    }

    /// Serialize as one line of JSON (secret-scrubbed as a final safety net).
    pub fn to_json_line(&self) -> String {
        let mut value = serde_json::to_value(self).expect("envelope serializes");
        redact::scrub_json(&mut value);
        let mut s = serde_json::to_string(&value).expect("value serializes");
        s.push('\n');
        s
    }
}

/// The published JSON Schema for the envelope, including every result type in `$defs`.
pub fn schema() -> serde_json::Value {
    let schema = schemars::schema_for!(Envelope);
    serde_json::to_value(schema).expect("schema serializes")
}
