//! The public error taxonomy: stable codes, categories, exit codes, retryability.
//!
//! `ErrorCode` values are part of the public JSON contract (see
//! docs/json-contract.md). Provider error strings never become codes; they are
//! carried as informational `provider_code` / `details.provider_message`.

use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::{JobStatus, ProviderId};

/// Process exit codes (documented in README and docs/json-contract.md).
pub mod exit {
    pub const SUCCESS: i32 = 0;
    /// Runtime or provider failure.
    pub const FAILURE: i32 = 1;
    /// Invalid request (usage, validation, configuration, or conflict): fix it before
    /// retrying. Either local validation rejected it (nothing was sent; the error has
    /// no `provider_status`) or the provider rejected it as given (`provider_status` set).
    pub const USAGE: i32 = 2;
    /// Credentials, access, or quota problem that needs account/config action.
    pub const ACCOUNT: i32 = 3;
    /// Not finished yet: the remote job continues; resume later.
    pub const PENDING: i32 = 4;
    /// Outcome uncertain: the provider may have accepted a paid request.
    pub const UNCERTAIN: i32 = 5;
    /// Interrupted by SIGINT (Ctrl-C), SIGTERM, or SIGHUP.
    pub const INTERRUPTED: i32 = 130;
}

/// Stable, public error codes.
///
/// Deserialization is tolerant: a code this binary does not know (written by a newer
/// Iris into a persisted job record) reads as `internal_error` instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    UsageError,
    ModelRequired,
    InvalidArgument,
    UnsupportedOperation,
    UnsupportedOption,
    UnknownModel,
    UnknownProvider,
    InputFileInvalid,
    CostLimitExceeded,
    ConfigInvalid,
    OutputExists,
    JobNotFound,
    MissingCredentials,
    AuthenticationFailed,
    PermissionDenied,
    QuotaExceeded,
    RateLimited,
    ContentBlocked,
    ProviderError,
    ProviderBadResponse,
    RemoteJobFailed,
    NetworkError,
    RequestTimeout,
    DownloadFailed,
    ArtifactExpired,
    InvalidMedia,
    StateInvalid,
    IoError,
    InternalError,
    WaitTimeout,
    JobNotReady,
    SubmissionUncertain,
    Interrupted,
}

/// Coarse, stable grouping of error codes. Deserialization is tolerant like [`ErrorCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Usage,
    Validation,
    Config,
    Conflict,
    NotFound,
    Auth,
    Access,
    Quota,
    RateLimit,
    Content,
    Provider,
    Network,
    Timeout,
    Artifact,
    Io,
    Internal,
    Pending,
    Uncertain,
    Interrupted,
}

impl ErrorCode {
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::UsageError,
        ErrorCode::ModelRequired,
        ErrorCode::InvalidArgument,
        ErrorCode::UnsupportedOperation,
        ErrorCode::UnsupportedOption,
        ErrorCode::UnknownModel,
        ErrorCode::UnknownProvider,
        ErrorCode::InputFileInvalid,
        ErrorCode::CostLimitExceeded,
        ErrorCode::ConfigInvalid,
        ErrorCode::OutputExists,
        ErrorCode::JobNotFound,
        ErrorCode::MissingCredentials,
        ErrorCode::AuthenticationFailed,
        ErrorCode::PermissionDenied,
        ErrorCode::QuotaExceeded,
        ErrorCode::RateLimited,
        ErrorCode::ContentBlocked,
        ErrorCode::ProviderError,
        ErrorCode::ProviderBadResponse,
        ErrorCode::RemoteJobFailed,
        ErrorCode::NetworkError,
        ErrorCode::RequestTimeout,
        ErrorCode::DownloadFailed,
        ErrorCode::ArtifactExpired,
        ErrorCode::InvalidMedia,
        ErrorCode::StateInvalid,
        ErrorCode::IoError,
        ErrorCode::InternalError,
        ErrorCode::WaitTimeout,
        ErrorCode::JobNotReady,
        ErrorCode::SubmissionUncertain,
        ErrorCode::Interrupted,
    ];

    pub fn as_str(self) -> &'static str {
        use ErrorCode::*;
        match self {
            UsageError => "usage_error",
            ModelRequired => "model_required",
            InvalidArgument => "invalid_argument",
            UnsupportedOperation => "unsupported_operation",
            UnsupportedOption => "unsupported_option",
            UnknownModel => "unknown_model",
            UnknownProvider => "unknown_provider",
            InputFileInvalid => "input_file_invalid",
            CostLimitExceeded => "cost_limit_exceeded",
            ConfigInvalid => "config_invalid",
            OutputExists => "output_exists",
            JobNotFound => "job_not_found",
            MissingCredentials => "missing_credentials",
            AuthenticationFailed => "authentication_failed",
            PermissionDenied => "permission_denied",
            QuotaExceeded => "quota_exceeded",
            RateLimited => "rate_limited",
            ContentBlocked => "content_blocked",
            ProviderError => "provider_error",
            ProviderBadResponse => "provider_bad_response",
            RemoteJobFailed => "remote_job_failed",
            NetworkError => "network_error",
            RequestTimeout => "request_timeout",
            DownloadFailed => "download_failed",
            ArtifactExpired => "artifact_expired",
            InvalidMedia => "invalid_media",
            StateInvalid => "state_invalid",
            IoError => "io_error",
            InternalError => "internal_error",
            WaitTimeout => "wait_timeout",
            JobNotReady => "job_not_ready",
            SubmissionUncertain => "submission_uncertain",
            Interrupted => "interrupted",
        }
    }

    pub fn category(self) -> ErrorCategory {
        use ErrorCategory as C;
        use ErrorCode::*;
        match self {
            UsageError | ModelRequired => C::Usage,
            InvalidArgument | UnsupportedOperation | UnsupportedOption | UnknownModel | UnknownProvider
            | InputFileInvalid | CostLimitExceeded => C::Validation,
            ConfigInvalid => C::Config,
            OutputExists => C::Conflict,
            JobNotFound => C::NotFound,
            MissingCredentials | AuthenticationFailed => C::Auth,
            PermissionDenied => C::Access,
            QuotaExceeded => C::Quota,
            RateLimited => C::RateLimit,
            ContentBlocked => C::Content,
            ProviderError | ProviderBadResponse | RemoteJobFailed => C::Provider,
            NetworkError => C::Network,
            RequestTimeout => C::Timeout,
            DownloadFailed | ArtifactExpired | InvalidMedia => C::Artifact,
            StateInvalid | IoError => C::Io,
            InternalError => C::Internal,
            WaitTimeout | JobNotReady => C::Pending,
            SubmissionUncertain => C::Uncertain,
            Interrupted => C::Interrupted,
        }
    }

    pub fn exit_code(self) -> i32 {
        match self.category() {
            ErrorCategory::Usage
            | ErrorCategory::Validation
            | ErrorCategory::Config
            | ErrorCategory::Conflict
            | ErrorCategory::NotFound => exit::USAGE,
            ErrorCategory::Auth | ErrorCategory::Access | ErrorCategory::Quota => exit::ACCOUNT,
            ErrorCategory::Pending => exit::PENDING,
            ErrorCategory::Uncertain => exit::UNCERTAIN,
            ErrorCategory::Interrupted => exit::INTERRUPTED,
            ErrorCategory::RateLimit
            | ErrorCategory::Content
            | ErrorCategory::Provider
            | ErrorCategory::Network
            | ErrorCategory::Timeout
            | ErrorCategory::Artifact
            | ErrorCategory::Io
            | ErrorCategory::Internal => exit::FAILURE,
        }
    }

    /// Default retryability ("is it reasonable to run the same command again?").
    /// `None` means unknown. Individual errors may override it.
    pub fn default_retryable(self) -> Option<bool> {
        use ErrorCode::*;
        match self {
            RateLimited | NetworkError | RequestTimeout | DownloadFailed | WaitTimeout | JobNotReady
            | Interrupted => Some(true),
            ProviderError | ProviderBadResponse | InvalidMedia | IoError | InternalError => None,
            _ => Some(false),
        }
    }
}

impl ErrorCategory {
    pub const ALL: &'static [ErrorCategory] = &[
        ErrorCategory::Usage,
        ErrorCategory::Validation,
        ErrorCategory::Config,
        ErrorCategory::Conflict,
        ErrorCategory::NotFound,
        ErrorCategory::Auth,
        ErrorCategory::Access,
        ErrorCategory::Quota,
        ErrorCategory::RateLimit,
        ErrorCategory::Content,
        ErrorCategory::Provider,
        ErrorCategory::Network,
        ErrorCategory::Timeout,
        ErrorCategory::Artifact,
        ErrorCategory::Io,
        ErrorCategory::Internal,
        ErrorCategory::Pending,
        ErrorCategory::Uncertain,
        ErrorCategory::Interrupted,
    ];
}

/// Deserialize a unit enum from its serialized string, falling back when unknown.
fn tolerant_enum<'de, D, T>(deserializer: D, all: &[T], fallback: T) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Serialize + Copy,
{
    let raw = String::deserialize(deserializer)?;
    Ok(all
        .iter()
        .copied()
        .find(|v| serde_json::to_value(v).ok().and_then(|j| j.as_str().map(|s| s == raw)).unwrap_or(false))
        .unwrap_or(fallback))
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        tolerant_enum(deserializer, ErrorCode::ALL, ErrorCode::InternalError)
    }
}

impl<'de> Deserialize<'de> for ErrorCategory {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        tolerant_enum(deserializer, ErrorCategory::ALL, ErrorCategory::Internal)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Fields of an [`IrisError`]. Accessible directly on `IrisError` through `Deref`.
#[derive(Debug, Clone)]
pub struct ErrorData {
    pub code: ErrorCode,
    /// Actionable, human-readable message. Must not contain secrets.
    pub message: String,
    pub retryable: Option<bool>,
    pub retry_after: Option<Duration>,
    pub hint: Option<String>,
    pub provider: Option<ProviderId>,
    /// HTTP status returned by the provider, if any.
    pub provider_status: Option<u16>,
    /// Provider's own error code/status string (informational, unstable).
    pub provider_code: Option<String>,
    /// Provider request id (e.g. `x-request-id`), sanitized.
    pub provider_request_id: Option<String>,
    pub job_id: Option<String>,
    pub remote_operation_id: Option<String>,
    pub job_status: Option<JobStatus>,
    /// Extra structured details (e.g. `provider_message`, `charge_possible`, `path`).
    pub details: serde_json::Map<String, serde_json::Value>,
}

/// The single error type flowing through Iris. Rendered by `output` into the
/// JSON error object (after secret scrubbing) or a human message. Boxed so that
/// `Result<T, IrisError>` stays small.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{}", .0.message)]
pub struct IrisError(Box<ErrorData>);

impl std::ops::Deref for IrisError {
    type Target = ErrorData;
    fn deref(&self) -> &ErrorData {
        &self.0
    }
}

impl std::ops::DerefMut for IrisError {
    fn deref_mut(&mut self) -> &mut ErrorData {
        &mut self.0
    }
}

impl IrisError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        IrisError(Box::new(ErrorData {
            code,
            message: message.into(),
            retryable: code.default_retryable(),
            retry_after: None,
            hint: None,
            provider: None,
            provider_status: None,
            provider_code: None,
            provider_request_id: None,
            job_id: None,
            remote_operation_id: None,
            job_status: None,
            details: serde_json::Map::new(),
        }))
    }

    pub fn usage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::UsageError, message)
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, message)
    }

    pub fn io(context: impl fmt::Display, err: &std::io::Error) -> Self {
        Self::new(ErrorCode::IoError, format!("{context}: {err}"))
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn with_retryable(mut self, retryable: Option<bool>) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_retry_after(mut self, after: Duration) -> Self {
        self.retry_after = Some(after);
        self
    }

    pub fn with_provider(mut self, provider: ProviderId) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn with_provider_status(mut self, status: u16) -> Self {
        self.provider_status = Some(status);
        self
    }

    pub fn with_provider_code(mut self, code: impl Into<String>) -> Self {
        self.provider_code = Some(code.into());
        self
    }

    pub fn with_provider_request_id(mut self, id: Option<String>) -> Self {
        if id.is_some() {
            self.provider_request_id = id;
        }
        self
    }

    pub fn with_job(mut self, job_id: impl Into<String>, status: Option<JobStatus>) -> Self {
        self.job_id = Some(job_id.into());
        if status.is_some() {
            self.job_status = status;
        }
        self
    }

    pub fn with_remote_operation(mut self, remote_id: impl Into<String>) -> Self {
        self.remote_operation_id = Some(remote_id.into());
        self
    }

    pub fn with_detail(mut self, key: &str, value: impl Into<serde_json::Value>) -> Self {
        self.details.insert(key.to_string(), value.into());
        self
    }

    pub fn exit_code(&self) -> i32 {
        self.code.exit_code()
    }

    pub fn category(&self) -> ErrorCategory {
        self.code.category()
    }
}

pub type Result<T, E = IrisError> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_a_distinct_snake_case_name_matching_serde() {
        let mut seen = std::collections::HashSet::new();
        for code in ErrorCode::ALL {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, serde_json::Value::String(code.as_str().to_string()));
            assert!(seen.insert(code.as_str()), "duplicate code {}", code.as_str());
        }
    }

    #[test]
    fn deserialization_round_trips_and_tolerates_unknown_values() {
        for code in ErrorCode::ALL {
            let json = serde_json::to_string(code).unwrap();
            assert_eq!(serde_json::from_str::<ErrorCode>(&json).unwrap(), *code);
        }
        for cat in ErrorCategory::ALL {
            let json = serde_json::to_string(cat).unwrap();
            assert_eq!(serde_json::from_str::<ErrorCategory>(&json).unwrap(), *cat);
        }
        assert_eq!(
            serde_json::from_str::<ErrorCode>("\"some_future_code\"").unwrap(),
            ErrorCode::InternalError
        );
        assert_eq!(serde_json::from_str::<ErrorCategory>("\"future\"").unwrap(), ErrorCategory::Internal);
    }

    #[test]
    fn exit_code_mapping_matches_contract() {
        assert_eq!(ErrorCode::UsageError.exit_code(), 2);
        assert_eq!(ErrorCode::UnsupportedOption.exit_code(), 2);
        assert_eq!(ErrorCode::OutputExists.exit_code(), 2);
        assert_eq!(ErrorCode::CostLimitExceeded.exit_code(), 2);
        assert_eq!(ErrorCode::JobNotFound.exit_code(), 2);
        assert_eq!(ErrorCode::MissingCredentials.exit_code(), 3);
        assert_eq!(ErrorCode::QuotaExceeded.exit_code(), 3);
        assert_eq!(ErrorCode::RateLimited.exit_code(), 1);
        assert_eq!(ErrorCode::DownloadFailed.exit_code(), 1);
        assert_eq!(ErrorCode::WaitTimeout.exit_code(), 4);
        assert_eq!(ErrorCode::JobNotReady.exit_code(), 4);
        assert_eq!(ErrorCode::SubmissionUncertain.exit_code(), 5);
        assert_eq!(ErrorCode::Interrupted.exit_code(), 130);
    }
}
