//! Capability declarations. Every model Iris knows is described by a static
//! [`ModelSpec`]; validation, `models show`, defaults, and cost estimates all
//! read from these declarations, so they must match the provider's documentation.

use crate::domain::{CostEstimate, Operation, ProviderId};
use crate::error::IrisError;

use super::options::{OptionValue, ResolvedOptions};

/// Lifecycle stage as documented by the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Ga,
    Preview,
    Deprecated,
}

/// Declared input capabilities for a model.
#[derive(Debug, Clone, Copy)]
pub struct InputSpec {
    /// Maximum number of `--image` inputs for `image.edit` (0 = edit unsupported).
    pub max_input_images: u32,
    /// Accepted media types for input images (sniffed from content, not extension).
    pub input_media_types: &'static [&'static str],
    /// Maximum size of a single input file in bytes.
    pub max_input_bytes: u64,
    /// Whether `--mask` is accepted for `image.edit`.
    pub mask: bool,
    /// Whether `--image` (first frame) is accepted for `video.generate`.
    pub first_frame: bool,
    /// Whether `--last-frame` is accepted for `video.generate`.
    pub last_frame: bool,
    /// Maximum number of `--ref` reference images for `video.generate`.
    pub max_reference_images: u32,
}

impl InputSpec {
    pub const NONE: InputSpec = InputSpec {
        max_input_images: 0,
        input_media_types: &[],
        max_input_bytes: 0,
        mask: false,
        first_frame: false,
        last_frame: false,
        max_reference_images: 0,
    };
}

/// Declared output capabilities.
#[derive(Debug, Clone, Copy)]
pub struct OutputSpec {
    /// Media types the provider can return.
    pub media_types: &'static [&'static str],
    /// Maximum number of outputs per request.
    pub max_count: u32,
}

/// Other declared limits.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Maximum prompt length in characters (Unicode scalar values), if documented.
    pub max_prompt_chars: Option<usize>,
}

/// Type and allowed values of an option.
#[derive(Debug, Clone, Copy)]
pub enum OptionKind {
    /// One of a fixed set of string values.
    Enum(&'static [&'static str]),
    /// Integer within an inclusive range.
    Integer { min: i64, max: i64 },
    /// Boolean (`true`/`false`; CLI flags like `--audio`/`--no-audio`).
    Boolean,
    /// Free text with a maximum length in characters.
    Text { max_chars: usize },
    /// String validated by a custom rule (e.g. flexible `WxH` sizes).
    Pattern {
        /// Human description of the accepted syntax, shown in `models show`.
        syntax: &'static str,
        validate: fn(&str) -> Result<(), String>,
    },
}

/// One accepted option. Names are snake_case and CLI-facing.
#[derive(Debug, Clone, Copy)]
pub struct OptionSpec {
    pub name: &'static str,
    pub kind: OptionKind,
    /// Value in effect when the option is omitted (documented provider default), if any.
    /// Iris does not send omitted options; this is for display and cost estimation.
    pub default: Option<&'static str>,
    /// Typed CLI flag exposing this option (e.g. `--quality`), or `None` if it is only
    /// reachable through `-O name=value`.
    pub flag: Option<&'static str>,
    /// Operations this option applies to.
    pub operations: &'static [Operation],
    pub description: &'static str,
}

/// A published price used for estimates.
#[derive(Debug, Clone, Copy)]
pub struct PriceRule {
    pub description: &'static str,
    /// Unit, e.g. "image", "second", "1M output tokens".
    pub unit: &'static str,
    pub usd: f64,
    pub source_url: &'static str,
    /// YYYY-MM-DD the price was checked.
    pub as_of: &'static str,
}

/// Cross-option validation hook for constraints a single `OptionSpec` cannot express
/// (e.g. "1080p requires 8 seconds"). Receives the operation, resolved options, and
/// the number of inputs by role.
pub type RequestValidator = fn(&ValidationInput<'_>) -> Result<(), IrisError>;

/// Cost estimator hook.
pub type CostEstimator = fn(&ModelSpec, &EstimateInput<'_>) -> Option<CostEstimate>;

/// Data available to a [`RequestValidator`].
#[derive(Debug)]
pub struct ValidationInput<'a> {
    pub operation: Operation,
    pub options: &'a ResolvedOptions,
    pub input_images: usize,
    pub has_mask: bool,
    pub has_first_frame: bool,
    pub has_last_frame: bool,
    pub reference_images: usize,
}

/// Data available to a [`CostEstimator`].
#[derive(Debug)]
pub struct EstimateInput<'a> {
    pub operation: Operation,
    pub options: &'a ResolvedOptions,
    /// Number of outputs requested (after defaults).
    pub count: u32,
}

/// Static declaration of one model.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub provider: ProviderId,
    pub display_name: &'static str,
    /// Alternative names accepted by `--model` (e.g. "nano-banana").
    pub aliases: &'static [&'static str],
    pub lifecycle: Lifecycle,
    pub operations: &'static [Operation],
    /// Operations for which this model is the provider's default.
    pub default_for: &'static [Operation],
    pub inputs: InputSpec,
    pub options: &'static [OptionSpec],
    pub outputs: OutputSpec,
    pub limits: Limits,
    pub pricing: &'static [PriceRule],
    /// Account-level requirements (tier, verification, allowlists) as documented.
    pub access_notes: &'static [&'static str],
    pub docs_url: &'static str,
    pub validate: Option<RequestValidator>,
    pub estimate: Option<CostEstimator>,
}

impl ModelSpec {
    pub fn supports(&self, op: Operation) -> bool {
        self.operations.contains(&op)
    }

    pub fn option(&self, name: &str) -> Option<&'static OptionSpec> {
        self.options.iter().find(|o| o.name == name)
    }

    /// Options applicable to `op`.
    pub fn options_for(&self, op: Operation) -> impl Iterator<Item = &'static OptionSpec> {
        self.options.iter().filter(move |o| o.operations.contains(&op))
    }

    /// Explicit value if set, else the declared default.
    pub fn effective(&self, opts: &ResolvedOptions, name: &str) -> Option<OptionValue> {
        opts.get(name).cloned().or_else(|| {
            let spec = self.option(name)?;
            spec.default.and_then(|d| OptionValue::parse(&spec.kind, d).ok())
        })
    }
}
