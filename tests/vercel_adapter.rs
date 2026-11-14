//! The Vercel AI Gateway adapter against a local wiremock server: the exact request
//! body and headers, response decoding, the error table, the paid-submit retry rules,
//! and the free access check. Offline: every request goes to 127.0.0.1, the key is a
//! fake set through `ProviderContext`, and nothing reads the process environment.

use std::io::Cursor;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use iris::catalog::{OptionValue, ResolvedOptions};
use iris::domain::{ProviderId, WarningCode};
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy, Timeouts};
use iris::providers::vercel::VercelProvider;
use iris::providers::{
    AccountAccess, ImageFailure, ImageOutput, ImageProvider, ImageRequest, Provider, ProviderContext,
    Registry,
};
use iris::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-vercel-key-000";
/// The origin of a port nothing listens on (see tests/openai_adapter.rs).
const DEAD_URL: &str = "http://127.0.0.1:9";
const GEN: &str = "/v1/images/generations";
const MODEL: &str = "bfl/flux-3-image";

// ---------------------------------------------------------------- fixtures

fn ctx_for(base_url: &str) -> ProviderContext {
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
        base_url: url::Url::parse(base_url).unwrap(),
        credential: Secret::new(KEY),
        timeouts: Timeouts {
            generate: Duration::from_secs(2),
            poll: Duration::from_secs(2),
            ..Timeouts::default()
        },
    }
}

fn ctx(server: &MockServer) -> ProviderContext {
    ctx_for(&format!("{}/v1", server.uri()))
}

fn jpeg() -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([200, 10, 10])))
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .unwrap();
    out.into_inner()
}

fn request(options: ResolvedOptions) -> ImageRequest {
    ImageRequest {
        model: MODEL.to_string(),
        prompt: "a watercolor fox".to_string(),
        images: Vec::new(),
        mask: None,
        options,
    }
}

fn sized(size: &str) -> ResolvedOptions {
    let mut o = ResolvedOptions::new();
    o.insert("count", OptionValue::Int(1));
    o.insert("size", OptionValue::Str(size.to_string()));
    o
}

fn images_body(items: Vec<Value>) -> Value {
    json!({"created": 1_790_000_000, "data": items})
}

fn b64_item(bytes: &[u8]) -> Value {
    json!({"b64_json": STANDARD.encode(bytes)})
}

fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body).insert_header("x-vercel-id", "iad1::abc-123")
}

fn gateway_error(status: u16, message: &str, kind: &str) -> ResponseTemplate {
    ResponseTemplate::new(status)
        .set_body_json(json!({"error": {"message": message, "type": kind}}))
        .insert_header("x-vercel-id", "iad1::err-456")
}

async fn mount(server: &MockServer, template: ResponseTemplate) {
    Mock::given(method("POST")).and(path(GEN)).respond_with(template).mount(server).await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

async fn generate(server: &MockServer, options: ResolvedOptions) -> Result<ImageOutput, ImageFailure> {
    VercelProvider::new().generate(&request(options), &ctx(server)).await
}

async fn generate_err(server: &MockServer) -> IrisError {
    generate(server, sized("1024x1024")).await.expect_err("expected an error").error
}

// ---------------------------------------------------------------- provider metadata

#[test]
fn provider_metadata_and_registry_entry() {
    let p = VercelProvider::new();
    assert_eq!(p.id(), ProviderId::Vercel);
    assert_eq!(p.id().default_base_url(), "https://ai-gateway.vercel.sh/v1");
    assert_eq!(p.id().credential_env(), "AI_GATEWAY_API_KEY");
    assert_eq!(p.docs_url(), "https://vercel.com/docs/ai-gateway/modalities/image-generation/openai");
    let h = p.credential_header();
    assert_eq!((h.name, h.prefix), ("authorization", "Bearer "));
    assert!(p.image().is_some());
    assert!(p.video().is_none(), "the gateway documents no HTTP API for video jobs");
    assert!(Registry::builtin().get(ProviderId::Vercel).is_some());
}

// ---------------------------------------------------------------- request encoding

#[tokio::test]
async fn generate_sends_exactly_model_prompt_size_and_b64_json_with_the_bearer_key() {
    let server = MockServer::start().await;
    mount(&server, ok(images_body(vec![b64_item(&jpeg())]))).await;
    let out = generate(&server, sized("2048x2048")).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].media_type, "image/jpeg");
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(out.provider_request_id.as_deref(), Some("iad1::abc-123"));
    assert!(out.usage.is_none());

    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(
        body,
        json!({"model": MODEL, "prompt": "a watercolor fox", "response_format": "b64_json", "size": "2048x2048"})
    );
    let header = |name: &str| reqs[0].headers.get(name).and_then(|v| v.to_str().ok());
    assert_eq!(header("authorization"), Some(format!("Bearer {KEY}").as_str()));
    assert_eq!(header("content-type"), Some("application/json"));
}

#[tokio::test]
async fn without_a_size_only_model_prompt_and_response_format_are_sent() {
    let server = MockServer::start().await;
    mount(&server, ok(images_body(vec![b64_item(&jpeg())]))).await;
    generate(&server, ResolvedOptions::new()).await.unwrap();
    let body: Value = serde_json::from_slice(&requests(&server).await[0].body).unwrap();
    assert_eq!(body, json!({"model": MODEL, "prompt": "a watercolor fox", "response_format": "b64_json"}));
}

#[tokio::test]
async fn unmapped_options_and_edits_are_internal_errors_and_nothing_is_sent() {
    let server = MockServer::start().await;
    let mut o = ResolvedOptions::new();
    o.insert("quality", OptionValue::Str("high".into()));
    let e = generate(&server, o).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InternalError);
    let e = VercelProvider::new().edit(&request(ResolvedOptions::new()), &ctx(&server)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InternalError);
    assert!(requests(&server).await.is_empty());
}

// ---------------------------------------------------------------- response decoding

#[tokio::test]
async fn an_unusable_item_is_kept_and_reported_without_dropping_the_usable_image() {
    let server = MockServer::start().await;
    let items = vec![json!({"b64_json": "%%%not base64"}), b64_item(&jpeg())];
    mount(&server, ok(images_body(items))).await;
    let out = generate(&server, sized("1024x1024")).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(out.images[0].item, 1);
    assert_eq!(out.unusable.len(), 1);
    assert_eq!(out.unusable[0].bytes, b"%%%not base64");
    let codes: Vec<&str> = out.warnings.iter().map(|w| w.code.as_str()).collect();
    assert_eq!(
        codes,
        [WarningCode::OutputItemUnusable.as_str(), WarningCode::UnexpectedOutputCount.as_str()]
    );
}

#[tokio::test]
async fn a_success_body_without_a_usable_image_is_provider_bad_response_and_never_retried() {
    for body in [
        json!({"data": []}),
        json!({"nope": true}),
        images_body(vec![json!({"url": "https://example.invalid/x.png"})]),
        images_body(vec![b64_item(b"plain text, not an image")]),
    ] {
        let server = MockServer::start().await;
        mount(&server, ok(body.clone())).await;
        let e = generate(&server, sized("1024x1024")).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::ProviderBadResponse, "{body}");
        assert_eq!(e.details.get("charge_possible"), Some(&json!(true)), "{body}");
        assert_eq!(requests(&server).await.len(), 1, "{body}");
    }
}

// ---------------------------------------------------------------- errors and retries

#[tokio::test]
async fn the_error_table_maps_each_documented_answer() {
    let cases: &[(u16, &str, ErrorCode)] = &[
        (400, "invalid_request_error", ErrorCode::InvalidArgument),
        (401, "authentication_error", ErrorCode::AuthenticationFailed),
        (402, "quota_for_entity_exceeded", ErrorCode::QuotaExceeded),
        (403, "forbidden", ErrorCode::PermissionDenied),
        (404, "model_not_found", ErrorCode::PermissionDenied),
        (409, "conflict", ErrorCode::InvalidArgument),
        (413, "payload_too_large", ErrorCode::InvalidArgument),
        (422, "invalid_request_error", ErrorCode::InvalidArgument),
    ];
    for (status, kind, code) in cases {
        let server = MockServer::start().await;
        mount(&server, gateway_error(*status, "no", kind)).await;
        let e = generate_err(&server).await;
        assert_eq!(e.code, *code, "HTTP {status}");
        assert_eq!(e.provider, Some(ProviderId::Vercel));
        assert_eq!(e.provider_status, Some(*status));
        assert_eq!(e.provider_code.as_deref(), Some(*kind));
        assert_eq!(e.provider_request_id.as_deref(), Some("iad1::err-456"));
        assert_eq!(requests(&server).await.len(), 1, "HTTP {status} is never retried");
    }
}

#[tokio::test]
async fn a_429_is_retried_because_the_gateway_documents_it_as_not_processed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(GEN))
        .respond_with(gateway_error(429, "Rate limit exceeded", "rate_limit_exceeded"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(GEN))
        .respond_with(ok(images_body(vec![b64_item(&jpeg())])))
        .with_priority(2)
        .mount(&server)
        .await;
    let out = generate(&server, sized("1024x1024")).await.unwrap();
    assert_eq!(out.images.len(), 1);
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn server_errors_are_submission_uncertain_and_never_retried() {
    for status in [408, 500, 502, 503, 504] {
        let server = MockServer::start().await;
        mount(&server, gateway_error(status, "boom", "internal_server_error")).await;
        let e = generate_err(&server).await;
        assert_eq!(e.code, ErrorCode::SubmissionUncertain, "HTTP {status}");
        assert_eq!(e.retryable, Some(false));
        assert_eq!(e.details.get("charge_possible"), Some(&json!(true)));
        assert_eq!(requests(&server).await.len(), 1, "HTTP {status}");
    }
}

#[tokio::test]
async fn a_timeout_after_sending_is_submission_uncertain_and_never_retried() {
    let server = MockServer::start().await;
    mount(&server, ok(images_body(vec![b64_item(&jpeg())])).set_delay(Duration::from_secs(5))).await;
    let e = generate_err(&server).await;
    assert_eq!(e.code, ErrorCode::SubmissionUncertain);
    assert_eq!(e.details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn a_refused_connection_is_not_a_possible_charge() {
    let e = VercelProvider::new()
        .generate(&request(sized("1024x1024")), &ctx_for(&format!("{DEAD_URL}/v1")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::NetworkError);
    assert_eq!(e.details.get("charge_possible"), Some(&json!(false)));
    assert_eq!(e.details.get("attempts"), Some(&json!(3)), "a refused connection is retried");
}

#[tokio::test]
async fn the_key_never_appears_in_an_error() {
    let server = MockServer::start().await;
    mount(&server, gateway_error(401, &format!("bad key {KEY}"), "authentication_error")).await;
    let e = generate_err(&server).await;
    let text = format!("{} {:?} {:?} {:?}", e.message, e.hint, e.details, e.provider_code);
    assert!(!text.contains(KEY), "{text}");
}

// ---------------------------------------------------------------- access check

async fn access(server: &MockServer, template: ResponseTemplate) -> Result<AccountAccess, IrisError> {
    Mock::given(method("GET")).and(path("/v1/models")).respond_with(template).mount(server).await;
    VercelProvider::new().check_access(MODEL, &ctx(server)).await
}

fn catalog(status: &str, account: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "object": "list",
        "data": [{"id": MODEL, "type": "image", "model_eligibility": {"evaluated_runtime": "http", "status": status}}],
        "request_context_availability": {"http": {"status": account}},
        "availability_status": "complete"
    }))
}

#[tokio::test]
async fn check_access_reads_the_teams_eligibility_with_one_authenticated_read() {
    let cases = [
        (catalog("eligible", "available"), AccountAccess::Available),
        (catalog("ineligible", "available"), AccountAccess::Unavailable),
        (catalog("eligible", "unavailable"), AccountAccess::Unavailable),
        (catalog("unknown", "available"), AccountAccess::Unknown),
        (
            ResponseTemplate::new(403).set_body_json(json!({"error": {"message": "no"}})),
            AccountAccess::Unknown,
        ),
    ];
    for (template, expected) in cases {
        let server = MockServer::start().await;
        assert_eq!(access(&server, template).await.unwrap(), expected);
        let reqs = requests(&server).await;
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url.query(), Some("include_availability"));
        assert_eq!(
            reqs[0].headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some(format!("Bearer {KEY}").as_str())
        );
    }
    let server = MockServer::start().await;
    let e = access(&server, gateway_error(401, "bad key", "authentication_error")).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::AuthenticationFailed);
}

#[tokio::test]
async fn checking_several_models_reads_the_list_once_and_answers_each() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(catalog("eligible", "available"))
        .mount(&server)
        .await;
    let results = VercelProvider::new().check_access_many(&[MODEL, "bfl/not-listed"], &ctx(&server)).await;
    let results: Vec<AccountAccess> = results.into_iter().map(Result::unwrap).collect();
    assert_eq!(results, [AccountAccess::Available, AccountAccess::Unavailable]);
    assert_eq!(requests(&server).await.len(), 1, "one read answers every model");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(gateway_error(401, "bad key", "authentication_error"))
        .mount(&server)
        .await;
    let results = VercelProvider::new().check_access_many(&[MODEL, "bfl/other"], &ctx(&server)).await;
    assert!(results.iter().all(|r| r.as_ref().unwrap_err().code == ErrorCode::AuthenticationFailed));
}

#[test]
fn the_access_scope_says_the_check_covers_eligibility_and_admission() {
    let scope = VercelProvider::new().access_scope();
    assert_eq!(scope.available, "eligible for this key's team");
    assert!(scope.basis.contains("funds, quota"), "{}", scope.basis);
}

#[tokio::test]
async fn a_429_that_does_not_clear_is_rate_limited_after_a_bounded_number_of_tries() {
    let server = MockServer::start().await;
    mount(&server, gateway_error(429, "Rate limit exceeded", "rate_limit_exceeded")).await;
    let e = generate_err(&server).await;
    assert_eq!(e.code, ErrorCode::RateLimited);
    let sent = requests(&server).await.len();
    assert!((2..=5).contains(&sent), "retried, but boundedly: {sent} requests");
    assert!(e.details.get("charge_possible").is_none_or(|v| v == &json!(false)), "{:?}", e.details);
}
