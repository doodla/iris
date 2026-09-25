//! Built-in catalog entries for the OpenAI Images API models.
//!
//! Values come from OpenAI's official documentation, checked 2026-09-24: the image
//! generation guide, the Images API reference and OpenAPI spec, the model pages, and
//! the pricing page:
//!
//! * models: `gpt-image-2.5-sunburst`, `gpt-image-2.5-flare`, `gpt-image-2`; their
//!   dated default snapshots are accepted as aliases. Deprecated `gpt-image-1*`,
//!   `chatgpt-image-latest`, and the removed `dall-e-*` models are deliberately not
//!   registered.
//! * options and their wire names (the adapter maps exactly these, see
//!   `providers::openai`): `count`→`n`, `size`, `quality`, `format`→`output_format`,
//!   `compression`→`output_compression`, `background`, `moderation`.
//! * prices: token rates per 1M tokens, identical for all three models.
//! * estimates: the output-token formula of the calculator on OpenAI's image
//!   generation guide. Billing uses the response's `usage`; see [`cost_from_usage`].
//! * summaries: the image generation guide's choice between the 2.5 models (Sunburst
//!   "for workflows where editing precision matters most", Flare "for fast,
//!   high-quality everyday image generation") and their model pages (Sunburst is
//!   "our most capable model", Flare "our fastest model"). For GPT Image 2 the guide
//!   says "For new integrations, use one of the GPT Image 2.5 models", so its summary
//!   gives that advice and the documented differences.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId, Usage};
use crate::error::IrisError;

use super::options::OptionValue;
use super::round_usd;
use super::types::{
    Constraint, EstimateInput, Estimator, InputSpec, Lifecycle, Limits, MaskSpec, ModelIdSyntax, ModelSpec,
    OptionKind, OptionSpec, OutputSpec, PriceRule, RequestRules, ValidationInput,
};

/// Official pricing page the rates below come from.
pub const PRICING_URL: &str = "https://developers.openai.com/api/docs/pricing";
/// Date the prices and the calculator formula were checked.
pub const PRICING_AS_OF: &str = "2026-09-24";
/// Image generation guide (capabilities, sizes, masks, errors).
pub const DOCS_URL: &str = "https://developers.openai.com/api/docs/guides/image-generation";

/// USD per 1M text input tokens (all three models).
pub const TEXT_INPUT_USD_PER_M: f64 = 5.00;
/// USD per 1M image input tokens (all three models).
pub const IMAGE_INPUT_USD_PER_M: f64 = 8.00;
/// USD per 1M image output tokens (all three models).
pub const IMAGE_OUTPUT_USD_PER_M: f64 = 30.00;

/// Both edges must be multiples of this (gpt-image-2 and the 2.5 models, per the image
/// generation guide and API reference; the same limits are hard-coded in OpenAI's
/// calculator).
pub const SIZE_MULTIPLE: u64 = 16;
/// Longest allowed edge in pixels.
pub const SIZE_MAX_EDGE: u64 = 3840;
/// Largest allowed long:short ratio.
pub const SIZE_MAX_RATIO: u64 = 3;
/// Smallest allowed total pixel count.
pub const SIZE_MIN_PIXELS: u64 = 655_360;
/// Largest allowed total pixel count.
pub const SIZE_MAX_PIXELS: u64 = 8_294_400;

/// Model ids the OpenAI adapter can send for a model Iris does not know: the id
/// travels in the JSON body (and percent-encoded in the metadata URL), so the
/// OpenAI id alphabet plus the separators of fine-tuned and snapshot ids is allowed.
pub const MODEL_ID_SYNTAX: ModelIdSyntax = ModelIdSyntax {
    max_len: 200,
    punctuation: "-._/:@",
    alphanumeric_start: false,
    description: "letters, digits, '-', '.', '_', '/', ':', and '@' only, at most 200 characters",
};

const BOTH: &[Operation] = &[Operation::ImageGenerate, Operation::ImageEdit];

const SIZE_SYNTAX: &str =
    "auto or WxH: multiples of 16, max edge 3840, long:short ≤ 3:1, 655,360–8,294,400 pixels";

/// Largest mask accepted: "less than 4MB" in the Images API reference, read as
/// 4,000,000 bytes (the stricter reading, like the other decimal byte limits).
pub const MASK_MAX_BYTES: u64 = 4_000_000;

const INPUTS: InputSpec = InputSpec {
    max_input_images: 16,
    input_media_types: &["image/png", "image/jpeg", "image/webp"],
    // The JSON edit body carries each image as a data URL capped at 20,971,520
    // characters (OpenAPI `ImageRefParam`), which bounds the binary size at about 15.7 MB.
    max_input_bytes: 15_700_000,
    // Images API reference and image generation guide: the mask is a PNG with an
    // alpha channel (fully transparent areas are edited), under 4 MB, with the same
    // dimensions as the first image, which it applies to.
    mask: Some(MaskSpec {
        media_types: &["image/png"],
        max_bytes: MASK_MAX_BYTES,
        requires_alpha: true,
        same_size_as_first_image: true,
    }),
    first_frame: false,
    last_frame: false,
    max_reference_images: 0,
    // No documented cap on the whole JSON edit body beyond the per-image data URLs.
    max_request: None,
};

const OUTPUTS: OutputSpec =
    OutputSpec { media_types: &["image/png", "image/jpeg", "image/webp"], max_count: 10 };

const LIMITS: Limits = Limits { max_prompt_chars: Some(32_000) };

const PRICING: &[PriceRule] = &[
    PriceRule {
        description: "Text input tokens (prompt)",
        unit: "1M text input tokens",
        usd: TEXT_INPUT_USD_PER_M,
        source_url: PRICING_URL,
        as_of: PRICING_AS_OF,
    },
    PriceRule {
        description: "Image input tokens (edit input images and mask)",
        unit: "1M image input tokens",
        usd: IMAGE_INPUT_USD_PER_M,
        source_url: PRICING_URL,
        as_of: PRICING_AS_OF,
    },
    PriceRule {
        description: "Image output tokens (generated images)",
        unit: "1M image output tokens",
        usd: IMAGE_OUTPUT_USD_PER_M,
        source_url: PRICING_URL,
        as_of: PRICING_AS_OF,
    },
];

const ACCESS_NOTES: &[&str] = &[
    "API Organization Verification may be required for GPT Image models",
    "Paid usage tier required (no free-tier limits listed)",
];

const COUNT: OptionSpec = OptionSpec {
    name: "count",
    kind: OptionKind::Integer { min: 1, max: 10 },
    default: Some("1"),
    flag: Some("--count"),
    operations: BOTH,
    description: "Number of images to produce in one request (sent as `n`).",
};

const SIZE: OptionSpec = OptionSpec {
    name: "size",
    kind: OptionKind::Pattern { syntax: SIZE_SYNTAX, validate: validate_size },
    default: Some("auto"),
    flag: Some("--size"),
    operations: BOTH,
    description: "Output size: `auto` (the model chooses; no pre-call cost estimate) or WIDTHxHEIGHT \
                  with both edges multiples of 16, the longer edge at most 3840, an aspect ratio of at \
                  most 3:1, and 655,360 to 8,294,400 pixels. Sizes above 2560x1440 are experimental. \
                  1024x1024, 1536x1024 and 1024x1536 are the recommended sizes. By OpenAI's published \
                  calculator formula a non-square size never needs more output tokens than a square one with \
                  the same number of pixels, so a larger non-square size can cost less than a smaller square \
                  one (low: 1536x1024 is 158 tokens, 1024x1024 is 196).",
};

const QUALITY_2_5: OptionSpec = OptionSpec {
    name: "quality",
    kind: OptionKind::Enum(&["low", "medium", "high", "xhigh", "max", "auto"]),
    default: Some("auto"),
    flag: Some("--quality"),
    operations: BOTH,
    description: "Rendering quality. Higher quality produces more output tokens and costs more; \
                  `auto` lets the model choose (no pre-call cost estimate).",
};

const QUALITY_2: OptionSpec = OptionSpec {
    name: "quality",
    kind: OptionKind::Enum(&["low", "medium", "high", "auto"]),
    default: Some("auto"),
    flag: Some("--quality"),
    operations: BOTH,
    description: "Rendering quality. Higher quality produces more output tokens and costs more; \
                  `auto` lets the model choose (no pre-call cost estimate).",
};

const FORMAT: OptionSpec = OptionSpec {
    name: "format",
    kind: OptionKind::Enum(&["png", "jpeg", "webp"]),
    default: Some("png"),
    flag: Some("--format"),
    operations: BOTH,
    description: "Output image format (sent as `output_format`).",
};

const COMPRESSION: OptionSpec = OptionSpec {
    name: "compression",
    kind: OptionKind::Integer { min: 0, max: 100 },
    default: Some("100"),
    flag: None,
    operations: BOTH,
    description: "Compression level 0-100 (sent as `output_compression`); only with jpeg or webp output.",
};

const BACKGROUND_2_5: OptionSpec = OptionSpec {
    name: "background",
    kind: OptionKind::Enum(&["transparent", "opaque", "auto"]),
    default: Some("auto"),
    flag: None,
    operations: BOTH,
    description: "Background: `transparent` (requires png or webp output), `opaque`, or `auto`.",
};

const BACKGROUND_2: OptionSpec = OptionSpec {
    name: "background",
    kind: OptionKind::Enum(&["transparent", "opaque", "auto"]),
    default: Some("auto"),
    flag: None,
    operations: BOTH,
    description: "Background: `transparent` (requires png or webp output; in preview on gpt-image-2), \
                  `opaque`, or `auto`.",
};

const MODERATION: OptionSpec = OptionSpec {
    name: "moderation",
    kind: OptionKind::Enum(&["low", "auto"]),
    default: Some("auto"),
    flag: None,
    operations: BOTH,
    description: "Content-moderation strictness: `auto` or `low` (less restrictive filtering).",
};

const OPTIONS_2_5: &[OptionSpec] =
    &[COUNT, SIZE, QUALITY_2_5, FORMAT, COMPRESSION, BACKGROUND_2_5, MODERATION];
const OPTIONS_2: &[OptionSpec] = &[COUNT, SIZE, QUALITY_2, FORMAT, COMPRESSION, BACKGROUND_2, MODERATION];

/// The cheapest single-output request of every GPT Image model: `low` quality at
/// 1440x480. The calculator formula scales the quality's base down by the aspect
/// ratio, so a size near 3:1 with few pixels needs the fewest output tokens (54 at
/// low, where 1024x1024 needs 196); 1408x480 and 1424x480 need as many for fewer
/// pixels. `tests/openai_catalog.rs` checks it against every valid size.
const LOWEST: &[(&str, &str)] = &[("quality", "low"), ("size", "1440x480")];

/// The OpenAI models Iris knows.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-image-2.5-sunburst",
        provider: ProviderId::OpenAi,
        display_name: "GPT Image 2.5 Sunburst",
        summary: "OpenAI's most capable image model, for workflows where editing precision matters most",
        aliases: &["gpt-image-2.5-sunburst-2026-09-08"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: OPTIONS_2_5,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: PRICING,
        access_notes: ACCESS_NOTES,
        docs_url: DOCS_URL,
        validate: Some(RULES),
        estimate: Some(Estimator { estimate: estimate_gpt_image_2_5, lowest: LOWEST }),
        estimate_usage: Some(cost_from_usage),
    },
    ModelSpec {
        id: "gpt-image-2.5-flare",
        provider: ProviderId::OpenAi,
        display_name: "GPT Image 2.5 Flare",
        summary: "OpenAI's fastest image model, for fast, high-quality everyday generation, at the same token \
                  rates as Sunburst",
        aliases: &["gpt-image-2.5-flare-2026-09-08"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: OPTIONS_2_5,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: PRICING,
        access_notes: ACCESS_NOTES,
        docs_url: DOCS_URL,
        validate: Some(RULES),
        estimate: Some(Estimator { estimate: estimate_gpt_image_2_5, lowest: LOWEST }),
        estimate_usage: Some(cost_from_usage),
    },
    ModelSpec {
        id: "gpt-image-2",
        provider: ProviderId::OpenAi,
        display_name: "GPT Image 2",
        summary: "The earlier GPT Image model; OpenAI says to use a 2.5 model for new integrations. Quality up to \
                  high, and at medium and high about 4x the 2.5 models' output tokens (OpenAI's calculator, \
                  indicative for 2.5)",
        aliases: &["gpt-image-2-2026-04-21"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: OPTIONS_2,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: PRICING,
        access_notes: ACCESS_NOTES,
        docs_url: DOCS_URL,
        validate: Some(RULES),
        estimate: Some(Estimator { estimate: estimate_gpt_image_2, lowest: LOWEST }),
        estimate_usage: Some(cost_from_usage),
    },
];

/// Parse a `WIDTHxHEIGHT` size (lowercase `x`, plain decimal digits without leading
/// zeros or signs). Returns `None` for anything else, including `auto`.
pub fn parse_size(raw: &str) -> Option<(u64, u64)> {
    let (w, h) = raw.split_once('x')?;
    let digits = |s: &str| {
        !s.is_empty() && s.len() <= 5 && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit())
    };
    if !digits(w) || !digits(h) {
        return None;
    }
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Validator of the `size` option: `auto`, or `WxH` within the
/// documented limits of gpt-image-2 and the 2.5 models.
pub fn validate_size(raw: &str) -> Result<(), String> {
    if raw == "auto" {
        return Ok(());
    }
    let Some((w, h)) = parse_size(raw) else {
        return Err(format!("'{raw}' is neither `auto` nor WIDTHxHEIGHT such as 1024x1024"));
    };
    if w % SIZE_MULTIPLE != 0 || h % SIZE_MULTIPLE != 0 {
        return Err(format!("width and height must both be multiples of {SIZE_MULTIPLE}"));
    }
    let (long, short) = (w.max(h), w.min(h));
    if long > SIZE_MAX_EDGE {
        return Err(format!("the longer edge must be at most {SIZE_MAX_EDGE} pixels"));
    }
    if long > SIZE_MAX_RATIO * short {
        return Err(format!("the aspect ratio (long:short) must be at most {SIZE_MAX_RATIO}:1"));
    }
    let pixels = w * h;
    if !(SIZE_MIN_PIXELS..=SIZE_MAX_PIXELS).contains(&pixels) {
        return Err(format!("{w}x{h} has {pixels} pixels; the total must be between 655,360 and 8,294,400"));
    }
    Ok(())
}

/// `output_compression` is documented for jpeg and webp output only.
pub const COMPRESSION_REQUIRES_JPEG_OR_WEBP: Constraint = Constraint {
    id: "compression_requires_jpeg_or_webp",
    options: &["compression", "format"],
    inputs: &[],
    description: "compression applies only to jpeg or webp output (the default format is png)",
};

/// A transparent background is documented for png and webp output only.
pub const TRANSPARENT_BACKGROUND_REQUIRES_PNG_OR_WEBP: Constraint = Constraint {
    id: "transparent_background_requires_png_or_webp",
    options: &["background", "format"],
    inputs: &[],
    description: "background=transparent requires png or webp output",
};

/// The cross-option rules of the GPT Image models.
const RULES: RequestRules = RequestRules {
    constraints: &[COMPRESSION_REQUIRES_JPEG_OR_WEBP, TRANSPARENT_BACKGROUND_REQUIRES_PNG_OR_WEBP],
    check: validate_options,
};

/// Cross-option rules one option spec cannot express: `compression` needs jpeg or
/// webp output, and a transparent background needs png or webp output. The
/// effective format is the explicit one, else the provider default `png`.
pub fn validate_options(input: &ValidationInput<'_>) -> Result<(), IrisError> {
    let format = input.options.get("format").and_then(OptionValue::as_str).unwrap_or("png");
    if input.options.contains("compression") && format == "png" {
        return Err(COMPRESSION_REQUIRES_JPEG_OR_WEBP
            .violation("-O compression applies only to jpeg or webp output, and the output format is png")
            .with_hint("add --format jpeg or --format webp, or remove -O compression")
            .with_detail("option", "compression"));
    }
    let background = input.options.get("background").and_then(OptionValue::as_str);
    if background == Some("transparent") && format == "jpeg" {
        return Err(TRANSPARENT_BACKGROUND_REQUIRES_PNG_OR_WEBP
            .violation("-O background=transparent requires png or webp output, not jpeg")
            .with_hint("use --format png or --format webp, or choose another background")
            .with_detail("option", "background"));
    }
    Ok(())
}

/// Per-quality base values of OpenAI's calculator for GPT Image 2.5 (Sunburst and Flare).
pub const CALCULATOR_BASE_2_5: &[(&str, u32)] =
    &[("low", 16), ("medium", 24), ("high", 48), ("xhigh", 64), ("max", 96)];
/// Per-quality base values of OpenAI's calculator for GPT Image 2.
pub const CALCULATOR_BASE_2: &[(&str, u32)] = &[("low", 16), ("medium", 48), ("high", 96)];

/// Estimated output tokens of one image by the formula of OpenAI's docs-site
/// calculator (`GptImageTokenCalculator`, loaded by the image generation guide),
/// with the same floating-point steps as the published JavaScript:
/// `s = base / (long / short)`, `u` = `s` rounded to the nearest integer with exact
/// halves going to the even neighbor, `tokens = ceil(base * u * (2e6 + w*h) / 4e6)`.
pub fn estimated_output_tokens(base: u32, width: u64, height: u64) -> u64 {
    let (long, short) = (width.max(height) as f64, width.min(height).max(1) as f64);
    let base = f64::from(base);
    let s = base / (long / short);
    let floor = s.floor();
    // JS: `s - l === .5 ? l + l % 2 : Math.round(s)`, where Math.round(x) = floor(x + 0.5).
    let u = if s - floor == 0.5 { floor + floor % 2.0 } else { (s + 0.5).floor() };
    (base * u * (2e6 + width as f64 * height as f64) / 4e6).ceil() as u64
}

/// The calculator base for `quality`, if the table has one (`auto` has none).
pub fn calculator_base(table: &[(&str, u32)], quality: &str) -> Option<u32> {
    table.iter().find(|(q, _)| *q == quality).map(|(_, b)| *b)
}

fn estimate_with(
    table: &[(&str, u32)],
    note: &str,
    spec: &ModelSpec,
    input: &EstimateInput<'_>,
) -> Result<CostEstimate, String> {
    let value = |name: &str| spec.effective(input.options, name).and_then(|v| v.as_str().map(str::to_string));
    let (quality, size) = (value("quality").unwrap_or_default(), value("size").unwrap_or_default());
    // `auto` quality or size: the cost depends on the model's choice, so there is no
    // point estimate.
    let base = calculator_base(table, &quality);
    let dimensions = parse_size(&size);
    let (Some(base), Some((w, h))) = (base, dimensions) else {
        return Err(chosen_by_the_model(spec, table, base.is_none(), dimensions.is_none()));
    };
    let tokens = estimated_output_tokens(base, w, h);
    let count = input.count.max(1);
    let amount = round_usd(tokens as f64 * f64::from(count) * IMAGE_OUTPUT_USD_PER_M / 1e6);
    let noun = if count == 1 { "image" } else { "images" };
    Ok(CostEstimate::usd(
        amount,
        format!(
            "estimate: {count} {noun} × {tokens} output tokens × ${IMAGE_OUTPUT_USD_PER_M:.2}/1M ({}, {quality}, \
             {size}); OpenAI calculator formula{note}; prompt and input-image tokens not included",
            spec.id
        ),
        PRICING_URL,
        PRICING_AS_OF,
    ))
}

/// Why there is no pre-call estimate when the model chooses the quality or the size
/// (`auto`), naming only the options to pass for one.
fn chosen_by_the_model(spec: &ModelSpec, table: &[(&str, u32)], quality: bool, size: bool) -> String {
    let flag =
        |name: &str| spec.option(name).and_then(|o| o.flag).map_or(format!("-O {name}=…"), str::to_string);
    let qualities: Vec<&str> = table.iter().map(|(q, _)| *q).collect();
    let (last, rest) = qualities.split_last().unwrap_or((&"", &[]));
    let pass_quality = format!("{} ({}, or {last})", flag("quality"), rest.join(", "));
    let pass_size = format!("{} WIDTHxHEIGHT (such as 1024x1024)", flag("size"));
    let (what, pass) = match (quality, size) {
        (true, true) => (
            "quality and size are auto, so the model chooses them",
            format!("{pass_quality} and {pass_size}"),
        ),
        (true, false) => ("quality is auto, so the model chooses it", pass_quality),
        (false, _) => ("size is auto, so the model chooses it", pass_size),
    };
    format!("{what} and the cost is unknown before the call; pass {pass} for an estimate")
}

/// Pre-call estimate for the GPT Image 2.5 models (Sunburst and Flare share it), or,
/// when the effective quality or size is `auto`, why there is none.
pub fn estimate_gpt_image_2_5(spec: &ModelSpec, input: &EstimateInput<'_>) -> Result<CostEstimate, String> {
    estimate_with(CALCULATOR_BASE_2_5, " (indicative for GPT Image 2.5)", spec, input)
}

/// Pre-call estimate for GPT Image 2, or, when the effective quality or size is
/// `auto`, why there is none.
pub fn estimate_gpt_image_2(spec: &ModelSpec, input: &EstimateInput<'_>) -> Result<CostEstimate, String> {
    estimate_with(CALCULATOR_BASE_2, "", spec, input)
}

/// Post-call estimate from the `usage` an OpenAI image response reported:
/// `(text_tokens × $5 + image_tokens × $8 + output_tokens × $30) / 1M`, with the input
/// split read from `usage.input_tokens_details` (kept in `provider_usage` by the
/// adapter).
///
/// The usage object covers the whole response (all `n` images), so the result is not
/// multiplied by the image count. Without an input split, all input tokens are priced
/// at the higher image-input rate (an upper bound, stated in the basis). The Images
/// API reports no cached-input split, so discounts are not applied. Returns `None`
/// without `output_tokens`, or for a model of another provider.
pub fn cost_from_usage(spec: &ModelSpec, usage: &Usage) -> Option<CostEstimate> {
    if spec.provider != ProviderId::OpenAi {
        return None;
    }
    let output = usage.output_tokens?;
    let details = usage.provider_usage.as_ref().and_then(|u| u.get("input_tokens_details"));
    let detail = |key: &str| details.and_then(|d| d.get(key)).and_then(serde_json::Value::as_u64);
    let (text, image, split) = match (detail("text_tokens"), detail("image_tokens")) {
        (None, None) => (0, usage.input_tokens.unwrap_or(0), false),
        (text, image) => (text.unwrap_or(0), image.unwrap_or(0), true),
    };
    let amount = round_usd(
        (text as f64 * TEXT_INPUT_USD_PER_M
            + image as f64 * IMAGE_INPUT_USD_PER_M
            + output as f64 * IMAGE_OUTPUT_USD_PER_M)
            / 1e6,
    );
    let input_part = if split {
        format!(
            "{text} text input tokens × ${TEXT_INPUT_USD_PER_M:.2}/1M + {image} image input tokens × \
             ${IMAGE_INPUT_USD_PER_M:.2}/1M"
        )
    } else {
        format!(
            "{image} input tokens × ${IMAGE_INPUT_USD_PER_M:.2}/1M (no text/image split reported; upper bound)"
        )
    };
    Some(CostEstimate::usd(
        amount,
        format!(
            "estimate from reported usage ({}): {input_part} + {output} output tokens × \
             ${IMAGE_OUTPUT_USD_PER_M:.2}/1M; cached-input discounts not reported",
            spec.id
        ),
        PRICING_URL,
        PRICING_AS_OF,
    ))
}
