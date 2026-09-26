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
    /// Every provider, in display order (config rows, `doctor`, and the order of
    /// `Registry::builtin()`, which `providers list` iterates). The compiler does not
    /// check that this list is complete; the `all_lists_every_provider_once` test
    /// below compares it with the enum's variants, the registry, and the catalog.
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

    /// The default API base URL (see docs/reference/configuration.md). Credentials are only
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

    /// What the operation produces: `image` or `video`.
    pub fn media(self) -> &'static str {
        match self {
            Operation::ImageGenerate | Operation::ImageEdit => "image",
            Operation::VideoGenerate => "video",
        }
    }

    /// The config file key that names the model for this operation when `-m/--model`
    /// is not given: `image.model` for the image operations, `video.model` for video.
    pub fn model_config_key(self) -> &'static str {
        match self {
            Operation::ImageGenerate | Operation::ImageEdit => "image.model",
            Operation::VideoGenerate => "video.model",
        }
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

/// Where a generation command's model came from. Iris never chooses a model
/// itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// `-m/--model`.
    Flag,
    /// The config file: `image.model` for the image operations, `video.model` for
    /// video.
    Config,
}

/// Whether a model's requests cost money, as the catalog declares it. An open set
/// (docs/reference/json-output.md): a later Iris may add a value, and human output uses each
/// value as the adjective for a request ("this is a paid request").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// Requests are billed to the provider account at its published prices; the
    /// provider offers no free tier.
    Paid,
}

impl Billing {
    /// Every value, in the documented order. The compiler does not check that it is
    /// complete; the `billing_values_are_distinct_snake_case_names` test compares it
    /// with the enum's variants.
    pub const ALL: &'static [Billing] = &[Billing::Paid];

    /// The serialized value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Billing::Paid => "paid",
        }
    }

    /// What the value means, for human output and the published schema.
    pub const fn description(self) -> &'static str {
        match self {
            Billing::Paid => {
                "requests are billed to the provider account at its published prices; no free tier"
            }
        }
    }
}

impl fmt::Display for Billing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
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

/// Every warning code Iris emits: the registry of the public, additive set listed in
/// docs/reference/json-output.md ("Warning codes"). A [`Warning`] is only built from one of
/// these ([`Warning::new`]); the contract tests compare this list with the
/// documented one and check that no other source file spells out a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WarningCode {
    /// A model resolved with `--capabilities-from`: its capabilities are assumed.
    UnverifiedModelCapabilities,
    /// An output file extension was added or changed to match the media type.
    OutputExtensionAdjusted,
    /// A file appeared at the target meanwhile; the output got a numbered name.
    OutputRenamed,
    /// A valid image of another type than requested or labeled; kept as it is.
    OutputFormatMismatch,
    /// No cost estimate could be made (the message says why, and which options to
    /// pass for one).
    CostEstimateUnavailable,
    /// A job record could not be read and was skipped.
    JobRecordUnreadable,
    /// The model returned text too (in the result's `text`).
    ProviderTextOutput,
    /// An identical file is already at the target; nothing was written.
    AlreadyDownloaded,
    /// The provider keeps a job's outputs only for a limited time.
    RetentionLimited,
    /// A preview model: behavior, limits, and availability may change.
    PreviewModel,
    /// A provider base URL is not the default; its API key is sent there.
    NonDefaultBaseUrl,
    /// The provider filtered some outputs of a job for safety.
    ContentFiltered,
    /// The provider returned another number of items than requested.
    UnexpectedOutputCount,
    /// A job's remote status could not be refreshed; the last known one is shown.
    StatusRefreshFailed,
    /// A returned item is not a usable image; it was skipped.
    OutputItemUnusable,
    /// Paid output could not be saved where requested and went to the state directory.
    OutputSavedElsewhere,
    /// The model takes no output format: the provider picks the image type, so an
    /// `-o` extension may be changed when the image is saved.
    OutputExtensionMayChange,
}

impl WarningCode {
    /// Every code, in the order of the documented list. The compiler does not check
    /// that it is complete; the `warning_codes_are_distinct_snake_case_names` test
    /// compares it with the enum's variants.
    pub const ALL: &'static [WarningCode] = &[
        WarningCode::UnverifiedModelCapabilities,
        WarningCode::OutputExtensionAdjusted,
        WarningCode::OutputRenamed,
        WarningCode::OutputFormatMismatch,
        WarningCode::CostEstimateUnavailable,
        WarningCode::JobRecordUnreadable,
        WarningCode::ProviderTextOutput,
        WarningCode::AlreadyDownloaded,
        WarningCode::RetentionLimited,
        WarningCode::PreviewModel,
        WarningCode::NonDefaultBaseUrl,
        WarningCode::ContentFiltered,
        WarningCode::UnexpectedOutputCount,
        WarningCode::StatusRefreshFailed,
        WarningCode::OutputItemUnusable,
        WarningCode::OutputSavedElsewhere,
        WarningCode::OutputExtensionMayChange,
    ];

    /// The public snake_case code.
    pub const fn as_str(self) -> &'static str {
        match self {
            WarningCode::UnverifiedModelCapabilities => "unverified_model_capabilities",
            WarningCode::OutputExtensionAdjusted => "output_extension_adjusted",
            WarningCode::OutputRenamed => "output_renamed",
            WarningCode::OutputFormatMismatch => "output_format_mismatch",
            WarningCode::CostEstimateUnavailable => "cost_estimate_unavailable",
            WarningCode::JobRecordUnreadable => "job_record_unreadable",
            WarningCode::ProviderTextOutput => "provider_text_output",
            WarningCode::AlreadyDownloaded => "already_downloaded",
            WarningCode::RetentionLimited => "retention_limited",
            WarningCode::PreviewModel => "preview_model",
            WarningCode::NonDefaultBaseUrl => "non_default_base_url",
            WarningCode::ContentFiltered => "content_filtered",
            WarningCode::UnexpectedOutputCount => "unexpected_output_count",
            WarningCode::StatusRefreshFailed => "status_refresh_failed",
            WarningCode::OutputItemUnusable => "output_item_unusable",
            WarningCode::OutputSavedElsewhere => "output_saved_elsewhere",
            WarningCode::OutputExtensionMayChange => "output_extension_may_change",
        }
    }
}

impl fmt::Display for WarningCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A non-fatal notice attached to a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    // Iris emits only `WarningCode`s; the field is a string so that a reader keeps a
    // code it does not know.
    /// Stable snake_case warning code (additive set; see docs/reference/json-output.md).
    pub code: String,
    pub message: String,
}

impl Warning {
    pub fn new(code: WarningCode, message: impl Into<String>) -> Self {
        Warning { code: code.as_str().to_string(), message: message.into() }
    }

    /// Whether this warning has `code`.
    pub fn is(&self, code: WarningCode) -> bool {
        self.code == code.as_str()
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
    /// Human-readable basis, e.g. "1 image × $0.067 (gemini-3.1-flash-image, 1K); input
    /// and thinking tokens not included".
    pub basis: String,
    pub source_url: String,
    /// Date (YYYY-MM-DD) the price table was checked.
    pub as_of: String,
}

/// A dollar amount as Iris prints it: `$`, then the amount with at least two decimals
/// and every decimal it has (`$0.80`, `$0.00588`, `$30.00`). It is never rounded, so a
/// printed estimate and a printed cap compare as the amounts do.
pub fn format_usd(amount: f64) -> String {
    let digits = amount.to_string();
    let decimals = digits.split_once('.').map_or(0, |(_, decimals)| decimals.len());
    let point = if digits.contains('.') { "" } else { "." };
    format!("${digits}{point}{}", "0".repeat(2usize.saturating_sub(decimals)))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::Value;

    use super::{ProviderId, format_usd};

    /// Dollars print with at least two decimals and every decimal the amount has, so
    /// no amount is rounded, up or down.
    #[test]
    fn dollar_amounts_keep_every_decimal_and_at_least_two() {
        let printed: Vec<String> =
            [0.8, 3.2, 1.0, 30.0, 0.067, 0.0336, 0.00588, 0.21072, 0.005, 1e-7, 1234.5]
                .into_iter()
                .map(format_usd)
                .collect();
        assert_eq!(
            printed,
            [
                "$0.80",
                "$3.20",
                "$1.00",
                "$30.00",
                "$0.067",
                "$0.0336",
                "$0.00588",
                "$0.21072",
                "$0.005",
                "$0.0000001",
                "$1234.50"
            ]
        );
    }

    /// The names an enum itself declares, read from its derived JSON Schema: the
    /// derive sees every variant, unlike a hand-written `ALL`.
    fn declared_names_of(schema: schemars::Schema) -> Vec<String> {
        fn collect(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(map) => {
                    if let Some(Value::String(name)) = map.get("const") {
                        out.push(name.clone());
                    }
                    if let Some(Value::Array(names)) = map.get("enum") {
                        out.extend(names.iter().filter_map(Value::as_str).map(str::to_string));
                    }
                    map.values().for_each(|v| collect(v, out));
                }
                Value::Array(items) => items.iter().for_each(|v| collect(v, out)),
                _ => {}
            }
        }
        let schema = serde_json::to_value(schema).unwrap();
        let mut names = Vec::new();
        collect(&schema, &mut names);
        names
    }

    fn declared_names() -> Vec<String> {
        declared_names_of(schemars::schema_for!(ProviderId))
    }

    #[test]
    fn all_lists_every_provider_once() {
        let all: Vec<&str> = ProviderId::ALL.iter().map(|p| p.as_str()).collect();
        let unique: BTreeSet<&str> = all.iter().copied().collect();
        assert_eq!(unique.len(), all.len(), "ProviderId::ALL has a duplicate: {all:?}");

        let declared = declared_names();
        assert!(!declared.is_empty(), "no variants found in the ProviderId schema");
        let declared: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
        assert_eq!(declared, unique, "ProviderId::ALL must list every ProviderId variant");

        let registered: Vec<ProviderId> =
            crate::providers::Registry::builtin().all().map(|p| p.id()).collect();
        assert_eq!(registered, ProviderId::ALL, "every provider has one adapter, registered in ALL order");

        let cataloged: BTreeSet<ProviderId> = crate::catalog::all().map(|m| m.provider).collect();
        let all_set: BTreeSet<ProviderId> = ProviderId::ALL.iter().copied().collect();
        assert_eq!(
            cataloged, all_set,
            "every provider has catalog models, and every model's provider is in ALL"
        );
    }

    #[test]
    fn warning_codes_are_distinct_snake_case_names() {
        use super::WarningCode;
        let names: BTreeSet<&str> = WarningCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(names.len(), WarningCode::ALL.len(), "duplicate warning code");
        let declared = declared_names_of(schemars::schema_for!(WarningCode));
        let declared: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
        assert_eq!(declared, names, "WarningCode::ALL must list every variant");
        for code in WarningCode::ALL {
            assert_eq!(serde_json::to_value(code).unwrap(), Value::from(code.as_str()));
        }
        for name in names {
            assert!(
                name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{name}"
            );
        }
    }

    #[test]
    fn billing_values_are_distinct_snake_case_names() {
        use super::Billing;
        let names: BTreeSet<&str> = Billing::ALL.iter().map(|b| b.as_str()).collect();
        assert_eq!(names.len(), Billing::ALL.len(), "duplicate billing value");
        let declared = declared_names_of(schemars::schema_for!(Billing));
        let declared: BTreeSet<&str> = declared.iter().map(String::as_str).collect();
        assert_eq!(declared, names, "Billing::ALL must list every variant");
        for b in Billing::ALL {
            assert_eq!(serde_json::to_value(b).unwrap(), Value::from(b.as_str()));
            assert_eq!(b.to_string(), b.as_str());
        }
    }

    #[test]
    fn identity_names_follow_the_id() {
        for &p in ProviderId::ALL {
            let id = p.as_str();
            assert_eq!(serde_json::to_value(p).unwrap(), Value::from(id), "serde name of {p:?}");
            assert_eq!(serde_json::from_value::<ProviderId>(Value::from(id)).unwrap(), p);
            assert_eq!(id.parse::<ProviderId>().unwrap(), p, "--provider {id}");
            assert_eq!(p.to_string(), id);
            assert_eq!(p.base_url_env(), format!("IRIS_{}_BASE_URL", id.to_ascii_uppercase()));
        }
    }
}
