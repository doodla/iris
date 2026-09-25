//! Catalog declarations and cross-field validators for Veo.
//! Offline; no credentials.

use iris::catalog::{
    self, CATALOG_AS_OF, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionKind, OptionSource,
    RawOption, validate_request,
};
use iris::domain::{Operation, ProviderId};
use iris::error::ErrorCode;

#[path = "catalog_support.rs"]
mod catalog_support;

const FAST: &str = "veo-3.1-fast-generate-preview";
const STANDARD: &str = "veo-3.1-generate-preview";
const LITE: &str = "veo-3.1-lite-generate-preview";

fn spec(id: &str) -> &'static ModelSpec {
    catalog::find(id).unwrap_or_else(|| panic!("{id} is in the catalog"))
}

#[derive(Default, Clone, Copy)]
struct Inputs {
    first: bool,
    last: bool,
    refs: usize,
}

fn validate(id: &str, opts: &[(&str, &str)], inputs: Inputs) -> Result<(), ErrorCode> {
    let raw: Vec<RawOption> = opts
        .iter()
        .map(|(n, v)| RawOption { name: n.to_string(), value: v.to_string(), source: OptionSource::Generic })
        .collect();
    let counts = InputCounts {
        first_frame: inputs.first,
        last_frame: inputs.last,
        references: inputs.refs,
        ..InputCounts::default()
    };
    validate_request(spec(id), Operation::VideoGenerate, &raw, counts).map(|_| ()).map_err(|e| e.code)
}

fn text() -> Inputs {
    Inputs::default()
}

fn first_frame() -> Inputs {
    Inputs { first: true, ..Inputs::default() }
}

fn refs(n: usize) -> Inputs {
    Inputs { refs: n, ..Inputs::default() }
}

fn estimate_usd(id: &str, opts: &[(&str, &str)]) -> f64 {
    let s = spec(id);
    let raw: Vec<RawOption> = opts
        .iter()
        .map(|(n, v)| RawOption { name: n.to_string(), value: v.to_string(), source: OptionSource::Generic })
        .collect();
    let options = validate_request(s, Operation::VideoGenerate, &raw, InputCounts::default()).unwrap();
    let e = (s.estimate.unwrap())(
        s,
        &EstimateInput { operation: Operation::VideoGenerate, options: &options, count: 1 },
    )
    .expect("estimate");
    assert!(e.estimated);
    assert_eq!(e.source_url, "https://ai.google.dev/gemini-api/docs/pricing");
    assert_eq!(e.as_of, CATALOG_AS_OF);
    assert!(e.basis.contains("blocked videos are not charged"), "{}", e.basis);
    e.amount
}

#[test]
fn the_three_veo_31_preview_models_are_declared() {
    let ids: Vec<&str> = catalog::veo::MODELS.iter().map(|m| m.id).collect();
    assert_eq!(ids, [FAST, STANDARD, LITE]);
    for m in catalog::veo::MODELS {
        assert_eq!(m.provider, ProviderId::Gemini);
        assert_eq!(m.lifecycle, Lifecycle::Preview);
        assert_eq!(m.operations, &[Operation::VideoGenerate]);
        assert_eq!(m.outputs.media_types, &["video/mp4"]);
        assert_eq!(m.outputs.max_count, 1);
        assert_eq!(m.limits.max_prompt_chars, Some(16_384));
        assert_eq!(m.docs_url, "https://ai.google.dev/gemini-api/docs/video");
        assert!(m.inputs.first_frame && m.inputs.last_frame && m.inputs.mask.is_none());
        assert_eq!(m.inputs.input_media_types, &["image/png", "image/jpeg"]);
        assert_eq!(m.inputs.max_input_bytes, 20_000_000);
        // The billing and key notes are the Gemini image models' (one wording for both catalogs).
        for note in [
            "Preview model",
            catalog::gemini::ACCESS_NOTE_PAID_TIER,
            catalog::gemini::ACCESS_NOTE_AUTH_KEY,
            "Audio is always generated and cannot be disabled",
            "Generated videos are deleted by the provider after 2 days; download before then",
            "No remote cancellation (the provider offers none for Veo operations)",
        ] {
            assert!(m.access_notes.contains(&note), "{}: {note}", m.id);
        }
        // Reference images (referenceType "ASSET") were confirmed live on Veo 3.1 Fast.
        assert!(!m.access_notes.iter().any(|n| n.contains("referenceType")), "{}", m.id);
        assert!(m.access_notes.iter().any(|n| n.contains("provider support is unverified")), "{}", m.id);
    }
    assert_eq!(spec(FAST).display_name, "Veo 3.1 Fast");
    assert_eq!(spec("veo-fast").id, FAST);
    assert_eq!(spec("veo").id, STANDARD);
    assert_eq!(spec("veo-lite").id, LITE);
    assert_eq!(spec(FAST).inputs.max_reference_images, 3);
    assert_eq!(spec(STANDARD).inputs.max_reference_images, 3);
    assert_eq!(spec(LITE).inputs.max_reference_images, 0);
    assert!(
        !spec(FAST).access_notes.iter().any(|n| n.contains("Reference images") && n.contains("unverified"))
    );
    assert!(spec(FAST).access_notes.iter().any(|n| n.contains("extension")));
}

#[test]
fn fast_is_the_video_default_and_shut_down_ids_are_unknown() {
    assert_eq!(catalog::default_model(ProviderId::Gemini, Operation::VideoGenerate).unwrap().id, FAST);
    assert_eq!(catalog::providers_for(Operation::VideoGenerate), [ProviderId::Gemini]);
    for gone in
        ["veo-2.0-generate-001", "veo-3.0-generate-001", "veo-3.0-fast-generate-001", "veo-3.1-generate-001"]
    {
        assert_eq!(catalog::resolve(gone, None, None).unwrap_err().code, ErrorCode::UnknownModel, "{gone}");
    }
}

#[test]
fn options_and_defaults_match_the_contract() {
    for m in catalog::veo::MODELS {
        let names: Vec<&str> = m.options.iter().map(|o| o.name).collect();
        // Veo 3.1 Lite refuses a negative prompt (seen live).
        let expected: &[&str] = if m.id == LITE {
            &["count", "duration", "resolution", "aspect_ratio", "person_generation"]
        } else {
            &["count", "duration", "resolution", "aspect_ratio", "negative_prompt", "person_generation"]
        };
        assert_eq!(names, expected, "{}", m.id);
        let o = |n: &str| m.option(n).unwrap();
        assert!(matches!(o("count").kind, OptionKind::Integer { min: 1, max: 1 }));
        assert!(matches!(o("duration").kind, OptionKind::Enum(v) if v == ["4", "6", "8"]));
        assert_eq!(o("duration").default, Some("8"));
        assert_eq!(o("resolution").default, Some("720p"));
        assert!(matches!(o("aspect_ratio").kind, OptionKind::Enum(v) if v == ["16:9", "9:16"]));
        assert_eq!(o("aspect_ratio").default, Some("16:9"));
        if m.id != LITE {
            assert!(matches!(o("negative_prompt").kind, OptionKind::Text { max_chars: 4000 }));
            assert_eq!(o("negative_prompt").default, None);
        }
        assert!(
            matches!(o("person_generation").kind, OptionKind::Enum(v) if v == ["allow_all", "allow_adult"])
        );
        assert_eq!(o("person_generation").default, None);
        assert!(o("duration").description.contains("Audio is always generated"));
    }
    let res = |id: &str| match spec(id).option("resolution").unwrap().kind {
        OptionKind::Enum(v) => v.to_vec(),
        _ => unreachable!(),
    };
    assert_eq!(res(FAST), ["720p", "1080p", "4k"]);
    assert_eq!(res(STANDARD), ["720p", "1080p", "4k"]);
    assert_eq!(res(LITE), ["720p", "1080p"]);
}

#[test]
fn undeclared_options_are_rejected_including_audio_and_seed() {
    for (name, value) in [("audio", "false"), ("seed", "1"), ("fps", "24"), ("enhance_prompt", "true")] {
        assert_eq!(validate(FAST, &[(name, value)], text()), Err(ErrorCode::UnsupportedOption), "{name}");
    }
    assert_eq!(validate(FAST, &[("duration", "5")], text()), Err(ErrorCode::InvalidArgument));
    assert_eq!(validate(FAST, &[("count", "2")], text()), Err(ErrorCode::InvalidArgument));
    assert_eq!(
        validate(FAST, &[("negative_prompt", &"x".repeat(4001))], text()),
        Err(ErrorCode::InvalidArgument)
    );
}

#[test]
fn high_resolutions_require_eight_seconds() {
    assert_eq!(
        validate(FAST, &[("resolution", "1080p"), ("duration", "4")], text()),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        validate(FAST, &[("resolution", "4k"), ("duration", "6")], text()),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(validate(FAST, &[("resolution", "1080p")], text()), Ok(()), "default duration is 8");
    assert_eq!(validate(STANDARD, &[("resolution", "4k"), ("duration", "8")], text()), Ok(()));
    assert_eq!(validate(LITE, &[("resolution", "720p"), ("duration", "4")], text()), Ok(()));
}

#[test]
fn reference_images_require_eight_seconds_and_forbid_frames() {
    assert_eq!(validate(FAST, &[], refs(3)), Ok(()));
    assert_eq!(validate(STANDARD, &[("duration", "8")], refs(1)), Ok(()));
    assert_eq!(validate(FAST, &[("duration", "6")], refs(1)), Err(ErrorCode::InvalidArgument));
    assert_eq!(validate(FAST, &[], refs(4)), Err(ErrorCode::InvalidArgument));
    let refs_and_frame = Inputs { first: true, refs: 1, ..Inputs::default() };
    assert_eq!(validate(FAST, &[], refs_and_frame), Err(ErrorCode::InvalidArgument));
    let refs_and_last = Inputs { last: true, refs: 1, ..Inputs::default() };
    assert_eq!(validate(FAST, &[], refs_and_last), Err(ErrorCode::InvalidArgument));
}

#[test]
fn last_frame_requires_a_first_frame() {
    let last_only = Inputs { last: true, ..Inputs::default() };
    assert_eq!(validate(LITE, &[], last_only), Err(ErrorCode::InvalidArgument));
    let both = Inputs { first: true, last: true, ..Inputs::default() };
    assert_eq!(validate(LITE, &[("duration", "4")], both), Ok(()));
}

#[test]
fn person_generation_depends_on_the_mode() {
    assert_eq!(validate(FAST, &[("person_generation", "allow_all")], text()), Ok(()));
    assert_eq!(
        validate(FAST, &[("person_generation", "allow_adult")], text()),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(validate(FAST, &[("person_generation", "allow_adult")], first_frame()), Ok(()));
    assert_eq!(validate(FAST, &[("person_generation", "allow_adult")], refs(1)), Ok(()));
    assert_eq!(
        validate(FAST, &[("person_generation", "allow_all")], first_frame()),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(
        validate(FAST, &[("person_generation", "allow_all")], refs(2)),
        Err(ErrorCode::InvalidArgument)
    );
    assert_eq!(validate(FAST, &[("person_generation", "everyone")], text()), Err(ErrorCode::InvalidArgument));
}

#[test]
fn lite_rejects_4k_and_reference_images() {
    assert_eq!(validate(LITE, &[("resolution", "4k")], text()), Err(ErrorCode::InvalidArgument));
    assert_eq!(validate(LITE, &[], refs(1)), Err(ErrorCode::UnsupportedOption));
    assert_eq!(validate(LITE, &[("resolution", "1080p")], text()), Ok(()));
}

#[test]
fn estimates_are_duration_times_the_rate_for_the_resolution() {
    assert!((estimate_usd(LITE, &[("resolution", "720p"), ("duration", "4")]) - 0.20).abs() < 1e-9);
    assert!((estimate_usd(LITE, &[("duration", "8"), ("resolution", "1080p")]) - 0.64).abs() < 1e-9);
    assert!((estimate_usd(FAST, &[]) - 0.80).abs() < 1e-9, "defaults: 8 s at 720p");
    assert!((estimate_usd(FAST, &[("resolution", "4k")]) - 2.40).abs() < 1e-9);
    assert!((estimate_usd(STANDARD, &[("duration", "4")]) - 1.60).abs() < 1e-9);
    assert!((estimate_usd(STANDARD, &[("resolution", "4k")]) - 4.80).abs() < 1e-9);
}

#[test]
fn pricing_is_per_second_with_source_and_date() {
    let usd = |id: &str| spec(id).pricing.iter().map(|p| p.usd).collect::<Vec<_>>();
    assert_eq!(usd(STANDARD), [0.40, 0.40, 0.60]);
    assert_eq!(usd(FAST), [0.10, 0.12, 0.30]);
    assert_eq!(usd(LITE), [0.05, 0.08]);
    for m in catalog::veo::MODELS {
        for p in m.pricing {
            assert_eq!(p.unit, "second");
            assert_eq!(p.source_url, "https://ai.google.dev/gemini-api/docs/pricing");
            assert_eq!(p.as_of, CATALOG_AS_OF);
            assert!(p.description.contains("audio"));
        }
    }
}

/// The rules `validate_video` enforces are exactly the constraints `models show`
/// publishes for each model (checked over every combination of declared values and
/// inputs); Lite, which takes no reference images, publishes no reference rules.
#[test]
fn every_cross_option_rule_is_a_declared_constraint() {
    for m in catalog::veo::MODELS {
        catalog_support::assert_constraints_cover_the_validator(m);
    }
    let ids = |id: &str| spec(id).validate.unwrap().constraints.iter().map(|c| c.id).collect::<Vec<_>>();
    let full = [
        "high_resolution_requires_duration_8",
        "references_exclude_frames",
        "references_require_duration_8",
        "negative_prompt_excludes_references",
        "last_frame_requires_first_frame",
        "person_generation_depends_on_image_inputs",
    ];
    assert_eq!(ids(FAST), full);
    assert_eq!(ids(STANDARD), full);
    assert_eq!(
        ids(LITE),
        [
            "high_resolution_requires_duration_8",
            "last_frame_requires_first_frame",
            "person_generation_depends_on_image_inputs"
        ]
    );
}

/// Both rules come from live requests: Veo 3.1 Lite answers that `negativePrompt`
/// "isn't supported by this model", and Veo 3.1 Fast refuses a negative prompt next
/// to a reference image while accepting it for text-to-video. Iris refuses both
/// locally, before anything is sent.
#[test]
fn negative_prompts_are_refused_where_the_provider_refuses_them() {
    assert_eq!(validate(LITE, &[("negative_prompt", "text")], text()), Err(ErrorCode::UnsupportedOption));
    assert_eq!(
        validate(LITE, &[("negative_prompt", "text")], first_frame()),
        Err(ErrorCode::UnsupportedOption)
    );
    for id in [FAST, STANDARD] {
        assert_eq!(validate(id, &[("negative_prompt", "text")], text()), Ok(()), "{id}");
        assert_eq!(validate(id, &[("negative_prompt", "text")], first_frame()), Ok(()), "{id}");
        assert_eq!(
            validate(id, &[("negative_prompt", "text"), ("duration", "8")], refs(1)),
            Err(ErrorCode::InvalidArgument),
            "{id}"
        );
        assert_eq!(validate(id, &[("duration", "8")], refs(1)), Ok(()), "{id}");
    }
}
