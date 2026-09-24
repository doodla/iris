//! Catalog declarations for the Gemini image models,
//! plus the typed-flag mapping rule for every Gemini-provider model (images and Veo).
//! Offline; no credentials.

use iris::catalog::{
    self, CATALOG_AS_OF, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionKind, OptionSource,
    OptionValue, RawOption, ResolvedOptions, validate_request,
};
use iris::domain::{Operation, ProviderId, Usage};
use iris::error::ErrorCode;

#[path = "catalog_support.rs"]
mod catalog_support;

const FLASH: &str = "gemini-3.1-flash-image";
const LITE: &str = "gemini-3.1-flash-lite-image";
const PRO: &str = "gemini-3-pro-image";

/// The typed flags of the command that runs `op` (`--count` → `count`, ...).
fn flag_table(op: Operation) -> &'static [(&'static str, &'static str)] {
    match op {
        Operation::VideoGenerate => iris::cli::args::VIDEO_FLAGS,
        _ => iris::cli::args::IMAGE_FLAGS,
    }
}

fn spec(id: &str) -> &'static ModelSpec {
    catalog::find(id).unwrap_or_else(|| panic!("{id} is in the catalog"))
}

fn raw(name: &str, value: &str) -> RawOption {
    let source = flag_table(Operation::ImageGenerate)
        .iter()
        .find(|(_, n)| *n == name)
        .map_or(OptionSource::Generic, |(flag, _)| OptionSource::Flag(flag));
    RawOption { name: name.to_string(), value: value.to_string(), source }
}

fn validate(
    id: &str,
    op: Operation,
    opts: &[(&str, &str)],
    images: usize,
) -> Result<ResolvedOptions, ErrorCode> {
    let raw: Vec<RawOption> = opts.iter().map(|(n, v)| raw(n, v)).collect();
    validate_request(spec(id), op, &raw, InputCounts { images, ..InputCounts::default() }).map_err(|e| e.code)
}

fn estimate(id: &str, opts: &[(&str, &str)]) -> iris::domain::CostEstimate {
    let s = spec(id);
    let options = validate(id, Operation::ImageGenerate, opts, 0).expect("valid options");
    let estimator = s.estimate.expect("every Gemini image model has an estimator");
    estimator(s, &EstimateInput { operation: Operation::ImageGenerate, options: &options, count: 1 })
        .expect("estimate for a declared resolution")
}

fn gemini_models() -> Vec<&'static ModelSpec> {
    catalog::all().filter(|m| m.provider == ProviderId::Gemini).collect()
}

#[test]
fn the_three_nano_banana_models_are_declared_with_ids_names_and_aliases() {
    let images: Vec<&str> = catalog::gemini::MODELS.iter().map(|m| m.id).collect();
    assert_eq!(images, [FLASH, LITE, PRO]);
    for m in catalog::gemini::MODELS {
        assert_eq!(m.provider, ProviderId::Gemini);
        assert_eq!(m.lifecycle, Lifecycle::Ga);
        assert_eq!(m.operations, &[Operation::ImageGenerate, Operation::ImageEdit]);
        assert_eq!(m.docs_url, "https://ai.google.dev/gemini-api/docs/image-generation");
        assert_eq!(m.outputs.media_types, &["image/jpeg", "image/png"]);
        assert_eq!(m.outputs.max_count, 1);
        assert_eq!(m.limits.max_prompt_chars, None);
        for note in [catalog::gemini::ACCESS_NOTE_PAID_TIER, catalog::gemini::ACCESS_NOTE_AUTH_KEY] {
            assert!(m.access_notes.contains(&note), "{}: {note}", m.id);
        }
    }
    assert_eq!(spec(FLASH).display_name, "Nano Banana 2 (Gemini 3.1 Flash Image)");
    assert_eq!(spec(LITE).display_name, "Nano Banana 2 Lite (Gemini 3.1 Flash Lite Image)");
    assert_eq!(spec(PRO).display_name, "Nano Banana Pro (Gemini 3 Pro Image)");
    assert_eq!(spec("nano-banana").id, FLASH);
    assert_eq!(spec("nano-banana-2").id, FLASH);
    assert_eq!(spec("nano-banana-2-lite").id, LITE);
    assert_eq!(spec("nano-banana-pro").id, PRO);
    assert!(spec(LITE).access_notes.iter().any(|n| n.contains("not optimized for multiple reference")));
}

/// The account notes state only what Google documents (checked 2026-09-24): no free
/// tier, and standard keys "will" be rejected "On September 2026" with no exact day, so
/// the notes must not claim the cutoff is already enforced. Veo shares the same notes.
#[test]
fn account_notes_state_only_the_documented_key_and_billing_rules() {
    assert_eq!(
        catalog::gemini::ACCESS_NOTE_PAID_TIER,
        "No free tier: the key's project needs a paid-tier billing plan (on Prepay, a positive credit balance)"
    );
    assert_eq!(
        catalog::gemini::ACCESS_NOTE_AUTH_KEY,
        "Use an auth API key: Google says the Gemini API will reject standard keys from September 2026 (no \
         exact day given); unrestricted standard keys are already rejected"
    );
    for m in gemini_models() {
        for note in [catalog::gemini::ACCESS_NOTE_PAID_TIER, catalog::gemini::ACCESS_NOTE_AUTH_KEY] {
            assert!(m.access_notes.contains(&note), "{}: {note}", m.id);
        }
        for note in m.access_notes {
            assert!(
                !note.contains("since September") && !note.contains("are rejected from"),
                "{}: {note}",
                m.id
            );
        }
    }
}

#[test]
fn flash_is_the_gemini_default_for_generate_and_edit() {
    assert_eq!(catalog::default_model(ProviderId::Gemini, Operation::ImageGenerate).unwrap().id, FLASH);
    assert_eq!(catalog::default_model(ProviderId::Gemini, Operation::ImageEdit).unwrap().id, FLASH);
    let resolved = catalog::resolve("nano-banana-pro", None, Some(ProviderId::Gemini)).unwrap();
    assert_eq!(resolved.id, PRO);
    let err = catalog::resolve("nano-banana", None, Some(ProviderId::OpenAi)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    for gone in ["gemini-2.5-flash-image", "gemini-3-pro-image-preview", "imagen-4.0-generate-001"] {
        assert_eq!(catalog::resolve(gone, None, None).unwrap_err().code, ErrorCode::UnknownModel, "{gone}");
    }
}

#[test]
fn inputs_accept_fourteen_references_of_the_documented_types_and_no_mask() {
    for m in catalog::gemini::MODELS {
        assert_eq!(m.inputs.max_input_images, 14, "{}", m.id);
        assert_eq!(
            m.inputs.input_media_types,
            &["image/png", "image/jpeg", "image/webp", "image/heic", "image/heif"]
        );
        assert_eq!(m.inputs.max_input_bytes, 14_000_000);
        assert!(m.inputs.mask.is_none());
        assert!(!m.inputs.first_frame && !m.inputs.last_frame);
        assert_eq!(m.inputs.max_reference_images, 0);
    }
    assert_eq!(validate(FLASH, Operation::ImageEdit, &[], 14), Ok(ResolvedOptions::new()));
    assert_eq!(validate(FLASH, Operation::ImageEdit, &[], 15), Err(ErrorCode::InvalidArgument));
    let mask = InputCounts { images: 1, mask: true, ..InputCounts::default() };
    assert_eq!(
        validate_request(spec(FLASH), Operation::ImageEdit, &[], mask).unwrap_err().code,
        ErrorCode::UnsupportedOption
    );
    assert_eq!(
        validate_request(spec(FLASH), Operation::VideoGenerate, &[], InputCounts::default())
            .unwrap_err()
            .code,
        ErrorCode::UnsupportedOperation
    );
}

#[test]
fn options_match_the_contract_per_model() {
    let names = |id: &str| spec(id).options.iter().map(|o| o.name).collect::<Vec<_>>();
    assert_eq!(names(FLASH), ["count", "aspect_ratio", "resolution", "thinking_level"]);
    assert_eq!(names(LITE), ["count", "aspect_ratio", "resolution", "thinking_level"]);
    assert_eq!(names(PRO), ["count", "aspect_ratio", "resolution"]);

    let values = |id: &str, name: &str| match spec(id).option(name).unwrap().kind {
        OptionKind::Enum(v) => v.to_vec(),
        other => panic!("{id} {name}: expected an enum, got {other:?}"),
    };
    assert_eq!(values(FLASH, "resolution"), ["512", "1K", "2K", "4K"]);
    assert_eq!(values(LITE, "resolution"), ["1K"]);
    assert_eq!(values(PRO, "resolution"), ["1K", "2K", "4K"]);
    assert_eq!(values(FLASH, "aspect_ratio").len(), 14);
    for extreme in ["1:4", "1:8", "4:1", "8:1"] {
        assert!(values(FLASH, "aspect_ratio").contains(&extreme));
        assert!(!values(PRO, "aspect_ratio").contains(&extreme));
        assert!(!values(LITE, "aspect_ratio").contains(&extreme));
    }
    assert_eq!(values(PRO, "aspect_ratio"), values(LITE, "aspect_ratio"));
    assert_eq!(values(PRO, "aspect_ratio").len(), 10);
    assert_eq!(values(FLASH, "thinking_level"), ["minimal", "high"]);

    for m in catalog::gemini::MODELS {
        let count = m.option("count").unwrap();
        assert!(matches!(count.kind, OptionKind::Integer { min: 1, max: 1 }));
        assert_eq!(count.default, Some("1"));
        assert_eq!(m.option("aspect_ratio").unwrap().default, None, "matches the input, else 1:1");
        assert_eq!(m.option("resolution").unwrap().default, Some("1K"));
    }
    assert_eq!(spec(FLASH).option("thinking_level").unwrap().default, Some("minimal"));
}

#[test]
fn undeclared_and_out_of_range_options_are_rejected_locally() {
    for (name, value) in
        [("size", "1024x1024"), ("quality", "high"), ("format", "png"), ("seed", "7"), ("background", "auto")]
    {
        assert_eq!(
            validate(FLASH, Operation::ImageGenerate, &[(name, value)], 0),
            Err(ErrorCode::UnsupportedOption),
            "{name}"
        );
    }
    assert_eq!(
        validate(PRO, Operation::ImageGenerate, &[("thinking_level", "high")], 0),
        Err(ErrorCode::UnsupportedOption)
    );
    assert_eq!(
        validate(LITE, Operation::ImageGenerate, &[("resolution", "2K")], 0),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        validate(PRO, Operation::ImageGenerate, &[("resolution", "512")], 0),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        validate(FLASH, Operation::ImageGenerate, &[("count", "2")], 0),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        validate(FLASH, Operation::ImageGenerate, &[("resolution", "1k")], 0),
        Err(ErrorCode::InvalidArgument),
        "the provider rejects a lowercase k"
    );
    let ok = validate(
        FLASH,
        Operation::ImageEdit,
        &[("aspect_ratio", "8:1"), ("resolution", "512"), ("thinking_level", "high")],
        2,
    )
    .unwrap();
    assert_eq!(ok.get("resolution"), Some(&OptionValue::Str("512".into())));
}

#[test]
fn every_declared_default_parses_with_its_own_kind() {
    for m in gemini_models() {
        for o in m.options {
            if let Some(d) = o.default {
                assert!(OptionValue::parse(&o.kind, d).is_ok(), "{} {} default {d}", m.id, o.name);
            }
            assert!(!o.description.is_empty(), "{} {}", m.id, o.name);
        }
    }
}

/// Each option declares the typed flag its command has for it (image commands for
/// the image models, `video generate` for Veo), or none when that command has no
/// flag of that name.
#[test]
fn typed_flags_follow_the_cli_flag_tables_for_every_gemini_provider_model() {
    let models = gemini_models();
    assert_eq!(models.len(), 6, "three image models and three Veo models");
    for m in models {
        for o in m.options {
            for op in o.operations {
                let expected = flag_table(*op).iter().find(|(_, n)| *n == o.name).map(|(f, _)| *f);
                assert_eq!(o.flag, expected, "{} option {} for {op}", m.id, o.name);
            }
        }
    }
}

#[test]
fn pre_call_estimate_is_the_per_image_price_for_the_effective_resolution() {
    let e = estimate(FLASH, &[]);
    assert!((e.amount - 0.067).abs() < 1e-9, "{}", e.amount);
    assert!(e.estimated);
    assert_eq!(e.currency, "USD");
    assert!(e.basis.contains("1K") && e.basis.contains("not included"), "{}", e.basis);
    assert_eq!(e.source_url, "https://ai.google.dev/gemini-api/docs/pricing");
    assert_eq!(e.as_of, CATALOG_AS_OF);

    let cases = [
        (FLASH, "512", 0.045),
        (FLASH, "2K", 0.101),
        (FLASH, "4K", 0.151),
        (LITE, "1K", 0.0336),
        (PRO, "1K", 0.134),
        (PRO, "2K", 0.134),
        (PRO, "4K", 0.24),
    ];
    for (id, res, usd) in cases {
        let e = estimate(id, &[("resolution", res)]);
        assert!((e.amount - usd).abs() < 1e-9, "{id} {res}: {}", e.amount);
    }
}

#[test]
fn published_prices_carry_source_and_date() {
    for m in catalog::gemini::MODELS {
        assert!(!m.pricing.is_empty());
        for p in m.pricing {
            assert_eq!(p.source_url, "https://ai.google.dev/gemini-api/docs/pricing");
            assert_eq!(p.as_of, CATALOG_AS_OF);
            assert!(p.usd > 0.0);
        }
    }
    let per_image: Vec<f64> =
        spec(FLASH).pricing.iter().filter(|p| p.unit == "image").map(|p| p.usd).collect();
    assert_eq!(per_image, [0.045, 0.067, 0.101, 0.151]);
}

#[test]
fn post_call_estimate_splits_usage_metadata_by_modality() {
    let usage = Usage {
        input_tokens: Some(14),
        output_tokens: Some(927),
        total_tokens: Some(941),
        provider_usage: Some(serde_json::json!({
            "promptTokenCount": 14,
            "candidatesTokenCount": 747,
            "thoughtsTokenCount": 180,
            "totalTokenCount": 941,
            "candidatesTokensDetails": [{"modality": "IMAGE", "tokenCount": 747}]
        })),
    };
    let e = catalog::gemini::estimate_from_usage(spec(FLASH), &usage).unwrap();
    let expected = 14.0 * 0.50 / 1e6 + 747.0 * 60.0 / 1e6 + 180.0 * 3.0 / 1e6;
    assert!((e.amount - expected).abs() < 1e-6, "{} vs {expected}", e.amount);
    assert!(e.basis.contains("from reported usage"), "{}", e.basis);

    // Without an itemized split every candidate token is priced as image output.
    let mut unitemized = usage.clone();
    unitemized.provider_usage =
        Some(serde_json::json!({"promptTokenCount": 10, "candidatesTokenCount": 1000}));
    let e = catalog::gemini::estimate_from_usage(spec(LITE), &unitemized).unwrap();
    assert!((e.amount - (10.0 * 0.25 + 1000.0 * 30.0) / 1e6).abs() < 1e-6, "{}", e.amount);
    assert!(e.basis.contains("not itemized"));

    let empty = Usage::default();
    assert!(catalog::gemini::estimate_from_usage(spec(FLASH), &empty).is_none());
}

/// The Gemini image models have no cross-option rules: nothing beyond per-option and
/// input checks rejects any combination of declared values, and none is published.
#[test]
fn every_cross_option_rule_is_a_declared_constraint() {
    for m in catalog::gemini::MODELS {
        assert!(m.validate.is_none(), "{}", m.id);
        catalog_support::assert_constraints_cover_the_validator(m);
    }
}
