//! Catalog declarations for the Vercel AI Gateway image models. Offline; no credentials.

use iris::catalog::{
    self, EstimateInput, InputCounts, Lifecycle, ModelSpec, OptionSource, RawOption, ResolvedOptions,
    StandardOutput, validate_request,
};
use iris::domain::{Operation, ProviderId};
use iris::error::ErrorCode;

#[path = "catalog_support.rs"]
mod catalog_support;

const FLUX: &str = "bfl/flux-3-image";
const KLEIN: &str = "bfl/flux-3-image-klein";

fn spec(id: &str) -> &'static ModelSpec {
    catalog::find(id).unwrap_or_else(|| panic!("{id} is in the catalog"))
}

fn raw(name: &str, value: &str) -> RawOption {
    let source = iris::cli::args::IMAGE_FLAGS
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
    validate_request(
        spec(id),
        op,
        &raw,
        InputCounts { images, ..InputCounts::default() },
        &catalog_support::builtin(),
    )
    .map_err(|e| e.code)
}

fn estimate(id: &str, opts: &[(&str, &str)]) -> iris::domain::CostEstimate {
    let s = spec(id);
    let options = validate(id, Operation::ImageGenerate, opts, 0).expect("valid options");
    (s.estimate.unwrap().estimate)(
        s,
        &EstimateInput { operation: Operation::ImageGenerate, options: &options, count: 1 },
    )
    .expect("an estimate")
}

#[test]
fn the_two_flux_3_models_are_declared_for_generation_only() {
    let ids: Vec<&str> = catalog::vercel::MODELS.iter().map(|m| m.id).collect();
    assert_eq!(ids, [FLUX, KLEIN]);
    let all: Vec<&str> = catalog::all().filter(|m| m.provider == ProviderId::Vercel).map(|m| m.id).collect();
    assert_eq!(all, ids);
    for m in catalog::vercel::MODELS {
        assert_eq!(m.lifecycle, Lifecycle::Ga);
        assert_eq!(m.operations, &[Operation::ImageGenerate]);
        assert_eq!(m.inputs.max_input_images, 0);
        assert_eq!(m.outputs.max_count, 1);
        assert!(m.estimate_usage.is_none(), "the endpoint documents no usage");
        for p in m.pricing {
            assert_eq!(p.source_url, "https://ai-gateway.vercel.sh/v1/models");
            assert_eq!(p.as_of, "2026-11-14");
        }
    }
}

#[test]
fn typed_flags_follow_the_cli_flag_table() {
    for m in catalog::vercel::MODELS {
        for o in m.options {
            let expected = iris::cli::args::IMAGE_FLAGS.iter().find(|(_, n)| *n == o.name).map(|(f, _)| *f);
            assert_eq!(o.flag, expected, "{} option {}", m.id, o.name);
        }
    }
}

#[test]
fn only_the_five_priced_square_sizes_and_one_image_are_accepted() {
    for id in [FLUX, KLEIN] {
        for size in ["768x768", "1024x1024", "1536x1536", "2048x2048", "4096x4096"] {
            assert!(validate(id, Operation::ImageGenerate, &[("size", size)], 0).is_ok(), "{id} {size}");
        }
        for size in ["1024x768", "auto", "512x512"] {
            assert_eq!(
                validate(id, Operation::ImageGenerate, &[("size", size)], 0),
                Err(ErrorCode::InvalidArgument)
            );
        }
        assert!(validate(id, Operation::ImageGenerate, &[("count", "1")], 0).is_ok());
        assert_eq!(
            validate(id, Operation::ImageGenerate, &[("count", "2")], 0),
            Err(ErrorCode::InvalidArgument)
        );
        for option in [("quality", "high"), ("format", "png")] {
            assert!(validate(id, Operation::ImageGenerate, &[option], 0).is_err(), "{id} {option:?}");
        }
        assert!(validate(id, Operation::ImageEdit, &[], 1).is_err(), "{id} has no edit");
    }
}

#[test]
fn estimates_use_the_listed_price_of_the_size_or_the_base_price() {
    let cases = [
        (FLUX, Some("768x768"), 0.041),
        (FLUX, Some("1024x1024"), 0.048),
        (FLUX, Some("4096x4096"), 0.607),
        (FLUX, None, 0.048),
        (KLEIN, Some("1536x1536"), 0.011),
        (KLEIN, None, 0.009),
    ];
    for (id, size, usd) in cases {
        let opts: Vec<(&str, &str)> = size.map(|s| ("size", s)).into_iter().collect();
        let e = estimate(id, &opts);
        assert!((e.amount - usd).abs() < 1e-9, "{id} {size:?}: {}", e.amount);
        assert!(e.estimated);
        assert_eq!(e.currency, "USD");
        assert!(e.basis.contains(size.unwrap_or("size not given")), "{}", e.basis);
    }
}

#[test]
fn the_standard_request_gives_one_1024x1024_image() {
    for m in catalog::vercel::MODELS {
        let gives = |o: &ResolvedOptions| match o.get("size").and_then(|v| v.as_str()) {
            Some("1024x1024") => StandardOutput::Image { width: 1024, height: 1024 },
            other => panic!("{}: no standard output for size {other:?}", m.id),
        };
        catalog_support::assert_standard_requests_give_the_standard_output(m, gives);
    }
}

#[test]
fn models_show_reports_the_day_the_vercel_entries_were_checked() {
    assert_eq!(catalog::checked_on(ProviderId::Vercel), "2026-11-14");
    assert_eq!(catalog::checked_on(ProviderId::OpenAi), catalog::CATALOG_AS_OF);
}
