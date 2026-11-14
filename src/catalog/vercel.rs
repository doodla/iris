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
//! Iris declares only what those sources support (docs/contributing/decisions.md,
//! "Vercel AI Gateway"):
//!
//! * models: FLUX 3 Image and FLUX 3 Image Klein, whose listed prices depend only on
//!   a size of five square classes, so a cost can be estimated before the request.
//!   The gateway lists many more image models, but most have no published price, or
//!   prices by token usage or options that its Images endpoint does not document.
//! * operations: `image.generate` only. The gateway's edit guide does not list these
//!   models among those that accept input images, and warns that a model without mask
//!   support may ignore the mask.
//! * options: `size`, the five square sizes the price list names, and `count`, fixed
//!   at 1 and never sent. The guide documents no other request field for these models.

use crate::domain::{Billing, CostEstimate, Operation, ProviderId, format_usd};

use super::round_usd;
use super::types::{
    DeclinedName, EstimateInput, Estimator, InputSpec, Lifecycle, Limits, ModelIdSyntax, ModelSpec,
    OptionKind, OptionSpec, OutputSpec, PriceRule,
};

/// The gateway's model list, where the prices below are published.
pub const PRICING_URL: &str = "https://ai-gateway.vercel.sh/v1/models";
/// Date the prices and capabilities were checked.
pub const PRICING_AS_OF: &str = "2026-11-14";
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

/// The square sizes the gateway's price list names for the FLUX 3 models.
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

/// The provider chooses the format; both are kept under their sniffed type.
const OUTPUTS: OutputSpec = OutputSpec { media_types: &["image/jpeg", "image/png"], max_count: 1 };

const LIMITS: Limits = Limits { max_prompt_chars: None };

/// Account notes from the pricing and rate-limit pages
/// (<https://vercel.com/docs/ai-gateway/pricing>, <https://vercel.com/docs/ai-gateway/rate-limits>),
/// checked 2026-11-14.
const ACCESS_NOTES: &[&str] = &[
    "Billed from the team's AI Gateway Credits at the provider's list price; a budget can refuse requests \
     (HTTP 402)",
    "The free tier has lower per-model rate limits and only free-tier-eligible models",
];

/// Published prices of one model: the base per-image price, and per size.
#[derive(Debug, Clone, Copy)]
struct Rates {
    model: &'static str,
    /// `pricing.image`: the price of an image without a size.
    base: f64,
    /// `pricing.image_dimension_quality_pricing`: (size, USD per image).
    per_size: &'static [(&'static str, f64)],
}

const RATES: &[Rates] = &[
    Rates {
        model: "bfl/flux-3-image",
        base: 0.048,
        per_size: &[
            ("768x768", 0.041),
            ("1024x1024", 0.048),
            ("1536x1536", 0.07),
            ("2048x2048", 0.1),
            ("4096x4096", 0.607),
        ],
    },
    Rates {
        model: "bfl/flux-3-image-klein",
        base: 0.009,
        per_size: &[
            ("768x768", 0.008),
            ("1024x1024", 0.009),
            ("1536x1536", 0.011),
            ("2048x2048", 0.014),
            ("4096x4096", 0.036),
        ],
    },
];

fn rates_for(model: &str) -> Option<&'static Rates> {
    RATES.iter().find(|r| r.model == model)
}

const fn price(description: &'static str, usd: f64) -> PriceRule {
    PriceRule { description, unit: "image", usd, source_url: PRICING_URL, as_of: PRICING_AS_OF }
}

const FLUX_3_PRICING: &[PriceRule] = &[
    price("Generated image, size not given", 0.048),
    price("Generated image, 768x768", 0.041),
    price("Generated image, 1024x1024", 0.048),
    price("Generated image, 1536x1536", 0.07),
    price("Generated image, 2048x2048", 0.1),
    price("Generated image, 4096x4096", 0.607),
];

const FLUX_3_KLEIN_PRICING: &[PriceRule] = &[
    price("Generated image, size not given", 0.009),
    price("Generated image, 768x768", 0.008),
    price("Generated image, 1024x1024", 0.009),
    price("Generated image, 1536x1536", 0.011),
    price("Generated image, 2048x2048", 0.014),
    price("Generated image, 4096x4096", 0.036),
];

/// No gateway names are declined yet.
pub const DECLINED: &[DeclinedName] = &[];

/// The request for the standard output of `image.generate`, one 1024x1024 image.
const STANDARD: &[&[(&str, &str)]] = &[&[("size", "1024x1024")]];

/// Built-in Vercel AI Gateway models.
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "bfl/flux-3-image",
        provider: ProviderId::Vercel,
        display_name: "FLUX 3 Image (Black Forest Labs, via Vercel AI Gateway)",
        summary: "Black Forest Labs' FLUX 3 image model, in five square sizes from 768x768 to 4096x4096",
        aliases: &[],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: GENERATE,
        inputs: InputSpec::NONE,
        options: OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: FLUX_3_PRICING,
        access_notes: ACCESS_NOTES,
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
        estimate_usage: None,
    },
    ModelSpec {
        id: "bfl/flux-3-image-klein",
        provider: ProviderId::Vercel,
        display_name: "FLUX 3 Image Klein (Black Forest Labs, via Vercel AI Gateway)",
        summary: "Black Forest Labs' smaller, faster FLUX 3 model with open weights, at about a fifth of FLUX 3 \
                  Image's price",
        aliases: &[],
        lifecycle: Lifecycle::Ga,
        billing: Billing::Paid,
        operations: GENERATE,
        inputs: InputSpec::NONE,
        options: OPTIONS,
        outputs: OUTPUTS,
        limits: LIMITS,
        pricing: FLUX_3_KLEIN_PRICING,
        access_notes: ACCESS_NOTES,
        docs_url: DOCS_URL,
        validate: None,
        estimate: Some(Estimator { estimate: estimate_image, standard: STANDARD }),
        estimate_usage: None,
    },
];

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
        PRICING_AS_OF,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_has_rates_for_every_declared_size_and_matching_price_rules() {
        for m in MODELS {
            let rates = rates_for(m.id).unwrap_or_else(|| panic!("{} has no rates", m.id));
            for size in SIZES {
                let (_, usd) = rates.per_size.iter().find(|(s, _)| s == size).expect(size);
                assert!(
                    m.pricing.iter().any(|p| p.description.ends_with(size) && p.usd == *usd),
                    "{} {size}",
                    m.id
                );
            }
            assert!(
                m.pricing.iter().any(|p| p.description.ends_with("size not given") && p.usd == rates.base)
            );
        }
    }
}
