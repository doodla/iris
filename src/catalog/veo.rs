//! Built-in catalog entries for Veo video generation on the Gemini Developer API
//! (`predictLongRunning`, provider-native asynchronous operations).
//!
//! These values come from verified research against Google's official
//! documentation (docs checked 2026-09-24). Only the three `veo-3.1-*-preview`
//! models remain on the Gemini API; the shut-down `veo-2.0-*`/`veo-3.0-*` ids are
//! deliberately not registered. The summaries follow the Veo 3.1 model page ("best
//! for professional-grade 4K output, natively synchronized audio generation, and
//! complex camera movements"), the Veo guide (Fast versions "optimizing for speed";
//! the parameter table's per-model options, checked 2026-09-25), and the published
//! per-second prices, where Google calls the model Veo 3.1 Standard.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId, format_usd};
use crate::error::IrisError;

use super::options::OptionValue;
use super::types::{
    Constraint, DeclinedName, EstimateInput, Estimator, InputSpec, Lifecycle, Limits, ModelSpec, OptionKind,
    OptionSpec, OutputSpec, PriceRule, RequestRules, RequestSizeLimit, ValidationInput,
};
use super::{CATALOG_AS_OF, round_usd};

/// Pricing page all Veo prices were taken from.
pub const PRICING_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";
/// Provider guide for these models.
pub const DOCS_URL: &str = "https://ai.google.dev/gemini-api/docs/video";

/// Iris default duration, as the `duration` option declares it (a declared default
/// is written as `-O` takes it).
const DURATION_DEFAULT: &str = "8";
/// The same default in seconds. The adapter always sends the effective duration
/// (explicit or this default) so the cost of a job is bounded.
pub const DEFAULT_DURATION: i64 = match i64::from_str_radix(DURATION_DEFAULT, 10) {
    Ok(seconds) => seconds,
    Err(_) => panic!("the declared default duration is a whole number of seconds"),
};
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
    kind: OptionKind::IntegerEnum(&[4, 6, 8]),
    default: Some(DURATION_DEFAULT),
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
    description: "What the video should not contain; sent only when given, and not with reference images",
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
    // No negative_prompt: the Gemini API answers "`negativePrompt` isn't supported
    // by this model" for Veo 3.1 Lite (live request, 2026-09-25), although the SDKs
    // map the field and a cookbook example once used it with Lite.
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

/// The request for the standard output of video generation, one 8-second 720p video
/// ([`StandardOutput`](super::StandardOutput)), which every Veo model supports; either
/// aspect ratio gives it at the same price.
const STANDARD: &[&[(&str, &str)]] = &[&[("duration", "8"), ("resolution", "720p")]];

/// Video names Iris gives no model. Dates and replacements are those of the Gemini
/// deprecations page (<https://ai.google.dev/gemini-api/docs/deprecations>), checked
/// 2026-09-24; the Veo 3.1 `-001` ids (GA, and Lite in preview) are models of Google
/// Cloud's Gemini Enterprise Agent Platform
/// (<https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/veo/3-1-generate>),
/// and Gemini Omni Flash is served by the Interactions API
/// (<https://ai.google.dev/gemini-api/docs/omni>).
pub const DECLINED: &[DeclinedName] = &[
    DeclinedName {
        names: &[],
        families: &["veo-2", "veo-2.0", "veo-3", "veo-3.0"],
        reason: "Google shut down Veo 2.0 and Veo 3.0 on the Gemini API, the last of them on 2026-06-30, \
                 and names Veo 3.1 as the replacement",
        instead: &["veo", "veo-fast", "veo-lite"],
    },
    DeclinedName {
        names: &["veo-3.1-generate-001"],
        families: &[],
        reason: "veo-3.1-generate-001 is a model of Google Cloud's Gemini Enterprise Agent Platform \
                 (Vertex AI), which uses other authentication and endpoints than the Gemini API",
        instead: &["veo"],
    },
    DeclinedName {
        names: &["veo-3.1-fast-generate-001"],
        families: &[],
        reason: "veo-3.1-fast-generate-001 is a model of Google Cloud's Gemini Enterprise Agent Platform \
                 (Vertex AI), which uses other authentication and endpoints than the Gemini API",
        instead: &["veo-fast"],
    },
    DeclinedName {
        names: &["veo-3.1-lite-generate-001"],
        families: &[],
        reason: "veo-3.1-lite-generate-001 is a model of Google Cloud's Gemini Enterprise Agent Platform \
                 (Vertex AI), which uses other authentication and endpoints than the Gemini API",
        instead: &["veo-lite"],
    },
    DeclinedName {
        names: &["omni"],
        families: &["gemini-omni"],
        reason: "Gemini Omni Flash is a video model of the Gemini Interactions API, which Iris does not \
                 implement",
        instead: &["veo", "veo-fast", "veo-lite"],
    },
];

/// Built-in Veo models.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "veo-3.1-fast-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1 Fast",
        summary: "Veo 3.1 optimized for speed: every Veo option Iris offers, 4k and reference images included, \
                  at a lower per-second price than Veo 3.1 Standard",
        aliases: &["veo-fast"],
        lifecycle: Lifecycle::Preview,
        billing: Billing::Paid,
        operations: VIDEO,
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
            NOTE_EXTENSION,
        ],
        docs_url: DOCS_URL,
        validate: Some(FULL_RULES),
        estimate: Some(Estimator { estimate: estimate_video, standard: STANDARD }),
        estimate_usage: None,
    },
    ModelSpec {
        id: "veo-3.1-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1",
        summary: "Veo 3.1 Standard, which Google calls best for professional-grade 4K output and complex camera \
                  movements; every Veo option Iris offers, at the highest per-second price",
        aliases: &["veo"],
        lifecycle: Lifecycle::Preview,
        billing: Billing::Paid,
        operations: VIDEO,
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
            NOTE_EXTENSION,
        ],
        docs_url: DOCS_URL,
        validate: Some(FULL_RULES),
        estimate: Some(Estimator { estimate: estimate_video, standard: STANDARD }),
        estimate_usage: None,
    },
    ModelSpec {
        id: "veo-3.1-lite-generate-preview",
        provider: ProviderId::Gemini,
        display_name: "Veo 3.1 Lite",
        summary: "The lowest-priced Veo model: up to 1080p, with no 4k, no reference images, and no negative \
                  prompt",
        aliases: &["veo-lite"],
        lifecycle: Lifecycle::Preview,
        billing: Billing::Paid,
        operations: VIDEO,
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
        validate: Some(LITE_RULES),
        estimate: Some(Estimator { estimate: estimate_video, standard: STANDARD }),
        estimate_usage: None,
    },
];

/// Effective value of a string option: explicit, else the Veo default.
fn effective<'a>(input: &'a ValidationInput<'_>, name: &str, default: &'a str) -> &'a str {
    input.options.get(name).and_then(|v| v.as_str()).unwrap_or(default)
}

/// Effective duration in seconds: explicit, else the Veo default.
fn effective_duration(input: &ValidationInput<'_>) -> i64 {
    input.options.get("duration").and_then(OptionValue::as_int).unwrap_or(DEFAULT_DURATION)
}

/// Cross-field rules from the provider's parameter table, enforced by [`validate_video`].
pub const HIGH_RESOLUTION_REQUIRES_DURATION_8: Constraint = Constraint {
    id: "high_resolution_requires_duration_8",
    options: &["resolution", "duration"],
    inputs: &[],
    description: "resolution 1080p or 4k requires duration 8 (the default)",
};
pub const REFERENCES_EXCLUDE_FRAMES: Constraint = Constraint {
    id: "references_exclude_frames",
    options: &[],
    inputs: &["reference", "first_frame", "last_frame"],
    description: "reference images cannot be combined with a first or last frame",
};
pub const REFERENCES_REQUIRE_DURATION_8: Constraint = Constraint {
    id: "references_require_duration_8",
    options: &["duration"],
    inputs: &["reference"],
    description: "reference images require duration 8 (the default)",
};
/// Observed live (2026-09-25): Veo 3.1 Fast accepted a negative prompt for
/// text-to-video but refused it next to a reference image ("Negative prompt is
/// not supported in your use case"). The same combination on Veo 3.1 was not
/// tried; it is refused too, since the provider documents no support for it.
pub const NEGATIVE_PROMPT_EXCLUDES_REFERENCES: Constraint = Constraint {
    id: "negative_prompt_excludes_references",
    options: &["negative_prompt"],
    inputs: &["reference"],
    description: "a negative prompt cannot be combined with reference images",
};
pub const LAST_FRAME_REQUIRES_FIRST_FRAME: Constraint = Constraint {
    id: "last_frame_requires_first_frame",
    options: &[],
    inputs: &["last_frame", "first_frame"],
    description: "a last frame requires a first frame (--image)",
};
pub const PERSON_GENERATION_DEPENDS_ON_IMAGE_INPUTS: Constraint = Constraint {
    id: "person_generation_depends_on_image_inputs",
    options: &["person_generation"],
    inputs: &["first_frame", "last_frame", "reference"],
    description: "person_generation=allow_all only without image inputs (text-to-video); allow_adult only \
                  with a first frame, last frame, or reference images",
};

/// Rules of the models that take reference images.
const FULL_RULES: RequestRules = RequestRules {
    constraints: &[
        HIGH_RESOLUTION_REQUIRES_DURATION_8,
        REFERENCES_EXCLUDE_FRAMES,
        REFERENCES_REQUIRE_DURATION_8,
        NEGATIVE_PROMPT_EXCLUDES_REFERENCES,
        LAST_FRAME_REQUIRES_FIRST_FRAME,
        PERSON_GENERATION_DEPENDS_ON_IMAGE_INPUTS,
    ],
    check: validate_video,
};

/// Rules of Veo 3.1 Lite (no reference images, so the reference rules cannot apply).
const LITE_RULES: RequestRules = RequestRules {
    constraints: &[
        HIGH_RESOLUTION_REQUIRES_DURATION_8,
        LAST_FRAME_REQUIRES_FIRST_FRAME,
        PERSON_GENERATION_DEPENDS_ON_IMAGE_INPUTS,
    ],
    check: validate_video,
};

/// Cross-field rules from the provider's parameter table (the constraints above).
fn validate_video(input: &ValidationInput<'_>) -> Result<(), IrisError> {
    let duration = effective_duration(input);
    let resolution = effective(input, "resolution", DEFAULT_RESOLUTION);
    let has_refs = input.reference_images > 0;
    let has_frames = input.has_first_frame || input.has_last_frame;

    if matches!(resolution, "1080p" | "4k") && duration != 8 {
        return Err(HIGH_RESOLUTION_REQUIRES_DURATION_8
            .violation(format!("resolution {resolution} requires --duration 8 (got {duration})"))
            .with_detail("option", "duration"));
    }
    if has_refs && has_frames {
        return Err(REFERENCES_EXCLUDE_FRAMES
            .violation("reference images (--ref) cannot be combined with --image or --last-frame"));
    }
    if has_refs && duration != 8 {
        return Err(REFERENCES_REQUIRE_DURATION_8
            .violation(format!("reference images (--ref) require --duration 8 (got {duration})"))
            .with_detail("option", "duration"));
    }
    if has_refs && input.options.get("negative_prompt").is_some() {
        return Err(NEGATIVE_PROMPT_EXCLUDES_REFERENCES
            .violation("--negative-prompt cannot be combined with reference images (--ref)")
            .with_detail("option", "negative_prompt"));
    }
    if input.has_last_frame && !input.has_first_frame {
        return Err(
            LAST_FRAME_REQUIRES_FIRST_FRAME.violation("--last-frame requires --image (the first frame)")
        );
    }
    if let Some(person) = input.options.get("person_generation").and_then(|v| v.as_str()) {
        let has_images = has_refs || has_frames;
        let message = match person {
            "allow_all" if has_images => Some(
                "person_generation=allow_all is only accepted for text-to-video; use allow_adult with image \
                 inputs",
            ),
            "allow_adult" if !has_images => Some(
                "person_generation=allow_adult is only accepted with image inputs (--image, --last-frame, or \
                 --ref); use allow_all for text-to-video",
            ),
            _ => None,
        };
        if let Some(message) = message {
            return Err(PERSON_GENERATION_DEPENDS_ON_IMAGE_INPUTS
                .violation(message)
                .with_detail("option", "person_generation"));
        }
    }
    Ok(())
}

/// USD per second for a model and resolution, if published.
pub fn rate_per_second(model: &str, resolution: &str) -> Option<f64> {
    RATES.iter().find(|(m, r, _)| *m == model && *r == resolution).map(|(_, _, usd)| *usd)
}

/// Estimate: effective duration × published rate for the effective resolution.
fn estimate_video(spec: &ModelSpec, input: &EstimateInput<'_>) -> Result<CostEstimate, String> {
    let seconds = spec.effective(input.options, "duration").and_then(|v| u32::try_from(v.as_int()?).ok());
    let resolution = spec.effective(input.options, "resolution").and_then(|v| v.as_str().map(str::to_string));
    let rate = resolution.as_deref().and_then(|resolution| rate_per_second(spec.id, resolution));
    let (Some(seconds), Some(resolution), Some(rate)) = (seconds, resolution, rate) else {
        // Unreachable for the catalog: a test checks a rate for every declared resolution.
        return Err(format!("Iris has no published per-second price of {} for this request", spec.id));
    };
    let videos = input.count.max(1);
    let amount = f64::from(seconds) * rate * f64::from(videos);
    Ok(CostEstimate::usd(
        round_usd(amount),
        format!(
            "{seconds} s × {}/s ({}, {resolution}, audio included); estimate; blocked videos are not charged",
            format_usd(rate),
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
            let none = crate::catalog::ResolvedOptions::new();
            assert_eq!(spec.effective(&none, "duration"), Some(OptionValue::Int(DEFAULT_DURATION)));
            assert_eq!(spec.option("resolution").unwrap().default, Some(DEFAULT_RESOLUTION));
            assert_eq!(spec.option("aspect_ratio").unwrap().default, Some(DEFAULT_ASPECT_RATIO));
        }
    }
}
