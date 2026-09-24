//! Built-in catalog entries for Veo video generation on the Gemini Developer API
//! (`predictLongRunning`, provider-native asynchronous operations).
//!
//! These values come from verified research against Google's official
//! documentation (docs checked 2026-09-24). Only the three `veo-3.1-*-preview`
//! models remain on the Gemini API; the shut-down `veo-2.0-*`/`veo-3.0-*` ids are
//! deliberately not registered.

use crate::domain::{CostEstimate, Operation, ProviderId};
use crate::error::IrisError;

use super::CATALOG_AS_OF;
use super::types::{
    EstimateInput, InputSpec, Lifecycle, Limits, ModelSpec, OptionKind, OptionSpec, OutputSpec, PriceRule,
    RequestSizeLimit, ValidationInput,
};

/// Pricing page all Veo prices were taken from.
pub const PRICING_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";
/// Provider guide for these models.
pub const DOCS_URL: &str = "https://ai.google.dev/gemini-api/docs/video";

/// Iris default duration in seconds. The adapter always sends the effective value
/// (explicit or this default) so the cost of a job is bounded.
pub const DEFAULT_DURATION: &str = "8";
/// Iris default resolution (always sent, see [`DEFAULT_DURATION`]).
pub const DEFAULT_RESOLUTION: &str = "720p";
/// Iris default aspect ratio (always sent, see [`DEFAULT_DURATION`]).
pub const DEFAULT_ASPECT_RATIO: &str = "16:9";

/// Adapter-enforced cap on the whole encoded `predictLongRunning` request: the
/// provider says to use the Files API above 100 MB of inline data, which Iris does
/// not implement, so requests must stay below this size.
pub const MAX_REQUEST_BYTES: usize = 100_000_000;

/// Generated videos are kept by the provider for 2 days.
pub const OUTPUT_RETENTION_HOURS: u64 = 48;

const VIDEO: &[Operation] = &[Operation::VideoGenerate];

const INPUT_TYPES: &[&str] = &["image/png", "image/jpeg"];

/// The cap for local validation: requests must stay *below* [`MAX_REQUEST_BYTES`].
/// The allowances bound the JSON the adapter writes around the prompt, the
/// parameters, and each inline image (a few hundred bytes in all).
const REQUEST_LIMIT: RequestSizeLimit = RequestSizeLimit {
    max_bytes: MAX_REQUEST_BYTES as u64 - 1,
    framing_bytes: 1024,
    per_input_framing_bytes: 256,
};

const fn inputs(max_reference_images: u32) -> InputSpec {
    InputSpec {
        max_input_images: 0,
        input_media_types: INPUT_TYPES,
        max_input_bytes: 20_000_000,
        mask: None,
        first_frame: true,
        last_frame: true,
        max_reference_images,
        max_request: Some(REQUEST_LIMIT),
    }
}

const OUTPUTS: OutputSpec = OutputSpec { media_types: &["video/mp4"], max_count: 1 };

/// The server limit is 1,024 tokens; Iris cannot count Veo tokens, so it applies a
/// generous character cap and lets the provider's 400 surface beyond that.
const LIMITS: Limits = Limits { max_prompt_chars: Some(16_384) };

const COUNT: OptionSpec = OptionSpec {
    name: "count",
    kind: OptionKind::Integer { min: 1, max: 1 },
    default: Some("1"),
    flag: Some("--count"),
    operations: VIDEO,
    description: "Number of videos. Veo generates one video per request; the value is validated locally \
                  and never sent",
};

const DURATION: OptionSpec = OptionSpec {
    name: "duration",
    kind: OptionKind::Enum(&["4", "6", "8"]),
    default: Some(DEFAULT_DURATION),
    flag: Some("--duration"),
    operations: VIDEO,
    description: "Video length in seconds (always sent). 1080p, 4k, and reference images require 8. \
                  Audio is always generated and cannot be disabled on the Gemini API",
};

const fn resolution(values: &'static [&'static str], description: &'static str) -> OptionSpec {
    OptionSpec {
        name: "resolution",
        kind: OptionKind::Enum(values),
        default: Some(DEFAULT_RESOLUTION),
        flag: Some("--resolution"),
        operations: VIDEO,
        description,
    }
}

const ASPECT_RATIO: OptionSpec = OptionSpec {
    name: "aspect_ratio",
    kind: OptionKind::Enum(&["16:9", "9:16"]),
    default: Some(DEFAULT_ASPECT_RATIO),
    flag: Some("--aspect-ratio"),
    operations: VIDEO,
    description: "Landscape (16:9) or portrait (9:16); always sent",
};

const NEGATIVE_PROMPT: OptionSpec = OptionSpec {
    name: "negative_prompt",
    kind: OptionKind::Text { max_chars: 4000 },
    default: None,
    flag: Some("--negative-prompt"),
    operations: VIDEO,
    description: "What the video should not contain; sent only when given",
};

const PERSON_GENERATION: OptionSpec = OptionSpec {
    name: "person_generation",
    kind: OptionKind::Enum(&["allow_all", "allow_adult"]),
    default: None,
    flag: None,
    operations: VIDEO,
    description: "Whether people may be generated: allow_all only for text-to-video, allow_adult only \
                  with image inputs (first frame, last frame, or references). Omitted unless given; some \
                  regions accept only allow_adult",
};

const FULL_OPTIONS: &[OptionSpec] = &[
    COUNT,
    DURATION,
    resolution(
        &["720p", "1080p", "4k"],
        "Output resolution (always sent); 1080p and 4k require an 8-second duration. The price depends \
         on it; audio is always included",
    ),
    ASPECT_RATIO,
    NEGATIVE_PROMPT,
    PERSON_GENERATION,
];

const LITE_OPTIONS: &[OptionSpec] = &[
    COUNT,
    DURATION,
    resolution(
        &["720p", "1080p"],
        "Output resolution (always sent); 1080p requires an 8-second duration and 4k is not available \
         on this model. The price depends on it; audio is always included",
    ),
    ASPECT_RATIO,
    NEGATIVE_PROMPT,
    PERSON_GENERATION,
];

const NOTE_PREVIEW: &str = "Preview model";
// The billing and key notes are shared with the Gemini image models (see their
// sources in `catalog::gemini`), so both catalogs say the same thing.
const NOTE_PAID: &str = super::gemini::ACCESS_NOTE_PAID_TIER;
const NOTE_AUTH_KEY: &str = super::gemini::ACCESS_NOTE_AUTH_KEY;
const NOTE_AUDIO: &str = "Audio is always generated and cannot be disabled";
const NOTE_RETENTION: &str =
    "Generated videos are deleted by the provider after 2 days; download before then";
const NOTE_NO_CANCEL: &str = "No remote cancellation (the provider offers none for Veo operations)";
const NOTE_NO_DELETE: &str = "Remote deletion of generated videos is not supported by Iris; provider support is \
                              unverified";
const NOTE_REF_WIRE: &str =
    "Reference-image wire format (referenceType casing) verified against official SDKs only, not live";
const NOTE_EXTENSION: &str =
    "Video extension (a previous Veo video as input) is provider-supported but not implemented by Iris";

/// (model, resolution, USD per second of generated video with audio).
const RATES: &[(&str, &str, f64)] = &[
    ("veo-3.1-generate-preview", "720p", 0.40),
    ("veo-3.1-generate-preview", "1080p", 0.40),
    ("veo-3.1-generate-preview", "4k", 0.60),
    ("veo-3.1-fast-generate-preview", "720p", 0.10),
    ("veo-3.1-fast-generate-preview", "1080p", 0.12),
    ("veo-3.1-fast-generate-preview", "4k", 0.30),
    ("veo-3.1-lite-generate-preview", "720p", 0.05),
    ("veo-3.1-lite-generate-preview", "1080p", 0.08),
];

const fn per_second(description: &'static str, usd: f64) -> PriceRule {
    PriceRule { description, unit: "second", usd, source_url: PRICING_URL, as_of: CATALOG_AS_OF }
}

/// Built-in Veo models.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "veo-3.1-fast-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1 Fast",
        aliases: &["veo-fast"],
        lifecycle: Lifecycle::Preview,
        operations: VIDEO,
        default_for: VIDEO,
        inputs: inputs(3),
        options: FULL_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: &[
            per_second("720p video with audio; only charged if generated", 0.10),
            per_second("1080p video with audio; only charged if generated", 0.12),
            per_second("4k video with audio; only charged if generated", 0.30),
        ],
        access_notes: &[
            NOTE_PREVIEW,
            NOTE_PAID,
            NOTE_AUTH_KEY,
            NOTE_AUDIO,
            NOTE_RETENTION,
            NOTE_NO_CANCEL,
            NOTE_NO_DELETE,
            NOTE_REF_WIRE,
            "Reference images on Veo 3.1 Fast are documented in the guide but unverified (the official \
             cookbook lists them for Veo 3.1 only)",
            NOTE_EXTENSION,
        ],
        docs_url: DOCS_URL,
        validate: Some(validate_video),
        estimate: Some(estimate_video),
        estimate_usage: None,
    },
    ModelSpec {
        id: "veo-3.1-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1",
        aliases: &["veo"],
        lifecycle: Lifecycle::Preview,
        operations: VIDEO,
        default_for: &[],
        inputs: inputs(3),
        options: FULL_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: &[
            per_second("720p video with audio; only charged if generated", 0.40),
            per_second("1080p video with audio; only charged if generated", 0.40),
            per_second("4k video with audio; only charged if generated", 0.60),
        ],
        access_notes: &[
            NOTE_PREVIEW,
            NOTE_PAID,
            NOTE_AUTH_KEY,
            NOTE_AUDIO,
            NOTE_RETENTION,
            NOTE_NO_CANCEL,
            NOTE_NO_DELETE,
            NOTE_REF_WIRE,
            NOTE_EXTENSION,
        ],
        docs_url: DOCS_URL,
        validate: Some(validate_video),
        estimate: Some(estimate_video),
        estimate_usage: None,
    },
    ModelSpec {
        id: "veo-3.1-lite-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1 Lite",
        aliases: &["veo-lite"],
        lifecycle: Lifecycle::Preview,
        operations: VIDEO,
        default_for: &[],
        inputs: inputs(0),
        options: LITE_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: &[
            per_second("720p video with audio; only charged if generated", 0.05),
            per_second("1080p video with audio; only charged if generated", 0.08),
        ],
        access_notes: &[
            NOTE_PREVIEW,
            NOTE_PAID,
            NOTE_AUTH_KEY,
            NOTE_AUDIO,
            NOTE_RETENTION,
            NOTE_NO_CANCEL,
            NOTE_NO_DELETE,
            "No 4k output and no reference images on this model",
        ],
        docs_url: DOCS_URL,
        validate: Some(validate_video),
        estimate: Some(estimate_video),
        estimate_usage: None,
    },
];

/// Effective value of a string option: explicit, else the Veo default.
fn effective<'a>(input: &'a ValidationInput<'_>, name: &str, default: &'a str) -> &'a str {
    input.options.get(name).and_then(|v| v.as_str()).unwrap_or(default)
}

fn invalid_option(option: &str, message: String) -> IrisError {
    IrisError::invalid(message).with_detail("option", option)
}

/// Cross-field rules from the provider's parameter table.
fn validate_video(input: &ValidationInput<'_>) -> Result<(), IrisError> {
    let duration = effective(input, "duration", DEFAULT_DURATION);
    let resolution = effective(input, "resolution", DEFAULT_RESOLUTION);
    let has_refs = input.reference_images > 0;
    let has_frames = input.has_first_frame || input.has_last_frame;

    if matches!(resolution, "1080p" | "4k") && duration != "8" {
        return Err(invalid_option(
            "duration",
            format!("resolution {resolution} requires --duration 8 (got {duration})"),
        ));
    }
    if has_refs && has_frames {
        return Err(IrisError::invalid(
            "reference images (--ref) cannot be combined with --image or --last-frame",
        ));
    }
    if has_refs && duration != "8" {
        return Err(invalid_option(
            "duration",
            format!("reference images (--ref) require --duration 8 (got {duration})"),
        ));
    }
    if input.has_last_frame && !input.has_first_frame {
        return Err(IrisError::invalid("--last-frame requires --image (the first frame)"));
    }
    if let Some(person) = input.options.get("person_generation").and_then(|v| v.as_str()) {
        let has_images = has_refs || has_frames;
        match person {
            "allow_all" if has_images => {
                return Err(invalid_option(
                    "person_generation",
                    "person_generation=allow_all is only accepted for text-to-video; use allow_adult with \
                     image inputs"
                        .to_string(),
                ));
            }
            "allow_adult" if !has_images => {
                return Err(invalid_option(
                    "person_generation",
                    "person_generation=allow_adult is only accepted with image inputs (--image, \
                     --last-frame, or --ref); use allow_all for text-to-video"
                        .to_string(),
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

/// USD per second for a model and resolution, if published.
pub fn rate_per_second(model: &str, resolution: &str) -> Option<f64> {
    RATES.iter().find(|(m, r, _)| *m == model && *r == resolution).map(|(_, _, usd)| *usd)
}

/// Estimate: effective duration × published rate for the effective resolution.
fn estimate_video(spec: &ModelSpec, input: &EstimateInput<'_>) -> Option<CostEstimate> {
    let duration = spec.effective(input.options, "duration")?;
    let seconds: u32 = duration.as_str()?.parse().ok()?;
    let resolution = spec.effective(input.options, "resolution")?;
    let resolution = resolution.as_str()?;
    let rate = rate_per_second(spec.id, resolution)?;
    let videos = input.count.max(1);
    let amount = f64::from(seconds) * rate * f64::from(videos);
    Some(CostEstimate::usd(
        (amount * 1e6).round() / 1e6,
        format!(
            "{seconds} s × ${rate}/s ({}, {resolution}, audio included); estimate; blocked videos are not \
             charged",
            spec.id
        ),
        PRICING_URL,
        CATALOG_AS_OF,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_table_matches_published_price_rules() {
        for spec in MODELS {
            let OptionKind::Enum(resolutions) = spec.option("resolution").unwrap().kind else {
                panic!("resolution is an enum");
            };
            assert_eq!(spec.pricing.len(), resolutions.len(), "{}", spec.id);
            for (rule, res) in spec.pricing.iter().zip(resolutions.iter()) {
                assert!(rule.description.starts_with(res), "{}: {} vs {res}", spec.id, rule.description);
                assert_eq!(Some(rule.usd), rate_per_second(spec.id, res), "{} {res}", spec.id);
            }
        }
    }

    #[test]
    fn declared_defaults_are_the_constants_the_adapter_sends() {
        for spec in MODELS {
            assert_eq!(spec.option("duration").unwrap().default, Some(DEFAULT_DURATION));
            assert_eq!(spec.option("resolution").unwrap().default, Some(DEFAULT_RESOLUTION));
            assert_eq!(spec.option("aspect_ratio").unwrap().default, Some(DEFAULT_ASPECT_RATIO));
        }
    }
}
