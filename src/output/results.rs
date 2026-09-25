//! Result payloads of every command (see docs/json-contract.md). These types ARE the JSON contract:
//! the published schema is generated from them.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::{Lifecycle, OptionValue};
use crate::domain::{
    Artifact, Billing, CostEstimate, DownloadState, JobStatus, ModelSource, Operation, ProviderId, Usage,
};
use crate::providers::AccountAccess;

use super::envelope::ErrorBody;

/// `image.generate` / `image.edit`.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ImageResult {
    pub provider: ProviderId,
    pub model: String,
    /// Where `model` came from: `flag` (`-m/--model`) or `config` (`image.model`).
    pub model_source: ModelSource,
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
    /// Where `model` came from: `flag` (`-m/--model`) or `config` (`video.model`);
    /// `null` when the job record does not say.
    pub model_source: Option<ModelSource>,
    pub operation: Operation,
    pub status: JobStatus,
    pub created_at: String,
    pub submitted_at: Option<String>,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub last_checked_at: Option<String>,
    /// Earliest time the provider may stop serving the outputs: submission time plus
    /// the provider's documented retention (it may keep them longer). `null` until
    /// the job has succeeded, and when the provider documents no retention.
    pub remote_expires_at: Option<String>,
    pub outputs: Vec<JobOutputView>,
    /// Downloaded artifacts (subset of `outputs[].artifact`).
    pub artifacts: Vec<Artifact>,
    pub error: Option<ErrorBody>,
    pub usage: Option<Usage>,
    pub cost_estimate: Option<CostEstimate>,
    /// Non-secret resolved request options (never the prompt text or input contents).
    pub request: serde_json::Map<String, serde_json::Value>,
    /// Where `jobs wait` and `jobs download` save the outputs when given neither
    /// `-o` nor `-d`: the target `video generate` recorded when it submitted the job.
    pub output_plan: OutputPlanView,
    /// The fingerprint of the prompt the job was submitted with, never its text (not
    /// even when `jobs.store_prompts` keeps the text in the record): enough to tell
    /// which request created a job, such as one left `submitting` by a process that
    /// was killed.
    pub prompt_fingerprint: PromptFingerprint,
}

/// A prompt's SHA-256 and length: enough to match a job to a prompt one has, not to
/// recover the prompt.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PromptFingerprint {
    /// Lowercase hex SHA-256 of the prompt as sent, encoded as UTF-8 (a
    /// `--prompt-file` or `--prompt-stdin` prompt without its trailing whitespace).
    pub sha256: String,
    /// The prompt's length in characters (Unicode scalar values).
    pub chars: u64,
}

/// The save target a job recorded when it was submitted.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OutputPlanView {
    /// The absolute `-o` path given at submission (with several outputs,
    /// `<stem>-<i>.<ext>`, `i` from 1); null when none was given.
    pub path: Option<String>,
    /// The absolute output directory resolved at submission (`-d`, `IRIS_OUTPUT_DIR`,
    /// config `output_dir`, or the current directory), where outputs are saved as
    /// `<job_id>.<ext>`; null when `-o` was given. When both are null, the output
    /// directory of the later command applies.
    pub dir: Option<String>,
    /// Whether `--overwrite` was given at submission: a file already at the target is
    /// replaced.
    pub overwrite: bool,
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
    /// What the model is for and its trade-off, in one line (from the provider's
    /// documentation).
    pub summary: String,
    pub aliases: Vec<String>,
    pub lifecycle: Lifecycle,
    /// Whether the model's requests cost money.
    pub billing: Billing,
    pub operations: Vec<Operation>,
    /// The estimate of the model's cheapest single-output request; null when Iris
    /// cannot estimate the model's cost before a call.
    pub lowest_estimate: Option<LowestEstimate>,
}

/// The cheapest single-output request of a model, as its own pre-call estimator
/// prices it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LowestEstimate {
    /// The option values to pass (with their typed flags or `-O name=value`); every
    /// other option keeps its default.
    pub options: BTreeMap<String, OptionValue>,
    pub cost_estimate: CostEstimate,
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
    /// The accepted values, typed like the option's values, when the option accepts
    /// only listed ones: every `enum`, and an `integer` such as a duration of 4, 6,
    /// or 8 seconds. Null otherwise.
    pub values: Option<Vec<OptionValue>>,
    /// Smallest value of an `integer` that accepts every whole number from `min` to
    /// `max`; null for other options.
    pub min: Option<i64>,
    /// Largest value of such an `integer`; null for other options.
    pub max: Option<i64>,
    /// Syntax description for pattern-validated and free-text strings.
    pub syntax: Option<String>,
    /// Longest accepted value in characters (Unicode scalar values) of a free-text
    /// option; null for other options.
    pub max_chars: Option<usize>,
    /// Value in effect when the option is omitted, typed like the option's values
    /// (a string, an integer, or a boolean); null when the provider documents none.
    pub default: Option<OptionValue>,
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
    /// What the model is for and its trade-off, in one line (from the provider's
    /// documentation).
    pub summary: String,
    pub aliases: Vec<String>,
    pub lifecycle: Lifecycle,
    /// Whether the model's requests cost money.
    pub billing: Billing,
    pub operations: Vec<Operation>,
    pub inputs: InputsView,
    pub options: Vec<OptionView>,
    /// Rules relating several options or inputs (empty when there are none).
    pub constraints: Vec<ConstraintView>,
    pub outputs: OutputsView,
    pub limits: LimitsView,
    pub pricing: Vec<PriceView>,
    /// The estimate of the model's cheapest single-output request; null when Iris
    /// cannot estimate the model's cost before a call.
    pub lowest_estimate: Option<LowestEstimate>,
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

/// How the real run of `video generate` waits for its job: the caller wait limit
/// and the time between status checks.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanWait {
    /// When it passes, the real run stops waiting and exits 4 (`wait_timeout`); the
    /// job continues remotely.
    pub timeout: WaitSetting,
    pub poll_interval: WaitSetting,
}

/// One wait setting of a plan: its value, where the value came from (as `config
/// show` reports it), and the ways to set it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct WaitSetting {
    pub seconds: f64,
    /// `flag`, `env`, `file`, or `default` (the built-in value).
    pub source: SettingSource,
    /// The command-line flag that sets it.
    pub flag: String,
    /// The environment variable that sets it.
    pub env_var: String,
    /// The config file key that sets it, as `config show` names the setting.
    pub key: String,
}

/// `--dry-run` of any generation command.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanResult {
    /// Always `true`.
    pub dry_run: bool,
    pub provider: ProviderId,
    pub model: String,
    /// Where `model` came from: `flag` (`-m/--model`) or `config` (`image.model` or
    /// `video.model`).
    pub model_source: ModelSource,
    pub operation: Operation,
    /// True if the real command would create a provider-native async job.
    pub async_job: bool,
    /// True if `--detach` was given: the real run would return right after the
    /// submission instead of waiting. Always false for synchronous commands.
    pub detach: bool,
    /// How the real run would wait for its job; null when it does not wait
    /// (`--detach`, and the synchronous commands).
    pub wait: Option<PlanWait>,
    /// The model's billing: whether the real run costs money.
    pub billing: Billing,
    pub options: serde_json::Map<String, serde_json::Value>,
    pub inputs: Vec<PlanInput>,
    /// Absolute output paths. A name the real run generates is shown as its
    /// pattern: `iris-<ulid>.<ext>` for an image, `<job_id>.<ext>` for a video
    /// (`-<i>` before the extension when there are several).
    pub outputs: Vec<String>,
    pub credential_present: bool,
    pub cost_estimate: Option<CostEstimate>,
}
