//! The OpenAI Images adapter against a local wiremock server:
//! exact request bodies and headers, response decoding, usage, the error table, the
//! paid-submit retry rules, and local input checks. Offline: every request goes to
//! 127.0.0.1 (wiremock, or a raw socket server for broken connections), the key is a
//! fake set through `ProviderContext`, and nothing reads the process environment.

use std::io::{Cursor, Read as _, Write as _};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use iris::catalog::{OptionValue, ResolvedOptions};
use iris::domain::ProviderId;
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy, Timeouts};
use iris::providers::openai::OpenAiProvider;
use iris::providers::{
    AccountAccess, ImageOutput, ImageProvider, ImageRequest, InputImage, InputRole, Provider,
    ProviderContext, Registry,
};
use iris::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-openai-key-000";
/// The origin of a port nothing listens on. Port 9 (discard) lies below every OS's
/// ephemeral port range, so no mock server started by a test running in parallel can
/// be assigned it (a bound-then-released ephemeral port could be reused by one, and
/// the paid requests of a "refused connection" test would then land in its mock).
const DEAD_URL: &str = "http://127.0.0.1:9";
const GEN: &str = "/v1/images/generations";
const EDIT: &str = "/v1/images/edits";

// ---------------------------------------------------------------- fixtures

fn http() -> HttpClient {
    HttpClient::new(&HttpSettings {
        connect_timeout: Duration::from_secs(2),
        retry: RetryPolicy {
            base: Duration::from_millis(5),
            factor: 2.0,
            cap: Duration::from_millis(20),
            max_retry_after: Duration::from_secs(60),
        },
        system_proxy: false,
    })
    .unwrap()
}

fn ctx_for(base_url: &str) -> ProviderContext {
    ProviderContext {
        http: http(),
        base_url: url::Url::parse(base_url).unwrap(),
        credential: Secret::new(KEY),
        timeouts: Timeouts {
            generate: Duration::from_secs(10),
            poll: Duration::from_secs(5),
            ..Timeouts::default()
        },
    }
}

fn ctx(server: &MockServer) -> ProviderContext {
    ctx_for(&format!("{}/v1", server.uri()))
}

fn encode(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn png_rgba(w: u32, h: u32) -> Vec<u8> {
    encode(
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([0, 0, 0, 0]))),
        image::ImageFormat::Png,
    )
}

fn png_rgb(w: u32, h: u32) -> Vec<u8> {
    encode(
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([9, 9, 9]))),
        image::ImageFormat::Png,
    )
}

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    encode(
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, h, image::Rgb([200, 10, 10]))),
        image::ImageFormat::Jpeg,
    )
}

fn webp(w: u32, h: u32) -> Vec<u8> {
    encode(
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(w, h, image::Rgba([1, 2, 3, 255]))),
        image::ImageFormat::WebP,
    )
}

fn input(role: InputRole, name: &str, media_type: &str, bytes: Vec<u8>) -> InputImage {
    InputImage {
        role,
        path: PathBuf::from(format!("/fixtures/{name}")),
        media_type: media_type.to_string(),
        bytes,
    }
}

fn options(pairs: &[(&str, OptionValue)]) -> ResolvedOptions {
    let mut o = ResolvedOptions::new();
    for (k, v) in pairs {
        o.insert(*k, v.clone());
    }
    o
}

fn s(v: &str) -> OptionValue {
    OptionValue::Str(v.to_string())
}

fn generate_request(opts: ResolvedOptions) -> ImageRequest {
    ImageRequest {
        model: "gpt-image-2.5-sunburst".to_string(),
        prompt: "a watercolor lighthouse".to_string(),
        images: Vec::new(),
        mask: None,
        options: opts,
    }
}

fn edit_request(images: Vec<InputImage>, mask: Option<InputImage>, opts: ResolvedOptions) -> ImageRequest {
    ImageRequest {
        model: "gpt-image-2.5-sunburst".to_string(),
        prompt: "add a red scarf".to_string(),
        images,
        mask,
        options: opts,
    }
}

/// An `ImagesResponse` with `images` as base64 and an optional `output_format` echo.
fn images_body(images: &[&[u8]], output_format: Option<&str>) -> Value {
    let data: Vec<Value> = images.iter().map(|b| json!({"b64_json": STANDARD.encode(b)})).collect();
    let mut body = json!({"created": 1_790_000_000, "data": data, "size": "1024x1024", "quality": "low"});
    if let Some(f) = output_format {
        body["output_format"] = json!(f);
    }
    body
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body).insert_header("x-request-id", "req_ok_123")
}

fn err_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body).insert_header("x-request-id", "req_err_456")
}

fn openai_error(message: &str, kind: &str, code: Option<&str>) -> Value {
    json!({"error": {"message": message, "type": kind, "param": null, "code": code}})
}

async fn mount(server: &MockServer, route: &str, template: ResponseTemplate) {
    Mock::given(method("POST")).and(path(route)).respond_with(template).mount(server).await;
}

/// `n` responses of `first`, then `then` for every later request.
async fn mount_sequence(
    server: &MockServer,
    route: &str,
    first: ResponseTemplate,
    n: u64,
    then: ResponseTemplate,
) {
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(first)
        .up_to_n_times(n)
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("POST")).and(path(route)).respond_with(then).with_priority(2).mount(server).await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

fn body_json(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap()
}

fn header<'a>(req: &'a Request, name: &str) -> Option<&'a str> {
    req.headers.get(name).and_then(|v| v.to_str().ok())
}

async fn generate(server: &MockServer, opts: ResolvedOptions) -> Result<ImageOutput, IrisError> {
    OpenAiProvider::new().generate(&generate_request(opts), &ctx(server)).await
}

async fn generate_err(server: &MockServer) -> IrisError {
    generate(server, ResolvedOptions::new()).await.expect_err("expected an error")
}

// ---------------------------------------------------------------- provider metadata

#[test]
fn provider_metadata_and_registry_entry() {
    let p = OpenAiProvider::new();
    assert_eq!(p.id(), ProviderId::OpenAi);
    assert_eq!(p.id().default_base_url(), "https://api.openai.com/v1");
    assert_eq!(p.docs_url(), "https://developers.openai.com/api/docs/guides/image-generation");
    let h = p.credential_header();
    assert_eq!((h.name, h.prefix), ("authorization", "Bearer "));
    assert!(p.image().is_some());
    assert!(p.video().is_none(), "OpenAI images are synchronous; no video/job surface");
    let registry = Registry::builtin();
    let registered = registry.get(ProviderId::OpenAi).unwrap();
    assert!(registered.image().is_some());
}

// ---------------------------------------------------------------- request encoding

#[tokio::test]
async fn generate_sends_exactly_the_mapped_options_and_the_documented_headers() {
    let server = MockServer::start().await;
    let a = webp(64, 48);
    let b = webp(64, 48);
    mount(&server, GEN, ok(images_body(&[&a, &b], Some("webp")))).await;
    let opts = options(&[
        ("count", OptionValue::Int(2)),
        ("size", s("1536x1024")),
        ("quality", s("low")),
        ("format", s("webp")),
        ("compression", OptionValue::Int(80)),
        ("background", s("opaque")),
        ("moderation", s("low")),
    ]);
    let out = generate(&server, opts).await.unwrap();

    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    let req = &reqs[0];
    assert_eq!(req.method.as_str(), "POST");
    assert_eq!(req.url.path(), GEN);
    assert_eq!(
        body_json(req),
        json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "a watercolor lighthouse",
            "n": 2,
            "size": "1536x1024",
            "quality": "low",
            "output_format": "webp",
            "output_compression": 80,
            "background": "opaque",
            "moderation": "low"
        })
    );
    assert_eq!(header(req, "authorization"), Some("Bearer test-openai-key-000"));
    assert_eq!(header(req, "content-type"), Some("application/json"));
    let client_id = header(req, "x-client-request-id").expect("X-Client-Request-Id");
    assert_eq!(client_id.len(), 26);
    assert!(client_id.is_ascii() && ulid::Ulid::from_string(client_id).is_ok(), "{client_id}");
    assert!(req.url.query().is_none(), "nothing in the query string");

    assert_eq!(out.images.len(), 2);
    assert_eq!(out.images[0].media_type, "image/webp");
    assert_eq!(out.images[0].bytes, a);
    assert_eq!(out.images[1].bytes, b);
    assert_eq!(out.provider_request_id.as_deref(), Some("req_ok_123"));
    assert!(out.text.is_none());
    assert!(out.warnings.is_empty());
}

#[tokio::test]
async fn generate_without_options_sends_only_model_and_prompt() {
    let server = MockServer::start().await;
    let img = png_rgb(32, 32);
    mount(&server, GEN, ok(images_body(&[&img], Some("png")))).await;
    let mut req = generate_request(ResolvedOptions::new());
    req.model = "gpt-image-2".to_string();
    let out = OpenAiProvider::new().generate(&req, &ctx(&server)).await.unwrap();
    let body = body_json(&requests(&server).await[0]);
    assert_eq!(body, json!({"model": "gpt-image-2", "prompt": "a watercolor lighthouse"}));
    for forbidden in ["response_format", "style", "input_fidelity", "user", "stream", "n", "quality", "size"]
    {
        assert!(body.get(forbidden).is_none(), "{forbidden}");
    }
    assert_eq!(out.images[0].media_type, "image/png");
}

#[tokio::test]
async fn edit_sends_every_image_and_the_mask_as_data_urls_with_mapped_options() {
    let server = MockServer::start().await;
    let out_png = png_rgb(40, 30);
    mount(&server, EDIT, ok(images_body(&[&out_png], Some("png")))).await;
    let first = png_rgb(40, 30);
    let second = jpeg(20, 20);
    let third = webp(16, 16);
    let mask = png_rgba(40, 30);
    let req = edit_request(
        vec![
            input(InputRole::Image, "a.png", "image/png", first.clone()),
            input(InputRole::Image, "b.jpg", "image/jpeg", second.clone()),
            input(InputRole::Image, "c.webp", "image/webp", third.clone()),
        ],
        Some(input(InputRole::Mask, "mask.png", "image/png", mask.clone())),
        options(&[
            ("count", OptionValue::Int(1)),
            ("size", s("1024x1024")),
            ("quality", s("high")),
            ("format", s("png")),
            ("background", s("transparent")),
            ("moderation", s("auto")),
        ]),
    );
    let out = OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();

    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), EDIT);
    assert_eq!(header(&reqs[0], "authorization"), Some("Bearer test-openai-key-000"));
    assert_eq!(header(&reqs[0], "content-type"), Some("application/json"));
    assert!(header(&reqs[0], "x-client-request-id").is_some());
    assert_eq!(
        body_json(&reqs[0]),
        json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "add a red scarf",
            "images": [
                {"image_url": format!("data:image/png;base64,{}", STANDARD.encode(&first))},
                {"image_url": format!("data:image/jpeg;base64,{}", STANDARD.encode(&second))},
                {"image_url": format!("data:image/webp;base64,{}", STANDARD.encode(&third))}
            ],
            "mask": {"image_url": format!("data:image/png;base64,{}", STANDARD.encode(&mask))},
            "n": 1,
            "size": "1024x1024",
            "quality": "high",
            "output_format": "png",
            "background": "transparent",
            "moderation": "auto"
        })
    );
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].bytes, out_png);
}

#[tokio::test]
async fn edit_without_mask_or_options_sends_no_mask_field() {
    let server = MockServer::start().await;
    let img = png_rgb(16, 16);
    mount(&server, EDIT, ok(images_body(&[&img], None))).await;
    let req = edit_request(
        vec![input(InputRole::Image, "a.png", "image/png", img.clone())],
        None,
        ResolvedOptions::new(),
    );
    OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    assert_eq!(
        body_json(&requests(&server).await[0]),
        json!({
            "model": "gpt-image-2.5-sunburst",
            "prompt": "add a red scarf",
            "images": [{"image_url": format!("data:image/png;base64,{}", STANDARD.encode(&img))}]
        })
    );
}

#[tokio::test]
async fn a_base_url_with_a_trailing_slash_keeps_the_v1_segment() {
    let server = MockServer::start().await;
    let img = png_rgb(8, 8);
    mount(&server, GEN, ok(images_body(&[&img], None))).await;
    let ctx = ctx_for(&format!("{}/v1/", server.uri()));
    OpenAiProvider::new().generate(&generate_request(ResolvedOptions::new()), &ctx).await.unwrap();
    assert_eq!(requests(&server).await[0].url.path(), GEN);
}

#[tokio::test]
async fn options_the_adapter_does_not_map_are_internal_errors_and_nothing_is_sent() {
    let server = MockServer::start().await;
    mount(&server, GEN, ok(images_body(&[&png_rgb(8, 8)], None))).await;
    for opts in [
        options(&[("seed", OptionValue::Int(7))]),
        options(&[("style", s("vivid"))]),
        options(&[("response_format", s("url"))]),
        options(&[("count", s("2"))]),                // wrong value type
        options(&[("size", OptionValue::Int(1024))]), // wrong value type
        options(&[("format", s("gif"))]),             // not an output format
    ] {
        let err = generate(&server, opts.clone()).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError, "{opts:?}");
    }
    let edit = edit_request(
        vec![input(InputRole::Image, "a.png", "image/png", png_rgb(8, 8))],
        None,
        options(&[("input_fidelity", s("high"))]),
    );
    assert_eq!(
        OpenAiProvider::new().edit(&edit, &ctx(&server)).await.unwrap_err().code,
        ErrorCode::InternalError
    );
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn generate_and_edit_refuse_requests_they_cannot_express_without_sending() {
    let server = MockServer::start().await;
    let p = OpenAiProvider::new();
    let img = input(InputRole::Image, "a.png", "image/png", png_rgb(8, 8));
    // A generation body has no place for input images: they are never dropped silently.
    let mut with_image = generate_request(ResolvedOptions::new());
    with_image.images.push(img.clone());
    assert_eq!(p.generate(&with_image, &ctx(&server)).await.unwrap_err().code, ErrorCode::InternalError);
    let mut with_mask = generate_request(ResolvedOptions::new());
    with_mask.mask = Some(input(InputRole::Mask, "m.png", "image/png", png_rgba(8, 8)));
    assert_eq!(p.generate(&with_mask, &ctx(&server)).await.unwrap_err().code, ErrorCode::InternalError);
    let no_images = edit_request(Vec::new(), None, ResolvedOptions::new());
    assert_eq!(p.edit(&no_images, &ctx(&server)).await.unwrap_err().code, ErrorCode::UsageError);
    let seventeen = edit_request(vec![img; 17], None, ResolvedOptions::new());
    assert_eq!(p.edit(&seventeen, &ctx(&server)).await.unwrap_err().code, ErrorCode::InvalidArgument);
    assert!(requests(&server).await.is_empty());
}

// ---------------------------------------------------------------- local input checks

async fn edit_err(server: &MockServer, images: Vec<InputImage>, mask: Option<InputImage>) -> IrisError {
    let req = edit_request(images, mask, ResolvedOptions::new());
    OpenAiProvider::new().edit(&req, &ctx(server)).await.expect_err("expected a local rejection")
}

#[tokio::test]
async fn mask_checks_reject_locally_with_zero_requests() {
    let server = MockServer::start().await;
    mount(&server, EDIT, ok(images_body(&[&png_rgb(8, 8)], None))).await;
    let first = || input(InputRole::Image, "photo.png", "image/png", png_rgb(64, 32));

    // Not a PNG.
    let err =
        edit_err(&server, vec![first()], Some(input(InputRole::Mask, "m.jpg", "image/jpeg", jpeg(64, 32))))
            .await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("must be a PNG"), "{}", err.message);
    assert_eq!(err.details.get("path"), Some(&json!("/fixtures/m.jpg")));

    // PNG without an alpha channel.
    let err =
        edit_err(&server, vec![first()], Some(input(InputRole::Mask, "m.png", "image/png", png_rgb(64, 32))))
            .await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("no alpha channel"), "{}", err.message);

    // Different dimensions from the FIRST input image (a later image matching does not help).
    let second = input(InputRole::Image, "other.png", "image/png", png_rgb(32, 32));
    let err = edit_err(
        &server,
        vec![first(), second],
        Some(input(InputRole::Mask, "m.png", "image/png", png_rgba(32, 32))),
    )
    .await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("32x32") && err.message.contains("64x32"), "{}", err.message);

    // Larger than 4 MB (checked before decoding).
    let mut big = png_rgba(64, 32);
    big.resize(4_000_001, 0);
    let err =
        edit_err(&server, vec![first()], Some(input(InputRole::Mask, "big.png", "image/png", big))).await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("4 MB"), "{}", err.message);

    // Corrupt PNG.
    let mut broken = png_rgba(64, 32);
    broken.truncate(40);
    let err =
        edit_err(&server, vec![first()], Some(input(InputRole::Mask, "cut.png", "image/png", broken))).await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);

    assert!(requests(&server).await.is_empty(), "mask problems must be caught before sending");

    // A valid mask with the first image's dimensions goes through.
    let ok_mask = input(InputRole::Mask, "m.png", "image/png", png_rgba(64, 32));
    let req = edit_request(vec![first()], Some(ok_mask), ResolvedOptions::new());
    OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn a_mask_is_accepted_for_a_jpeg_first_image_of_the_same_size() {
    let server = MockServer::start().await;
    mount(&server, EDIT, ok(images_body(&[&png_rgb(8, 8)], None))).await;
    let req = edit_request(
        vec![input(InputRole::Image, "photo.jpg", "image/jpeg", jpeg(48, 16))],
        Some(input(InputRole::Mask, "m.png", "image/png", png_rgba(48, 16))),
        ResolvedOptions::new(),
    );
    OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn data_url_length_is_capped_per_image_before_sending() {
    let server = MockServer::start().await;
    mount(&server, EDIT, ok(images_body(&[&png_rgb(8, 8)], None))).await;
    // "data:image/png;base64," is 22 characters; 15,728,622 bytes encode to 20,971,496.
    let sized = |n: usize| {
        let mut bytes = png_rgb(8, 8);
        bytes.resize(n, 0);
        input(InputRole::Image, "huge.png", "image/png", bytes)
    };
    let err = edit_err(&server, vec![sized(15_728_623)], None).await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    assert!(err.message.contains("20971520"), "{}", err.message);
    assert!(requests(&server).await.is_empty());

    // Exactly at the limit is sent (20,971,518 characters).
    let req = edit_request(vec![sized(15_728_622)], None, ResolvedOptions::new());
    OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    let url = body_json(&reqs[0])["images"][0]["image_url"].as_str().unwrap().len();
    assert_eq!(url, 20_971_518);
}

#[tokio::test]
async fn input_images_of_unsupported_types_are_rejected_by_content_not_by_label() {
    let server = MockServer::start().await;
    // Labeled PNG, but the bytes are not an image.
    let err =
        edit_err(&server, vec![input(InputRole::Image, "x.png", "image/png", b"GIF89a....".to_vec())], None)
            .await;
    assert_eq!(err.code, ErrorCode::InputFileInvalid);
    // Labeled JPEG, content is PNG: sent with the sniffed type.
    mount(&server, EDIT, ok(images_body(&[&png_rgb(8, 8)], None))).await;
    let png = png_rgb(8, 8);
    let req = edit_request(
        vec![input(InputRole::Image, "x.jpg", "image/jpeg", png.clone())],
        None,
        ResolvedOptions::new(),
    );
    OpenAiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    let body = body_json(&requests(&server).await[0]);
    assert_eq!(
        body["images"][0]["image_url"],
        json!(format!("data:image/png;base64,{}", STANDARD.encode(&png)))
    );
}

// ---------------------------------------------------------------- response decoding

#[tokio::test]
async fn output_media_type_comes_from_the_magic_bytes_and_matches_the_echo_and_requested_format() {
    // (content, output_format echo, requested --format, expected media type)
    let cases = [
        (png_rgb(8, 8), Some("png"), None, "image/png"),
        (jpeg(8, 8), Some("jpeg"), None, "image/jpeg"),
        (webp(8, 8), Some("webp"), None, "image/webp"),
        (jpeg(8, 8), None, Some("jpeg"), "image/jpeg"), // no echo: requested format
        (webp(8, 8), Some("WEBP"), Some("webp"), "image/webp"), // echo is case-insensitive
        (png_rgb(8, 8), None, None, "image/png"),       // no echo, no format: provider default png
    ];
    for (bytes, echo, requested, expected) in cases {
        let server = MockServer::start().await;
        mount(&server, GEN, ok(images_body(&[&bytes], echo))).await;
        let opts = match requested {
            Some(f) => options(&[("format", s(f))]),
            None => ResolvedOptions::new(),
        };
        let out = generate(&server, opts).await.unwrap();
        assert_eq!(out.images[0].media_type, expected, "{echo:?} {requested:?}");
        assert_eq!(out.images[0].bytes, bytes);
        assert!(out.warnings.is_empty(), "{echo:?} {requested:?}: {:?}", out.warnings);
    }
}

/// A minimal valid GIF (1x1). OpenAI never returns one; it stands for "a valid image
/// of a type nobody asked for".
const GIF_1X1: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\x3b";

#[tokio::test]
async fn a_valid_image_of_another_type_is_kept_under_its_real_type_with_a_warning() {
    // (name, content, output_format echo, requested --format, kept media type, text the warning names)
    let cases = [
        ("echo png, bytes jpeg", jpeg(8, 8), Some("png"), None, "image/jpeg", "declared output_format png"),
        (
            "requested webp, bytes png",
            png_rgb(8, 8),
            None,
            Some("webp"),
            "image/png",
            "asked for output_format webp",
        ),
        (
            "echo png matches the bytes, requested jpeg",
            png_rgb(8, 8),
            Some("png"),
            Some("jpeg"),
            "image/png",
            "asked for output_format jpeg",
        ),
        (
            "no echo, no format, bytes gif",
            GIF_1X1.to_vec(),
            None,
            None,
            "image/gif",
            "default output_format is png",
        ),
    ];
    for (name, bytes, echo, requested, kept, names) in cases {
        let server = MockServer::start().await;
        mount(&server, GEN, ok(images_body(&[&bytes], echo))).await;
        let opts = match requested {
            Some(f) => options(&[("format", s(f))]),
            None => ResolvedOptions::new(),
        };
        let out = generate(&server, opts).await.unwrap_or_else(|e| panic!("{name}: {}", e.message));
        assert_eq!(out.images.len(), 1, "{name}");
        assert_eq!(out.images[0].media_type, kept, "{name}");
        assert_eq!(out.images[0].bytes, bytes, "{name}: kept verbatim");
        assert_eq!(out.provider_request_id.as_deref(), Some("req_ok_123"), "{name}");
        assert_eq!(out.warnings.len(), 1, "{name}: {:?}", out.warnings);
        assert_eq!(out.warnings[0].code, "output_format_mismatch", "{name}");
        let message = &out.warnings[0].message;
        assert!(message.contains(kept) && message.contains(names), "{name}: {message}");
        if name.starts_with("echo png matches") {
            assert!(!message.contains("declared"), "{name}: the met echo is not a complaint: {message}");
        }
        assert_eq!(requests(&server).await.len(), 1, "{name}: a paid call is never repeated");
    }

    // Only the mismatched image of a multi-image answer is reported.
    let server = MockServer::start().await;
    let (good, odd) = (png_rgb(8, 8), jpeg(8, 8));
    mount(&server, GEN, ok(images_body(&[&good, &odd], Some("png")))).await;
    let out = generate(&server, options(&[("count", OptionValue::Int(2))])).await.unwrap();
    let types: Vec<&str> = out.images.iter().map(|i| i.media_type.as_str()).collect();
    assert_eq!(types, ["image/png", "image/jpeg"]);
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(out.warnings[0].message.contains("response item 1 "), "{}", out.warnings[0].message);
}

#[tokio::test]
async fn a_different_number_of_images_than_requested_is_kept_with_a_warning() {
    let img = png_rgb(8, 8);
    // (requested count, images returned, warning expected)
    let cases = [(Some(4), 1, true), (None, 2, true), (Some(2), 2, false), (None, 1, false)];
    for (count, returned, warns) in cases {
        let server = MockServer::start().await;
        let images: Vec<&[u8]> = (0..returned).map(|_| img.as_slice()).collect();
        mount(&server, GEN, ok(images_body(&images, Some("png")))).await;
        let opts = match count {
            Some(n) => options(&[("count", OptionValue::Int(n))]),
            None => ResolvedOptions::new(),
        };
        let out = generate(&server, opts).await.unwrap();
        assert_eq!(out.images.len(), returned, "{count:?}: every returned image is kept");
        let codes: Vec<&str> = out.warnings.iter().map(|w| w.code.as_str()).collect();
        if warns {
            assert_eq!(codes, ["unexpected_output_count"], "{count:?}");
            let wanted = count.unwrap_or(1);
            let message = &out.warnings[0].message;
            assert!(
                message.contains(&format!("returned {returned} "))
                    && message.contains(&format!("({returned} usable) for a request of {wanted} ")),
                "{message}"
            );
        } else {
            assert!(codes.is_empty(), "{count:?}: {codes:?}");
        }
        assert_eq!(requests(&server).await.len(), 1);
    }
}

#[tokio::test]
async fn unpadded_base64_is_accepted() {
    let server = MockServer::start().await;
    // Trailing bytes after IEND do not change the sniffed type; make the length need padding.
    let mut img = png_rgb(5, 7);
    while img.len().is_multiple_of(3) {
        img.push(0);
    }
    let encoded = STANDARD_NO_PAD.encode(&img);
    assert!(STANDARD.encode(&img).ends_with('='), "fixture must need padding");
    mount(&server, GEN, ok(json!({"created": 1, "data": [{"b64_json": encoded}], "output_format": "png"})))
        .await;
    assert_eq!(generate(&server, ResolvedOptions::new()).await.unwrap().images[0].bytes, img);
}

#[tokio::test]
async fn unusable_success_bodies_are_provider_bad_response_and_never_retried() {
    // An ISO-BMFF `ftyp` box: sniffed as video/mp4, which is not an image.
    let mp4_head = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isommp41";
    let cases: Vec<(&str, ResponseTemplate)> = vec![
        ("zero images", ok(json!({"created": 1, "data": []}))),
        ("no data", ok(json!({"created": 1}))),
        ("data not an array", ok(json!({"created": 1, "data": {"b64_json": "x"}}))),
        (
            "not json",
            ResponseTemplate::new(200)
                .set_body_string("<html>ok</html>")
                .insert_header("x-request-id", "req_ok_123"),
        ),
        (
            "url instead of b64",
            ok(json!({"created": 1, "data": [{"url": "https://files.example/x.png?sig=abc"}]})),
        ),
        ("missing b64", ok(json!({"created": 1, "data": [{"revised_prompt": "x"}]}))),
        ("bad base64", ok(json!({"created": 1, "data": [{"b64_json": "@@not base64@@"}]}))),
        (
            "not an image",
            ok(json!({"created": 1, "data": [{"b64_json": STANDARD.encode(b"{\"error\":1}")}]})),
        ),
        ("a video, not an image", ok(images_body(&[mp4_head], Some("png")))),
    ];
    for (name, template) in cases {
        let server = MockServer::start().await;
        mount(&server, GEN, template).await;
        let err = generate(&server, ResolvedOptions::new()).await.expect_err(name);
        assert_eq!(err.code, ErrorCode::ProviderBadResponse, "{name}: {}", err.message);
        assert_eq!(err.provider, Some(ProviderId::OpenAi), "{name}");
        assert_eq!(err.provider_status, Some(200), "{name}");
        assert_eq!(err.provider_request_id.as_deref(), Some("req_ok_123"), "{name}");
        assert_eq!(err.details.get("charge_possible"), Some(&json!(true)), "{name}");
        assert!(!err.message.contains("sig=abc"), "{name}: {}", err.message);
        assert_eq!(requests(&server).await.len(), 1, "{name}: a paid call is never repeated");
    }
    // The video case says what the content was.
    let server = MockServer::start().await;
    mount(&server, GEN, ok(images_body(&[mp4_head], Some("png")))).await;
    let err = generate(&server, ResolvedOptions::new()).await.unwrap_err();
    assert_eq!(err.details.get("actual_media_type"), Some(&json!("video/mp4")));
    assert_eq!(err.details.get("expected_media_type"), Some(&json!("image/png")));
}

#[tokio::test]
async fn one_unusable_item_never_drops_the_usable_images() {
    let good = png_rgb(8, 8);
    let mp4_head = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isommp41";
    // (case, the bad item, where it sits, text its warning must contain)
    let cases: Vec<(&str, Value, usize, &str)> = vec![
        ("a URL", json!({"url": "https://files.example/x.png?sig=abc"}), 1, "is a URL"),
        ("missing data", json!({"revised_prompt": "x"}), 0, "has no b64_json"),
        ("bad base64", json!({"b64_json": "@@not base64@@"}), 1, "not valid base64"),
        (
            "an error body",
            json!({"b64_json": STANDARD.encode(b"{\"error\":1}")}),
            0,
            "not a recognized image",
        ),
        ("a video", json!({"b64_json": STANDARD.encode(mp4_head)}), 1, "video/mp4"),
    ];
    for (name, bad, at, reason) in cases {
        let mut data = vec![json!({"b64_json": STANDARD.encode(&good)})];
        data.insert(at, bad);
        let server = MockServer::start().await;
        mount(
            &server,
            GEN,
            ok(json!({"created": 1, "data": data, "output_format": "png", "usage": {"output_tokens": 9}})),
        )
        .await;
        let out = generate(&server, options(&[("count", OptionValue::Int(2))]))
            .await
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        assert_eq!(out.images.len(), 1, "{name}: the usable image is kept");
        assert_eq!(out.images[0].bytes, good, "{name}");
        let codes: Vec<&str> = out.warnings.iter().map(|w| w.code.as_str()).collect();
        assert_eq!(codes, ["output_item_unusable"], "{name}: two items came back for n=2");
        let message = &out.warnings[0].message;
        assert!(
            message.contains(&format!("response item {at} ")) && message.contains(reason),
            "{name}: {message}"
        );
        assert!(!message.contains("sig=abc"), "{name}: {message}");
        assert_eq!(out.usage.as_ref().and_then(|u| u.output_tokens), Some(9), "{name}: usage is kept");
        assert_eq!(out.provider_request_id.as_deref(), Some("req_ok_123"), "{name}");
        assert_eq!(requests(&server).await.len(), 1, "{name}: a paid call is never repeated");
    }

    // With n=1, the unusable extra item is reported along with the count, which
    // counts items, not images.
    let server = MockServer::start().await;
    let data = json!([{"b64_json": STANDARD.encode(&good)}, {"b64_json": "@@"}]);
    mount(&server, GEN, ok(json!({"created": 1, "data": data}))).await;
    let out = generate(&server, ResolvedOptions::new()).await.unwrap();
    let codes: Vec<&str> = out.warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(codes, ["output_item_unusable", "unexpected_output_count"]);
    let count = &out.warnings[1].message;
    assert!(count.contains("returned 2 items (1 usable) for a request of 1"), "{count}");

    // Several items and none usable: one provider_bad_response naming each.
    let server = MockServer::start().await;
    let data = json!([{"url": "https://files.example/a.png"}, {"b64_json": "@@"}]);
    mount(&server, GEN, ok(json!({"created": 1, "data": data}))).await;
    let err = generate(&server, options(&[("count", OptionValue::Int(2))])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ProviderBadResponse);
    assert!(
        err.message.contains("response item 0 is a URL")
            && err.message.contains("response item 1 is not valid"),
        "{}",
        err.message
    );
    assert_eq!(err.details.get("charge_possible"), Some(&json!(true)));
}

#[tokio::test]
async fn usage_is_parsed_into_normalized_fields_and_a_numbers_only_provider_object() {
    let server = MockServer::start().await;
    let img = png_rgb(8, 8);
    let mut body = images_body(&[&img], Some("png"));
    body["usage"] = json!({
        "total_tokens": 220,
        "input_tokens": 24,
        "output_tokens": 196,
        "input_tokens_details": {"text_tokens": 24, "image_tokens": 0},
        "output_tokens_details": {"image_tokens": 196, "text_tokens": 0},
        "note": "strings are dropped"
    });
    mount(&server, GEN, ok(body)).await;
    let out = generate(&server, ResolvedOptions::new()).await.unwrap();
    let usage = out.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens, usage.total_tokens),
        (Some(24), Some(196), Some(220))
    );
    assert_eq!(
        usage.provider_usage.clone().unwrap(),
        json!({
            "total_tokens": 220,
            "input_tokens": 24,
            "output_tokens": 196,
            "input_tokens_details": {"text_tokens": 24, "image_tokens": 0},
            "output_tokens_details": {"image_tokens": 196, "text_tokens": 0}
        })
    );
    let spec = iris::catalog::find("gpt-image-2.5-sunburst").unwrap();
    assert_eq!(iris::catalog::openai::cost_from_usage(spec, &usage).unwrap().amount, 0.006);
}

#[tokio::test]
async fn absent_or_malformed_usage_never_fails_the_call() {
    for usage in [None, Some(json!("n/a")), Some(json!({"input_tokens": "24"})), Some(json!([1, 2]))] {
        let server = MockServer::start().await;
        let img = png_rgb(8, 8);
        let mut body = images_body(&[&img], Some("png"));
        if let Some(u) = usage.clone() {
            body["usage"] = u;
        }
        mount(&server, GEN, ok(body)).await;
        let out = generate(&server, ResolvedOptions::new()).await.unwrap();
        assert!(out.usage.is_none(), "{usage:?}");
        assert_eq!(out.images.len(), 1);
    }
}

#[tokio::test]
async fn revised_prompts_become_text_when_present() {
    let server = MockServer::start().await;
    let img = png_rgb(8, 8);
    mount(
        &server,
        GEN,
        ok(json!({"created": 1, "output_format": "png", "data": [
            {"b64_json": STANDARD.encode(&img), "revised_prompt": "A lighthouse, watercolor"},
            {"b64_json": STANDARD.encode(&img), "revised_prompt": "A lighthouse, watercolor"}
        ]})),
    )
    .await;
    let out = generate(&server, ResolvedOptions::new()).await.unwrap();
    assert_eq!(out.images.len(), 2);
    assert_eq!(out.text.as_deref(), Some("A lighthouse, watercolor"));
}

// ---------------------------------------------------------------- error mapping

#[tokio::test]
async fn error_table_maps_every_documented_status() {
    struct Case {
        name: &'static str,
        status: u16,
        body: Value,
        code: ErrorCode,
        retryable: Option<bool>,
        provider_code: Option<&'static str>,
    }
    let cases = [
        Case {
            name: "400 invalid_request_error",
            status: 400,
            body: openai_error("Invalid value for 'size'.", "invalid_request_error", Some("invalid_value")),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_value"),
        },
        Case {
            name: "400 without a code",
            status: 400,
            body: openai_error("Bad request.", "invalid_request_error", None),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "400 image_generation_user_error (not a moderation block)",
            status: 400,
            body: openai_error(
                "The mask does not match the image.",
                "image_generation_user_error",
                Some("invalid_mask"),
            ),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_mask"),
        },
        Case {
            name: "400 image_generation_user_error without a code",
            status: 400,
            body: openai_error("The request cannot be fulfilled.", "image_generation_user_error", None),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("image_generation_user_error"),
        },
        Case {
            name: "402 (undocumented; payment required)",
            status: 402,
            body: openai_error("Payment required.", "invalid_request_error", None),
            code: ErrorCode::QuotaExceeded,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "408",
            status: 408,
            body: json!({}),
            code: ErrorCode::SubmissionUncertain,
            retryable: Some(false),
            provider_code: None,
        },
        Case {
            name: "413",
            status: 413,
            body: openai_error("Request entity too large.", "invalid_request_error", None),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "401",
            status: 401,
            body: openai_error(
                "Incorrect API key provided: test-ope***000.",
                "invalid_request_error",
                Some("invalid_api_key"),
            ),
            code: ErrorCode::AuthenticationFailed,
            retryable: Some(false),
            provider_code: Some("invalid_api_key"),
        },
        Case {
            name: "403",
            status: 403,
            body: openai_error(
                "Your organization must be verified to use the model.",
                "invalid_request_error",
                None,
            ),
            code: ErrorCode::PermissionDenied,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "404",
            status: 404,
            body: openai_error(
                "The model does not exist or you do not have access to it.",
                "invalid_request_error",
                Some("model_not_found"),
            ),
            code: ErrorCode::PermissionDenied,
            retryable: Some(false),
            provider_code: Some("model_not_found"),
        },
        Case {
            name: "409",
            status: 409,
            body: openai_error("Conflict.", "invalid_request_error", None),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "422",
            status: 422,
            body: openai_error("Unprocessable.", "invalid_request_error", None),
            code: ErrorCode::InvalidArgument,
            retryable: Some(false),
            provider_code: Some("invalid_request_error"),
        },
        Case {
            name: "500",
            status: 500,
            body: openai_error("The server had an error.", "server_error", None),
            code: ErrorCode::SubmissionUncertain,
            retryable: Some(false),
            provider_code: Some("server_error"),
        },
        Case {
            name: "502",
            status: 502,
            body: json!({}),
            code: ErrorCode::SubmissionUncertain,
            retryable: Some(false),
            provider_code: None,
        },
        Case {
            name: "504",
            status: 504,
            body: json!({}),
            code: ErrorCode::SubmissionUncertain,
            retryable: Some(false),
            provider_code: None,
        },
        Case {
            name: "503 without server_is_overloaded",
            status: 503,
            body: openai_error("Service unavailable.", "service_unavailable_error", None),
            code: ErrorCode::SubmissionUncertain,
            retryable: Some(false),
            provider_code: Some("service_unavailable_error"),
        },
    ];
    for case in cases {
        let server = MockServer::start().await;
        mount(&server, GEN, err_response(case.status, case.body.clone())).await;
        let err = generate_err(&server).await;
        let name = case.name;
        assert_eq!(err.code, case.code, "{name}: {}", err.message);
        assert_eq!(err.retryable, case.retryable, "{name}");
        assert_eq!(err.provider, Some(ProviderId::OpenAi), "{name}");
        assert_eq!(err.provider_status, Some(case.status), "{name}");
        assert_eq!(err.provider_request_id.as_deref(), Some("req_err_456"), "{name}");
        assert_eq!(err.provider_code.as_deref(), case.provider_code, "{name}");
        if let Some(msg) = case.body["error"]["message"].as_str() {
            assert_eq!(err.details.get("provider_message"), Some(&json!(msg)), "{name}");
        }
        assert!(err.hint.is_some(), "{name}: every mapped error carries a hint");
        assert_eq!(requests(&server).await.len(), 1, "{name}: never retried");
        if case.status == 408 || case.status >= 500 {
            // OpenAI may have processed the request: uncertain (exit 5), never "retry it".
            assert_eq!(err.exit_code(), 5, "{name}");
            assert_eq!(err.details.get("charge_possible"), Some(&json!(true)), "{name}");
            assert!(err.hint.as_deref().unwrap().contains("did not retry automatically"), "{name}");
            assert!(err.details.get("client_request_id").is_some(), "{name}");
            assert!(err.job_id.is_none(), "{name}: a synchronous call has no job");
        } else {
            assert!(err.details.get("charge_possible").is_none(), "{name}");
        }
        if case.code == ErrorCode::QuotaExceeded {
            assert!(err.hint.as_deref().unwrap().contains("billing"), "{name}: {:?}", err.hint);
        }
    }
}

#[tokio::test]
async fn image_generation_user_errors_other_than_moderation_are_invalid_argument() {
    let server = MockServer::start().await;
    mount(
        &server,
        EDIT,
        err_response(
            400,
            openai_error(
                "The mask does not match the first image.",
                "image_generation_user_error",
                Some("invalid_mask"),
            ),
        ),
    )
    .await;
    let images = vec![input(InputRole::Image, "a.png", "image/png", png_rgb(8, 8))];
    let err = OpenAiProvider::new()
        .edit(&edit_request(images, None, ResolvedOptions::new()), &ctx(&server))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.code.exit_code(), 2);
    assert_eq!(err.provider_code.as_deref(), Some("invalid_mask"));
    assert_eq!(err.details.get("provider_message"), Some(&json!("The mask does not match the first image.")));
    assert!(!err.message.contains("content policy"), "not a moderation block: {}", err.message);
    assert!(err.message.contains("mask does not match"), "{}", err.message);
    let hint = err.hint.as_deref().unwrap();
    assert!(hint.contains("prompt") && hint.contains("mask") && hint.contains("options"), "{hint}");
    assert!(err.details.get("moderation_stage").is_none());
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn moderation_blocks_become_content_blocked_with_stage_and_categories() {
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(
            400,
            json!({"error": {
                "message": "Your request was rejected by the safety system.",
                "type": "image_generation_user_error",
                "param": null,
                "code": "moderation_blocked",
                "moderation_details": {"moderation_stage": "input", "categories": ["harassment", "violence"]}
            }}),
        ),
    )
    .await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::ContentBlocked);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.provider_code.as_deref(), Some("moderation_blocked"));
    assert_eq!(err.details.get("moderation_stage"), Some(&json!("input")));
    assert_eq!(err.details.get("categories"), Some(&json!(["harassment", "violence"])));
    assert!(err.message.contains("input") && err.message.contains("harassment"), "{}", err.message);
    assert_eq!(requests(&server).await.len(), 1);

    // code moderation_blocked even with another type.
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(400, openai_error("Blocked.", "invalid_request_error", Some("moderation_blocked"))),
    )
    .await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::ContentBlocked);
    assert!(err.details.get("moderation_stage").is_none() && err.details.get("categories").is_none());
    assert_eq!(requests(&server).await.len(), 1);

    // moderation_details marks a moderation block even under another code.
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(
            400,
            json!({"error": {
                "message": "The generated image was blocked.",
                "type": "image_generation_user_error",
                "param": null,
                "code": "output_blocked",
                "moderation_details": {"moderation_stage": "output", "categories": []}
            }}),
        ),
    )
    .await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::ContentBlocked);
    assert_eq!(err.provider_code.as_deref(), Some("output_blocked"));
    assert_eq!(err.details.get("moderation_stage"), Some(&json!("output")));
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn permission_errors_explain_verification_region_and_model_access() {
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(403, openai_error("Country not supported.", "invalid_request_error", None)),
    )
    .await;
    let hint = generate_err(&server).await.hint.clone().unwrap();
    assert!(
        hint.contains("Organization Verification") && hint.contains("region") && hint.contains("project"),
        "{hint}"
    );

    let server = MockServer::start().await;
    mount(&server, GEN, err_response(404, openai_error("Not found.", "invalid_request_error", None))).await;
    let err = generate_err(&server).await;
    assert!(
        err.message.contains("gpt-image-2.5-sunburst") && err.message.contains("not available"),
        "{}",
        err.message
    );

    let server = MockServer::start().await;
    mount(&server, GEN, err_response(401, openai_error("Bad key.", "invalid_request_error", None))).await;
    assert!(generate_err(&server).await.hint.clone().unwrap().contains("OPENAI_API_KEY"));
}

#[tokio::test]
async fn quota_and_billing_429s_are_final_quota_exceeded_after_one_request() {
    let bodies = [
        openai_error("You exceeded your current quota.", "insufficient_quota", Some("insufficient_quota")),
        openai_error("No credits.", "insufficient_quota", Some("credit_balance_exhausted")),
        openai_error("Spend limit.", "insufficient_quota", Some("organization_spend_limit_exceeded")),
        openai_error("Project limit.", "insufficient_quota", Some("project_spend_limit_exceeded")),
        openai_error("Usage limit.", "insufficient_quota", Some("organization_usage_limit_exceeded")),
        openai_error("No credits.", "requests", Some("credit_balance_exhausted")),
        openai_error("Quota.", "insufficient_quota", None),
    ];
    for body in bodies {
        let server = MockServer::start().await;
        mount(&server, GEN, err_response(429, body.clone()).insert_header("retry-after", "1")).await;
        let err = generate_err(&server).await;
        assert_eq!(err.code, ErrorCode::QuotaExceeded, "{body}");
        assert_eq!(err.retryable, Some(false), "{body}");
        assert_eq!(err.provider_status, Some(429));
        assert!(err.hint.as_deref().unwrap().contains("retrying will not help"), "{body}");
        assert_eq!(err.retry_after, None, "{body}: a quota error is not an invitation to retry");
        assert_eq!(requests(&server).await.len(), 1, "{body}: quota errors are never retried");
    }
}

#[tokio::test]
async fn rate_limit_429s_are_retried_then_succeed() {
    let bodies = [
        openai_error("Rate limit reached for requests", "requests", Some("rate_limit_exceeded")),
        openai_error("Slow down.", "rate_limit_error", Some("slow_down")),
        openai_error("Rate limit reached for gpt-image on images per min (IPM).", "requests", None),
        json!({}),
    ];
    let img = png_rgb(8, 8);
    for body in bodies {
        let server = MockServer::start().await;
        mount_sequence(
            &server,
            GEN,
            err_response(429, body.clone()),
            1,
            ok(images_body(&[&img], Some("png"))),
        )
        .await;
        let out = generate(&server, ResolvedOptions::new()).await.unwrap();
        assert_eq!(out.images.len(), 1, "{body}");
        let reqs = requests(&server).await;
        assert_eq!(reqs.len(), 2, "{body}: one retry after the rejection");
        let ids: Vec<&str> = reqs.iter().map(|r| header(r, "x-client-request-id").unwrap()).collect();
        assert_ne!(ids[0], ids[1], "each attempt carries a fresh X-Client-Request-Id");
        assert_eq!(out.provider_request_id.as_deref(), Some("req_ok_123"));
    }
}

#[tokio::test]
async fn rate_limit_retries_honor_retry_after_and_stop_after_three_attempts() {
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(429, openai_error("Slow down.", "rate_limit_error", Some("slow_down")))
            .insert_header("retry-after-ms", "30"),
    )
    .await;
    let started = std::time::Instant::now();
    let err = generate_err(&server).await;
    assert!(started.elapsed() >= Duration::from_millis(60), "two waits of 30ms");
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.provider_code.as_deref(), Some("slow_down"));
    assert_eq!(err.retry_after, Some(Duration::from_millis(30)));
    assert_eq!(requests(&server).await.len(), 3, "PaidSubmit makes at most 3 attempts");
}

#[tokio::test]
async fn a_retry_after_beyond_the_cap_stops_with_rate_limited_after_one_request() {
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(429, openai_error("Rate limit.", "requests", None)).insert_header("retry-after", "600"),
    )
    .await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::from_secs(600)));
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn overloaded_503_is_retried_then_succeeds() {
    let server = MockServer::start().await;
    let img = png_rgb(8, 8);
    mount_sequence(
        &server,
        GEN,
        err_response(
            503,
            openai_error(
                "The model is overloaded.",
                "service_unavailable_error",
                Some("server_is_overloaded"),
            ),
        ),
        2,
        ok(images_body(&[&img], Some("png"))),
    )
    .await;
    let out = generate(&server, ResolvedOptions::new()).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(requests(&server).await.len(), 3);

    // Persistent overload: provider_error, retryable, after 3 attempts.
    let server = MockServer::start().await;
    mount(
        &server,
        GEN,
        err_response(
            503,
            openai_error("Overloaded.", "service_unavailable_error", Some("server_is_overloaded")),
        ),
    )
    .await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::ProviderError);
    assert_eq!(err.retryable, Some(true));
    assert_eq!(err.provider_code.as_deref(), Some("server_is_overloaded"));
    assert!(err.details.get("charge_possible").is_none(), "OpenAI says the request was not processed");
    assert_eq!(requests(&server).await.len(), 3);
}

#[tokio::test]
async fn x_should_retry_false_is_respected() {
    for (status, body) in [
        (429, openai_error("Slow down.", "rate_limit_error", Some("slow_down"))),
        (503, openai_error("Overloaded.", "service_unavailable_error", Some("server_is_overloaded"))),
    ] {
        let server = MockServer::start().await;
        mount(&server, GEN, err_response(status, body).insert_header("x-should-retry", "false")).await;
        let err = generate_err(&server).await;
        assert_eq!(err.provider_status, Some(status));
        assert_eq!(requests(&server).await.len(), 1, "{status}: x-should-retry: false stops retries");
    }
}

#[tokio::test]
async fn a_timeout_after_sending_is_submission_uncertain_with_charge_possible_and_no_retry() {
    let server = MockServer::start().await;
    let img = png_rgb(8, 8);
    mount(&server, GEN, ok(images_body(&[&img], Some("png"))).set_delay(Duration::from_secs(3))).await;
    let mut ctx = ctx(&server);
    ctx.timeouts.generate = Duration::from_millis(300);
    let err =
        OpenAiProvider::new().generate(&generate_request(ResolvedOptions::new()), &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{}", err.message);
    assert_eq!(err.exit_code(), 5);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(err.details.get("transport"), Some(&json!("timeout")));
    assert!(err.job_id.is_none());
    let hint = err.hint.as_deref().unwrap();
    assert!(
        hint.contains("Iris did not retry automatically; the provider may have billed this request"),
        "{hint}"
    );
    assert_eq!(err.provider, Some(ProviderId::OpenAi));
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1, "an ambiguous paid request is never resent");
    let sent_id = header(&reqs[0], "x-client-request-id").unwrap();
    assert_eq!(err.details.get("client_request_id"), Some(&json!(sent_id)));
    assert!(hint.contains(sent_id));
}

/// A raw HTTP/1.1 server on 127.0.0.1 for failures wiremock cannot produce. For
/// every connection it reads the whole request (head and `Content-Length` body),
/// records the request head, writes `reply` (possibly nothing), and closes the
/// connection. Returns the `/v1` base URL, the connection count, and the heads.
fn raw_server(reply: &'static [u8]) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let heads = Arc::new(Mutex::new(Vec::new()));
    let (count, seen) = (connections.clone(), heads.clone());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            count.fetch_add(1, Ordering::SeqCst);
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            if let Some(head) = read_request(&mut stream) {
                seen.lock().unwrap().push(head);
            }
            let _ = stream.write_all(reply);
            let _ = stream.flush();
            // Dropping the stream closes the connection.
        }
    });
    (format!("http://{addr}/v1"), connections, heads)
}

/// Read one request completely; returns its head in lowercase.
fn read_request(stream: &mut std::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let head_end = loop {
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
    let body_len: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while buf.len() < head_end + 4 + body_len {
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(head)
}

#[tokio::test]
async fn a_connection_lost_after_sending_is_submission_uncertain_not_a_timeout() {
    // (case, reply, what the message says happened, the status that arrived)
    let cases: [(&str, &'static [u8], &str, Option<u16>); 2] = [
        (
            "closed after the request, before any response",
            b"",
            "the connection failed after the request was sent",
            None,
        ),
        (
            "2xx headers, then a truncated body",
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 5000\r\n\
              x-request-id: req_trunc\r\n\r\n{\"created\": 1, \"data\": [{\"b64_json\": \"iVBOR",
            "the answer could not be read in full",
            Some(200),
        ),
    ];
    for (name, reply, what, status) in cases {
        let (base, connections, heads) = raw_server(reply);
        let err = OpenAiProvider::new()
            .generate(&generate_request(ResolvedOptions::new()), &ctx_for(&base))
            .await
            .expect_err(name);
        assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{name}: {}", err.message);
        assert_eq!(err.retryable, Some(false), "{name}");
        // Nothing timed out: the connection failed or the answer was cut off, and the
        // transport detail says so.
        assert_eq!(err.details.get("transport"), Some(&json!("other")), "{name}");
        assert_eq!(err.provider_status, status, "{name}");
        assert_eq!(err.provider, Some(ProviderId::OpenAi), "{name}");
        assert_eq!(err.details.get("charge_possible"), Some(&json!(true)), "{name}");
        let hint = err.hint.as_deref().unwrap();
        assert!(
            hint.contains("Iris did not retry automatically; the provider may have billed this request"),
            "{name}: {hint}"
        );
        assert!(err.message.contains(&format!("({what}; ")), "{name}: {}", err.message);
        assert_eq!(
            connections.load(Ordering::SeqCst),
            1,
            "{name}: an ambiguous paid request is never resent"
        );
        let heads = heads.lock().unwrap().clone();
        assert_eq!(heads.len(), 1, "{name}");
        let sent_id = heads[0]
            .lines()
            .find_map(|l| l.strip_prefix("x-client-request-id:"))
            .map(str::trim)
            .unwrap_or_else(|| panic!("{name}: no x-client-request-id in {:?}", heads[0]));
        let reported = err.details.get("client_request_id").and_then(Value::as_str).unwrap();
        assert_eq!(reported.to_ascii_lowercase(), sent_id, "{name}");
    }
}

#[tokio::test]
async fn a_refused_connection_is_retried_and_reported_as_network_error_without_charge() {
    let ctx = ctx_for(&format!("{DEAD_URL}/v1"));
    let err =
        OpenAiProvider::new().generate(&generate_request(ResolvedOptions::new()), &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NetworkError);
    assert_eq!(err.details.get("charge_possible"), Some(&json!(false)));
    assert_eq!(err.details.get("attempts"), Some(&json!(3)));
}

#[tokio::test]
async fn provider_messages_are_redacted_and_truncated() {
    let server = MockServer::start().await;
    let long = format!(
        "Invalid image_url https://files.example.com/img.png?X-Signature=topsecret&alt=media {}",
        "x".repeat(700)
    );
    mount(&server, GEN, err_response(400, openai_error(&long, "invalid_request_error", None))).await;
    let err = generate_err(&server).await;
    let msg = err.details.get("provider_message").unwrap().as_str().unwrap();
    assert!(!msg.contains("topsecret") && msg.contains("X-Signature=REDACTED"), "{msg}");
    assert!(msg.chars().count() <= 501, "{}", msg.chars().count());
    assert!(!err.message.contains("topsecret"));

    // A non-JSON gateway page is kept only as a scrubbed excerpt.
    let server = MockServer::start().await;
    mount(&server, GEN, ResponseTemplate::new(502).set_body_string("<html>Bad Gateway</html>")).await;
    let err = generate_err(&server).await;
    assert_eq!(err.code, ErrorCode::SubmissionUncertain);
    assert_eq!(err.details.get("provider_message"), Some(&json!("<html>Bad Gateway</html>")));
    assert!(err.provider_code.is_none());
}

#[tokio::test]
async fn the_key_never_appears_in_errors_or_outputs() {
    // The mock echoes the key back. The test never sets or reads the process
    // environment, so only the adapter's scrub of the context credential can remove it.
    let echoed = format!("Incorrect API key provided: {KEY}.");
    let cases = [
        err_response(401, openai_error(&echoed, "invalid_request_error", Some("invalid_api_key"))),
        err_response(400, openai_error(&echoed, "invalid_request_error", Some(KEY))),
        ResponseTemplate::new(502).set_body_string(format!("<html>upstream said {KEY}</html>")),
        err_response(
            429,
            json!({"error": {"message": echoed, "type": "insufficient_quota", "code": "insufficient_quota"}}),
        ),
    ];
    for template in cases {
        let server = MockServer::start().await;
        mount(&server, GEN, template).await;
        let err = generate_err(&server).await;
        let dump =
            format!("{err:?} {} {:?} {:?} {:?}", err.message, err.hint, err.provider_code, err.details);
        assert!(!dump.contains(KEY), "{dump}");
        assert!(dump.contains("[REDACTED]"), "the echo was replaced, not dropped: {dump}");
    }

    // check_access errors too.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(openai_error(
            &echoed,
            "invalid_request_error",
            None,
        )))
        .mount(&server)
        .await;
    let err = OpenAiProvider::new().check_access("gpt-image-2", &ctx(&server)).await.unwrap_err();
    let dump = format!("{err:?} {} {:?}", err.message, err.details);
    assert!(!dump.contains(KEY) && dump.contains("[REDACTED]"), "{dump}");

    let ctx = ctx(&server);
    assert!(!format!("{ctx:?}").contains(KEY));
}

// ---------------------------------------------------------------- check_access

async fn access(status: u16, model: &str) -> (Result<AccountAccess, IrisError>, Vec<Request>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(status)
                .set_body_json(json!({"id": model, "object": "model", "created": 1, "owned_by": "system"}))
                .insert_header("x-request-id", "req_models_1"),
        )
        .mount(&server)
        .await;
    let result = OpenAiProvider::new().check_access(model, &ctx(&server)).await;
    let reqs = requests(&server).await;
    (result, reqs)
}

#[tokio::test]
async fn check_access_reads_the_free_model_endpoint() {
    let (result, reqs) = access(200, "gpt-image-2.5-sunburst").await;
    assert_eq!(result.unwrap(), AccountAccess::Available);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method.as_str(), "GET");
    assert_eq!(reqs[0].url.path(), "/v1/models/gpt-image-2.5-sunburst");
    assert_eq!(header(&reqs[0], "authorization"), Some("Bearer test-openai-key-000"));
    assert!(reqs[0].body.is_empty());

    let (result, reqs) = access(404, "gpt-image-2").await;
    assert_eq!(result.unwrap(), AccountAccess::Unavailable);
    assert_eq!(reqs.len(), 1);

    let (result, _) = access(401, "gpt-image-2").await;
    let err = result.unwrap_err();
    assert_eq!(err.code, ErrorCode::AuthenticationFailed);
    assert_eq!(err.provider_request_id.as_deref(), Some("req_models_1"));

    for status in [400, 403, 409] {
        let (result, reqs) = access(status, "gpt-image-2").await;
        assert_eq!(result.unwrap(), AccountAccess::Unknown, "{status}");
        assert_eq!(reqs.len(), 1, "{status}");
    }
    // Server errors are retried (a free, idempotent read) and then reported as unknown.
    let (result, reqs) = access(500, "gpt-image-2").await;
    assert_eq!(result.unwrap(), AccountAccess::Unknown);
    assert_eq!(reqs.len(), 5);
}

#[tokio::test]
async fn check_access_keeps_the_v1_segment_of_a_base_url_with_a_trailing_slash() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let ctx = ctx_for(&format!("{}/v1/", server.uri()));
    let access = OpenAiProvider::new().check_access("gpt-image-2", &ctx).await.unwrap();
    assert_eq!(access, AccountAccess::Available);
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), "/v1/models/gpt-image-2");
}

#[tokio::test]
async fn check_access_encodes_the_model_id_as_one_path_segment() {
    let (result, reqs) = access(200, "ft:gpt-image/../x y").await;
    assert_eq!(result.unwrap(), AccountAccess::Available);
    assert_eq!(reqs[0].url.path(), "/v1/models/ft:gpt-image%2F..%2Fx%20y");
}

#[tokio::test]
async fn check_access_reports_transport_failures_as_errors() {
    let ctx = ctx_for(&format!("{DEAD_URL}/v1"));
    let err = OpenAiProvider::new().check_access("gpt-image-2", &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NetworkError);
}
