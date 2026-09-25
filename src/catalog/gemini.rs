//! Built-in catalog entries for Gemini native image generation ("Nano Banana") on
//! the Gemini Developer API (`generateContent`, synchronous).
//!
//! These values come from verified research against Google's official
//! documentation (docs checked 2026-09-24). Where the provider's pages
//! disagree, the conservative value is declared (Flash Lite: 1K only and the ten
//! aspect ratios listed on its model card). The summaries follow the image
//! generation guide's description of each model (checked 2026-09-25) and the
//! published per-image prices.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId, Usage, format_usd};

use super::types::{
    DeclinedName, EstimateInput, Estimator, InputSpec, Lifecycle, Limits, ModelIdSyntax, ModelSpec,
    OptionKind, OptionSpec, OutputSpec, PriceRule, RequestSizeLimit,
};
use super::{CATALOG_AS_OF, round_usd};

/// Pricing page all Gemini image prices were taken from.
pub const PRICING_URL: &str = "https://ai.google.dev/gemini-api/docs/pricing";
/// Provider guide for these models.
pub const DOCS_URL: &str = "https://ai.google.dev/gemini-api/docs/image-generation";

/// Adapter-enforced cap on the whole encoded `generateContent` request (prompt plus
/// base64 reference images). The provider documents "20MB" for inline data; Iris
/// reads that conservatively as 20,000,000 bytes.
pub const MAX_REQUEST_BYTES: usize = 20_000_000;

/// The same cap for local validation. The allowances bound the JSON the adapter
/// writes around the prompt, the options, and each `inlineData` part (a few hundred
/// bytes in all, well under these values).
const REQUEST_LIMIT: RequestSizeLimit = RequestSizeLimit {
    max_bytes: MAX_REQUEST_BYTES as u64,
    framing_bytes: 1024,
    per_input_framing_bytes: 128,
};

/// Model ids the Gemini adapter (images and Veo) can send: the id is a URL path
/// segment (`models/{id}:generateContent`), so only characters that cannot change
/// the endpoint are accepted, the same rule the adapter enforces before sending.
pub const MODEL_ID_SYNTAX: ModelIdSyntax = ModelIdSyntax {
    max_len: 128,
    punctuation: "._-",
    alphanumeric_start: true,
    description: "letters, digits, '.', '_', and '-' only, starting with a letter or digit, at most 128 characters",
};

const BOTH: &[Operation] = &[Operation::ImageGenerate, Operation::ImageEdit];

const INPUT_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/heic", "image/heif"];

const INPUTS: InputSpec = InputSpec {
    max_input_images: 14,
    input_media_types: INPUT_TYPES,
    max_input_bytes: 14_000_000,
    mask: None,
    first_frame: false,
    last_frame: false,
    max_reference_images: 0,
    max_request: Some(REQUEST_LIMIT),
};

/// The provider chooses the format. JPEG comes first because live runs of
/// `gemini-3.1-flash-image` returned JPEG for both generate and edit, so default
/// file names get the right extension; PNG outputs are still saved (as `.png`).
const OUTPUTS: OutputSpec = OutputSpec { media_types: &["image/jpeg", "image/png"], max_count: 1 };

const LIMITS: Limits = Limits { max_prompt_chars: None };

/// Account note shared by every Gemini API model (images and Veo). Source: the
/// pricing page ("Free Tier: Not available" for these models) and the billing page
/// (<https://ai.google.dev/gemini-api/docs/billing>: the paid tier needs a billing plan;
/// Prepay is the default and stops at a zero balance; some accounts are on Postpay),
/// checked 2026-09-24.
pub const ACCESS_NOTE_PAID_TIER: &str =
    "No free tier: the key's project needs a paid-tier billing plan (on Prepay, a positive credit balance)";
/// Key-type note shared by every Gemini API model. Source:
/// <https://ai.google.dev/gemini-api/docs/api-key>, checked 2026-09-24. The page says
/// new keys are auth keys, unrestricted standard keys are rejected, and "On September
/// 2026" the API "will reject requests from standard keys", without an exact day, so
/// Iris does not claim that the cutoff is already enforced.
pub const ACCESS_NOTE_AUTH_KEY: &str = "Use an auth API key: Google says the Gemini API will reject standard keys \
                                        from September 2026 (no exact day given); unrestricted standard keys are \
                                        already rejected";

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

/// Gemini image names Iris gives no model. Dates and replacements are those of the
/// deprecations page (<https://ai.google.dev/gemini-api/docs/deprecations>) and the
/// models page's note on the 2.5 models, checked 2026-09-24. Google's "Nano Banana"
/// is `gemini-2.5-flash-image`, so the bare nickname is declined too: accepting it
/// for another model would silently run a differently branded model than the one
/// asked for.
pub const DECLINED: &[DeclinedName] = &[
    DeclinedName {
        names: &["nano-banana", "gemini-2.5-flash-image"],
        families: &[],
        reason: "Google's \"Nano Banana\" is gemini-2.5-flash-image, which Google has limited to projects that \
                 already used it since 2026-09-18, so Iris does not register it",
        instead: &["nano-banana-2", "nano-banana-pro"],
    },
    DeclinedName {
        names: &["gemini-3.1-flash-image-preview"],
        families: &[],
        reason: "Google shut down gemini-3.1-flash-image-preview on 2026-06-25 and names gemini-3.1-flash-image \
                 as its replacement",
        instead: &["gemini-3.1-flash-image"],
    },
    DeclinedName {
        names: &["gemini-3-pro-image-preview"],
        families: &[],
        reason: "Google shut down gemini-3-pro-image-preview on 2026-06-25 and names gemini-3-pro-image as its \
                 replacement",
        instead: &["gemini-3-pro-image"],
    },
    DeclinedName {
        names: &["gemini-2.5-flash-image-preview"],
        families: &[],
        reason: "Google shut down gemini-2.5-flash-image-preview on 2026-01-15, and its replacement, \
                 gemini-2.5-flash-image, is limited to projects that already used it",
        instead: &["nano-banana-2", "nano-banana-pro"],
    },
    DeclinedName {
        names: &[],
        families: &["imagen"],
        reason: "Google shut down every Imagen model on the Gemini API, Imagen 4 on 2026-08-17, and names \
                 gemini-3.1-flash-image as the replacement",
        instead: &["nano-banana-2"],
    },
];

/// The pixel size of a 1:1 image at 1K: 1024x1024 in the aspect-ratio and image-size
/// table of the image generation guide
/// (<https://ai.google.dev/gemini-api/docs/image-generation#aspect_ratios_and_image_size>,
/// checked 2026-09-24), for Nano Banana 2 and Nano Banana Pro. The table has no row for
/// Nano Banana 2 Lite; its list of aspect ratios links to the table, so its 1K is read
/// from the same table.
pub const SQUARE_1K_PIXELS: (u64, u64) = (1024, 1024);

/// The request for the standard output of the image operations, one 1024x1024 image
/// ([`StandardOutput`](super::StandardOutput)): 1K at 1:1 ([`SQUARE_1K_PIXELS`]). The
/// aspect ratio is explicit because an edit without one matches its input image.
const STANDARD: &[&[(&str, &str)]] = &[&[("aspect_ratio", "1:1"), ("resolution", "1K")]];

/// Built-in Gemini image models.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-3.1-flash-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana 2 (Gemini 3.1 Flash Image)",
        summary: "Google's most versatile image model, balancing speed with 4K output, world knowledge and text \
                  rendering; good with multiple reference images",
        aliases: &["nano-banana-2"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: FLASH_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: FLASH_PRICING,
        access_notes: &[ACCESS_NOTE_PAID_TIER, ACCESS_NOTE_AUTH_KEY],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
        estimate_usage: Some(estimate_from_usage),
    },
    ModelSpec {
        id: "gemini-3.1-flash-lite-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana 2 Lite (Gemini 3.1 Flash Lite Image)",
        summary: "Google's fastest and cheapest image model: 1K only, and not optimized for multiple reference \
                  images or multi-turn editing",
        aliases: &["nano-banana-2-lite"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: LITE_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: LITE_PRICING,
        access_notes: &[
            ACCESS_NOTE_PAID_TIER,
            ACCESS_NOTE_AUTH_KEY,
            "Provider note: not optimized for multiple reference images or multi-turn editing",
        ],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
        estimate_usage: Some(estimate_from_usage),
    },
    ModelSpec {
        id: "gemini-3-pro-image",
        provider: ProviderId::Gemini,
        display_name: "Nano Banana Pro (Gemini 3 Pro Image)",
        summary: "Google's premium image model for the most complex visual tasks and professional assets; the \
                  highest per-image price at each resolution",
        aliases: &["nano-banana-pro"],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: BOTH,
        inputs: INPUTS,
        options: PRO_OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: PRO_PRICING,
        access_notes: &[ACCESS_NOTE_PAID_TIER, ACCESS_NOTE_AUTH_KEY],
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
        estimate_usage: Some(estimate_from_usage),
    },
];

fn rates_for(model: &str) -> Option<&'static Rates> {
    RATES.iter().find(|r| r.model == model)
}

/// Pre-call estimate: the published per-image price for the effective resolution,
/// times the number of images. Input and thinking tokens are not included (the
/// basis says so), because they are only known after the call.
fn estimate_image(spec: &ModelSpec, input: &EstimateInput<'_>) -> Result<CostEstimate, String> {
    let resolution = spec.effective(input.options, "resolution").and_then(|v| v.as_str().map(str::to_string));
    let price = rates_for(spec.id)
        .zip(resolution.as_deref())
        .and_then(|(rates, resolution)| rates.per_image.iter().find(|(r, _)| *r == resolution));
    let (Some(resolution), Some((_, per_image))) = (resolution, price) else {
        // Unreachable for the catalog: a test checks a price for every declared resolution.
        return Err(format!("Iris has no published per-image price of {} for this request", spec.id));
    };
    let count = input.count.max(1);
    Ok(CostEstimate::usd(
        round_usd(f64::from(count) * per_image),
        format!(
            "{count} image × {} ({}, {resolution}); input and thinking tokens not included",
            format_usd(*per_image),
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
            "{prompt} input tokens × {}/1M + {image_tokens} image output tokens × {}/1M + \
             {text_tokens} text and thinking tokens × {}/1M ({}, from reported usage{note})",
            format_usd(rates.input_per_m),
            format_usd(rates.image_output_per_m),
            format_usd(rates.text_output_per_m),
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
