//! Capability declarations. Every model Iris knows is described by a static
//! [`ModelSpec`]; validation, `models show`, option defaults, and cost estimates all
//! read from these declarations, so they must match the provider's documentation.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId};
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

/// Declared input capabilities for a model. Everything here is checked locally
/// (`artifacts::read_input_image`, `artifacts::check_request_inputs`) before a dry
/// run returns and before any credential is needed, so an adapter never has to be
/// the first to refuse an input.
#[derive(Debug, Clone, Copy)]
pub struct InputSpec {
    /// Maximum number of `--image` inputs for `image.edit` (0 = edit unsupported).
    pub max_input_images: u32,
    /// Accepted media types for input images (sniffed from content, not extension).
    pub input_media_types: &'static [&'static str],
    /// Maximum size of a single input file in bytes.
    pub max_input_bytes: u64,
    /// `--mask` for `image.edit` and its rules; `None` if masks are not accepted.
    pub mask: Option<MaskSpec>,
    /// Whether `--image` (first frame) is accepted for `video.generate`.
    pub first_frame: bool,
    /// Whether `--last-frame` is accepted for `video.generate`.
    pub last_frame: bool,
    /// Maximum number of `--ref` reference images for `video.generate`.
    pub max_reference_images: u32,
    /// Cap on the whole encoded request when inputs are sent inline, if documented.
    pub max_request: Option<RequestSizeLimit>,
}

impl InputSpec {
    pub const NONE: InputSpec = InputSpec {
        max_input_images: 0,
        input_media_types: &[],
        max_input_bytes: 0,
        mask: None,
        first_frame: false,
        last_frame: false,
        max_reference_images: 0,
        max_request: None,
    };
}

/// Rules for the `--mask` of `image.edit`.
#[derive(Debug, Clone, Copy)]
pub struct MaskSpec {
    /// Accepted media types (sniffed from content).
    pub media_types: &'static [&'static str],
    /// Largest accepted mask file in bytes.
    pub max_bytes: u64,
    /// Whether the mask needs an alpha channel (its transparent areas mark the edit).
    pub requires_alpha: bool,
    /// Whether the mask must have the pixel dimensions of the first `--image`.
    pub same_size_as_first_image: bool,
}

/// A documented cap on a whole request whose inputs travel inline (base64 in a
/// JSON body). Iris cannot build the provider's body outside the adapter, so it
/// checks an upper bound of its size: the JSON-escaped prompt and option values,
/// the base64 of every input, and generous allowances for the JSON around them.
/// The bound is never below the body the adapter encodes (tests compare them), so
/// every request the adapter would refuse is refused before a dry run returns.
#[derive(Debug, Clone, Copy)]
pub struct RequestSizeLimit {
    /// Largest accepted request body in bytes.
    pub max_bytes: u64,
    /// Allowance for the fixed JSON of a request (keys, punctuation, enum values).
    pub framing_bytes: u64,
    /// Allowance for the JSON around each inline input (keys and its media type).
    pub per_input_framing_bytes: u64,
}

impl RequestSizeLimit {
    /// Upper bound of the encoded request size for `prompt`, `options`, and inputs
    /// of `input_sizes` bytes each.
    pub fn upper_bound(
        &self,
        prompt: &str,
        options: &ResolvedOptions,
        input_sizes: impl IntoIterator<Item = u64>,
    ) -> u64 {
        let json_len = |text: &str| serde_json::to_string(text).map_or(u64::MAX, |s| s.len() as u64);
        let options: u64 = options
            .iter()
            .map(|(name, value)| json_len(name).saturating_add(json_len(&value.to_string())))
            .fold(0, u64::saturating_add);
        let inputs: u64 = input_sizes
            .into_iter()
            .map(|n| n.div_ceil(3).saturating_mul(4).saturating_add(self.per_input_framing_bytes))
            .fold(0, u64::saturating_add);
        self.framing_bytes.saturating_add(json_len(prompt)).saturating_add(options).saturating_add(inputs)
    }
}

/// The syntax of model ids a provider's adapter can send, so that an unknown id
/// (`--capabilities-from`) is rejected locally, before a dry run or a real run
/// accepts it, whenever the adapter would refuse it.
#[derive(Debug, Clone, Copy)]
pub struct ModelIdSyntax {
    /// Longest accepted id, in bytes.
    pub max_len: usize,
    /// Characters accepted besides ASCII letters and digits.
    pub punctuation: &'static str,
    /// Whether the first character must be an ASCII letter or digit.
    pub alphanumeric_start: bool,
    /// The rule in words, for error messages.
    pub description: &'static str,
}

impl ModelIdSyntax {
    pub fn accepts(&self, id: &str) -> bool {
        !id.is_empty()
            && id.len() <= self.max_len
            && (!self.alphanumeric_start || id.as_bytes()[0].is_ascii_alphanumeric())
            && id.chars().all(|c| c.is_ascii_alphanumeric() || self.punctuation.contains(c))
    }
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
    /// Boolean (`true`/`false`, e.g. `-O name=true`).
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

/// A rule relating several options or inputs of one request, enforced by a model's
/// validator and published by `models show` as `constraints`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constraint {
    /// Stable snake_case id; a request that breaks the rule fails with
    /// `invalid_argument` and this id in `details.constraint`.
    pub id: &'static str,
    /// Options involved (catalog option names).
    pub options: &'static [&'static str],
    /// Inputs involved: `image`, `mask`, `first_frame`, `last_frame`, `reference`.
    pub inputs: &'static [&'static str],
    pub description: &'static str,
}

impl Constraint {
    /// The `invalid_argument` error for a request that breaks this rule.
    pub fn violation(&self, message: impl Into<String>) -> IrisError {
        IrisError::invalid(message).with_detail("constraint", self.id)
    }
}

/// A model's cross-option validation: the check, and the rules it enforces declared
/// next to it (every rejection it makes names one of `constraints`; the catalog
/// tests check that for every combination of declared values).
#[derive(Debug, Clone, Copy)]
pub struct RequestRules {
    pub constraints: &'static [Constraint],
    pub check: RequestValidator,
}

/// Cost estimator hook (before the call, from options). Without an estimate it
/// returns why, and what to pass to get one: the message of the
/// `cost_estimate_unavailable` warning.
pub type CostEstimator = fn(&ModelSpec, &EstimateInput<'_>) -> Result<CostEstimate, String>;

/// A model's pre-call cost estimate: the estimator, and the request it estimates
/// lowest.
#[derive(Debug, Clone, Copy)]
pub struct Estimator {
    pub estimate: CostEstimator,
    /// Option values (`name`, value as `-O` takes it) of the model's cheapest
    /// single-output request; the options not listed keep their defaults.
    /// `models list` and `models show` report its estimate as `lowest_estimate`, and
    /// the catalog tests check that no valid request is estimated lower.
    pub lowest: &'static [(&'static str, &'static str)],
}

/// Post-call cost estimator hook, from provider-reported usage. The usage already
/// covers every output of the response, so implementations must not multiply by count.
pub type UsageEstimator = fn(&ModelSpec, &crate::domain::Usage) -> Option<CostEstimate>;

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

/// A model name Iris deliberately gives no model: a model its provider deprecated,
/// shut down, limited, or serves only elsewhere, a family of them, or a nickname of
/// one (docs/decisions.md, "Built-in models"). `-m` with such a name is
/// `unknown_model` with a hint that says why and what to use instead, never
/// `--capabilities-from`, which would send the name anyway. Each provider's catalog
/// module declares its own, from the provider documentation cited there.
#[derive(Debug, Clone, Copy)]
pub struct DeclinedName {
    /// Names matched exactly, and their dated snapshots (`<name>-YYYY-MM-DD`), ignoring
    /// ASCII case.
    pub names: &'static [&'static str],
    /// Family stems: a stem matches itself and every name that continues it with
    /// `-` (ignoring ASCII case), so `veo-3` matches `veo-3` and `veo-3-fast`, and
    /// `veo-3.0` matches `veo-3.0-generate-001`.
    pub families: &'static [&'static str],
    /// Why Iris registers no model for it, as the provider documents it.
    pub reason: &'static str,
    /// Catalog ids or aliases to use instead, most fitting first.
    pub instead: &'static [&'static str],
}

/// Static declaration of one model.
#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub provider: ProviderId,
    pub display_name: &'static str,
    /// One line for choosing the model: what it is for and its trade-off, in the
    /// provider's documented terms (or, where the provider says nothing, factual
    /// differences such as options and prices).
    pub summary: &'static str,
    /// Alternative names accepted by `--model` (e.g. "nano-banana-2").
    pub aliases: &'static [&'static str],
    pub lifecycle: Lifecycle,
    /// Whether requests cost money, as the provider documents it.
    pub billing: Billing,
    pub operations: &'static [Operation],
    pub inputs: InputSpec,
    pub options: &'static [OptionSpec],
    pub outputs: OutputSpec,
    pub limits: Limits,
    pub pricing: &'static [PriceRule],
    /// Account-level requirements (tier, verification, allowlists) as documented.
    pub access_notes: &'static [&'static str],
    pub docs_url: &'static str,
    /// Cross-option rules and the validator enforcing them, if any.
    pub validate: Option<RequestRules>,
    /// Pre-call cost estimate, if the model's published prices support one.
    pub estimate: Option<Estimator>,
    /// Post-call estimate from reported usage (preferred over `estimate` when it returns a value).
    pub estimate_usage: Option<UsageEstimator>,
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

    /// The options of the model's cheapest single-output request ([`Estimator::lowest`])
    /// and the estimate its own estimator gives them, for its first operation (the
    /// catalog tests check that every operation gives the same); `None` without an
    /// estimator.
    pub fn lowest_estimate(&self) -> Option<(ResolvedOptions, CostEstimate)> {
        let estimator = self.estimate?;
        let mut options = ResolvedOptions::new();
        for (name, value) in estimator.lowest {
            options.insert(*name, OptionValue::parse(&self.option(name)?.kind, value).ok()?);
        }
        let operation = *self.operations.first()?;
        let estimate =
            (estimator.estimate)(self, &EstimateInput { operation, options: &options, count: 1 }).ok()?;
        Some((options, estimate))
    }
}
