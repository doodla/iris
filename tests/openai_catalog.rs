//! The OpenAI catalog entries (checked against OpenAI's official documentation,
//! 2026-09-24): models, aliases, options (kinds, defaults, flags), size and
//! cross-option validation, prices, and cost estimates. Offline, no credentials.

use iris::catalog::{
    self, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionKind, OptionSource, OptionValue, RawOption,
    ResolvedOptions, StandardOutput, openai,
};
use iris::domain::{Operation, ProviderId, Usage};
use iris::error::ErrorCode;
use serde_json::json;

#[path = "catalog_support.rs"]
mod catalog_support;

const IDS: [&str; 3] = ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare", "gpt-image-2"];

fn model(id: &str) -> &'static ModelSpec {
    openai::MODELS.iter().find(|m| m.id == id).unwrap_or_else(|| panic!("{id} not in the OpenAI catalog"))
}

fn raw(pairs: &[(&str, &str)]) -> Vec<RawOption> {
    pairs
        .iter()
        .map(|(k, v)| RawOption { name: k.to_string(), value: v.to_string(), source: OptionSource::Generic })
        .collect()
}

fn validate(
    id: &str,
    op: Operation,
    pairs: &[(&str, &str)],
) -> Result<ResolvedOptions, iris::error::IrisError> {
    let inputs = if op == Operation::ImageEdit {
        InputCounts { images: 1, ..Default::default() }
    } else {
        InputCounts::default()
    };
    catalog::validate_request(model(id), op, &raw(pairs), inputs, &catalog_support::builtin())
}

fn estimate(id: &str, pairs: &[(&str, &str)], count: u32) -> Result<iris::domain::CostEstimate, String> {
    let spec = model(id);
    let options = validate(id, Operation::ImageGenerate, pairs).unwrap();
    (spec.estimate.unwrap().estimate)(
        spec,
        &EstimateInput { operation: Operation::ImageGenerate, options: &options, count },
    )
}

#[test]
fn models_ids_display_names_lifecycle_and_operations_are_declared() {
    let ids: Vec<&str> = openai::MODELS.iter().map(|m| m.id).collect();
    assert_eq!(ids, IDS);
    let names: Vec<&str> = openai::MODELS.iter().map(|m| m.display_name).collect();
    assert_eq!(names, ["GPT Image 2.5 Sunburst", "GPT Image 2.5 Flare", "GPT Image 2"]);
    for m in openai::MODELS {
        assert_eq!(m.provider, ProviderId::OpenAi, "{}", m.id);
        assert_eq!(m.lifecycle, Lifecycle::Ga, "{}", m.id);
        assert_eq!(m.operations, [Operation::ImageGenerate, Operation::ImageEdit], "{}", m.id);
        assert_eq!(m.docs_url, "https://developers.openai.com/api/docs/guides/image-generation");
        assert_eq!(
            m.access_notes,
            [
                "API Organization Verification may be required for GPT Image models",
                "Paid usage tier required (no free-tier limits listed)"
            ]
        );
        assert_eq!(m.limits.max_prompt_chars, Some(32_000));
        assert!(m.validate.is_some() && m.estimate.is_some(), "{}", m.id);
    }
}

#[test]
fn dated_snapshots_are_aliases_of_their_base_model_and_retired_models_are_unknown() {
    for (alias, base) in [
        ("gpt-image-2.5-sunburst-2026-09-08", "gpt-image-2.5-sunburst"),
        ("gpt-image-2.5-flare-2026-09-08", "gpt-image-2.5-flare"),
        ("gpt-image-2-2026-04-21", "gpt-image-2"),
    ] {
        assert_eq!(model(base).aliases, [alias]);
        assert_eq!(catalog::find(alias).unwrap().id, base);
        let resolved = catalog::resolve(alias, None).unwrap();
        assert_eq!(resolved.spec.id, base);
    }
}

/// Deprecated and removed OpenAI image models, and their dated snapshots, are
/// unknown models whose hint says why, with OpenAI's date, and what to use instead
/// (the GPT Image 2.5 models OpenAI recommends for new integrations, and
/// gpt-image-2, the replacement its notices name), never `--capabilities-from`.
#[test]
fn declined_openai_names_say_why_and_what_to_use_instead() {
    let instead = "use gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2";
    for (name, why) in [
        ("gpt-image-1", "deprecated gpt-image-1 (shutdown on 2026-10-23)"),
        ("gpt-image-1.5", "(shutdown on 2026-12-01)"),
        ("gpt-image-1-mini", "(shutdown on 2026-12-01)"),
        ("chatgpt-image-latest", "(shutdown on 2026-12-01)"),
        // Dated snapshots of a declined name are declined with it.
        ("gpt-image-1.5-2025-12-16", "(shutdown on 2026-12-01)"),
        ("gpt-image-1-2025-04-15", "deprecated gpt-image-1 (shutdown on 2026-10-23)"),
        ("dall-e-3", "removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12"),
        ("dall-e-2", "removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12"),
        ("DALL-E-3", "removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12"),
        ("dalle-3", "removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12"),
        ("dall-e", "removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12"),
    ] {
        assert!(catalog::find(name).is_none(), "{name} must not be registered (deprecated or removed)");
        let err = catalog::resolve(name, None).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnknownModel);
        let hint = err.hint.as_deref().unwrap();
        assert!(hint.contains(why) && hint.ends_with(instead), "{name}: {hint}");
        assert!(!hint.contains("--capabilities-from"), "{hint}");
    }
    // A name only close to them is an ordinary unknown name.
    assert!(catalog::declined("gpt-image-2.5").is_none());
}

#[test]
fn inputs_and_outputs_are_declared() {
    for m in openai::MODELS {
        let i = m.inputs;
        assert_eq!(i.max_input_images, 16);
        assert_eq!(i.input_media_types, ["image/png", "image/jpeg", "image/webp"]);
        assert_eq!(i.max_input_bytes, 15_700_000);
        let mask = i.mask.expect("GPT Image models accept a mask");
        assert_eq!(mask.media_types, ["image/png"]);
        assert_eq!(mask.max_bytes, 4_000_000, "\"less than 4MB\", read strictly");
        assert!(mask.requires_alpha && mask.same_size_as_first_image);
        assert!(i.max_request.is_none(), "no documented cap on the whole edit body");
        assert!(!i.first_frame && !i.last_frame);
        assert_eq!(i.max_reference_images, 0);
        assert_eq!(m.outputs.media_types, ["image/png", "image/jpeg", "image/webp"]);
        assert_eq!(m.outputs.max_count, 10);
        // A maximal input still fits the 20,971,520-character data URL.
        let url_chars = "data:image/jpeg;base64,".len() + 4 * (i.max_input_bytes as usize).div_ceil(3);
        assert!(url_chars <= 20_971_520, "{url_chars}");
    }
}

#[test]
fn declared_options_have_the_documented_names_kinds_defaults_and_operations() {
    for m in openai::MODELS {
        let names: Vec<&str> = m.options.iter().map(|o| o.name).collect();
        assert_eq!(names, ["count", "size", "quality", "format", "compression", "background", "moderation"]);
        let default = |n: &str| m.option(n).unwrap().default;
        assert_eq!(default("count"), Some("1"));
        assert_eq!(default("size"), Some("auto"));
        assert_eq!(default("quality"), Some("auto"));
        assert_eq!(default("format"), Some("png"));
        assert_eq!(default("compression"), Some("100"));
        assert_eq!(default("background"), Some("auto"));
        assert_eq!(default("moderation"), Some("auto"));
        assert!(matches!(m.option("count").unwrap().kind, OptionKind::Integer { min: 1, max: 10 }));
        assert!(matches!(m.option("compression").unwrap().kind, OptionKind::Integer { min: 0, max: 100 }));
        assert!(matches!(m.option("size").unwrap().kind, OptionKind::Pattern { .. }));
        let enum_values = |n: &str| match m.option(n).unwrap().kind {
            OptionKind::Enum(v) => v.to_vec(),
            other => panic!("{n} is {other:?}"),
        };
        assert_eq!(enum_values("format"), ["png", "jpeg", "webp"]);
        assert_eq!(enum_values("background"), ["transparent", "opaque", "auto"]);
        assert_eq!(enum_values("moderation"), ["low", "auto"]);
        let quality = enum_values("quality");
        if m.id == "gpt-image-2" {
            assert_eq!(quality, ["low", "medium", "high", "auto"]);
        } else {
            assert_eq!(quality, ["low", "medium", "high", "xhigh", "max", "auto"]);
        }
        for o in m.options {
            assert_eq!(o.operations, [Operation::ImageGenerate, Operation::ImageEdit], "{}", o.name);
            assert!(!o.description.is_empty(), "{}", o.name);
        }
    }
}

#[test]
fn every_declared_option_parses_its_own_default() {
    for m in openai::MODELS {
        for o in m.options {
            let d = o.default.unwrap_or_else(|| panic!("{}.{} has no default", m.id, o.name));
            OptionValue::parse(&o.kind, d)
                .unwrap_or_else(|e| panic!("{}.{} default {d:?}: {e}", m.id, o.name));
        }
    }
}

/// An option with a typed flag on the image commands must declare exactly that flag,
/// and an option without one must not be shadowed by a typed flag.
#[test]
fn typed_flags_follow_the_cli_flag_table() {
    for m in openai::MODELS {
        for o in m.options {
            let expected =
                iris::cli::args::IMAGE_FLAGS.iter().find(|(_, name)| *name == o.name).map(|(flag, _)| *flag);
            assert_eq!(o.flag, expected, "{}.{}", m.id, o.name);
        }
    }
}

#[test]
fn size_validator_accepts_and_rejects_per_the_documented_limits() {
    let accepted = [
        "auto",
        "1024x1024",
        "1536x1024",
        "1024x1536",
        "3840x2160",
        "2160x3840",
        "1408x480",  // exactly within 3:1
        "1440x480",  // exactly 3:1
        "816x816",   // cheapest square
        "1024x640",  // exactly 655,360 pixels
        "2880x2880", // exactly 8,294,400 pixels
    ];
    for size in accepted {
        assert_eq!(openai::validate_size(size), Ok(()), "{size}");
    }
    let rejected = [
        ("", "neither"),
        ("AUTO", "neither"),
        ("1024X1024", "neither"),
        ("1024x", "neither"),
        ("x1024", "neither"),
        ("1024*1024", "neither"),
        (" 1024x1024", "neither"),
        ("1024x1024 ", "neither"),
        ("+1024x1024", "neither"),
        ("01024x1024", "neither"),
        ("-16x1024", "neither"),
        ("1024x1024x1024", "neither"),
        ("1000x1000", "multiples of 16"),
        ("1024x1000", "multiples of 16"),
        ("3856x2160", "longer edge"),
        ("4096x2048", "longer edge"),
        ("1456x480", "aspect ratio"),
        ("2400x784", "aspect ratio"),
        ("1008x640", "pixels"), // 645,120 < 655,360
        ("512x512", "pixels"),
        ("2896x2880", "pixels"), // 8,340,480 > 8,294,400
        ("99999999x16", "neither"),
    ];
    for (size, why) in rejected {
        let err = openai::validate_size(size).expect_err(size);
        assert!(err.contains(why), "{size}: {err}");
    }
    // Through option validation: invalid_argument naming the syntax.
    let err = validate("gpt-image-2", Operation::ImageGenerate, &[("size", "1000x1000")]).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(
        err.message.contains("multiples of 16") && err.message.contains("auto or WxH"),
        "{}",
        err.message
    );
}

#[test]
fn cross_option_rules_reject_compression_with_png_and_transparency_with_jpeg() {
    for id in IDS {
        for op in [Operation::ImageGenerate, Operation::ImageEdit] {
            // compression needs jpeg/webp; the default format is png.
            let err = validate(id, op, &[("compression", "80")]).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument, "{id}");
            assert_eq!(err.details.get("option"), Some(&json!("compression")));
            let err = validate(id, op, &[("compression", "80"), ("format", "png")]).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
            assert!(validate(id, op, &[("compression", "80"), ("format", "jpeg")]).is_ok());
            assert!(validate(id, op, &[("compression", "0"), ("format", "webp")]).is_ok());
            // transparent needs png/webp; png is the default.
            let err = validate(id, op, &[("background", "transparent"), ("format", "jpeg")]).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidArgument);
            assert_eq!(err.details.get("option"), Some(&json!("background")));
            assert!(validate(id, op, &[("background", "transparent")]).is_ok());
            assert!(validate(id, op, &[("background", "transparent"), ("format", "webp")]).is_ok());
            assert!(validate(id, op, &[("background", "opaque"), ("format", "jpeg")]).is_ok());
        }
    }
}

#[test]
fn values_outside_the_declared_sets_and_undeclared_options_are_rejected_before_sending() {
    let invalid = [
        ("gpt-image-2", ("quality", "xhigh")),
        ("gpt-image-2", ("quality", "max")),
        ("gpt-image-2.5-flare", ("quality", "standard")),
        ("gpt-image-2.5-sunburst", ("quality", "hd")),
        ("gpt-image-2.5-sunburst", ("count", "0")),
        ("gpt-image-2.5-sunburst", ("count", "11")),
        ("gpt-image-2.5-sunburst", ("compression", "101")),
        ("gpt-image-2.5-sunburst", ("format", "gif")),
        ("gpt-image-2.5-sunburst", ("moderation", "high")),
        ("gpt-image-2.5-sunburst", ("background", "white")),
    ];
    for (id, pair) in invalid {
        let err = validate(id, Operation::ImageGenerate, &[pair]).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{id} {pair:?}");
    }
    assert!(validate("gpt-image-2.5-sunburst", Operation::ImageGenerate, &[("quality", "xhigh")]).is_ok());
    assert!(validate("gpt-image-2.5-flare", Operation::ImageGenerate, &[("quality", "max")]).is_ok());
    // Never sent: rejected as unsupported because they are not declared.
    for name in [
        "response_format",
        "style",
        "input_fidelity",
        "user",
        "stream",
        "partial_images",
        "seed",
        "aspect_ratio",
        "resolution",
        "negative_prompt",
    ] {
        let err = validate("gpt-image-2.5-sunburst", Operation::ImageEdit, &[(name, "x")]).unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedOption, "{name}");
    }
    let err = validate("gpt-image-2.5-sunburst", Operation::VideoGenerate, &[]).unwrap_err();
    assert_eq!(err.code, ErrorCode::UnsupportedOperation);
}

#[test]
fn edit_accepts_up_to_16_images_and_a_mask() {
    let spec = model("gpt-image-2.5-sunburst");
    let ok = InputCounts { images: 16, mask: true, ..Default::default() };
    assert!(
        catalog::validate_request(spec, Operation::ImageEdit, &[], ok, &catalog_support::builtin()).is_ok()
    );
    let too_many = InputCounts { images: 17, ..Default::default() };
    let err =
        catalog::validate_request(spec, Operation::ImageEdit, &[], too_many, &catalog_support::builtin())
            .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
}

#[test]
fn pricing_rules_carry_the_token_rates_source_and_date() {
    for m in openai::MODELS {
        let rules: Vec<(&str, f64)> = m.pricing.iter().map(|p| (p.unit, p.usd)).collect();
        assert_eq!(
            rules,
            [("1M text input tokens", 5.0), ("1M image input tokens", 8.0), ("1M image output tokens", 30.0)]
        );
        for p in m.pricing {
            assert_eq!(p.source_url, "https://developers.openai.com/api/docs/pricing");
            assert_eq!(p.as_of, "2026-09-24");
            assert_eq!(p.as_of, catalog::CATALOG_AS_OF);
        }
    }
}

#[test]
fn estimator_uses_the_calculator_formula_for_explicit_quality_and_size() {
    // 1024x1024 low on the 2.5 models: 196 output tokens ≈ $0.006 (OpenAI calculator).
    for id in ["gpt-image-2.5-sunburst", "gpt-image-2.5-flare"] {
        let e = estimate(id, &[("quality", "low"), ("size", "1024x1024")], 1).unwrap();
        assert!(e.estimated);
        assert_eq!(e.currency, "USD");
        assert_eq!(e.amount, 0.00588);
        assert!((e.amount - 0.006).abs() < 0.0005);
        assert_eq!(e.source_url, "https://developers.openai.com/api/docs/pricing");
        assert_eq!(e.as_of, "2026-09-24");
        assert!(e.basis.contains("196 output tokens") && e.basis.contains(id), "{}", e.basis);
        assert!(e.basis.starts_with("estimate"), "{}", e.basis);
    }
    // Count multiplies; other qualities and sizes follow the research table.
    assert_eq!(estimate(IDS[0], &[("quality", "low"), ("size", "1024x1024")], 3).unwrap().amount, 0.01764);
    assert_eq!(
        estimate(IDS[0], &[("quality", "low"), ("size", "1024x1024"), ("count", "2")], 2).unwrap().amount,
        0.01176
    );
    let tokens_2_5 = [
        ("low", "1536x1024", 158),
        ("low", "1024x640", 107),
        ("low", "2048x2048", 397),
        ("low", "3840x2160", 371),
        ("low", "1408x480", 54),
        ("low", "816x816", 171),
        ("medium", "1024x1024", 439),
        ("high", "1024x1024", 1756),
        ("xhigh", "1024x1024", 3122),
        ("max", "1024x1024", 7024),
        ("max", "2880x2880", 23719),
        ("xhigh", "3840x2160", 5930),
        // Exact halves round to the even neighbor, as in the calculator (values from the
        // an independent re-implementation of OpenAI's calculator).
        ("low", "2048x1600", 254),   // s = 12.5 → 12
        ("low", "672x1024", 108),    // s = 10.5 → 10
        ("low", "624x1536", 72),     // s = 6.5 → 6
        ("medium", "544x1536", 137), // s = 8.5 → 8
        ("high", "560x1536", 618),   // s = 17.5 → 18
    ];
    for (q, size, tokens) in tokens_2_5 {
        let e = estimate(IDS[1], &[("quality", q), ("size", size)], 1).unwrap();
        assert!(e.basis.contains(&format!("{tokens} output tokens")), "{q} {size}: {}", e.basis);
        assert_eq!(e.amount, (tokens as f64 * 30.0 / 1e6 * 1e6).round() / 1e6, "{q} {size}");
    }
    // GPT Image 2 has its own bases (medium 48, high 96): the published per-image prices.
    for (q, size, usd) in
        [("low", "1024x1024", 0.006), ("medium", "1024x1024", 0.053), ("high", "1536x1024", 0.165)]
    {
        let e = estimate("gpt-image-2", &[("quality", q), ("size", size)], 1).unwrap();
        assert_eq!((e.amount * 1000.0).round() / 1000.0, usd, "{q} {size}");
    }
}

/// With `auto` quality or size the model chooses, so there is no estimate; the
/// reason names only the options that are `auto`, with the values that give one.
#[test]
fn estimator_names_the_auto_options_to_pass_for_an_estimate() {
    for id in IDS {
        let qualities =
            if id == "gpt-image-2" { "low, medium, or high" } else { "low, medium, high, xhigh, or max" };
        let quality = format!("--quality ({qualities})");
        let size = "--size WIDTHxHEIGHT (such as 1024x1024)";
        let unknown = "and the cost is unknown before the call";
        let both = format!(
            "quality and size are auto, so the model chooses them {unknown}; pass {quality} and {size} for an \
             estimate"
        );
        assert_eq!(estimate(id, &[], 1).unwrap_err(), both, "{id}: the defaults are auto");
        assert_eq!(estimate(id, &[("quality", "auto"), ("size", "auto")], 1).unwrap_err(), both, "{id}");
        let size_auto =
            format!("size is auto, so the model chooses it {unknown}; pass {size} for an estimate");
        assert_eq!(estimate(id, &[("quality", "low")], 1).unwrap_err(), size_auto, "{id}");
        assert_eq!(estimate(id, &[("quality", "low"), ("size", "auto")], 1).unwrap_err(), size_auto, "{id}");
        let quality_auto =
            format!("quality is auto, so the model chooses it {unknown}; pass {quality} for an estimate");
        assert_eq!(estimate(id, &[("size", "1024x1024")], 1).unwrap_err(), quality_auto, "{id}");
        assert_eq!(
            estimate(id, &[("quality", "auto"), ("size", "1024x1024")], 1).unwrap_err(),
            quality_auto,
            "{id}"
        );
    }
}

/// Every GPT Image model is priced on one 1024x1024 image at every quality that has
/// an estimate, lowest quality first, by the calculator formula; the size the
/// requests give is read with the catalog's own size parser.
#[test]
fn the_standard_output_is_priced_at_every_quality_with_an_estimate() {
    for m in openai::MODELS {
        let gives = |options: &ResolvedOptions| {
            let size = m.effective(options, "size").unwrap();
            let (width, height) =
                openai::parse_size(size.as_str().unwrap()).unwrap_or_else(|| panic!("{size}"));
            StandardOutput::Image { width, height }
        };
        let amounts = catalog_support::assert_standard_requests_give_the_standard_output(m, gives);
        let OptionKind::Enum(qualities) = m.option("quality").unwrap().kind else {
            panic!("quality is an enum")
        };
        let with_an_estimate: Vec<&str> = qualities
            .iter()
            .copied()
            .filter(|quality| estimate(m.id, &[("quality", quality), ("size", "1024x1024")], 1).is_ok())
            .collect();
        let standard: Vec<&str> = m
            .estimate
            .unwrap()
            .standard
            .iter()
            .map(|set| set.iter().find(|(name, _)| *name == "quality").map(|(_, value)| *value).unwrap())
            .collect();
        assert_eq!(standard, with_an_estimate, "{}", m.id);
        assert!(amounts.windows(2).all(|pair| pair[0] < pair[1]), "{}: {amounts:?}", m.id);
    }
    let amounts = |id: &str| {
        let (_, estimates) = model(id).standard_cost().unwrap();
        estimates.iter().map(|(_, e)| e.amount).collect::<Vec<_>>()
    };
    // 196, 439, 1756, 3122, and 7024 output tokens at $30/1M.
    let tiers_2_5 = [0.00588, 0.01317, 0.05268, 0.09366, 0.21072];
    assert_eq!(amounts("gpt-image-2.5-sunburst"), tiers_2_5);
    assert_eq!(amounts("gpt-image-2.5-flare"), tiers_2_5);
    // 196, 1756, and 7024 output tokens: OpenAI publishes $0.006, $0.053, and $0.211.
    assert_eq!(amounts("gpt-image-2"), [0.00588, 0.05268, 0.21072]);
}

/// The `size` description says why a non-square size can cost less than a square
/// one, with the calculator's own token counts, and its claim holds for every valid
/// size and quality: a non-square size never needs more output tokens than a square
/// one with the same number of pixels (the formula's `u` is the base at 1:1).
#[test]
fn the_size_description_explains_that_a_non_square_size_can_need_fewer_tokens() {
    let low = openai::calculator_base(openai::CALCULATOR_BASE_2_5, "low").unwrap();
    assert_eq!(openai::calculator_base(openai::CALCULATOR_BASE_2, "low"), Some(low));
    let wide = openai::estimated_output_tokens(low, 1536, 1024);
    let square = openai::estimated_output_tokens(low, 1024, 1024);
    assert!(wide < square);
    for m in openai::MODELS {
        let description = m.option("size").unwrap().description;
        let words = description.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            words.contains(
                "By OpenAI's published calculator formula a non-square size never needs more output tokens \
                 than a square one with the same number of pixels, so a larger non-square size can cost less \
                 than a smaller square one"
            ),
            "{words}"
        );
        assert!(
            words.contains(&format!("(low: 1536x1024 is {wide} tokens, 1024x1024 is {square})")),
            "{words}"
        );
    }
    let bases = openai::CALCULATOR_BASE_2_5.iter().chain(openai::CALCULATOR_BASE_2).map(|(_, base)| *base);
    for base in bases {
        let square =
            |pixels: u64| (f64::from(base) * f64::from(base) * (2e6 + pixels as f64) / 4e6).ceil() as u64;
        for w in (16..=openai::SIZE_MAX_EDGE).step_by(16) {
            for h in (16..=openai::SIZE_MAX_EDGE).step_by(16) {
                if openai::validate_size(&format!("{w}x{h}")).is_ok() {
                    let tokens = openai::estimated_output_tokens(base, w, h);
                    assert!(tokens <= square(w * h), "base {base}: {w}x{h} needs {tokens}");
                }
            }
        }
    }
}

/// The summary of GPT Image 2 compares its output tokens with the 2.5 models' by
/// OpenAI's calculator: equal at low, about four times as many at medium and high.
#[test]
fn gpt_image_2_needs_about_four_times_the_output_tokens_at_medium_and_high() {
    for (w, h) in [(1024, 1024), (1536, 1024), (1024, 1536), (1440, 480), (2048, 2048), (3840, 2160)] {
        let tokens = |table, quality| {
            openai::estimated_output_tokens(openai::calculator_base(table, quality).unwrap(), w, h)
        };
        assert_eq!(tokens(openai::CALCULATOR_BASE_2, "low"), tokens(openai::CALCULATOR_BASE_2_5, "low"));
        for quality in ["medium", "high"] {
            let ratio = tokens(openai::CALCULATOR_BASE_2, quality) as f64
                / tokens(openai::CALCULATOR_BASE_2_5, quality) as f64;
            assert!((3.5..=4.5).contains(&ratio), "{quality} {w}x{h}: {ratio}");
        }
    }
}

#[test]
fn calculator_reproduces_all_nine_published_gpt_image_2_prices() {
    let published = [
        ("low", 1024, 1024, 0.006),
        ("low", 1024, 1536, 0.005),
        ("low", 1536, 1024, 0.005),
        ("medium", 1024, 1024, 0.053),
        ("medium", 1024, 1536, 0.041),
        ("medium", 1536, 1024, 0.041),
        ("high", 1024, 1024, 0.211),
        ("high", 1024, 1536, 0.165),
        ("high", 1536, 1024, 0.165),
    ];
    for (q, w, h, usd) in published {
        let base = openai::calculator_base(openai::CALCULATOR_BASE_2, q).unwrap();
        let tokens = openai::estimated_output_tokens(base, w, h);
        let price = tokens as f64 * openai::IMAGE_OUTPUT_USD_PER_M / 1e6;
        assert_eq!((price * 1000.0).round() / 1000.0, usd, "{q} {w}x{h}: {tokens} tokens");
    }
    assert!(openai::calculator_base(openai::CALCULATOR_BASE_2, "xhigh").is_none());
    assert!(openai::calculator_base(openai::CALCULATOR_BASE_2_5, "auto").is_none());
}

#[test]
fn cost_from_usage_prices_text_image_and_output_tokens() {
    let spec = model(IDS[0]);
    let usage = Usage {
        input_tokens: Some(24),
        output_tokens: Some(196),
        total_tokens: Some(220),
        provider_usage: Some(json!({
            "input_tokens": 24, "output_tokens": 196, "total_tokens": 220,
            "input_tokens_details": {"text_tokens": 24, "image_tokens": 0}
        })),
    };
    let e = openai::cost_from_usage(spec, &usage).unwrap();
    assert_eq!(e.amount, 0.006); // 24 × $5/1M + 196 × $30/1M = 0.00012 + 0.00588
    assert!(e.estimated && e.basis.contains("reported usage"), "{}", e.basis);

    // An edit: 1,000 image input tokens at $8/1M on top.
    let edit = Usage {
        provider_usage: Some(json!({"input_tokens_details": {"text_tokens": 24, "image_tokens": 1000}})),
        ..usage.clone()
    };
    assert_eq!(openai::cost_from_usage(spec, &edit).unwrap().amount, 0.014);

    // No split: all input priced at the image rate (upper bound), and said so.
    let no_split = Usage { provider_usage: None, input_tokens: Some(1000), ..usage.clone() };
    let e = openai::cost_from_usage(spec, &no_split).unwrap();
    assert_eq!(e.amount, 0.01388);
    assert!(e.basis.contains("upper bound"), "{}", e.basis);

    // No output tokens: nothing to estimate.
    assert!(openai::cost_from_usage(spec, &Usage { output_tokens: None, ..usage }).is_none());
}

/// The rules `validate_options` enforces are exactly the constraints `models show`
/// publishes (checked over every combination of declared values).
#[test]
fn every_cross_option_rule_is_a_declared_constraint() {
    for m in openai::MODELS {
        catalog_support::assert_constraints_cover_the_validator(m);
        let ids: Vec<&str> = m.validate.unwrap().constraints.iter().map(|c| c.id).collect();
        assert_eq!(ids, ["compression_requires_jpeg_or_webp", "transparent_background_requires_png_or_webp"]);
    }
}
