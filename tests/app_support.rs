//! Shared helpers for the app-level and in-process CLI tests: a fake catalog,
//! fake providers, media fixtures, and context builders. Other test crates include
//! this file with `#[path = "app_support.rs"] mod support;`; compiled on its own
//! it is an empty test crate.
//!
//! Nothing here reads real credentials: settings come from an explicit
//! `EnvSnapshot` with fake keys, and HTTP only reaches 127.0.0.1 mock servers.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use iris::app::{AppContext, Catalog, Clock, Deps, Interrupt, Progress};
use iris::catalog::{
    EstimateInput, InputSpec, Lifecycle, Limits, ModelSpec, OptionKind, OptionSpec, OutputSpec, PriceRule,
    ValidationInput,
};
use iris::config::{CliOverrides, EnvSnapshot, Platform, Resolved, SettingSource, Settings};
use iris::domain::{CostEstimate, Operation, ProviderId, Warning};
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy};
use iris::providers::{
    AccountAccess, CredentialHeader, GeneratedImage, ImageOutput, ImageProvider, ImageRequest, Provider,
    ProviderContext, Registry, RemoteArtifact, RemoteStatus, SubmittedOperation, VideoProvider, VideoRequest,
};
use tokio::sync::Notify;

use Operation::{ImageEdit, ImageGenerate, VideoGenerate};

pub const OPENAI_KEY: &str = "test-openai-key-000";
pub const GEMINI_KEY: &str = "test-gemini-key-000";

// ----- fake catalog -----------------------------------------------------------

fn size_rule(v: &str) -> Result<(), String> {
    if v == "auto" {
        return Ok(());
    }
    let (w, h) = v.split_once('x').ok_or("expected WxH")?;
    match (w.parse::<u32>(), h.parse::<u32>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok(()),
        _ => Err("expected WxH".to_string()),
    }
}

static IMAGE_OPS: &[Operation] = &[ImageGenerate, ImageEdit];

pub static FAKE_IMAGE_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "count",
        kind: OptionKind::Integer { min: 1, max: 4 },
        default: Some("1"),
        flag: Some("--count"),
        operations: IMAGE_OPS,
        description: "Number of images",
    },
    OptionSpec {
        name: "size",
        kind: OptionKind::Pattern { syntax: "auto or WxH", validate: size_rule },
        default: Some("auto"),
        flag: Some("--size"),
        operations: IMAGE_OPS,
        description: "Image size",
    },
    OptionSpec {
        name: "quality",
        kind: OptionKind::Enum(&["low", "high", "auto"]),
        default: Some("auto"),
        flag: Some("--quality"),
        operations: IMAGE_OPS,
        description: "Quality",
    },
    OptionSpec {
        name: "format",
        kind: OptionKind::Enum(&["png", "jpeg", "webp"]),
        default: Some("png"),
        flag: Some("--format"),
        operations: IMAGE_OPS,
        description: "Output format",
    },
    OptionSpec {
        name: "background",
        kind: OptionKind::Enum(&["transparent", "opaque", "auto"]),
        default: Some("auto"),
        flag: None,
        operations: IMAGE_OPS,
        description: "Background",
    },
];

/// $0.01 per image, only when quality is explicit (like OpenAI's `auto` rule).
fn fake_image_estimate(spec: &ModelSpec, input: &EstimateInput<'_>) -> Option<CostEstimate> {
    let quality = spec.effective(input.options, "quality")?;
    if quality.as_str() == Some("auto") {
        return None;
    }
    Some(CostEstimate::usd(
        0.01 * f64::from(input.count),
        format!("{} image(s) x $0.01 (fake)", input.count),
        "https://example.invalid/pricing",
        "2026-09-24",
    ))
}

pub static FAKE_IMAGE_MODEL: ModelSpec = ModelSpec {
    id: "fake-image-1",
    provider: ProviderId::OpenAi,
    display_name: "Fake Image 1",
    aliases: &["fake-img"],
    lifecycle: Lifecycle::Ga,
    operations: IMAGE_OPS,
    default_for: IMAGE_OPS,
    inputs: InputSpec {
        max_input_images: 2,
        input_media_types: &["image/png", "image/jpeg", "image/webp"],
        max_input_bytes: 1_000_000,
        mask: true,
        first_frame: false,
        last_frame: false,
        max_reference_images: 0,
    },
    options: FAKE_IMAGE_OPTIONS,
    outputs: OutputSpec { media_types: &["image/png", "image/jpeg", "image/webp"], max_count: 4 },
    limits: Limits { max_prompt_chars: Some(100) },
    pricing: &[PriceRule {
        description: "per image (fake)",
        unit: "image",
        usd: 0.01,
        source_url: "https://example.invalid/pricing",
        as_of: "2026-09-24",
    }],
    access_notes: &["Fake access note"],
    docs_url: "https://example.invalid/docs",
    validate: None,
    estimate: Some(fake_image_estimate),
    estimate_usage: None,
};

pub static FAKE_GEMINI_IMAGE_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "count",
        kind: OptionKind::Integer { min: 1, max: 1 },
        default: Some("1"),
        flag: Some("--count"),
        operations: IMAGE_OPS,
        description: "Number of images",
    },
    OptionSpec {
        name: "aspect_ratio",
        kind: OptionKind::Enum(&["1:1", "16:9"]),
        default: None,
        flag: Some("--aspect-ratio"),
        operations: IMAGE_OPS,
        description: "Aspect ratio",
    },
];

/// A Gemini-like image model: no `format` option, no mask, no estimator.
pub static FAKE_GEMINI_IMAGE: ModelSpec = ModelSpec {
    id: "fake-gemini-image",
    provider: ProviderId::Gemini,
    display_name: "Fake Gemini Image",
    aliases: &[],
    lifecycle: Lifecycle::Ga,
    operations: IMAGE_OPS,
    default_for: IMAGE_OPS,
    inputs: InputSpec {
        max_input_images: 3,
        input_media_types: &["image/png", "image/jpeg"],
        max_input_bytes: 1_000_000,
        mask: false,
        first_frame: false,
        last_frame: false,
        max_reference_images: 0,
    },
    options: FAKE_GEMINI_IMAGE_OPTIONS,
    outputs: OutputSpec { media_types: &["image/png", "image/jpeg"], max_count: 1 },
    limits: Limits { max_prompt_chars: None },
    pricing: &[],
    access_notes: &[],
    docs_url: "https://example.invalid/gemini",
    validate: None,
    estimate: None,
    estimate_usage: None,
};

static VIDEO_OPS: &[Operation] = &[VideoGenerate];

pub static FAKE_VIDEO_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "count",
        kind: OptionKind::Integer { min: 1, max: 1 },
        default: Some("1"),
        flag: Some("--count"),
        operations: VIDEO_OPS,
        description: "Number of videos",
    },
    OptionSpec {
        name: "duration",
        kind: OptionKind::Enum(&["4", "6", "8"]),
        default: Some("8"),
        flag: Some("--duration"),
        operations: VIDEO_OPS,
        description: "Seconds",
    },
    OptionSpec {
        name: "resolution",
        kind: OptionKind::Enum(&["720p", "1080p"]),
        default: Some("720p"),
        flag: Some("--resolution"),
        operations: VIDEO_OPS,
        description: "Resolution",
    },
    OptionSpec {
        name: "negative_prompt",
        kind: OptionKind::Text { max_chars: 100 },
        default: None,
        flag: Some("--negative-prompt"),
        operations: VIDEO_OPS,
        description: "What to avoid",
    },
];

fn fake_video_rules(v: &ValidationInput<'_>) -> Result<(), IrisError> {
    if v.has_last_frame && !v.has_first_frame {
        return Err(IrisError::invalid("--last-frame requires --image (first frame)"));
    }
    Ok(())
}

/// $0.10 per second of video.
fn fake_video_estimate(spec: &ModelSpec, input: &EstimateInput<'_>) -> Option<CostEstimate> {
    let seconds: f64 = spec.effective(input.options, "duration")?.as_str()?.parse().ok()?;
    Some(CostEstimate::usd(
        seconds * 0.10,
        format!("{seconds} s x $0.10 (fake; estimate)"),
        "https://example.invalid/pricing",
        "2026-09-24",
    ))
}

pub static FAKE_VIDEO_MODEL: ModelSpec = ModelSpec {
    id: "fake-video-1",
    provider: ProviderId::Gemini,
    display_name: "Fake Video 1",
    aliases: &["fake-vid"],
    lifecycle: Lifecycle::Preview,
    operations: VIDEO_OPS,
    default_for: VIDEO_OPS,
    inputs: InputSpec {
        max_input_images: 0,
        input_media_types: &["image/png", "image/jpeg"],
        max_input_bytes: 1_000_000,
        mask: false,
        first_frame: true,
        last_frame: true,
        max_reference_images: 2,
    },
    options: FAKE_VIDEO_OPTIONS,
    outputs: OutputSpec { media_types: &["video/mp4"], max_count: 1 },
    limits: Limits { max_prompt_chars: Some(200) },
    pricing: &[PriceRule {
        description: "per second (fake)",
        unit: "second",
        usd: 0.10,
        source_url: "https://example.invalid/pricing",
        as_of: "2026-09-24",
    }],
    access_notes: &["Preview model (fake)"],
    docs_url: "https://example.invalid/video",
    validate: Some(fake_video_rules),
    estimate: Some(fake_video_estimate),
    estimate_usage: None,
};

pub fn fake_catalog() -> Catalog {
    Catalog::with_models(vec![&FAKE_IMAGE_MODEL, &FAKE_GEMINI_IMAGE, &FAKE_VIDEO_MODEL])
}

// ----- fake providers -----------------------------------------------------------

/// Scripted synchronous image adapter.
#[derive(Default)]
pub struct FakeImages {
    /// Results returned in order; when empty, one 8x8 PNG.
    pub results: Mutex<VecDeque<Result<ImageOutput, IrisError>>>,
    pub calls: AtomicUsize,
    pub last_request: Mutex<Option<ImageRequest>>,
    /// Run once at the start of the next call (e.g. create a conflicting file).
    pub on_call: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// If set, the call waits for this before answering.
    pub gate: Option<Arc<Notify>>,
}

#[async_trait]
impl ImageProvider for FakeImages {
    async fn generate(&self, req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        self.answer(req).await
    }
    async fn edit(&self, req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        self.answer(req).await
    }
}

impl FakeImages {
    async fn answer(&self, req: &ImageRequest) -> Result<ImageOutput, IrisError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_request.lock().unwrap() = Some(req.clone());
        if let Some(hook) = self.on_call.lock().unwrap().take() {
            hook();
        }
        if let Some(gate) = &self.gate {
            gate.notified().await;
        }
        self.results.lock().unwrap().pop_front().unwrap_or_else(|| Ok(image_output(vec![png(8, 8)])))
    }

    pub fn push(&self, result: Result<ImageOutput, IrisError>) {
        self.results.lock().unwrap().push_back(result);
    }
}

/// Scripted asynchronous video adapter.
#[derive(Default)]
pub struct FakeVideo {
    /// Submit results in order; when empty, a fresh operation id.
    pub submits: Mutex<VecDeque<Result<SubmittedOperation, IrisError>>>,
    /// Poll results in order; when empty, `Running`.
    pub polls: Mutex<VecDeque<Result<RemoteStatus, IrisError>>>,
    pub submit_calls: AtomicUsize,
    pub poll_calls: AtomicUsize,
    pub last_request: Mutex<Option<VideoRequest>>,
    /// Notified when `submit` is entered.
    pub submit_entered: Arc<Notify>,
    /// If set, `submit` waits for this before answering.
    pub submit_gate: Option<Arc<Notify>>,
    pub retention: Option<Duration>,
}

#[async_trait]
impl VideoProvider for FakeVideo {
    async fn submit(
        &self,
        req: &VideoRequest,
        _ctx: &ProviderContext,
    ) -> Result<SubmittedOperation, IrisError> {
        let n = self.submit_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_request.lock().unwrap() = Some(req.clone());
        self.submit_entered.notify_one();
        if let Some(gate) = &self.submit_gate {
            gate.notified().await;
        }
        self.submits.lock().unwrap().pop_front().unwrap_or_else(|| {
            Ok(SubmittedOperation {
                remote_id: format!("models/fake-video-1/operations/op{n}"),
                provider_request_id: Some("req-fake".into()),
            })
        })
    }

    async fn poll(&self, _remote_id: &str, _ctx: &ProviderContext) -> Result<RemoteStatus, IrisError> {
        self.poll_calls.fetch_add(1, Ordering::SeqCst);
        self.polls.lock().unwrap().pop_front().unwrap_or(Ok(RemoteStatus::Running { progress: None }))
    }

    fn output_retention(&self) -> Option<Duration> {
        self.retention
    }
}

impl FakeVideo {
    pub fn push_poll(&self, result: Result<RemoteStatus, IrisError>) {
        self.polls.lock().unwrap().push_back(result);
    }
    pub fn push_submit(&self, result: Result<SubmittedOperation, IrisError>) {
        self.submits.lock().unwrap().push_back(result);
    }
}

/// A provider with optional fake image and video adapters.
pub struct FakeProvider {
    pub id: ProviderId,
    pub images: Option<FakeImages>,
    pub video: Option<FakeVideo>,
    pub access: Mutex<Result<AccountAccess, IrisError>>,
    pub access_calls: AtomicUsize,
}

impl FakeProvider {
    pub fn openai() -> FakeProvider {
        FakeProvider {
            id: ProviderId::OpenAi,
            images: Some(FakeImages::default()),
            video: None,
            access: Mutex::new(Ok(AccountAccess::Available)),
            access_calls: AtomicUsize::new(0),
        }
    }

    pub fn gemini() -> FakeProvider {
        FakeProvider {
            id: ProviderId::Gemini,
            images: Some(FakeImages::default()),
            video: Some(FakeVideo {
                retention: Some(Duration::from_secs(48 * 3600)),
                ..FakeVideo::default()
            }),
            access: Mutex::new(Ok(AccountAccess::Available)),
            access_calls: AtomicUsize::new(0),
        }
    }

    pub fn images(&self) -> &FakeImages {
        self.images.as_ref().expect("image adapter")
    }

    pub fn videos(&self) -> &FakeVideo {
        self.video.as_ref().expect("video adapter")
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> ProviderId {
        self.id
    }
    fn default_base_url(&self) -> &'static str {
        "http://127.0.0.1:9"
    }
    fn credential_header(&self) -> CredentialHeader {
        match self.id {
            ProviderId::OpenAi => CredentialHeader { name: "authorization", prefix: "Bearer " },
            ProviderId::Gemini => CredentialHeader { name: "x-goog-api-key", prefix: "" },
        }
    }
    fn docs_url(&self) -> &'static str {
        "https://example.invalid/provider"
    }
    async fn check_access(
        &self,
        _model_id: &str,
        _ctx: &ProviderContext,
    ) -> Result<AccountAccess, IrisError> {
        self.access_calls.fetch_add(1, Ordering::SeqCst);
        self.access.lock().unwrap().clone()
    }
    fn image(&self) -> Option<&dyn ImageProvider> {
        self.images.as_ref().map(|i| i as &dyn ImageProvider)
    }
    fn video(&self) -> Option<&dyn VideoProvider> {
        self.video.as_ref().map(|v| v as &dyn VideoProvider)
    }
}

pub fn registry(providers: Vec<Arc<FakeProvider>>) -> Registry {
    Registry::with_providers(providers.into_iter().map(|p| p as Arc<dyn Provider>).collect())
}

// ----- media fixtures -------------------------------------------------------------

pub fn encode(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let img = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        width,
        height,
        image::Rgba([10, 120, 200, 255]),
    ));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

pub fn png(width: u32, height: u32) -> Vec<u8> {
    encode(image::ImageFormat::Png, width, height)
}

pub fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let img =
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30])));
    let mut buf = Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Jpeg).unwrap();
    buf.into_inner()
}

pub fn image_output(images: Vec<Vec<u8>>) -> ImageOutput {
    ImageOutput {
        images: images
            .into_iter()
            .map(|bytes| {
                let media_type = iris::artifacts::media::sniff(&bytes).unwrap_or("image/png").to_string();
                GeneratedImage { media_type, bytes }
            })
            .collect(),
        text: None,
        usage: None,
        provider_request_id: Some("req_fake_1".into()),
        warnings: Vec::new(),
    }
}

fn bx(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = ((8 + payload.len()) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// A minimal valid MP4 (ftyp + moov/mvhd + mdat) lasting `seconds`.
pub fn mp4(seconds: u32) -> Vec<u8> {
    let mut ftyp = b"isom".to_vec();
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    for brand in [b"isom", b"iso2", b"mp41"] {
        ftyp.extend_from_slice(brand);
    }
    let mut mvhd = vec![0u8; 4];
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&0u32.to_be_bytes());
    mvhd.extend_from_slice(&1000u32.to_be_bytes());
    mvhd.extend_from_slice(&(seconds * 1000).to_be_bytes());
    mvhd.extend_from_slice(&[0u8; 80]);
    let trak = bx(b"trak", &bx(b"tkhd", &[0u8; 84]));
    let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat());
    [bx(b"ftyp", &ftyp), moov, bx(b"mdat", &[0xAB; 512])].concat()
}

// ----- environment and context ---------------------------------------------------------

/// A temp directory with `home/`, `work/` (current directory = default output
/// directory), and `state/`.
pub struct Sandbox {
    pub dir: tempfile::TempDir,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["home", "work", "state"] {
            std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        Sandbox { dir }
    }
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }
    pub fn work(&self) -> PathBuf {
        self.dir.path().join("work")
    }
    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    pub fn path(&self, rel: &str) -> PathBuf {
        self.work().join(rel)
    }

    /// Environment with fake keys for both providers.
    pub fn env(&self) -> EnvSnapshot {
        self.env_without_keys().with_var("OPENAI_API_KEY", OPENAI_KEY).with_var("GEMINI_API_KEY", GEMINI_KEY)
    }

    pub fn env_without_keys(&self) -> EnvSnapshot {
        EnvSnapshot::new(Platform::Linux, Some(self.home()), self.work())
            .with_var("HOME", self.home().to_str().unwrap())
            .with_var("IRIS_STATE_DIR", self.state().to_str().unwrap())
    }
}

impl Default for Sandbox {
    fn default() -> Self {
        Sandbox::new()
    }
}

/// Settings from `env` with fast polling (the 2s minimum is a CLI rule; tests
/// set the resolved value directly).
pub fn settings(env: &EnvSnapshot) -> Settings {
    settings_with(env, &CliOverrides::default())
}

pub fn settings_with(env: &EnvSnapshot, overrides: &CliOverrides) -> Settings {
    let mut s = Settings::load(overrides, env).unwrap();
    if s.poll_interval.source == SettingSource::Default {
        s.poll_interval = Resolved { value: Duration::from_millis(20), source: SettingSource::Flag };
    }
    if s.wait_timeout.source == SettingSource::Default {
        s.wait_timeout = Resolved { value: Duration::from_secs(10), source: SettingSource::Flag };
    }
    s
}

/// Point a provider's base URL at a mock server.
pub fn set_base_url(settings: &mut Settings, provider: ProviderId, url: &str) {
    let parsed = iris::config::parse_base_url(url).unwrap();
    let p = match provider {
        ProviderId::OpenAi => &mut settings.openai,
        ProviderId::Gemini => &mut settings.gemini,
    };
    p.base_url = Resolved { value: parsed, source: SettingSource::Env };
}

/// HTTP client for 127.0.0.1 mock servers: no system proxy, millisecond backoff.
pub fn test_http() -> HttpClient {
    HttpClient::new(&HttpSettings { system_proxy: false, ..HttpSettings::default() })
        .unwrap()
        .with_retry_policy(RetryPolicy {
            base: Duration::from_millis(1),
            factor: 2.0,
            cap: Duration::from_millis(5),
            max_retry_after: Duration::from_secs(60),
        })
}

pub fn deps(providers: Vec<Arc<FakeProvider>>, interrupt: Interrupt) -> Deps {
    Deps {
        registry: registry(providers),
        catalog: fake_catalog(),
        http: Some(test_http()),
        interrupt,
        clock: Clock::system(),
    }
}

/// An app context over fake providers and the fake catalog.
pub fn context(settings: Settings, providers: Vec<Arc<FakeProvider>>) -> AppContext {
    AppContext::new(settings, deps(providers, Interrupt::manual()), Progress::silent())
}

pub fn context_with_interrupt(
    settings: Settings,
    providers: Vec<Arc<FakeProvider>>,
    interrupt: Interrupt,
) -> AppContext {
    AppContext::new(settings, deps(providers, interrupt), Progress::silent())
}

pub fn has_warning(warnings: &[Warning], code: &str) -> bool {
    warnings.iter().any(|w| w.code == code)
}

pub fn err_code<T: std::fmt::Debug>(r: Result<T, IrisError>) -> ErrorCode {
    r.expect_err("expected an error").code
}

pub fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

pub fn remote_success(uri: &str) -> RemoteStatus {
    RemoteStatus::Succeeded {
        outputs: vec![RemoteArtifact { uri: uri.to_string(), media_type: Some("video/mp4".into()) }],
        usage: None,
        warnings: Vec::new(),
    }
}
