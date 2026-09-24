//! `image generate` / `image edit` workflows with fake providers and a fake
//! catalog: validation before any request, saving and validating paid output,
//! the rename fallback, cost estimates, and dry runs.

#[path = "app_support.rs"]
mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use iris::app::image::{self, ImageArgs};
use iris::app::{GenerationArgs, GenerationOutcome, Interrupt};
use iris::catalog::{OptionSource, OptionValue, RawOption};
use iris::domain::{Operation, ProviderId, Usage, Warning};
use iris::error::{ErrorCode, IrisError};
use iris::output::results::{ImageResult, PlanResult};
use support::*;

fn args(prompt: &str) -> ImageArgs {
    ImageArgs {
        common: GenerationArgs { prompt: prompt.to_string(), ..GenerationArgs::default() },
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
    a.common.provider = Some(ProviderId::Gemini);
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
    a.common.provider = Some(ProviderId::Gemini);
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

#[tokio::test]
async fn extra_images_text_and_provider_warnings_are_all_kept() {
    let f = Fixture::new();
    let mut out = image_output(vec![png(4, 4), png(5, 5)]);
    out.text = Some("here you go".into());
    out.warnings.push(Warning::new("provider_text_output", "the model returned text"));
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

#[tokio::test]
async fn invalid_media_from_the_provider_is_not_saved() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![b"{\"error\": \"nope\"}".to_vec()])));
    let mut a = args("x");
    a.common.output = Some(f.sandbox.path("bad.png"));
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidMedia);
    assert_eq!(e.provider_request_id.as_deref(), Some("req_fake_1"));
    assert!(files_in(&f.sandbox.work()).is_empty(), "{:?}", files_in(&f.sandbox.work()));
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
    a.common.model = Some("fake-image-1".into());
    a.common.provider = Some(ProviderId::Gemini);
    let (r, _) = f.run(Operation::ImageGenerate, a).await;
    assert_eq!(err_code(r), ErrorCode::InvalidArgument);

    let mut a = args("x");
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

#[tokio::test]
async fn provider_defaults_follow_the_configured_image_provider() {
    let f = Fixture::new();
    let env = f.sandbox.env().with_var("IRIS_IMAGE_PROVIDER", "gemini");
    let ctx = context(settings(&env), vec![f.openai.clone(), f.gemini.clone()]);
    let mut w = Vec::new();
    let res = completed(image::run(&ctx, Operation::ImageGenerate, args("x"), &mut w).await.unwrap());
    assert_eq!(res.provider, ProviderId::Gemini);
    // --model's provider beats IRIS_IMAGE_PROVIDER.
    let mut a = args("x");
    a.common.model = Some("fake-img".into());
    let res = completed(image::run(&ctx, Operation::ImageGenerate, a, &mut w).await.unwrap());
    assert_eq!(res.provider, ProviderId::OpenAi);
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
    assert_eq!(req.operation, Operation::ImageEdit);
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
    assert!(files_in(&f.sandbox.work()).is_empty());
}
