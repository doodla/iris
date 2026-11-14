//! Built-in catalog entries for image-only models on the Vercel AI Gateway, through
//! its OpenAI-compatible Images endpoint (`POST /v1/images/generations`, synchronous).
//!
//! Sources, checked 2026-11-14:
//!
//! * the image generation guide for the OpenAI-compatible API
//!   (<https://vercel.com/docs/ai-gateway/modalities/image-generation/openai>): image-only
//!   models use `/v1/images/generations`, take `model`, `prompt`, `n`, and
//!   `response_format: "b64_json"`, and return base64 images in `data[]`;
//! * the gateway's model list (`GET https://ai-gateway.vercel.sh/v1/models`, the
//!   documented catalog endpoint of the REST API reference): each model's
//!   description, and its prices (`pricing.image`, and per-size prices in
//!   `pricing.image_dimension_quality_pricing`).
//!
//! * the Black Forest Labs provider options that the guide links to for these
//!   models (<https://ai-sdk.dev/providers/ai-sdk-providers/black-forest-labs>): for
//!   `flux-3-image-klein`, "To request exact dimensions, pass `size`".
//!
//! Iris declares only what those sources support (docs/contributing/decisions.md,
//! "Vercel AI Gateway"):
//!
//! * models: FLUX 3 Image Klein. `models list` compares image models on one 1024x1024
//!   image, so a model needs a request documented to give that size: Klein's `size`
//!   sets exact dimensions, and its prices name 1024x1024. `bfl/flux-3-image` is
//!   declined: the same page says its `size` "is used only to derive a supported
//!   aspect ratio" and a `resolution` tier sets the pixel area, with no documented
//!   pixel size per tier.
//! * operations: `image.generate` only. The gateway's model list gives Klein text
//!   input only, and its edit guide does not list Klein among the models that accept
//!   input images.
//! * options: `size`, the five square sizes the price list names, and `count`, fixed
//!   at 1 and never sent. A live request on 2026-11-14 asked for 768x768 and got
//!   768x768, as documented; the app also checks every saved image against the
//!   requested size (`output_size_mismatch`).
//! * lifecycle: `ga`. The gateway's model list has no lifecycle field, and the model
//!   is not marked preview or deprecated, so Iris assumes general availability.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId, format_usd};

use super::round_usd;
use super::types::{
    DeclinedName, EstimateInput, Estimator, InputSpec, Lifecycle, Limits, ModelIdSyntax, ModelSpec,
    OptionKind, OptionSpec, OutputSpec, PriceRule,
};

/// The gateway's model list, where the prices below are published.
pub const PRICING_URL: &str = "https://ai-gateway.vercel.sh/v1/models";
/// Date these catalog entries (capabilities and prices) were checked.
pub const CATALOG_AS_OF: &str = "2026-11-14";
/// Image generation guide for the OpenAI-compatible API.
pub const DOCS_URL: &str = "https://vercel.com/docs/ai-gateway/modalities/image-generation/openai";

/// Model ids the Vercel adapter can send: gateway ids are `<creator>/<model>` and
/// travel only in the JSON body, so the separators of those ids are accepted.
pub const MODEL_ID_SYNTAX: ModelIdSyntax = ModelIdSyntax {
    max_len: 200,
    punctuation: "-._/",
    alphanumeric_start: true,
    description: "letters, digits, '-', '.', '_', and '/' only, starting with a letter or digit, at most 200 \
                  characters",
};

const GENERATE: &[Operation] = &[Operation::ImageGenerate];

/// The square sizes the gateway's price list names for Klein.
pub const SIZES: &[&str] = &["768x768", "1024x1024", "1536x1536", "2048x2048", "4096x4096"];

const COUNT: OptionSpec = OptionSpec {
    name: "count",
    kind: OptionKind::Integer { min: 1, max: 1 },
    default: Some("1"),
    flag: Some("--count"),
    operations: GENERATE,
    description: "Number of images. Iris requests one image per call through the gateway; the value is \
                  validated locally and never sent",
};

const SIZE: OptionSpec = OptionSpec {
    name: "size",
    kind: OptionKind::Enum(SIZES),
    default: None,
    flag: Some("--size"),
    operations: GENERATE,
    description: "Output size, one of the five square classes the gateway prices. When omitted, the model \
                  chooses, at the gateway's base per-image price",
};

const OPTIONS: &[OptionSpec] = &[COUNT, SIZE];

/// The provider chooses the format; both are kept under their sniffed type. Klein's
/// `size` sets the exact output dimensions (the Black Forest Labs provider options
/// cited above), so every saved image is checked against it.
const OUTPUTS: OutputSpec =
    OutputSpec { media_types: &["image/jpeg", "image/png"], max_count: 1, exact_size_option: Some("size") };

const LIMITS: Limits = Limits { max_prompt_chars: None };

/// Account notes from the pricing page (<https://vercel.com/docs/ai-gateway/pricing>),
/// checked 2026-11-14.
const ACCESS_NOTES: &[&str] = &[
    "Billed from the team's AI Gateway Credits at the provider's list price, unless you bring your own \
     provider key, which the provider bills directly",
    "The free tier's monthly credit covers only a subset of models (the gateway's Free Tier filter); other \
     models need purchased AI Gateway Credits",
    "A budget can refuse requests (HTTP 402); add-on surcharges, such as 5% on a regional endpoint, are not \
     in Iris's estimate",
];

/// The published prices of one model, declared once: [`Rates::rules`] (what `models
/// show` lists) is built from the same values the estimator reads.
#[derive(Debug, Clone, Copy)]
struct Rates {
    /// `pricing.image`: the price of an image without a size.
    base: f64,
    /// `pricing.image_dimension_quality_pricing`: (size, USD per image).
    per_size: &'static [(&'static str, f64)],
    rules: &'static [PriceRule],
}

const fn price(description: &'static str, usd: f64) -> PriceRule {
    PriceRule { description, unit: "image", usd, source_url: PRICING_URL, as_of: CATALOG_AS_OF }
}

/// [`Rates`] from a base price and the per-size prices.
macro_rules! rates {
    ($base:expr; $(($size:literal, $usd:expr)),+ $(,)?) => {
        Rates {
            base: $base,
            per_size: &[$(($size, $usd)),+],
            rules: &[
                price("Generated image, size not given", $base),
                $(price(concat!("Generated image, ", $size), $usd)),+
            ],
        }
    };
}

const FLUX_3_KLEIN: Rates = rates!(0.009;
    ("768x768", 0.008), ("1024x1024", 0.009), ("1536x1536", 0.011), ("2048x2048", 0.014), ("4096x4096", 0.036));

fn rates_for(model: &str) -> Option<&'static Rates> {
    match model {
        "bfl/flux-3-image-klein" => Some(&FLUX_3_KLEIN),
        _ => None,
    }
}

/// Gateway models Iris gives no model, with the reason (sources in the module docs).
pub const DECLINED: &[DeclinedName] = &[DeclinedName {
    names: &["bfl/flux-3-image"],
    families: &[],
    reason: "The Black Forest Labs provider options that the Vercel AI Gateway links to say that bfl/flux-3-image \
             uses `size` only for its aspect ratio and sets the pixel area by a resolution tier, with no documented \
             pixel size per tier, so Iris can't request or price a known output size",
    instead: &["bfl/flux-3-image-klein"],
}];

/// The request for the standard output of `image.generate`, one 1024x1024 image.
const STANDARD: &[&[(&str, &str)]] = &[&[("size", "1024x1024")]];

/// Built-in Vercel AI Gateway models.
pub static MODELS: &[ModelSpec] = &[ModelSpec {
    id: "bfl/flux-3-image-klein",
    provider: ProviderId::Vercel,
    display_name: "FLUX 3 Image Klein (Black Forest Labs, via Vercel AI Gateway)",
    summary: "Black Forest Labs' smaller, faster FLUX 3 image model with open weights, at exact square sizes \
                  from 768x768 to 4096x4096",
    aliases: &[],
    lifecycle: Lifecycle::Ga,
    billing: Billing::PaidWithFreeTier,
    operations: GENERATE,
    inputs: InputSpec::NONE,
    options: OPTIONS,
    outputs: OUTPUTS,
    limits: LIMITS,
    pricing: FLUX_3_KLEIN.rules,
    access_notes: ACCESS_NOTES,
    docs_url: DOCS_URL,
    validate: None,
    estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
    estimate_usage: None,
}];

/// Per-image price for the requested size, or the base price when no size was given.
fn estimate_image(spec: &ModelSpec, input: &EstimateInput<'_>) -> Result<CostEstimate, String> {
    let Some(rates) = rates_for(spec.id) else {
        // Unreachable for the catalog: a test checks a rate table for every model.
        return Err(format!("Iris has no published per-image price of {} for this request", spec.id));
    };
    let size = spec.effective(input.options, "size").and_then(|v| v.as_str().map(str::to_string));
    let (per_image, what) = match size.as_deref() {
        None => (rates.base, "size not given".to_string()),
        Some(size) => match rates.per_size.iter().find(|(s, _)| *s == size) {
            Some((_, usd)) => (*usd, size.to_string()),
            None => return Err(format!("Iris has no published price of {} at size {size}", spec.id)),
        },
    };
    let count = input.count.max(1);
    Ok(CostEstimate::usd(
        round_usd(f64::from(count) * per_image),
        format!(
            "{count} image × {} ({}, {what}); the gateway's listed price",
            format_usd(per_image),
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
    fn every_model_has_a_price_for_every_declared_size() {
        for m in MODELS {
            let rates = rates_for(m.id).unwrap_or_else(|| panic!("{} has no rates", m.id));
            let listed: Vec<(&str, f64)> = m.pricing.iter().map(|p| (p.description, p.usd)).collect();
            let declared: Vec<(&str, f64)> = rates.rules.iter().map(|p| (p.description, p.usd)).collect();
            assert_eq!(listed, declared, "{} lists its own rate table", m.id);
            for size in SIZES {
                assert!(rates.per_size.iter().any(|(s, _)| s == size), "{} {size}", m.id);
            }
        }
    }
}
