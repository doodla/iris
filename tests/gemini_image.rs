//! Gemini image adapter (`generateContent`) against a local wiremock server:
//! exact request JSON, header auth, response rules, error mapping, retry
//! decisions, the 20 MB request cap, and `check_access`. Offline; fake key only.

use std::path::PathBuf;
use std::time::Duration;

use iris::catalog::{OptionValue, ResolvedOptions};
use iris::domain::{Operation, ProviderId};
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy, Timeouts};
use iris::providers::gemini::GeminiProvider;
use iris::providers::{
    AccountAccess, ImageOutput, ImageProvider, ImageRequest, InputImage, InputRole, Provider,
    ProviderContext, Registry, VideoProvider,
};
use iris::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-gemini-key-000";
const MODEL: &str = "gemini-3.1-flash-image";
const GENERATE_PATH: &str = "/v1/models/gemini-3.1-flash-image:generateContent";

fn ctx_with(server: &MockServer, generate_timeout: Duration) -> ProviderContext {
    let http = HttpClient::new(&HttpSettings {
        connect_timeout: Duration::from_secs(2),
        retry: RetryPolicy {
            base: Duration::from_millis(5),
            factor: 2.0,
            cap: Duration::from_millis(20),
            max_retry_after: Duration::from_secs(60),
        },
        system_proxy: false,
    })
    .unwrap();
    ProviderContext {
        http,
        base_url: url::Url::parse(&server.uri()).unwrap(),
        credential: Secret::new(KEY),
        timeouts: Timeouts {
            connect: Duration::from_secs(2),
            generate: generate_timeout,
            submit: Duration::from_secs(5),
            poll: Duration::from_secs(5),
            download_idle: Duration::from_secs(5),
        },
    }
}

fn ctx(server: &MockServer) -> ProviderContext {
    ctx_with(server, Duration::from_secs(5))
}

fn encode(format: image::ImageFormat) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([200, 40, 30]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn png() -> Vec<u8> {
    encode(image::ImageFormat::Png)
}

fn jpeg() -> Vec<u8> {
    encode(image::ImageFormat::Jpeg)
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn input(bytes: Vec<u8>, media_type: &str, name: &str) -> InputImage {
    InputImage {
        role: InputRole::Image,
        path: PathBuf::from(format!("/tmp/{name}")),
        file_name: name.to_string(),
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
        operation: Operation::ImageGenerate,
        model: MODEL.to_string(),
        prompt: "A red kite over green hills, watercolor".to_string(),
        images: vec![],
        mask: None,
        options: opts,
    }
}

fn image_part(mime: &str, bytes: &[u8]) -> Value {
    json!({"inlineData": {"mimeType": mime, "data": b64(bytes)}})
}

fn response_with(parts: Vec<Value>, finish: &str) -> Value {
    json!({
        "candidates": [{"content": {"role": "model", "parts": parts}, "finishReason": finish, "index": 0}],
        "usageMetadata": {
            "promptTokenCount": 14,
            "candidatesTokenCount": 747,
            "thoughtsTokenCount": 180,
            "totalTokenCount": 941,
            "promptTokensDetails": [{"modality": "TEXT", "tokenCount": 14}],
            "candidatesTokensDetails": [{"modality": "IMAGE", "tokenCount": 747}]
        },
        "modelVersion": MODEL,
        "responseId": "resp-abc123"
    })
}

async fn mount_ok(server: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

fn body_of(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap()
}

/// Every request carried the fake key in `x-goog-api-key` and no `key=` query.
fn assert_header_auth(reqs: &[Request]) {
    assert!(!reqs.is_empty());
    for r in reqs {
        assert_eq!(r.headers.get("x-goog-api-key").unwrap().to_str().unwrap(), KEY);
        assert!(r.url.query().is_none_or(|q| !q.contains("key=")), "{}", r.url);
        assert!(!r.url.as_str().contains(KEY));
        assert!(r.headers.get("authorization").is_none());
    }
}

async fn generate(server: &MockServer, req: &ImageRequest) -> Result<ImageOutput, IrisError> {
    GeminiProvider::new().generate(req, &ctx(server)).await
}

#[tokio::test]
async fn text_to_image_sends_the_exact_minimal_body_with_header_auth() {
    let server = MockServer::start().await;
    let image = png();
    mount_ok(&server, response_with(vec![image_part("image/png", &image)], "STOP")).await;

    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();

    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    assert_header_auth(&reqs);
    assert_eq!(reqs[0].url.path(), GENERATE_PATH);
    assert_eq!(reqs[0].headers.get("content-type").unwrap(), "application/json");
    assert_eq!(
        body_of(&reqs[0]),
        json!({
            "contents": [{"role": "user", "parts": [{"text": "A red kite over green hills, watercolor"}]}],
            "generationConfig": {"responseModalities": ["IMAGE"]},
            "store": false
        })
    );

    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].media_type, "image/png");
    assert_eq!(out.images[0].bytes, image);
    assert_eq!(out.text, None);
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(out.provider_request_id.as_deref(), Some("resp-abc123"));
    let usage = out.usage.unwrap();
    assert_eq!(usage.input_tokens, Some(14));
    assert_eq!(usage.output_tokens, Some(747 + 180));
    assert_eq!(usage.total_tokens, Some(941));
    assert_eq!(usage.provider_usage.unwrap()["candidatesTokensDetails"][0]["modality"], "IMAGE");
}

#[tokio::test]
async fn edit_sends_the_prompt_first_then_every_reference_in_order_with_all_options() {
    let server = MockServer::start().await;
    mount_ok(&server, response_with(vec![image_part("image/jpeg", &jpeg())], "STOP")).await;
    let (first, second) = (png(), jpeg());
    let req = ImageRequest {
        operation: Operation::ImageEdit,
        model: MODEL.to_string(),
        prompt: "Put the cat from the first image into the kitchen from the second".to_string(),
        images: vec![
            input(first.clone(), "image/png", "cat.png"),
            input(second.clone(), "image/jpeg", "k.jpg"),
        ],
        mask: None,
        options: options(&[
            ("count", OptionValue::Int(1)),
            ("aspect_ratio", s("4:3")),
            ("resolution", s("512")),
            ("thinking_level", s("high")),
        ]),
    };

    let out = GeminiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    assert_eq!(out.images[0].media_type, "image/jpeg");

    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    assert_header_auth(&reqs);
    assert_eq!(
        body_of(&reqs[0]),
        json!({
            "contents": [{"role": "user", "parts": [
                {"text": "Put the cat from the first image into the kitchen from the second"},
                {"inlineData": {"mimeType": "image/png", "data": b64(&first)}},
                {"inlineData": {"mimeType": "image/jpeg", "data": b64(&second)}}
            ]}],
            "generationConfig": {
                "responseModalities": ["IMAGE"],
                "imageConfig": {"aspectRatio": "4:3", "imageSize": "512"},
                "thinkingConfig": {"thinkingLevel": "HIGH"}
            },
            "store": false
        })
    );
}

#[tokio::test]
async fn each_option_maps_to_exactly_its_wire_field() {
    let cases: Vec<(&str, OptionValue, Value)> = vec![
        ("count", OptionValue::Int(1), json!({"responseModalities": ["IMAGE"]})),
        (
            "aspect_ratio",
            s("21:9"),
            json!({"responseModalities": ["IMAGE"], "imageConfig": {"aspectRatio": "21:9"}}),
        ),
        ("resolution", s("4K"), json!({"responseModalities": ["IMAGE"], "imageConfig": {"imageSize": "4K"}})),
        (
            "thinking_level",
            s("minimal"),
            json!({"responseModalities": ["IMAGE"], "thinkingConfig": {"thinkingLevel": "MINIMAL"}}),
        ),
        (
            "thinking_level",
            s("high"),
            json!({"responseModalities": ["IMAGE"], "thinkingConfig": {"thinkingLevel": "HIGH"}}),
        ),
    ];
    for (name, value, expected) in cases {
        let server = MockServer::start().await;
        mount_ok(&server, response_with(vec![image_part("image/png", &png())], "STOP")).await;
        generate(&server, &generate_request(options(&[(name, value.clone())]))).await.unwrap();
        let body = body_of(&requests(&server).await[0]);
        assert_eq!(body["generationConfig"], expected, "{name}={value}");
        assert_eq!(body["store"], json!(false));
        assert!(
            body.get("candidateCount").is_none() && body["generationConfig"].get("candidateCount").is_none()
        );
    }
}

#[tokio::test]
async fn options_the_adapter_does_not_map_are_internal_errors_and_nothing_is_sent() {
    let server = MockServer::start().await;
    mount_ok(&server, response_with(vec![image_part("image/png", &png())], "STOP")).await;
    for (name, value) in
        [("quality", s("high")), ("seed", OptionValue::Int(3)), ("count", OptionValue::Int(2))]
    {
        let err = generate(&server, &generate_request(options(&[(name, value)]))).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError, "{name}");
    }
    let mut masked = generate_request(ResolvedOptions::new());
    masked.operation = Operation::ImageEdit;
    masked.images = vec![input(png(), "image/png", "a.png")];
    masked.mask = Some(input(png(), "image/png", "mask.png"));
    let err = GeminiProvider::new().edit(&masked, &ctx(&server)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InternalError);
    let mut wrong_op = generate_request(ResolvedOptions::new());
    wrong_op.images = vec![input(png(), "image/png", "a.png")];
    assert_eq!(generate(&server, &wrong_op).await.unwrap_err().code, ErrorCode::InternalError);
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn model_ids_that_could_change_the_url_are_rejected_before_sending() {
    let server = MockServer::start().await;
    for model in ["../v1beta/files/x", "models/gemini-x", "gemini:x", "a?b", ""] {
        let mut req = generate_request(ResolvedOptions::new());
        req.model = model.to_string();
        assert_eq!(generate(&server, &req).await.unwrap_err().code, ErrorCode::InvalidArgument, "{model}");
    }
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn thought_parts_are_never_saved() {
    let server = MockServer::start().await;
    let (thought, final_image) = (jpeg(), png());
    mount_ok(
        &server,
        response_with(
            vec![
                json!({"text": "Planning the composition", "thought": true}),
                json!({"inlineData": {"mimeType": "image/jpeg", "data": b64(&thought)}, "thought": true}),
                json!({"inlineData": {"mimeType": "image/jpeg", "data": b64(&thought)}, "thought": true}),
                json!({"inlineData": {"mimeType": "image/jpeg", "data": b64(&thought)}, "thought": true}),
                json!({"inlineData": {"mimeType": "image/png", "data": b64(&final_image)}, "thoughtSignature": "c2ln"}),
            ],
            "STOP",
        ),
    )
    .await;
    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].bytes, final_image);
    assert_eq!(out.text, None, "thought text is not model output");
    assert!(out.warnings.is_empty());
}

#[tokio::test]
async fn text_parts_are_returned_with_a_warning() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        response_with(vec![json!({"text": "Here is your kite."}), image_part("image/png", &png())], "STOP"),
    )
    .await;
    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.text.as_deref(), Some("Here is your kite."));
    assert_eq!(out.warnings.len(), 1);
    assert_eq!(out.warnings[0].code, "provider_text_output");
}

#[tokio::test]
async fn extra_images_are_all_kept_with_a_warning() {
    let server = MockServer::start().await;
    mount_ok(
        &server,
        response_with(vec![image_part("image/png", &png()), image_part("image/jpeg", &jpeg())], "STOP"),
    )
    .await;
    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();
    assert_eq!(
        out.images.iter().map(|i| i.media_type.as_str()).collect::<Vec<_>>(),
        ["image/png", "image/jpeg"]
    );
    assert_eq!(out.warnings.len(), 1);
    assert_eq!(out.warnings[0].code, "unexpected_output_count");
}

async fn no_image_error(body: Value) -> IrisError {
    let server = MockServer::start().await;
    mount_ok(&server, body).await;
    let err = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap_err();
    assert_eq!(requests(&server).await.len(), 1, "a 200 without an image is never retried");
    assert_eq!(err.provider, Some(ProviderId::Gemini));
    err
}

#[tokio::test]
async fn text_only_answers_map_by_finish_reason() {
    let err = no_image_error(response_with(vec![json!({"text": "I can't draw that."})], "NO_IMAGE")).await;
    assert_eq!(err.code, ErrorCode::RemoteJobFailed);
    assert_eq!(err.provider_code.as_deref(), Some("NO_IMAGE"));
    assert_eq!(err.details["model_text"], "I can't draw that.");
    assert_eq!(err.provider_request_id.as_deref(), Some("resp-abc123"));

    // STOP with text only is still "no image".
    let err = no_image_error(response_with(vec![json!({"text": "Which kite?"})], "STOP")).await;
    assert_eq!(err.code, ErrorCode::RemoteJobFailed);

    for reason in ["IMAGE_SAFETY", "SAFETY", "PROHIBITED_CONTENT", "IMAGE_RECITATION", "SPII", "LANGUAGE"] {
        let mut body = response_with(vec![], reason);
        body["candidates"][0]["finishMessage"] = json!("Unable to show the generated image.");
        let err = no_image_error(body).await;
        assert_eq!(err.code, ErrorCode::ContentBlocked, "{reason}");
        assert_eq!(err.provider_code.as_deref(), Some(reason));
        assert_eq!(err.details["provider_message"], "Unable to show the generated image.");
        assert_eq!(err.exit_code(), 1);
    }

    let err = no_image_error(response_with(vec![], "PUP_LIMITED_DISABLED")).await;
    assert_eq!(err.code, ErrorCode::PermissionDenied);

    for reason in ["IMAGE_OTHER", "OTHER", "MAX_TOKENS", "MALFORMED_RESPONSE"] {
        assert_eq!(no_image_error(response_with(vec![], reason)).await.code, ErrorCode::RemoteJobFailed);
    }
}

#[tokio::test]
async fn a_blocked_prompt_is_content_blocked() {
    let err = no_image_error(json!({
        "promptFeedback": {"blockReason": "SAFETY", "safetyRatings": []},
        "usageMetadata": {"promptTokenCount": 9, "totalTokenCount": 9}
    }))
    .await;
    assert_eq!(err.code, ErrorCode::ContentBlocked);
    assert_eq!(err.provider_code.as_deref(), Some("SAFETY"));
    assert_eq!(err.details["block_reason"], "SAFETY");
}

/// Warning codes of an output, in order.
fn codes(out: &ImageOutput) -> Vec<&str> {
    out.warnings.iter().map(|w| w.code.as_str()).collect()
}

#[tokio::test]
async fn images_are_typed_by_their_bytes_and_kept_whatever_their_label() {
    // (case, part, kept media type, warning expected)
    let jpeg_bytes = jpeg();
    let cases: Vec<(&str, Value, &str, bool)> = vec![
        ("labeled png, content jpeg", image_part("image/png", &jpeg_bytes), "image/jpeg", true),
        ("no mimeType", json!({"inlineData": {"data": b64(&jpeg_bytes)}}), "image/jpeg", true),
        (
            "empty mimeType",
            json!({"inlineData": {"mimeType": "", "data": b64(&jpeg_bytes)}}),
            "image/jpeg",
            true,
        ),
        ("a non-image label", image_part("application/octet-stream", &jpeg_bytes), "image/jpeg", true),
        ("image/jpg alias", image_part("image/jpg", &jpeg_bytes), "image/jpeg", false),
        ("correct label", image_part("image/jpeg", &jpeg_bytes), "image/jpeg", false),
    ];
    for (name, part, kept, warns) in cases {
        let server = MockServer::start().await;
        mount_ok(&server, response_with(vec![part], "STOP")).await;
        let out = generate(&server, &generate_request(ResolvedOptions::new()))
            .await
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        assert_eq!(out.images.len(), 1, "{name}");
        assert_eq!(out.images[0].media_type, kept, "{name}");
        assert_eq!(out.images[0].bytes, jpeg_bytes, "{name}: kept verbatim");
        if warns {
            assert_eq!(codes(&out), ["output_format_mismatch"], "{name}");
            let message = &out.warnings[0].message;
            assert!(message.contains("image 0") && message.contains(kept), "{name}: {message}");
        } else {
            assert!(out.warnings.is_empty(), "{name}: {:?}", out.warnings);
        }
        assert_eq!(requests(&server).await.len(), 1, "{name}: never repeated");
    }
}

#[tokio::test]
async fn one_bad_item_never_drops_the_good_ones() {
    let (good, odd) = (png(), jpeg());
    // A correct image and a mislabeled one: both kept, only the second is reported.
    let server = MockServer::start().await;
    mount_ok(
        &server,
        response_with(vec![image_part("image/png", &good), image_part("image/png", &odd)], "STOP"),
    )
    .await;
    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();
    let types: Vec<&str> = out.images.iter().map(|i| i.media_type.as_str()).collect();
    assert_eq!(types, ["image/png", "image/jpeg"]);
    assert_eq!(codes(&out), ["output_format_mismatch", "unexpected_output_count"]);
    assert!(out.warnings[0].message.contains("image 1 "), "{}", out.warnings[0].message);

    // A correct image next to items that are not images: the image is kept, each
    // unusable item is named in its own warning.
    let bad_items = vec![
        json!({"inlineData": {"mimeType": "image/png", "data": "not base64 at all!!"}}),
        image_part("image/png", b"{\"error\": \"not an image\"}"),
        json!({"inlineData": {"mimeType": "image/png"}}),
    ];
    for (i, bad) in bad_items.into_iter().enumerate() {
        let server = MockServer::start().await;
        mount_ok(&server, response_with(vec![bad, image_part("image/png", &good)], "STOP")).await;
        let out = generate(&server, &generate_request(ResolvedOptions::new()))
            .await
            .unwrap_or_else(|e| panic!("case {i}: {}", e.message));
        assert_eq!(out.images.len(), 1, "case {i}");
        assert_eq!(out.images[0].bytes, good, "case {i}");
        assert_eq!(codes(&out), ["output_item_unusable", "unexpected_output_count"], "case {i}");
        assert!(out.warnings[0].message.contains("image 0 "), "case {i}: {}", out.warnings[0].message);
        assert!(out.usage.is_some(), "case {i}: usage is still reported");
    }
}

#[tokio::test]
async fn an_answer_without_any_usable_image_is_a_bad_response_that_may_be_charged() {
    for data in ["not base64 at all!!", &b64(b"{\"error\": \"not an image\"}")] {
        let server = MockServer::start().await;
        mount_ok(
            &server,
            response_with(vec![json!({"inlineData": {"mimeType": "image/png", "data": data}})], "STOP"),
        )
        .await;
        let err = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::ProviderBadResponse, "{data}");
        assert_eq!(err.details["charge_possible"], true, "{data}");
        assert_eq!(err.details["declared_media_type"], "image/png", "{data}");
        assert_ne!(err.retryable, Some(true), "{data}");
        assert_eq!(requests(&server).await.len(), 1, "{data}");
    }
    // Inline data of another kind is judged by its bytes too: a video is not an image.
    let mp4_head = b"\x00\x00\x00\x18ftypisom\x00\x00\x02\x00isommp41";
    let server = MockServer::start().await;
    mount_ok(&server, response_with(vec![image_part("video/mp4", mp4_head)], "STOP")).await;
    let err = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ProviderBadResponse);
    assert_eq!(err.details["sniffed_media_type"], "video/mp4");
}

#[tokio::test]
async fn an_unparseable_success_body_is_a_bad_response_that_may_be_charged() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string("<html>proxy page</html>"))
        .mount(&server)
        .await;
    let err = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ProviderBadResponse);
    assert_eq!(err.details["charge_possible"], true);
    assert_eq!(requests(&server).await.len(), 1);
}

fn google_error(code: u16, status: &str, message: &str, details: Value) -> Value {
    json!({"error": {"code": code, "message": message, "status": status, "details": details}})
}

fn error_info(reason: &str) -> Value {
    json!({"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": reason, "domain": "googleapis.com",
           "metadata": {"service": "generativelanguage.googleapis.com"}})
}

fn retry_info(delay: &str) -> Value {
    json!({"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": delay})
}

/// Mount `status` + `body` for every request and run one generate call.
async fn error_case(status: u16, body: Value) -> (IrisError, usize) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    let err = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap_err();
    (err, requests(&server).await.len())
}

#[tokio::test]
async fn every_error_table_row_maps_to_its_public_code() {
    let rows: Vec<(u16, Value, ErrorCode, Option<&str>)> = vec![
        (
            400,
            google_error(
                400,
                "INVALID_ARGUMENT",
                "API key not valid. Please pass a valid API key.",
                json!([error_info("API_KEY_INVALID")]),
            ),
            ErrorCode::AuthenticationFailed,
            Some("INVALID_ARGUMENT:API_KEY_INVALID"),
        ),
        (
            400,
            google_error(400, "FAILED_PRECONDITION", "Please enable billing.", json!([])),
            ErrorCode::PermissionDenied,
            Some("FAILED_PRECONDITION"),
        ),
        (
            400,
            google_error(400, "INVALID_ARGUMENT", "Unsupported aspect ratio.", json!([])),
            ErrorCode::InvalidArgument,
            Some("INVALID_ARGUMENT"),
        ),
        (
            401,
            google_error(401, "UNAUTHENTICATED", "no key", json!([])),
            ErrorCode::AuthenticationFailed,
            None,
        ),
        (
            402,
            google_error(402, "RESOURCE_EXHAUSTED", "Your prepay credit balance is depleted.", json!([])),
            ErrorCode::QuotaExceeded,
            Some("RESOURCE_EXHAUSTED"),
        ),
        (
            403,
            google_error(
                403,
                "PERMISSION_DENIED",
                "Your API key doesn't have the required permissions.",
                json!([]),
            ),
            ErrorCode::PermissionDenied,
            Some("PERMISSION_DENIED"),
        ),
        (
            404,
            google_error(404, "NOT_FOUND", "models/gemini-x is not found", json!([])),
            ErrorCode::PermissionDenied,
            Some("NOT_FOUND"),
        ),
        (500, google_error(500, "INTERNAL", "internal error", json!([])), ErrorCode::ProviderError, None),
        (502, json!({}), ErrorCode::ProviderError, None),
        (
            503,
            google_error(503, "UNAVAILABLE", "The model is overloaded.", json!([])),
            ErrorCode::ProviderError,
            None,
        ),
        (
            504,
            google_error(504, "DEADLINE_EXCEEDED", "Deadline exceeded.", json!([])),
            ErrorCode::RequestTimeout,
            None,
        ),
    ];
    for (status, body, code, provider_code) in rows {
        let (err, sent) = error_case(status, body).await;
        assert_eq!(err.code, code, "HTTP {status}");
        assert_eq!(sent, 1, "HTTP {status} must not be retried for a paid image call");
        assert_eq!(err.provider_status, Some(status));
        assert_eq!(err.provider, Some(ProviderId::Gemini));
        if let Some(pc) = provider_code {
            assert_eq!(err.provider_code.as_deref(), Some(pc), "HTTP {status}");
        }
        if status >= 500 {
            assert_eq!(err.retryable, Some(true), "HTTP {status}");
        } else {
            assert_eq!(err.retryable, Some(false), "HTTP {status}");
        }
        // Google does not charge requests that fail with an HTTP error.
        assert!(err.details.get("charge_possible").is_none(), "HTTP {status}");
    }

    let (err, _) = error_case(404, google_error(404, "NOT_FOUND", "not found", json!([]))).await;
    assert!(err.hint.as_deref().unwrap().contains("model not found or not available"), "{:?}", err.hint);
    let (err, _) = error_case(
        400,
        google_error(400, "INVALID_ARGUMENT", "API key not valid.", json!([error_info("API_KEY_INVALID")])),
    )
    .await;
    assert!(err.hint.as_deref().unwrap().contains("GEMINI_API_KEY"));
    assert_eq!(err.exit_code(), 3);
    assert_eq!(err.details["provider_message"], "API key not valid.");
}

#[tokio::test]
async fn provider_messages_are_truncated_and_urls_redacted() {
    let long = format!("see https://example.com/x?token=secretvalue {}", "x".repeat(900));
    let (err, _) = error_case(400, google_error(400, "INVALID_ARGUMENT", &long, json!([]))).await;
    let msg = err.details["provider_message"].as_str().unwrap();
    assert!(msg.chars().count() <= 501, "{}", msg.len());
    assert!(!msg.contains("secretvalue"), "{msg}");
    assert!(msg.contains("token=REDACTED"), "{msg}");
}

#[tokio::test]
async fn a_rate_limit_is_retried_and_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(429).set_body_json(google_error(
            429,
            "RESOURCE_EXHAUSTED",
            "Resource has been exhausted (e.g. check quota).",
            json!([retry_info("0.01s")]),
        )))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(response_with(vec![image_part("image/png", &png())], "STOP")),
        )
        .with_priority(2)
        .mount(&server)
        .await;
    let out = generate(&server, &generate_request(ResolvedOptions::new())).await.unwrap();
    assert_eq!(out.images.len(), 1);
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 2);
    assert_header_auth(&reqs);
    assert_eq!(body_of(&reqs[0]), body_of(&reqs[1]), "the retry resends the same request");
}

#[tokio::test]
async fn persistent_rate_limits_stop_after_three_attempts() {
    let (err, sent) = error_case(429, google_error(429, "RESOURCE_EXHAUSTED", "slow down", json!([]))).await;
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(sent, 3);
    assert_eq!(err.retryable, Some(true));
    let hint = err.hint.as_deref().unwrap();
    assert!(hint.contains("no free tier") && hint.contains("billing"), "{hint}");
}

/// `google.rpc.QuotaFailure` with one violation, in the proto3 JSON form Google
/// sends (`quotaValue` is an int64, so a string).
fn quota_failure(quota_id: &str, quota_value: &str) -> Value {
    json!({"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{
        "quotaMetric": "generativelanguage.googleapis.com/generate_content_free_tier_requests",
        "quotaId": quota_id,
        "quotaDimensions": {"location": "global", "model": MODEL},
        "quotaValue": quota_value
    }]})
}

#[tokio::test]
async fn a_zero_quota_is_quota_exceeded_sent_once_with_billing_guidance() {
    // What a project without billing is expected to get: the image models have no
    // free tier, so the free-tier limit is 0. The RetryInfo must not be honored.
    let (err, sent) = error_case(
        429,
        google_error(
            429,
            "RESOURCE_EXHAUSTED",
            "You exceeded your current quota, please check your plan and billing details.",
            json!([
                quota_failure("GenerateRequestsPerDayPerProjectPerModel-FreeTier", "0"),
                retry_info("0.01s")
            ]),
        ),
    )
    .await;
    assert_eq!(err.code, ErrorCode::QuotaExceeded, "{}", err.message);
    assert_eq!(sent, 1, "an exhausted quota is never retried");
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.exit_code(), 3);
    assert_eq!(err.retry_after, None);
    assert_eq!(err.provider_status, Some(429));
    assert_eq!(err.provider_code.as_deref(), Some("RESOURCE_EXHAUSTED"));
    assert_eq!(err.details["quota_id"], "GenerateRequestsPerDayPerProjectPerModel-FreeTier");
    assert_eq!(err.details["quota_limit"], 0);
    let hint = err.hint.as_deref().unwrap();
    assert!(hint.contains("no free tier") && hint.contains("billing account"), "{hint}");
}

#[tokio::test]
async fn a_used_up_daily_quota_is_quota_exceeded_and_sent_once() {
    let (err, sent) = error_case(
        429,
        google_error(
            429,
            "RESOURCE_EXHAUSTED",
            "Quota exceeded.",
            json!([quota_failure("GenerateRequestsPerDayPerProjectPerModel", "250"), retry_info("0.01s")]),
        ),
    )
    .await;
    assert_eq!(err.code, ErrorCode::QuotaExceeded);
    assert_eq!(sent, 1);
    assert_eq!(err.details["quota_limit"], 250);
    assert!(err.hint.as_deref().unwrap().contains("midnight Pacific"));
}

#[tokio::test]
async fn a_per_minute_quota_stays_a_retried_rate_limit() {
    let (err, sent) = error_case(
        429,
        google_error(
            429,
            "RESOURCE_EXHAUSTED",
            "Quota exceeded.",
            json!([quota_failure("GenerateRequestsPerMinutePerProjectPerModel", "10"), retry_info("0.01s")]),
        ),
    )
    .await;
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(sent, 3);
    assert_eq!(err.retryable, Some(true));
    assert!(err.details.get("quota_id").is_none());
}

#[tokio::test]
async fn a_retry_delay_beyond_the_cap_is_reported_instead_of_waited() {
    let (err, sent) =
        error_case(429, google_error(429, "RESOURCE_EXHAUSTED", "daily limit", json!([retry_info("120s")])))
            .await;
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(sent, 1);
    assert_eq!(err.retry_after, Some(Duration::from_secs(120)));
}

#[tokio::test]
async fn a_timeout_after_sending_is_submission_uncertain_and_never_resent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GENERATE_PATH))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1500)))
        .mount(&server)
        .await;
    let err = GeminiProvider::new()
        .generate(&generate_request(ResolvedOptions::new()), &ctx_with(&server, Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{}", err.message);
    assert_eq!(err.exit_code(), 5);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.details["charge_possible"], true);
    assert_eq!(err.details["transport"], "timeout");
    assert!(err.job_id.is_none());
    assert!(err.hint.as_deref().unwrap().contains("did not retry"));
    assert_eq!(requests(&server).await.len(), 1);
}

/// A raw 127.0.0.1 server that reads one whole request per connection and closes
/// the connection without answering. Returns its origin and the connection count.
fn closing_server() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::Read as _;
    use std::sync::atomic::Ordering;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = connections.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            count.fetch_add(1, Ordering::SeqCst);
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 16 * 1024];
            // Read the head, then the Content-Length body, then close.
            let head_end = loop {
                match stream.read(&mut chunk) {
                    Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                    _ => break None,
                }
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(pos);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
            let body_len: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + 4 + body_len {
                match stream.read(&mut chunk) {
                    Ok(n) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                    _ => break,
                }
            }
        }
    });
    (format!("http://{addr}"), connections)
}

#[tokio::test]
async fn a_connection_dropped_after_sending_is_submission_uncertain_not_a_timeout() {
    let (origin, connections) = closing_server();
    let mut ctx = ctx_with(&MockServer::start().await, Duration::from_secs(5));
    ctx.base_url = url::Url::parse(&origin).unwrap();
    let err =
        GeminiProvider::new().generate(&generate_request(ResolvedOptions::new()), &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{}", err.message);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.details["charge_possible"], true);
    assert_eq!(err.details["transport"], "other", "nothing timed out");
    assert!(err.message.contains("after the request was sent"), "{}", err.message);
    assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1, "never resent");
}

#[tokio::test]
async fn http_timeout_answers_keep_their_code_are_sent_once_and_are_not_marked_as_charged() {
    for (status, rpc_status) in [(408u16, "DEADLINE_EXCEEDED"), (504, "DEADLINE_EXCEEDED")] {
        let (err, sent) =
            error_case(status, google_error(status, rpc_status, "Deadline exceeded.", json!([]))).await;
        assert_eq!(err.code, ErrorCode::RequestTimeout, "HTTP {status}");
        assert_eq!(sent, 1, "HTTP {status} must not be resent for a paid image call");
        assert_eq!(err.provider_status, Some(status));
        assert_eq!(err.retryable, Some(true));
        assert!(err.details.get("charge_possible").is_none(), "HTTP {status}: Google does not charge it");
        assert!(err.hint.as_deref().unwrap().contains("did not retry"), "HTTP {status}: {:?}", err.hint);
    }
    let (err, _) = error_case(500, google_error(500, "INTERNAL", "internal error", json!([]))).await;
    assert_eq!(err.code, ErrorCode::ProviderError);
    assert_eq!(err.retryable, Some(true));
    assert!(err.details.get("charge_possible").is_none());
    assert!(err.hint.as_deref().unwrap().contains("not charged"), "{:?}", err.hint);
}

#[tokio::test]
async fn requests_above_twenty_megabytes_fail_locally_with_zero_requests() {
    let server = MockServer::start().await;
    mount_ok(&server, response_with(vec![image_part("image/png", &png())], "STOP")).await;
    let mut big = png();
    big.resize(15_000_001, 0); // base64 alone is over 20,000,000 bytes
    let req = ImageRequest {
        operation: Operation::ImageEdit,
        model: MODEL.to_string(),
        prompt: "enlarge".to_string(),
        images: vec![input(big, "image/png", "big.png")],
        mask: None,
        options: ResolvedOptions::new(),
    };
    let err = GeminiProvider::new().edit(&req, &ctx(&server)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.message.contains("20000000"), "{}", err.message);
    assert_eq!(err.details["limit_bytes"], 20_000_000);
    assert!(requests(&server).await.is_empty());

    // Images that fit on their own but push the whole JSON body over the cap are
    // caught after encoding (exact size).
    let mut edge = png();
    edge.resize(14_999_997, 0); // base64: 19,999,996 bytes, plus prompt and JSON framing
    let req_edge = ImageRequest { images: vec![input(edge, "image/png", "edge.png")], ..req.clone() };
    let err = GeminiProvider::new().edit(&req_edge, &ctx(&server)).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(err.details["request_bytes"].as_u64().unwrap() > 20_000_000);
    assert!(!err.message.contains("at least"), "{}", err.message);
    assert!(requests(&server).await.is_empty());

    // Just under the cap is sent.
    let mut ok = png();
    ok.resize(14_000_000, 0);
    let req = ImageRequest { images: vec![input(ok, "image/png", "ok.png")], ..req };
    GeminiProvider::new().edit(&req, &ctx(&server)).await.unwrap();
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn check_access_uses_free_model_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gemini-3.1-flash-image"))
        .and(header("x-goog-api-key", KEY))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "models/gemini-3.1-flash-image"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1beta/models/veo-3.1-lite-generate-preview"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name": "models/veo-3.1-lite-generate-preview"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gemini-3-pro-image"))
        .respond_with(ResponseTemplate::new(404).set_body_json(google_error(
            404,
            "NOT_FOUND",
            "not found",
            json!([]),
        )))
        .mount(&server)
        .await;
    let p = GeminiProvider::new();
    let c = ctx(&server);
    assert_eq!(p.check_access(MODEL, &c).await.unwrap(), AccountAccess::Available);
    assert_eq!(p.check_access("veo-3.1-lite-generate-preview", &c).await.unwrap(), AccountAccess::Available);
    assert_eq!(p.check_access("gemini-3-pro-image", &c).await.unwrap(), AccountAccess::Unavailable);
    assert_eq!(p.check_access("../x", &c).await.unwrap_err().code, ErrorCode::InvalidArgument);
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 3);
    assert_header_auth(&reqs);
}

#[tokio::test]
async fn check_access_reports_rejected_keys_and_unknown_outcomes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gemini-3.1-flash-image"))
        .respond_with(ResponseTemplate::new(400).set_body_json(google_error(
            400,
            "INVALID_ARGUMENT",
            "API key not valid.",
            json!([error_info("API_KEY_INVALID")]),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gemini-3-pro-image"))
        .respond_with(ResponseTemplate::new(403).set_body_json(google_error(
            403,
            "PERMISSION_DENIED",
            "no",
            json!([]),
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models/gemini-3.1-flash-lite-image"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let p = GeminiProvider::new();
    let c = ctx(&server);
    assert_eq!(p.check_access(MODEL, &c).await.unwrap_err().code, ErrorCode::AuthenticationFailed);
    assert_eq!(p.check_access("gemini-3-pro-image", &c).await.unwrap_err().code, ErrorCode::PermissionDenied);
    assert_eq!(p.check_access("gemini-3.1-flash-lite-image", &c).await.unwrap(), AccountAccess::Unknown);
    let lite_calls = requests(&server)
        .await
        .iter()
        .filter(|r| r.url.path() == "/v1/models/gemini-3.1-flash-lite-image")
        .count();
    assert_eq!(lite_calls, 5, "metadata reads are retried (IdempotentRead)");
}

#[test]
fn the_provider_declares_origin_base_url_header_auth_and_retention() {
    let p = GeminiProvider::new();
    assert_eq!(p.id(), ProviderId::Gemini);
    assert_eq!(p.default_base_url(), "https://generativelanguage.googleapis.com");
    assert_eq!(p.default_base_url(), iris::config::DEFAULT_GEMINI_BASE_URL);
    assert_eq!(p.credential_header().name, "x-goog-api-key");
    assert_eq!(p.credential_header().prefix, "");
    assert!(p.image().is_some() && p.video().is_some());
    assert_eq!(p.output_retention(), Some(Duration::from_secs(48 * 3600)));
    let registry = Registry::builtin();
    let gemini = registry.get(ProviderId::Gemini).unwrap();
    assert!(gemini.image().is_some() && gemini.video().is_some());
}
