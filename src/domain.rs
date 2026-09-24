//! Plain domain types shared by every layer (providers, jobs, output, CLI).
//!
//! Nothing in here performs I/O. Provider wire formats never appear here.

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A media provider known to Iris.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderId {
    /// OpenAI developer API (api.openai.com).
    #[serde(rename = "openai")]
    OpenAi,
    /// Google Gemini developer API (generativelanguage.googleapis.com), including Veo.
    Gemini,
}

/// The one place that names each provider's fixed identity: its id, display name,
/// credential variable, default API base URL, and base URL override variable.
/// Configuration, diagnostics, and redaction iterate [`ProviderId::ALL`] instead
/// of listing providers themselves.
impl ProviderId {
    /// Every provider, in display order (config rows, `providers list`, `doctor`).
    pub const ALL: &'static [ProviderId] = &[ProviderId::OpenAi, ProviderId::Gemini];

    /// The provider's id: its serde name, `--provider` value, and the name of its
    /// `[providers.<id>]` config table.
    pub const fn as_str(self) -> &'static str {
        match self {
            ProviderId::OpenAi => "openai",
            ProviderId::Gemini => "gemini",
        }
    }

    /// Human-readable name for messages.
    pub fn display_name(self) -> &'static str {
        match self {
            ProviderId::OpenAi => "OpenAI",
            ProviderId::Gemini => "Google Gemini API",
        }
    }

    /// The only environment variable Iris reads this provider's credential from.
    pub const fn credential_env(self) -> &'static str {
        match self {
            ProviderId::OpenAi => "OPENAI_API_KEY",
            ProviderId::Gemini => "GEMINI_API_KEY",
        }
    }

    /// The default API base URL (see docs/configuration.md). Credentials are only
    /// ever sent to the origin of the configured base URL.
    pub const fn default_base_url(self) -> &'static str {
        match self {
            // Endpoint paths are appended to it.
            ProviderId::OpenAi => "https://api.openai.com/v1",
            // The origin: the adapter appends the API version (`/v1` or `/v1beta`).
            ProviderId::Gemini => "https://generativelanguage.googleapis.com",
        }
    }

    /// The environment variable overriding the base URL (`IRIS_<PROVIDER>_BASE_URL`;
    /// the config file key is `providers.<id>.base_url`).
    pub const fn base_url_env(self) -> &'static str {
        match self {
            ProviderId::OpenAi => "IRIS_OPENAI_BASE_URL",
            ProviderId::Gemini => "IRIS_GEMINI_BASE_URL",
        }
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parses a provider id, case-insensitively; `google` is accepted for `gemini`.
impl FromStr for ProviderId {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let name = s.to_ascii_lowercase();
        if name == "google" {
            return Ok(ProviderId::Gemini);
        }
        ProviderId::ALL.iter().copied().find(|p| p.as_str() == name).ok_or_else(|| {
            let known: Vec<&str> = ProviderId::ALL.iter().map(|p| p.as_str()).collect();
            format!("unknown provider '{name}' (expected: {})", known.join(", "))
        })
    }
}

/// A media operation. Serialized as `image.generate`, `image.edit`, `video.generate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub enum Operation {
    #[serde(rename = "image.generate")]
    ImageGenerate,
    #[serde(rename = "image.edit")]
    ImageEdit,
    #[serde(rename = "video.generate")]
    VideoGenerate,
}

impl Operation {
    pub const ALL: &'static [Operation] =
        &[Operation::ImageGenerate, Operation::ImageEdit, Operation::VideoGenerate];

    pub fn as_str(self) -> &'static str {
        match self {
            Operation::ImageGenerate => "image.generate",
            Operation::ImageEdit => "image.edit",
            Operation::VideoGenerate => "video.generate",
        }
    }

    /// True for operations the provider runs as a native asynchronous job.
    pub fn is_async_job(self) -> bool {
        matches!(self, Operation::VideoGenerate)
    }
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Operation {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "image.generate" => Ok(Operation::ImageGenerate),
            "image.edit" => Ok(Operation::ImageEdit),
            "video.generate" => Ok(Operation::VideoGenerate),
            other => Err(format!(
                "unknown operation '{other}' (expected: image.generate, image.edit, video.generate)"
            )),
        }
    }
}

/// Normalized status of a persisted provider-native job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Record written; submission request in flight (or the process died during it).
    Submitting,
    /// The provider may or may not have accepted the paid request. Never auto-resubmitted.
    SubmissionUnknown,
    /// Accepted by the provider and not finished.
    Running,
    /// Finished successfully; outputs may or may not be downloaded yet.
    Succeeded,
    /// The provider rejected the submission or reported the job failed.
    Failed,
    /// The remote operation or its outputs are no longer available.
    Expired,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Submitting => "submitting",
            JobStatus::SubmissionUnknown => "submission_unknown",
            JobStatus::Running => "running",
            JobStatus::Succeeded => "succeeded",
            JobStatus::Failed => "failed",
            JobStatus::Expired => "expired",
        }
    }

    /// Terminal from Iris's point of view: polling cannot change it.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::SubmissionUnknown | JobStatus::Succeeded | JobStatus::Failed | JobStatus::Expired
        )
    }
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JobStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "submitting" => Ok(JobStatus::Submitting),
            "submission_unknown" => Ok(JobStatus::SubmissionUnknown),
            "running" => Ok(JobStatus::Running),
            "succeeded" => Ok(JobStatus::Succeeded),
            "failed" => Ok(JobStatus::Failed),
            "expired" => Ok(JobStatus::Expired),
            other => Err(format!("unknown job status '{other}'")),
        }
    }
}

/// Download state of one output of a succeeded job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Pending,
    Downloaded,
    Failed,
    Expired,
}

/// A non-fatal notice attached to a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    /// Stable snake_case warning code (additive set; see docs/json-contract.md).
    pub code: String,
    pub message: String,
}

impl Warning {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Warning { code: code.into(), message: message.into() }
    }
}

/// Token/usage information reported by a provider, normalized where possible.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    /// Provider usage object as reported (numbers only; unstable shape).
    pub provider_usage: Option<serde_json::Value>,
}

/// A cost estimate. Always an estimate computed from Iris's published price table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CostEstimate {
    /// Always `true`: this is an estimate, not a bill.
    pub estimated: bool,
    pub currency: String,
    pub amount: f64,
    /// Human-readable basis, e.g. "1 image x $0.011 (gpt-image-1, low, 1024x1024)".
    pub basis: String,
    pub source_url: String,
    /// Date (YYYY-MM-DD) the price table was checked.
    pub as_of: String,
}

impl CostEstimate {
    pub fn usd(amount: f64, basis: impl Into<String>, source_url: &str, as_of: &str) -> Self {
        CostEstimate {
            estimated: true,
            currency: "USD".to_string(),
            amount,
            basis: basis.into(),
            source_url: source_url.to_string(),
            as_of: as_of.to_string(),
        }
    }
}

/// A media file saved locally by Iris.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Artifact {
    /// 0-based position among the outputs of one request/job.
    pub index: u32,
    /// Absolute local path.
    pub path: String,
    pub media_type: String,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file contents.
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_seconds: Option<f64>,
}
