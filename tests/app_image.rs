//! `image generate` / `image edit` workflows with fake providers and a fake
//! catalog: validation before any request, saving and validating paid output,
//! the rename fallback, cost estimates, and dry runs.

#[path = "app_support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use iris::app::image::{self, ImageArgs};
use iris::app::{AppContext, Catalog, GenerationArgs, GenerationOutcome, Interrupt, Progress};
use iris::catalog::{ModelSpec, OptionSource, OptionValue, RawOption};
use iris::config::{Resolved, SettingSource};
use iris::domain::{ModelSource, Operation, ProviderId, Usage, Warning, WarningCode};
use iris::error::{ErrorCode, IrisError};
use iris::output::results::{ImageResult, PlanResult};
use iris::providers::{GeneratedImage, ImageFailure, ImageOutput, UnusableOutput};
use support::*;

/// A request for the OpenAI-like fake model (`-m fake-image-1`).
fn args(prompt: &str) -> ImageArgs {
    ImageArgs {
        common: GenerationArgs {
            prompt: prompt.to_string(),
            model: Some("fake-image-1".into()),
            ..GenerationArgs::default()
        },
        ..ImageArgs::default()
    }
}

fn flag(name: &str, value: &str, flag: &'static str) -> RawOption {
    RawOption { name: name.into(), value: value.into(), source: OptionSource::Flag(flag) }
}

fn generic(name: &str, value: &str) -> RawOption {
    RawOption { name: name.into(), value: value.into(), source: OptionSource::Generic }
}

fn completed(outcome: GenerationOutcome<ImageResult>) -> ImageResult {
    match outcome {
        GenerationOutcome::Completed(r) => r,
        GenerationOutcome::Planned(_) => panic!("expected a completed result"),
    }
}

fn planned(outcome: GenerationOutcome<ImageResult>) -> PlanResult {
    match outcome {
        GenerationOutcome::Planned(p) => p,
        GenerationOutcome::Completed(_) => panic!("expected a plan"),
    }
}

struct Fixture {
    sandbox: Sandbox,
    openai: Arc<FakeProvider>,
    gemini: Arc<FakeProvider>,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            sandbox: Sandbox::new(),
            openai: Arc::new(FakeProvider::openai()),
            gemini: Arc::new(FakeProvider::gemini()),
        }
    }

    async fn run(
        &self,
        op: Operation,
        a: ImageArgs,
    ) -> (Result<GenerationOutcome<ImageResult>, IrisError>, Vec<Warning>) {
        let ctx = context(settings(&self.sandbox.env()), vec![self.openai.clone(), self.gemini.clone()]);
        let mut warnings = Vec::new();
        let r = image::run(&ctx, op, a, &mut warnings).await;
        (r, warnings)
    }

    fn calls(&self) -> usize {
        self.openai.images().calls.load(Ordering::SeqCst) + self.gemini.images().calls.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn reported_usage_takes_precedence_over_the_pre_call_estimate() {
    let f = Fixture::new();
    let mut out = image_output(vec![png(8, 8)]);
    out.usage = Some(Usage { output_tokens: Some(250), ..Default::default() });
    f.openai.images().push(Ok(out));
    let mut a = args("a red kite");
    a.common.options = vec![flag("quality", "low", "--quality")];
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let cost = completed(r.unwrap()).cost_estimate.expect("estimate");
    assert!(cost.estimated);
    assert!((cost.amount - 0.25).abs() < 1e-9, "usage-based, not the $0.01 pre-call estimate");
    assert!(cost.basis.contains("from usage"));
    assert!(!has_warning(&warnings, "cost_estimate_unavailable"));
}

#[tokio::test]
async fn generate_saves_every_image_validated_with_absolute_paths_and_an_estimate() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(16, 8), png(4, 4)])));
    let mut a = args("a red kite");
    a.common.options = vec![flag("count", "2", "--count"), flag("quality", "low", "--quality")];
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.provider, ProviderId::OpenAi);
    assert_eq!(res.model, "fake-image-1");
    assert_eq!(res.artifacts.len(), 2);
    for (i, art) in res.artifacts.iter().enumerate() {
        let path = PathBuf::from(&art.path);
        assert!(path.is_absolute());
        assert_eq!(path.parent().unwrap(), f.sandbox.work());
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("iris-") && name.ends_with(&format!("-{}.png", i + 1)), "{name}");
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(art.bytes, bytes.len() as u64);
        assert_eq!(art.sha256, iris::artifacts::sha256_bytes(&bytes));
        assert_eq!(art.media_type, "image/png");
        assert_eq!(art.index, i as u32);
    }
    assert_eq!((res.artifacts[0].width, res.artifacts[0].height), (Some(16), Some(8)));
    assert_eq!(res.provider_request_id.as_deref(), Some("req_fake_1"));
    let cost = res.cost_estimate.expect("estimate");
    assert!(cost.estimated);
    assert!((cost.amount - 0.02).abs() < 1e-9);
    assert!(!has_warning(&warnings, "cost_estimate_unavailable"));

    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(req.model, "fake-image-1");
    assert_eq!(req.prompt, "a red kite");
    assert_eq!(req.options.get("count"), Some(&OptionValue::Int(2)));
    assert_eq!(req.options.get("quality"), Some(&OptionValue::Str("low".into())));
    assert!(!req.options.contains("size"), "omitted options are not sent");
    assert!(f.sandbox.state().read_dir().unwrap().next().is_none(), "no job record for sync images");
}

#[tokio::test]
async fn a_file_that_appears_after_preflight_is_kept_and_the_output_is_renamed() {
    let f = Fixture::new();
    let target = f.sandbox.path("out.png");
    let t = target.clone();
    *f.openai.images().on_call.lock().unwrap() =
        Some(Box::new(move || std::fs::write(&t, b"someone else").unwrap()));
    let mut a = args("race");
    a.common.output = Some(target.clone());
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else", "the other file is untouched");
    assert_eq!(res.artifacts[0].path, f.sandbox.path("out.1.png").to_str().unwrap());
    assert!(has_warning(&warnings, "output_renamed"));
}

#[tokio::test]
async fn existing_outputs_are_refused_before_any_request_unless_overwrite() {
    let f = Fixture::new();
    let target = f.sandbox.path("out.png");
    std::fs::write(&target, b"old").unwrap();
    let mut a = args("x");
    a.common.output = Some(target.clone());
    let (r, _) = f.run(Operation::ImageGenerate, a.clone()).await;
    assert_eq!(err_code(r), ErrorCode::OutputExists);
    assert_eq!(f.calls(), 0);

    a.common.overwrite = true;
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.artifacts[0].path, target.to_str().unwrap());
    assert_ne!(std::fs::read(&target).unwrap(), b"old");
}

#[tokio::test]
async fn unsupported_and_invalid_options_fail_locally_with_the_flag_named() {
    let f = Fixture::new();
    let cases: Vec<(Vec<RawOption>, ErrorCode, &str)> = vec![
        (vec![generic("nope", "1")], ErrorCode::UnsupportedOption, "-O nope"),
        (vec![flag("duration", "4", "--duration")], ErrorCode::UnsupportedOption, "--duration"),
        (vec![flag("quality", "ultra", "--quality")], ErrorCode::InvalidArgument, "--quality"),
        (vec![flag("count", "9", "--count")], ErrorCode::InvalidArgument, "--count"),
        (
            vec![generic("background", "opaque"), generic("background", "auto")],
            ErrorCode::InvalidArgument,
            "more than once",
        ),
    ];
    for (options, code, needle) in cases {
        let mut a = args("x");
        a.common.options = options;
        let (r, _) = f.run(Operation::ImageGenerate, a).await;
        let e = r.unwrap_err();
        assert_eq!(e.code, code, "{needle}: {}", e.message);
        assert!(e.message.contains(needle), "{needle}: {}", e.message);
    }
    // Unsupported operation input: a mask on a model without mask support.
    let input = f.sandbox.path("in.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    let mut a = args("x");
    a.common.model = Some("fake-gemini-image".into());
    a.images = vec![input.clone()];
    a.mask = Some(input);
    let (r, _) = f.run(Operation::ImageEdit, a).await;
    assert_eq!(err_code(r), ErrorCode::UnsupportedOption);
    assert_eq!(f.calls(), 0, "nothing was sent");
}

#[tokio::test]
async fn missing_credentials_are_reported_after_all_local_validation() {
    let f = Fixture::new();
    let ctx = context(settings(&f.sandbox.env_without_keys()), vec![f.openai.clone()]);
    let mut w = Vec::new();
    let mut a = args("x");
    a.common.options = vec![generic("nope", "1")];
    assert_eq!(
        err_code(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await),
        ErrorCode::UnsupportedOption
    );

    let e = image::run(&ctx, Operation::ImageGenerate, args("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    assert_eq!(e.exit_code(), 3);
    assert!(e.message.contains("OPENAI_API_KEY"));
    assert_eq!(f.calls(), 0);

    // A missing key creates no output directory ...
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("new/deeper/x.png"));
    assert_eq!(
        err_code(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await),
        ErrorCode::MissingCredentials
    );
    assert!(!f.sandbox.path("new").exists(), "nothing is created before the credential check");
    // ... while an unusable output location is still reported first, as local validation.
    std::fs::write(f.sandbox.path("blocker"), b"file").unwrap();
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("blocker/x.png"));
    let code = err_code(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await);
    assert_ne!(code, ErrorCode::MissingCredentials, "the output location is checked before the key");
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn dry_run_plans_without_credentials_files_or_requests() {
    let f = Fixture::new();
    let ctx = context(settings(&f.sandbox.env_without_keys()), vec![f.openai.clone()]);
    let mut w = Vec::new();
    let mut a = args("x");
    a.common.dry_run = true;
    a.common.options = vec![flag("quality", "high", "--quality"), flag("count", "3", "--count")];
    a.common.output = Some(f.sandbox.path("sub/dir/pic.webp"));
    let plan = planned(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await.unwrap());
    assert!(plan.dry_run);
    assert!(!plan.async_job);
    assert!(!plan.credential_present);
    assert_eq!(plan.model, "fake-image-1");
    assert_eq!(
        plan.outputs,
        (1..=3)
            .map(|i| f.sandbox.path(&format!("sub/dir/pic-{i}.webp")).display().to_string())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        plan.options.get("format").and_then(|v| v.as_str()),
        Some("webp"),
        "extension implies the format"
    );
    assert!((plan.cost_estimate.unwrap().amount - 0.03).abs() < 1e-9);
    assert!(!f.sandbox.path("sub").exists(), "dry run creates no directories");
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn output_extension_selects_the_format_and_a_different_returned_type_is_kept() {
    let f = Fixture::new();
    // -o *.jpg implies format=jpeg for a model that declares `format`.
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("photo.jpg"));
    f.openai.images().push(Ok(image_output(vec![jpeg(8, 8)])));
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    completed(r.unwrap());
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(req.options.get("format"), Some(&OptionValue::Str("jpeg".into())));

    // A model without `format` returns JPEG for photo.png: saved as photo.jpg.
    let mut a = args("x");
    a.common.model = Some("fake-gemini-image".into());
    a.common.output = Some(f.sandbox.path("gem.png"));
    f.gemini.images().push(Ok(image_output(vec![jpeg(8, 8)])));
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.artifacts[0].path, f.sandbox.path("gem.jpg").to_str().unwrap());
    assert_eq!(res.artifacts[0].media_type, "image/jpeg");
    assert!(has_warning(&warnings, "output_extension_adjusted"));
    assert!(has_warning(&warnings, "cost_estimate_unavailable"), "the model has no estimator");
    assert!(!f.sandbox.path("gem.png").exists());

    // Contradicting -o and --format is refused locally.
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("x.png"));
    a.common.options = vec![flag("format", "jpeg", "--format")];
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(err_code(r), ErrorCode::InvalidArgument);
}

/// A model without a `format` option cannot be asked for the type an `-o`
/// extension names: the plan warns that the provider chooses the type and the
/// extension may change, in a dry run and a real run alike. A model with `format`,
/// or a run without `-o`, gets no such warning.
#[tokio::test]
async fn an_extension_the_model_cannot_be_asked_for_is_flagged_at_plan_time() {
    let f = Fixture::new();
    let request = |model: &str, output: Option<&str>, dry_run: bool| {
        let mut a = args("x");
        a.common.model = Some(model.into());
        a.common.output = output.map(|o| f.sandbox.path(o));
        a.common.dry_run = dry_run;
        a
    };
    let may_change = |warnings: &[Warning]| {
        warnings.iter().filter(|w| w.is(WarningCode::OutputExtensionMayChange)).cloned().collect::<Vec<_>>()
    };

    let (r, warnings) =
        f.run(Operation::ImageGenerate, request("fake-gemini-image", Some("g.png"), true)).await;
    let plan = planned(r.unwrap());
    assert_eq!(plan.outputs, [f.sandbox.path("g.png").to_str().unwrap()]);
    let flagged = may_change(&warnings);
    assert_eq!(flagged.len(), 1, "{warnings:?}");
    assert!(flagged[0].message.contains("fake-gemini-image"), "{}", flagged[0].message);
    assert!(flagged[0].message.contains("image/png, image/jpeg"), "{}", flagged[0].message);
    assert!(flagged[0].message.contains(f.sandbox.path("g.png").to_str().unwrap()), "{}", flagged[0].message);

    f.gemini.images().push(Ok(image_output(vec![jpeg(8, 8)])));
    let (r, warnings) =
        f.run(Operation::ImageGenerate, request("fake-gemini-image", Some("g.png"), false)).await;
    let res = completed(r.unwrap());
    assert_eq!(res.artifacts[0].path, f.sandbox.path("g.jpg").to_str().unwrap());
    assert_eq!(may_change(&warnings).len(), 1, "{warnings:?}");
    assert!(has_warning(&warnings, "output_extension_adjusted"));

    // Without an extension the plan picks one, which may change too.
    let (_, warnings) = f.run(Operation::ImageGenerate, request("fake-gemini-image", Some("g2"), true)).await;
    assert_eq!(may_change(&warnings).len(), 1, "{warnings:?}");
    // No -o, or a model that takes the format from the extension: nothing to flag.
    let (_, warnings) = f.run(Operation::ImageGenerate, request("fake-gemini-image", None, true)).await;
    assert!(may_change(&warnings).is_empty(), "{warnings:?}");
    let (_, warnings) = f.run(Operation::ImageGenerate, request("fake-image-1", Some("o.png"), true)).await;
    assert!(may_change(&warnings).is_empty(), "{warnings:?}");
}

#[tokio::test]
async fn extra_images_text_and_provider_warnings_are_all_kept() {
    let f = Fixture::new();
    let mut out = image_output(vec![png(4, 4), png(5, 5)]);
    out.text = Some("here you go".into());
    out.warnings.push(Warning::new(WarningCode::ProviderTextOutput, "the model returned text"));
    f.gemini.images().push(Ok(out));
    let mut a = args("x");
    a.common.model = Some("fake-gemini-image".into());
    a.common.output = Some(f.sandbox.path("two.png"));
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.artifacts.len(), 2, "paid output is never discarded");
    assert!(res.artifacts[0].path.ends_with("two-1.png"), "{}", res.artifacts[0].path);
    assert!(res.artifacts[1].path.ends_with("two-2.png"));
    assert_eq!(res.text.as_deref(), Some("here you go"));
    assert!(has_warning(&warnings, "provider_text_output"));
}

/// Content the provider labeled an image that does not decode is still paid output:
/// the command fails with `invalid_media`, and the bytes are kept as received in the
/// state directory (`details.fallback_paths`, and a warning naming the file), never
/// at the requested path.
#[tokio::test]
async fn invalid_media_from_the_provider_is_kept_as_received() {
    let f = Fixture::new();
    let content = b"{\"error\": \"nope\"}".to_vec();
    f.openai.images().push(Ok(image_output(vec![content.clone()])));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("bad.png"));
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidMedia);
    assert_eq!(e.provider_request_id.as_deref(), Some("req_fake_1"));
    assert_eq!(e.details.get("charge_possible"), Some(&serde_json::json!(true)), "{:?}", e.details);
    assert_eq!(e.details.get("saved"), Some(&serde_json::json!([])));
    let fallback = e.details["fallback_paths"].as_array().unwrap();
    assert_eq!(fallback.len(), 1, "{:?}", e.details);
    let raw = PathBuf::from(fallback[0].as_str().unwrap());
    assert_eq!(raw.parent().unwrap(), f.sandbox.state().join("unsaved"));
    assert!(raw.extension().is_some_and(|ext| ext == "bin"), "{}", raw.display());
    assert_eq!(std::fs::read(&raw).unwrap(), content);
    let named = warnings
        .iter()
        .filter(|w| w.is(WarningCode::OutputSavedElsewhere))
        .any(|w| w.message.contains(raw.to_str().unwrap()) && w.message.contains("response item 0"));
    assert!(named, "{warnings:?}");
    assert!(e.hint.as_deref().unwrap().contains("may have billed"), "{:?}", e.hint);
    assert_ne!(e.retryable, Some(true));
    assert!(files_in(&f.sandbox.work()).is_empty(), "{:?}", files_in(&f.sandbox.work()));
}

/// Returned items that are not usable images reach the app as raw content: next to
/// usable images they are kept in the state directory with a warning naming the
/// file (they are not artifacts); with no usable image, the error lists them.
#[tokio::test]
async fn content_that_is_not_an_image_is_kept_as_received() {
    let f = Fixture::new();
    let unusable = |item: usize, bytes: &[u8]| UnusableOutput { item, bytes: bytes.to_vec() };

    let mut out = image_output(vec![png(4, 4)]);
    out.unusable = vec![unusable(1, b"not an image"), unusable(2, b"%%%not base64%%%")];
    f.openai.images().push(Ok(out));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("ok.png"));
    a.common.options = vec![flag("count", "3", "--count")];
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.artifacts.len(), 1, "only images are artifacts");
    let kept: Vec<&Warning> = warnings.iter().filter(|w| w.is(WarningCode::OutputSavedElsewhere)).collect();
    assert_eq!(kept.len(), 2, "{warnings:?}");
    let unsaved = f.sandbox.state().join("unsaved");
    let mut raw = files_in(&unsaved);
    raw.sort();
    assert_eq!(raw.len(), 2, "{raw:?}");
    for (name, (item, content)) in raw.iter().zip([(1, &b"not an image"[..]), (2, b"%%%not base64%%%")]) {
        assert!(name.ends_with(&format!("-{item}.bin")), "{name}");
        assert_eq!(std::fs::read(unsaved.join(name)).unwrap(), content);
        assert!(kept.iter().any(|w| w.message.contains(&*unsaved.join(name).to_string_lossy())), "{kept:?}");
    }

    // No usable image: the provider's error, with every kept file.
    f.openai.images().push_failure(ImageFailure {
        error: IrisError::new(ErrorCode::ProviderBadResponse, "no usable image")
            .with_provider(ProviderId::OpenAi)
            .with_detail("charge_possible", true),
        unusable: vec![unusable(0, b"first"), unusable(1, b"second")],
    });
    let (r, warnings) = f.run(Operation::ImageGenerate, args("x")).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::ProviderBadResponse);
    assert_ne!(e.retryable, Some(true));
    let fallback: Vec<PathBuf> = e.details["fallback_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| PathBuf::from(p.as_str().unwrap()))
        .collect();
    assert_eq!(fallback.len(), 2, "{:?}", e.details);
    assert_eq!(std::fs::read(&fallback[0]).unwrap(), b"first");
    assert_eq!(std::fs::read(&fallback[1]).unwrap(), b"second");
    // Warnings name the files too: the only place human mode shows them.
    for path in &fallback {
        let named = warnings
            .iter()
            .any(|w| w.is(WarningCode::OutputSavedElsewhere) && w.message.contains(path.to_str().unwrap()));
        assert!(named, "{warnings:?}");
    }
    assert_eq!(files_in(&f.sandbox.work()), ["ok.png"], "nothing new at the requested location");
}

/// Every kept file is named by the item's position in the response, so content
/// from a skipped item and from an image that does not decode never compete for a
/// name, and a failing command lists both.
#[tokio::test]
async fn kept_content_is_named_by_response_position_and_all_of_it_is_listed() {
    let f = Fixture::new();
    // Item 0 was skipped by the adapter; item 1 looked like a PNG but is truncated.
    let truncated = png(4, 4)[..40].to_vec();
    let out = ImageOutput {
        images: vec![GeneratedImage { item: 1, media_type: "image/png".into(), bytes: truncated.clone() }],
        unusable: vec![UnusableOutput { item: 0, bytes: b"skipped".to_vec() }],
        ..image_output(Vec::new())
    };
    f.openai.images().push(Ok(out));
    let (r, warnings) = f.run(Operation::ImageGenerate, args("x")).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidMedia);
    assert_eq!(e.details["index"], 0, "the artifact index of the image that could not be saved");
    let fallback: Vec<String> = e.details["fallback_paths"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect();
    assert_eq!(fallback.len(), 2, "{:?}", e.details);
    assert!(fallback[0].ends_with("-0.bin"), "{fallback:?}");
    assert!(fallback[1].ends_with("-1.bin"), "{fallback:?}");
    assert_eq!(std::fs::read(&fallback[0]).unwrap(), b"skipped");
    assert_eq!(std::fs::read(&fallback[1]).unwrap(), truncated);
    assert_eq!(files_in(&f.sandbox.state().join("unsaved")).len(), 2, "no renamed duplicates");
    let elsewhere = warnings.iter().filter(|w| w.is(WarningCode::OutputSavedElsewhere)).count();
    assert_eq!(elsewhere, 2, "{warnings:?}");
}

/// If the state directory cannot take the content either, a success says so in a
/// warning and a failure in `details.fallback_error`; neither is hidden.
#[tokio::test]
async fn content_that_cannot_be_kept_either_is_reported() {
    let f = Fixture::new();
    std::fs::create_dir_all(f.sandbox.state()).unwrap();
    std::fs::write(f.sandbox.state().join("unsaved"), b"not a directory").unwrap();

    let mut out = image_output(vec![png(4, 4)]);
    out.unusable = vec![UnusableOutput { item: 1, bytes: b"lost".to_vec() }];
    f.openai.images().push(Ok(out));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("ok.png"));
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(completed(r.unwrap()).artifacts.len(), 1, "the image itself is saved where requested");
    let said = warnings
        .iter()
        .any(|w| w.is(WarningCode::OutputItemUnusable) && w.message.contains("could not be kept either"));
    assert!(said, "{warnings:?}");
    assert!(!warnings.iter().any(|w| w.is(WarningCode::OutputSavedElsewhere)), "{warnings:?}");

    f.openai.images().push_failure(ImageFailure {
        error: IrisError::new(ErrorCode::ProviderBadResponse, "no usable image")
            .with_provider(ProviderId::OpenAi)
            .with_detail("charge_possible", true),
        unusable: vec![UnusableOutput { item: 0, bytes: b"lost".to_vec() }],
    });
    let (r, _) = f.run(Operation::ImageGenerate, args("x")).await;
    let e = r.unwrap_err();
    assert_eq!(e.details["fallback_paths"], serde_json::json!([]), "{:?}", e.details);
    assert!(e.details["fallback_error"].as_str().is_some(), "{:?}", e.details);
    assert_ne!(e.retryable, Some(true));
}

/// Make the next call replace the (preflighted) directory `dir` with a regular
/// file, so saving there fails with an I/O error after the paid call.
fn break_dir_during_call(f: &Fixture, dir: PathBuf) {
    *f.openai.images().on_call.lock().unwrap() = Some(Box::new(move || {
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"not a directory").unwrap();
    }));
}

#[tokio::test]
async fn an_image_that_cannot_be_saved_where_requested_is_saved_in_the_state_directory() {
    let f = Fixture::new();
    let image = png(9, 7);
    f.openai.images().push(Ok(image_output(vec![image.clone()])));
    break_dir_during_call(&f, f.sandbox.path("sub"));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("sub/out.png"));
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.expect("the image was kept, so the command succeeds"));
    assert_eq!(res.artifacts.len(), 1);
    let art = &res.artifacts[0];
    let path = PathBuf::from(&art.path);
    let unsaved = f.sandbox.state().join("unsaved");
    assert_eq!(path.parent().unwrap(), unsaved, "{}", art.path);
    let name = path.file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with("-0.png") && name.len() == 26 + "-0.png".len(), "<ulid>-<index>.<ext>: {name}");
    assert_eq!(std::fs::read(&path).unwrap(), image, "the file holds the provider's bytes");
    assert_eq!(art.sha256, iris::artifacts::sha256_bytes(&image));
    assert_eq!((art.width, art.height), (Some(9), Some(7)));
    let warning = warnings.iter().find(|w| w.code == "output_saved_elsewhere").expect("warning");
    assert!(
        warning.message.contains(&art.path) && warning.message.contains("out.png"),
        "{}",
        warning.message
    );
    assert!(res.cost_estimate.is_some() || has_warning(&warnings, "cost_estimate_unavailable"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&unsaved).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "the fallback directory is private");
    }
    let leftovers: Vec<String> = files_in(&unsaved).into_iter().filter(|n| n.contains("iris-part")).collect();
    assert!(leftovers.is_empty(), "no temp files are left: {leftovers:?}");
}

#[tokio::test]
async fn a_save_error_lists_the_images_kept_elsewhere_and_says_it_may_be_charged() {
    let f = Fixture::new();
    let image = png(4, 4);
    f.openai.images().push(Ok(image_output(vec![image.clone(), b"{\"error\": 1}".to_vec()])));
    break_dir_during_call(&f, f.sandbox.path("pics"));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("pics/p.png"));
    a.common.options = vec![flag("count", "2", "--count")];
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidMedia, "the second item is not an image");
    assert_eq!(e.details.get("index"), Some(&serde_json::json!(1)));
    assert_eq!(e.details.get("charge_possible"), Some(&serde_json::json!(true)));
    let fallback = e.details["fallback_paths"].as_array().unwrap();
    assert_eq!(fallback.len(), 2, "the image and the raw content: {:?}", e.details);
    let kept = PathBuf::from(fallback[0].as_str().unwrap());
    assert_eq!(std::fs::read(&kept).unwrap(), image);
    let raw = PathBuf::from(fallback[1].as_str().unwrap());
    assert!(raw.to_str().unwrap().ends_with("-1.bin"), "{}", raw.display());
    assert_eq!(std::fs::read(&raw).unwrap(), b"{\"error\": 1}", "kept exactly as received");
    assert_eq!(e.details["saved"], serde_json::json!([kept.to_str().unwrap()]), "saved lists images only");
    assert!(has_warning(&warnings, "output_saved_elsewhere"));
    assert_eq!(e.provider_request_id.as_deref(), Some("req_fake_1"));
}

#[tokio::test]
async fn when_even_the_fallback_fails_the_error_says_so_and_may_be_charged() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(4, 4)])));
    // Both the output directory and the fallback location become unusable.
    let (dir, unsaved) = (f.sandbox.path("sub"), f.sandbox.state().join("unsaved"));
    *f.openai.images().on_call.lock().unwrap() = Some(Box::new(move || {
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::write(&dir, b"x").unwrap();
        std::fs::write(&unsaved, b"x").unwrap();
    }));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("sub/out.png"));
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::IoError, "{}", e.message);
    assert_eq!(e.details.get("charge_possible"), Some(&serde_json::json!(true)));
    assert!(e.details.get("fallback_error").and_then(|v| v.as_str()).is_some(), "{:?}", e.details);
    assert_eq!(e.details.get("fallback_paths"), Some(&serde_json::json!([])));
    assert_ne!(e.retryable, Some(true));
}

#[tokio::test]
async fn a_possibly_charged_error_is_never_reported_as_retryable() {
    let f = Fixture::new();
    f.openai.images().push(Err(IrisError::new(ErrorCode::RequestTimeout, "slow")
        .with_provider(ProviderId::OpenAi)
        .with_detail("charge_possible", true)));
    let (r, _) = f.run(Operation::ImageGenerate, args("x")).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::RequestTimeout);
    assert_eq!(e.retryable, Some(false));

    // Without charge_possible the provider's retryability is kept.
    f.openai.images().push(Err(IrisError::new(ErrorCode::RequestTimeout, "slow")));
    let (r, _) = f.run(Operation::ImageGenerate, args("x")).await;
    assert_eq!(r.unwrap_err().retryable, Some(true));
}

/// A completed answer the provider bills but that holds no image (`details.charged`)
/// is a known outcome: it keeps the provider's retryability, and its reported usage
/// gets a cost estimate when the model has one (never for borrowed capabilities).
#[tokio::test]
async fn a_charged_answer_without_an_image_gets_an_estimate_from_its_usage() {
    let f = Fixture::new();
    let usage = Usage {
        input_tokens: Some(10),
        output_tokens: Some(5),
        total_tokens: Some(15),
        provider_usage: None,
    };
    let charged = || {
        IrisError::new(ErrorCode::ProviderError, "the model returned no image")
            .with_provider(ProviderId::OpenAi)
            .with_retryable(Some(true))
            .with_detail("charged", true)
            .with_detail("usage", serde_json::to_value(&usage).unwrap())
    };
    f.openai.images().push(Err(charged()));
    let (r, _) = f.run(Operation::ImageGenerate, args("x")).await;
    let e = r.unwrap_err();
    assert_eq!(e.retryable, Some(true), "charged is not charge_possible: the outcome is known");
    assert_eq!(e.details["usage"]["output_tokens"], 5);
    assert_eq!(e.details["cost_estimate"]["amount"], 0.005, "{:?}", e.details);
    assert_eq!(e.details["cost_estimate"]["estimated"], true);

    f.openai.images().push(Err(charged()));
    let mut a = args("x");
    a.common.model = Some("fake-image-9".into());
    a.common.capabilities_from = Some("fake-image-1".into());
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    let e = r.unwrap_err();
    assert!(e.details.get("usage").is_some());
    assert!(e.details.get("cost_estimate").is_none(), "no prices for borrowed capabilities: {:?}", e.details);
}

#[tokio::test]
async fn provider_errors_pass_through_and_nothing_is_saved() {
    let f = Fixture::new();
    f.openai
        .images()
        .push(Err(IrisError::new(ErrorCode::RateLimited, "slow down").with_provider(ProviderId::OpenAi)));
    let (r, _) = f.run(Operation::ImageGenerate, args("x")).await;
    assert_eq!(err_code(r), ErrorCode::RateLimited);
    assert!(files_in(&f.sandbox.work()).is_empty());
}

#[tokio::test]
async fn prompt_limits_and_model_resolution_errors() {
    let f = Fixture::new();
    let (r, _) = f.run(Operation::ImageGenerate, args(&"x".repeat(101))).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(e.message.contains("101"), "{}", e.message);

    let mut a = args("x");
    a.common.model = Some("no-such-model".into());
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(err_code(r), ErrorCode::UnknownModel);

    let mut a = args("x");
    a.common.model = None;
    a.common.capabilities_from = Some("fake-image-1".into());
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(err_code(r), ErrorCode::UsageError);

    // Video-only model for an image operation.
    let mut a = args("x");
    a.common.model = Some("fake-video-1".into());
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(err_code(r), ErrorCode::UnsupportedOperation);
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn unknown_models_can_borrow_declared_capabilities_with_a_warning() {
    let f = Fixture::new();
    let mut a = args("x");
    a.common.model = Some("fake-image-2-preview".into());
    a.common.capabilities_from = Some("fake-img".into());
    let (r, warnings) = f.run(Operation::ImageGenerate, a).await;
    let res = completed(r.unwrap());
    assert_eq!(res.model, "fake-image-2-preview");
    assert!(has_warning(&warnings, "unverified_model_capabilities"));
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(req.model, "fake-image-2-preview", "the unknown id is what gets sent");
}

/// Iris never picks a model: without `-m` the config file's `image.model` is used
/// (reported as `model_source: config`), `-m` wins over it, and with neither the
/// command fails with `model_required` before anything is sent.
#[tokio::test]
async fn the_model_comes_from_the_flag_or_the_config_file_and_is_otherwise_required() {
    let f = Fixture::new();
    let without_model = || {
        let mut a = args("x");
        a.common.model = None;
        a
    };
    for op in [Operation::ImageGenerate, Operation::ImageEdit] {
        for dry_run in [false, true] {
            let mut a = without_model();
            a.common.dry_run = dry_run;
            let (r, _) = f.run(op, a).await;
            let e = r.unwrap_err();
            assert_eq!(e.code, ErrorCode::ModelRequired, "{op} dry_run={dry_run}");
            assert_eq!(e.exit_code(), 2);
            assert_eq!(e.provider_status, None);
            assert_eq!(e.details["operation"], op.as_str());
            assert_eq!(e.details["config_key"], "image.model");
            // The config file is the default one, so the suggested command does not name it.
            let config_file = f.sandbox.home().join(".config/iris/config.toml");
            let hint = format!(
                "run `iris models list --operation {op}` and pass -m <MODEL>, or set model under [image] in {}",
                config_file.display()
            );
            assert_eq!(e.hint.as_deref(), Some(hint.as_str()));
            let candidates: Vec<&str> = e.details["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["model"].as_str().unwrap())
                .collect();
            assert_eq!(candidates, ["fake-image-1", "fake-gemini-image"], "image models, in catalog order");
            let fake = &e.details["candidates"][0];
            assert_eq!(fake["summary"], FAKE_IMAGE_MODEL.summary);
            assert_eq!(fake["lowest_estimate"]["options"], serde_json::json!({"quality": "low"}));
            assert_eq!(fake["lowest_estimate"]["cost_estimate"]["amount"], 0.01);
            assert!(e.details["candidates"][1]["lowest_estimate"].is_null(), "no estimator");
        }
    }
    assert_eq!(f.calls(), 0, "nothing was sent");

    let mut configured = settings(&f.sandbox.env());
    configured.image_model =
        Resolved { value: Some("fake-gemini-image".into()), source: SettingSource::File };
    let ctx = context(configured, vec![f.openai.clone(), f.gemini.clone()]);
    let mut w = Vec::new();
    let res = completed(image::run(&ctx, Operation::ImageGenerate, without_model(), &mut w).await.unwrap());
    assert_eq!((res.provider, res.model.as_str()), (ProviderId::Gemini, "fake-gemini-image"));
    assert_eq!(res.model_source, ModelSource::Config);
    let mut a = without_model();
    a.common.dry_run = true;
    let plan = planned(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await.unwrap());
    assert_eq!((plan.model.as_str(), plan.model_source), ("fake-gemini-image", ModelSource::Config));
    // -m wins over the config file.
    let res = completed(image::run(&ctx, Operation::ImageGenerate, args("x"), &mut w).await.unwrap());
    assert_eq!((res.provider, res.model.as_str()), (ProviderId::OpenAi, "fake-image-1"));
    assert_eq!(res.model_source, ModelSource::Flag);
}

/// A configured `image.model` that does not implement the operation being run is
/// `unsupported_operation` naming the key, before anything is sent.
#[tokio::test]
async fn a_configured_model_without_the_operation_names_the_config_key() {
    static GENERATE_ONLY: ModelSpec = ModelSpec {
        id: "fake-generate-only",
        aliases: &[],
        operations: &[Operation::ImageGenerate],
        ..FAKE_IMAGE_MODEL
    };
    let f = Fixture::new();
    let mut configured = settings(&f.sandbox.env());
    configured.image_model =
        Resolved { value: Some("fake-generate-only".into()), source: SettingSource::File };
    let mut deps = deps(vec![f.openai.clone()], Interrupt::manual());
    deps.catalog = Catalog::with_models(vec![&GENERATE_ONLY]);
    let ctx = AppContext::new(configured, deps, Progress::silent());
    let input = f.sandbox.path("in.png");
    std::fs::write(&input, png(4, 4)).unwrap();
    let mut a = args("x");
    a.common.model = None;
    a.images = vec![input];
    let e = image::run(&ctx, Operation::ImageEdit, a, &mut Vec::new()).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::UnsupportedOperation);
    assert_eq!(e.details["config_key"], "image.model");
    assert!(e.message.contains("image.model") && e.message.contains("fake-generate-only"), "{}", e.message);
    assert_eq!(f.calls(), 0);
}

#[tokio::test]
async fn edit_reads_and_validates_inputs_before_sending() {
    let f = Fixture::new();
    let a_png = f.sandbox.path("a.png");
    let b_jpg = f.sandbox.path("b.jpg");
    std::fs::write(&a_png, png(6, 6)).unwrap();
    std::fs::write(&b_jpg, jpeg(6, 6)).unwrap();
    let mut a = args("combine");
    a.images = vec![a_png.clone(), b_jpg.clone()];
    a.mask = Some(a_png.clone());
    let (r, _) = f.run(Operation::ImageEdit, a).await;
    completed(r.unwrap());
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(*f.openai.images().last_operation.lock().unwrap(), Some(Operation::ImageEdit));
    assert_eq!(req.images.len(), 2);
    assert_eq!(req.images[1].media_type, "image/jpeg");
    assert!(req.mask.is_some());

    // Too many inputs, missing file, not an image: all local.
    let before = f.calls();
    let mut a = args("x");
    a.images = vec![a_png.clone(), a_png.clone(), a_png.clone()];
    assert_eq!(err_code(f.run(Operation::ImageEdit, a).await.0), ErrorCode::InvalidArgument);
    let mut a = args("x");
    a.images = vec![f.sandbox.path("missing.png")];
    assert_eq!(err_code(f.run(Operation::ImageEdit, a).await.0), ErrorCode::InputFileInvalid);
    let text = f.sandbox.path("notes.png");
    std::fs::write(&text, "not an image").unwrap();
    let mut a = args("x");
    a.images = vec![text];
    assert_eq!(err_code(f.run(Operation::ImageEdit, a).await.0), ErrorCode::InputFileInvalid);
    assert_eq!(f.calls(), before);
}

#[tokio::test]
async fn ctrl_c_during_the_request_is_reported_as_possibly_charged() {
    let f = Fixture::new();
    let gate = Arc::new(tokio::sync::Notify::new());
    let openai = Arc::new(FakeProvider {
        images: Some(FakeImages { gate: Some(gate.clone()), ..FakeImages::default() }),
        ..FakeProvider::openai()
    });
    let interrupt = Interrupt::manual();
    let ctx = context_with_interrupt(settings(&f.sandbox.env()), vec![openai.clone()], interrupt.clone());
    let mut w = Vec::new();
    let run = image::run(&ctx, Operation::ImageGenerate, args("x"), &mut w);
    let trigger = async {
        while openai.images().calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        interrupt.trigger();
    };
    let (r, ()) = tokio::join!(run, trigger);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Interrupted);
    assert_eq!(e.exit_code(), 130);
    assert_eq!(e.details.get("charge_possible"), Some(&serde_json::Value::Bool(true)));
    assert_eq!(e.retryable, Some(false), "the request may have been billed: not presented as retryable");
    assert!(files_in(&f.sandbox.work()).is_empty());
}
