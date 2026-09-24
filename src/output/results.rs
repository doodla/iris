//! Result payloads of every command (see docs/json-contract.md). These types ARE the JSON contract:
//! the published schema is generated from them.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::Lifecycle;
use crate::domain::{Artifact, CostEstimate, DownloadState, JobStatus, Operation, ProviderId, Usage};
use crate::providers::AccountAccess;

use super::envelope::ErrorBody;

/// `image.generate` / `image.edit`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImageResult {
    pub provider: ProviderId,
    pub model: String,
    pub operation: Operation,
    /// Always `succeeded` (failures are errors).
    pub status: JobStatus,
    pub created_at: String,
    pub completed_at: String,
    pub provider_request_id: Option<String>,
    pub artifacts: Vec<Artifact>,
    /// Text returned by the model alongside images, if any.
    pub text: Option<String>,
    pub usage: Option<Usage>,
    pub cost_estimate: Option<CostEstimate>,
}

/// One output of a job.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobOutputView {
    pub index: u32,
    pub media_type: Option<String>,
    pub download_state: DownloadState,
    pub artifact: Option<Artifact>,
    pub last_error: Option<ErrorBody>,
}

/// A persisted provider-native job, as shown to users and agents.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobView {
    pub job_id: String,
    pub remote_operation_id: Option<String>,
    pub provider: ProviderId,
    pub model: String,
    pub operation: Operation,
    pub status: JobStatus,
    pub created_at: String,
    pub submitted_at: Option<String>,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub last_checked_at: Option<String>,
    /// Estimated time after which the provider no longer serves the outputs.
    pub remote_expires_at: Option<String>,
    pub outputs: Vec<JobOutputView>,
    /// Downloaded artifacts (subset of `outputs[].artifact`).
    pub artifacts: Vec<Artifact>,
    pub error: Option<ErrorBody>,
    pub usage: Option<Usage>,
    pub cost_estimate: Option<CostEstimate>,
    /// Non-secret resolved request options (never the prompt text or input contents).
    pub request: serde_json::Map<String, serde_json::Value>,
}

/// `video.generate`, `jobs.status`, `jobs.wait`, `jobs.download`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobResult {
    pub job: JobView,
    /// Suggested follow-up commands.
    pub next_steps: Vec<String>,
}

/// `jobs.list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobListResult {
    pub jobs: Vec<JobView>,
}

/// `jobs.delete`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct JobDeleteResult {
    pub deleted: Vec<String>,
    /// Always `none`: deletion is local only.
    pub remote_effect: String,
    pub note: String,
}

/// Summary row of `models.list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelSummary {
    pub id: String,
    pub provider: ProviderId,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub lifecycle: Lifecycle,
    pub operations: Vec<Operation>,
    pub default_for: Vec<Operation>,
}

/// `models.list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelListResult {
    pub models: Vec<ModelSummary>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InputsView {
    pub max_input_images: u32,
    pub input_media_types: Vec<String>,
    pub max_input_bytes: u64,
    /// Whether `--mask` is accepted (its rules are in `mask_requirements`).
    pub mask: bool,
    /// Rules a `--mask` must meet; null when masks are not accepted.
    pub mask_requirements: Option<MaskRequirementsView>,
    pub first_frame: bool,
    pub last_frame: bool,
    pub max_reference_images: u32,
    /// Largest encoded request (prompt, options, and base64 inputs sent inline) the
    /// provider accepts, if it documents one; Iris checks an upper bound of the size
    /// locally.
    pub max_request_bytes: Option<u64>,
}

/// Rules for the `--mask` of `image.edit`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct MaskRequirementsView {
    /// Accepted media types (sniffed from the content).
    pub media_types: Vec<String>,
    pub max_bytes: u64,
    /// The mask needs an alpha channel: its fully transparent areas are edited.
    pub alpha_channel_required: bool,
    /// The mask must have the pixel dimensions of the first `--image`.
    pub same_size_as_first_image: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OptionView {
    pub name: String,
    /// `enum`, `integer`, `boolean`, `string`.
    #[serde(rename = "type")]
    pub kind: String,
    pub values: Option<Vec<String>>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    /// Syntax description for pattern-validated strings.
    pub syntax: Option<String>,
    pub default: Option<String>,
    /// Typed CLI flag, or null if only settable with `-O name=value`.
    pub flag: Option<String>,
    pub operations: Vec<Operation>,
    pub description: String,
}

/// A rule relating several options or inputs of one request. A request that breaks
/// it fails with `invalid_argument` and this `id` in `details.constraint`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConstraintView {
    /// Stable snake_case id, e.g. `high_resolution_requires_duration_8`.
    pub id: String,
    /// Options involved (the `name`s in `options`).
    pub options: Vec<String>,
    /// Inputs involved: `image`, `mask`, `first_frame`, `last_frame`, `reference`.
    pub inputs: Vec<String>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutputsView {
    pub media_types: Vec<String>,
    pub max_count: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LimitsView {
    pub max_prompt_chars: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PriceView {
    pub description: String,
    pub unit: String,
    pub usd: f64,
    pub source_url: String,
    pub as_of: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AccessView {
    pub credential_env: String,
    pub credential_present: bool,
    /// Documented account requirements (tier, verification, allowlists).
    pub requirements: Vec<String>,
    /// Result of `--check-access` (`not_checked` without it): `available` means the
    /// provider's model metadata is visible to this key. Billing tier, prepaid credit,
    /// and organization verification are not checked.
    pub account_access: AccountAccess,
    pub checked_at: Option<String>,
}

/// `models.show`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelCapabilities {
    pub id: String,
    pub provider: ProviderId,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub lifecycle: Lifecycle,
    pub operations: Vec<Operation>,
    pub default_for: Vec<Operation>,
    pub inputs: InputsView,
    pub options: Vec<OptionView>,
    /// Rules relating several options or inputs (empty when there are none).
    pub constraints: Vec<ConstraintView>,
    pub outputs: OutputsView,
    pub limits: LimitsView,
    pub pricing: Vec<PriceView>,
    pub access: AccessView,
    /// `catalog` (declared by Iris, checked on `catalog_as_of`).
    pub capabilities_source: String,
    pub catalog_as_of: String,
    pub docs_url: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelShowResult {
    pub model: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProviderView {
    pub id: ProviderId,
    pub display_name: String,
    pub credential_env: String,
    pub credential_present: bool,
    pub operations: Vec<Operation>,
    pub base_url: String,
    pub docs_url: String,
}

/// `providers.list`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProviderListResult {
    pub providers: Vec<ProviderView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    Flag,
    Env,
    File,
    Default,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SettingView {
    pub key: String,
    pub value: serde_json::Value,
    pub source: SettingSource,
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CredentialView {
    pub env: String,
    pub present: bool,
}

/// `config.show`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfigShowResult {
    pub config_file: String,
    pub config_file_exists: bool,
    pub settings: Vec<SettingView>,
    pub credentials: Vec<CredentialView>,
}

/// `config.path`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ConfigPathResult {
    pub config_file: String,
    pub state_dir: String,
    pub jobs_dir: String,
}

#[derive(Debug, Clone, Copy, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DoctorCheck {
    /// Unique within one result, e.g. `credentials.openai` or `access.gemini.<model>`.
    pub id: String,
    pub status: CheckStatus,
    pub message: String,
}

/// `doctor`. The command exits 0 whenever the checks ran; read `healthy`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DoctorResult {
    /// False if any check has status `error`.
    pub healthy: bool,
    pub checks: Vec<DoctorCheck>,
}

/// `schema`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct SchemaResult {
    pub schema: serde_json::Value,
}

/// `completions`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CompletionsResult {
    pub shell: String,
    pub script: String,
}

/// `version`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct VersionResult {
    pub name: String,
    pub version: String,
    pub schema_version: u32,
    pub target: String,
    pub git_commit: Option<String>,
}

/// `--help` in JSON mode.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HelpResult {
    pub help: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanInput {
    /// `image`, `mask`, `first_frame`, `last_frame`, `reference`.
    pub role: String,
    pub path: String,
    pub media_type: String,
    pub bytes: u64,
}

/// `--dry-run` of any generation command.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanResult {
    /// Always `true`.
    pub dry_run: bool,
    pub provider: ProviderId,
    pub model: String,
    pub operation: Operation,
    /// True if the real command would create a provider-native async job.
    pub async_job: bool,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub inputs: Vec<PlanInput>,
    pub outputs: Vec<String>,
    pub credential_present: bool,
    pub cost_estimate: Option<CostEstimate>,
}
