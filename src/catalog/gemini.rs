//! Built-in catalog entries for Gemini native image generation ("Nano Banana") on
//! the Gemini Developer API (`generateContent`, synchronous).
//!
//! These values come from verified research against Google's official
//! documentation (docs checked 2026-09-24). Where the provider's pages
//! disagree, the conservative value is declared (Flash Lite: 1K only and the ten
//! aspect ratios listed on its model card).

use crate::domain::{CostEstimate, Operation, ProviderId, Usage};

use super::CATALOG_AS_OF;
use super::types::{
    EstimateInput, InputSpec, Lifecycle, Limits, ModelSpec, OptionKind, OptionSpec, OutputSpec, PriceRule,
};

/// Pricing page all Gemini image prices were taken from.
pub const PRICING_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";
/// Provider guide for these models.
pub const DOCS_URL: &str = "https://ai.google.dev/gemini-api/docs/image-generation";

/// Adapter-enforced cap on the whole encoded `generateContent` request (prompt plus
/// base64 reference images). The provider documents "20MB" for inline data; Iris
/// reads that conservatively as 20,000,000 bytes.
pub const MAX_REQUEST_BYTES: usize = 20_000_000;

const BOTH: &[Operation] = &[Operation::ImageGenerate, Operation::ImageEdit];

const INPUT_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/heic", "image/heif"];

const INPUTS: InputSpec = InputSpec {
    max_input_images: 14,
    input_media_types: INPUT_TYPES,
    max_input_bytes: 14_000_000,
    mask: false,
    first_frame: false,
    last_frame: false,
    max_reference_images: 0,
};

/// The provider chooses the format. JPEG comes first because live runs of
/// `gemini-3.1-flash-image` returned JPEG for both generate and edit, so default
/// file names get the right extension; PNG outputs are still saved (as `.png`).
const OUTPUTS: OutputSpec = OutputSpec { media_types: &["image/jpeg", "image/png"], max_count: 1 };

const LIMITS: Limits = Limits { max_prompt_chars: None };

const NOTE_BILLING: &str = "No free tier for image models: billing (Prepay) required";
const NOTE_AUTH_KEY: &str = "Standard (legacy) API keys are rejected from September 2026; use an auth key";

const FLASH_ASPECT_RATIOS: &[&str] =
    &["1:1", "1:4", "1:8", "2:3", "3:2", "3:4", "4:1", "4:3", "4:5", "5:4", "8:1", "9:16", "16:9", "21:9"];
const TEN_ASPECT_RATIOS: &[&str] = &["1:1", "2:3", "3:2", "3:4", "4:3", "4:5", "5:4", "9:16", "16:9", "21:9"];

const COUNT: OptionSpec = OptionSpec {
    name: "count",
    kind: OptionKind::Integer { min: 1, max: 1 },
    default: Some("1"),
    flag: Some("--count"),
    operations: BOTH,
    description: "Number of images. Gemini image models return one image per request; the value is \
                  validated locally and never sent",
};

const fn aspect_ratio(values: &'static [&'static str]) -> OptionSpec {
    OptionSpec {
        name: "aspect_ratio",
        kind: OptionKind::Enum(values),
        default: None,
        flag: Some("--aspect-ratio"),
        operations: BOTH,
        description: "Aspect ratio of the generated image. When omitted, the model matches the input \
                      image (edit) or uses 1:1",
    }
}

const fn resolution(values: &'static [&'static str], description: &'static str) -> OptionSpec {
    OptionSpec {
        name: "resolution",
        kind: OptionKind::Enum(values),
        default: Some("1K"),
        flag: Some("--resolution"),
        operations: BOTH,
        description,
    }
}

const THINKING_LEVEL: OptionSpec = OptionSpec {
    name: "thinking_level",
    kind: OptionKind::Enum(&["minimal", "high"]),
    default: Some("minimal"),
    flag: None,
    operations: BOTH,
    description: "How much the model reasons before drawing. Thinking is always on and its tokens are \
                  billed; `high` may help complex compositions at a higher cost",
};

const FLASH_OPTIONS: &[OptionSpec] = &[
    COUNT,
    aspect_ratio(FLASH_ASPECT_RATIOS),
    resolution(
        &["512", "1K", "2K", "4K"],
        "Output size class: 512, 1K (about 1024 px), 2K, or 4K. The price depends on it",
    ),
    THINKING_LEVEL,
];

const LITE_OPTIONS: &[OptionSpec] = &[
    COUNT,
    aspect_ratio(TEN_ASPECT_RATIOS),
    resolution(&["1K"], "Output size class; this model generates 1K images only"),
    THINKING_LEVEL,
];

const PRO_OPTIONS: &[OptionSpec] = &[
    COUNT,
    aspect_ratio(TEN_ASPECT_RATIOS),
    resolution(&["1K", "2K", "4K"], "Output size class: 1K, 2K, or 4K. The price depends on it"),
];

/// Published standard-tier rates of one image model.
#[derive(Debug, Clone, Copy)]
struct Rates {
    model: &'static str,
    /// (resolution, USD per generated image).
    per_image: &'static [(&'static str, f64)],
    /// USD per 1M input tokens (text and image).
    input_per_m: f64,
    /// USD per 1M text and thinking output tokens.
    text_output_per_m: f64,
    /// USD per 1M image output tokens.
    image_output_per_m: f64,
}

const RATES: &[Rates] = &[
    Rates {
        model: "gemini-3.1-flash-image",
        per_image: &[("512", 0.045), ("1K", 0.067), ("2K", 0.101), ("4K", 0.151)],
        input_per_m: 0.50,
        text_output_per_m: 3.00,
        image_output_per_m: 60.00,
    },
    Rates {
        model: "gemini-3.1-flash-lite-image",
        per_image: &[("1K", 0.0336)],
        input_per_m: 0.25,
        text_output_per_m: 1.50,
        image_output_per_m: 30.00,
    },
    Rates {
        model: "gemini-3-pro-image",
        per_image: &[("1K", 0.134), ("2K", 0.134), ("4K", 0.24)],
        input_per_m: 2.00,
        text_output_per_m: 12.00,
        image_output_per_m: 120.00,
    },
];

const fn price(description: &'static str, unit: &'static str, usd: f64) -> PriceRule {
    PriceRule { description, unit, usd, source_url: PRICING_URL, as_of: CATALOG_AS_OF }
}

const FLASH_PRICING: &[PriceRule] = &[
    price("Generated image, 512 (747 output tokens)", "image", 0.045),
    price("Generated image, 1K (1120 output tokens)", "image", 0.067),
    price("Generated image, 2K (1680 output tokens)", "image", 0.101),
    price("Generated image, 4K (2520 output tokens)", "image", 0.151),
    price("Image output tokens", "1M tokens", 60.00),
    price("Text and image input tokens", "1M tokens", 0.50),
    price("Text and thinking output tokens", "1M tokens", 3.00),
];

const LITE_PRICING: &[PriceRule] = &[
    price("Generated image, 1K (1120 output tokens)", "image", 0.0336),
    price("Image output tokens", "1M tokens", 30.00),
    price("Text and image input tokens", "1M tokens", 0.25),
    price("Text and thinking output tokens", "1M tokens", 1.50),
];

const PRO_PRICING: &[PriceRule] = &[
    price("Generated image, 1K or 2K (1120 output tokens)", "image", 0.134),
    price("Generated image, 4K (2000 output tokens)", "image", 0.24),
    price("Image output tokens", "1M tokens", 120.00),
    price("Text and image input tokens (an input image is 560 tokens)", "1M tokens", 2.00),
    price("Text and thinking output tokens", "1M tokens", 12.00),
];

/// Built-in Gemini image models.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-3.1-flash-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana 2 (Gemini 3.1 Flash Image)",
        aliases: &["nano-banana-2", "nano-banana"],
        lifecycle: Lifecycle::Ga,
        operations: BOTH,
        default_for: BOTH,
        inputs: INPUTS,
        options: FLASH_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: FLASH_PRICING,
        access_notes: &[NOTE_BILLING, NOTE_AUTH_KEY],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(estimate_image),
        estimate_usage: Some(estimate_from_usage),
    },
    ModelSpec {
        id: "gemini-3.1-flash-lite-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana 2 Lite (Gemini 3.1 Flash Lite Image)",
        aliases: &["nano-banana-2-lite"],
        lifecycle: Lifecycle::Ga,
        operations: BOTH,
        default_for: &[],
        inputs: INPUTS,
        options: LITE_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: LITE_PRICING,
        access_notes: &[
            NOTE_BILLING,
            NOTE_AUTH_KEY,
            "Provider note: not optimized for multiple reference images or multi-turn editing",
        ],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(estimate_image),
        estimate_usage: Some(estimate_from_usage),
    },
    ModelSpec {
        id: "gemini-3-pro-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana Pro (Gemini 3 Pro Image)",
        aliases: &["nano-banana-pro"],
        lifecycle: Lifecycle::Ga,
        operations: BOTH,
        default_for: &[],
        inputs: INPUTS,
        options: PRO_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: PRO_PRICING,
        access_notes: &[NOTE_BILLING, NOTE_AUTH_KEY],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(estimate_image),
        estimate_usage: Some(estimate_from_usage),
    },
];

fn rates_for(model: &str) -> Option<&'static Rates> {
    RATES.iter().find(|r| r.model == model)
}

/// Round to a millionth of a dollar so estimates print cleanly.
fn round_usd(amount: f64) -> f64 {
    (amount * 1e6).round() / 1e6
}

/// Pre-call estimate: the published per-image price for the effective resolution,
/// times the number of images. Input and thinking tokens are not included (the
/// basis says so), because they are only known after the call.
fn estimate_image(spec: &ModelSpec, input: &EstimateInput<'_>) -> Option<CostEstimate> {
    let rates = rates_for(spec.id)?;
    let resolution = spec.effective(input.options, "resolution")?;
    let resolution = resolution.as_str()?;
    let (_, per_image) = rates.per_image.iter().find(|(r, _)| *r == resolution)?;
    let count = input.count.max(1);
    Some(CostEstimate::usd(
        round_usd(f64::from(count) * per_image),
        format!(
            "{count} image × ${per_image} ({}, {resolution}); input and thinking tokens not included",
            spec.id
        ),
        PRICING_URL,
        CATALOG_AS_OF,
    ))
}

/// Post-call estimate from the `usageMetadata` the Gemini adapter stores in
/// [`Usage::provider_usage`]: prompt tokens at the input rate, image
/// output tokens at the image rate, and the remaining output plus thinking tokens at
/// the text rate. When the response does not itemize image tokens, every candidate
/// token is priced at the (higher) image rate, and the basis says so.
///
/// Returns `None` for models without a rate table or when the usage carries no
/// token counts.
pub fn estimate_from_usage(spec: &ModelSpec, usage: &Usage) -> Option<CostEstimate> {
    let rates = rates_for(spec.id)?;
    let meta = usage.provider_usage.as_ref()?.as_object()?;
    let count = |key: &str| meta.get(key).and_then(serde_json::Value::as_u64);
    let prompt = count("promptTokenCount").unwrap_or(0);
    let candidates = count("candidatesTokenCount").unwrap_or(0);
    let thoughts = count("thoughtsTokenCount").unwrap_or(0);
    if prompt == 0 && candidates == 0 && thoughts == 0 {
        return None;
    }
    let itemized_image = meta.get("candidatesTokensDetails").and_then(|d| d.as_array()).map(|details| {
        details
            .iter()
            .filter(|d| d.get("modality").and_then(|m| m.as_str()) == Some("IMAGE"))
            .filter_map(|d| d.get("tokenCount").and_then(serde_json::Value::as_u64))
            .sum::<u64>()
    });
    let (image_tokens, note) = match itemized_image {
        Some(n) => (n.min(candidates), ""),
        None => (candidates, "; image tokens not itemized, all output priced as image"),
    };
    let text_tokens = candidates - image_tokens + thoughts;
    let per_token = |tokens: u64, per_m: f64| tokens as f64 * per_m / 1_000_000.0;
    let amount = per_token(prompt, rates.input_per_m)
        + per_token(image_tokens, rates.image_output_per_m)
        + per_token(text_tokens, rates.text_output_per_m);
    Some(CostEstimate::usd(
        round_usd(amount),
        format!(
            "{prompt} input tokens × ${}/1M + {image_tokens} image output tokens × ${}/1M + \
             {text_tokens} text and thinking tokens × ${}/1M ({}, from reported usage{note})",
            rates.input_per_m, rates.image_output_per_m, rates.text_output_per_m, spec.id
        ),
        PRICING_URL,
        CATALOG_AS_OF,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rate_table_matches_the_published_price_rules_and_resolutions() {
        for rates in RATES {
            let spec = MODELS.iter().find(|m| m.id == rates.model).expect("rates for a catalog model");
            let published =
                |unit: &str, usd: f64| spec.pricing.iter().any(|p| p.unit == unit && p.usd == usd);
            for (_, usd) in rates.per_image {
                assert!(published("image", *usd), "{}: per-image rate {usd} not published", spec.id);
            }
            assert!(published("1M tokens", rates.input_per_m), "{}", spec.id);
            assert!(published("1M tokens", rates.text_output_per_m), "{}", spec.id);
            assert!(published("1M tokens", rates.image_output_per_m), "{}", spec.id);
            let OptionKind::Enum(resolutions) = spec.option("resolution").unwrap().kind else {
                panic!("resolution is an enum");
            };
            for r in resolutions {
                assert!(rates.per_image.iter().any(|(res, _)| res == r), "{}: no price for {r}", spec.id);
            }
        }
        assert_eq!(RATES.len(), MODELS.len());
    }
}
