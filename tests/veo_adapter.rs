//! Veo adapter (`predictLongRunning` + operation polling) against a local wiremock
//! server: submit bodies, always-sent defaults, the paid-submission uncertainty
//! rules, poll status mapping, operation-name and output-URI validation.
//! Offline; fake key only.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iris::catalog::{OptionValue, ResolvedOptions};
use iris::domain::ProviderId;
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy, Timeouts};
use iris::providers::gemini::{GeminiProvider, is_operation_name, validate_output_uri};
use iris::providers::{
    InputImage, InputRole, ProviderContext, RemoteStatus, SubmittedOperation, VideoProvider, VideoRequest,
};
use iris::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const KEY: &str = "test-gemini-key-000";
const LITE: &str = "veo-3.1-lite-generate-preview";
const FAST: &str = "veo-3.1-fast-generate-preview";
const OPERATION: &str = "models/veo-3.1-lite-generate-preview/operations/abc123xyz";

fn ctx_with(server: &MockServer, submit: Duration) -> ProviderContext {
    ctx_for(&server.uri(), submit)
}

fn ctx_for(base: &str, submit: Duration) -> ProviderContext {
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
        base_url: url::Url::parse(base).unwrap(),
        credential: Secret::new(KEY),
        timeouts: Timeouts {
            connect: Duration::from_secs(2),
            generate: Duration::from_secs(5),
            submit,
            poll: Duration::from_secs(5),
            download_idle: Duration::from_secs(5),
        },
    }
}

fn ctx(server: &MockServer) -> ProviderContext {
    ctx_with(server, Duration::from_secs(5))
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Inputs are passed through as given (the app validated them); magic bytes suffice.
fn image(role: InputRole, media_type: &str, bytes: &[u8]) -> InputImage {
    InputImage {
        role,
        path: PathBuf::from("/tmp/frame"),
        file_name: "frame".to_string(),
        media_type: media_type.to_string(),
        bytes: bytes.to_vec(),
    }
}

const PNG_HEAD: &[u8] = b"\x89PNG\r\n\x1a\n-first";
const JPEG_HEAD: &[u8] = b"\xff\xd8\xff\xe0-last";

fn s(v: &str) -> OptionValue {
    OptionValue::Str(v.to_string())
}

fn request(model: &str, pairs: &[(&str, OptionValue)]) -> VideoRequest {
    let mut options = ResolvedOptions::new();
    for (k, v) in pairs {
        options.insert(*k, v.clone());
    }
    VideoRequest {
        model: model.to_string(),
        prompt: "A slow aerial shot over a calm alpine lake at sunrise".to_string(),
        first_frame: None,
        last_frame: None,
        references: vec![],
        options,
    }
}

fn submit_path(model: &str) -> String {
    format!("/v1beta/models/{model}:predictLongRunning")
}

async fn mount_submit(server: &MockServer, model: &str, template: ResponseTemplate) {
    Mock::given(method("POST")).and(path(submit_path(model))).respond_with(template).mount(server).await;
}

fn accepted() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"name": OPERATION}))
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

fn body_of(req: &Request) -> Value {
    serde_json::from_slice(&req.body).unwrap()
}

fn assert_header_auth(reqs: &[Request]) {
    assert!(!reqs.is_empty());
    for r in reqs {
        assert_eq!(r.headers.get("x-goog-api-key").unwrap().to_str().unwrap(), KEY);
        assert!(r.url.query().is_none_or(|q| !q.contains("key=")), "{}", r.url);
        assert!(!r.url.as_str().contains(KEY));
    }
}

async fn submit(server: &MockServer, req: &VideoRequest) -> Result<SubmittedOperation, IrisError> {
    GeminiProvider::new().submit(req, &ctx(server)).await
}

/// Submit `req` against a server that accepts it; return the JSON body sent.
async fn sent_body(req: &VideoRequest) -> Value {
    let server = MockServer::start().await;
    mount_submit(&server, &req.model, accepted()).await;
    let op = submit(&server, req).await.unwrap();
    assert_eq!(op.remote_id, OPERATION);
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 1);
    assert_header_auth(&reqs);
    assert_eq!(reqs[0].headers.get("content-type").unwrap().to_str().unwrap(), "application/json");
    body_of(&reqs[0])
}

#[tokio::test]
async fn text_to_video_always_sends_the_default_duration_resolution_and_aspect() {
    let body = sent_body(&request(LITE, &[])).await;
    assert_eq!(
        body,
        json!({
            "instances": [{"prompt": "A slow aerial shot over a calm alpine lake at sunrise"}],
            "parameters": {"aspectRatio": "16:9", "resolution": "720p", "durationSeconds": 8}
        })
    );
    for absent in
        ["sampleCount", "numberOfVideos", "generateAudio", "seed", "negativePrompt", "personGeneration"]
    {
        assert!(body["parameters"].get(absent).is_none(), "{absent}");
    }
    assert!(body.get("labels").is_none() && body.get("webhookConfig").is_none());
}

#[tokio::test]
async fn explicit_options_map_to_their_wire_fields() {
    let req = request(
        FAST,
        &[
            ("count", OptionValue::Int(1)),
            ("duration", s("4")),
            ("resolution", s("720p")),
            ("aspect_ratio", s("9:16")),
            ("negative_prompt", s("people, text")),
            ("person_generation", s("allow_all")),
        ],
    );
    let body = sent_body(&req).await;
    assert_eq!(
        body["parameters"],
        json!({
            "aspectRatio": "9:16",
            "resolution": "720p",
            "durationSeconds": 4,
            "negativePrompt": "people, text",
            "personGeneration": "allow_all"
        })
    );
    assert!(body["parameters"]["durationSeconds"].is_u64(), "an integer, not a string");
    let body = sent_body(&request(FAST, &[("resolution", s("4k"))])).await;
    assert_eq!(body["parameters"], json!({"aspectRatio": "16:9", "resolution": "4k", "durationSeconds": 8}));
}

#[tokio::test]
async fn first_and_last_frames_use_the_sdk_image_encoding() {
    let mut req = request(LITE, &[("duration", s("6"))]);
    req.first_frame = Some(image(InputRole::FirstFrame, "image/png", PNG_HEAD));
    req.last_frame = Some(image(InputRole::LastFrame, "image/jpeg", JPEG_HEAD));
    let body = sent_body(&req).await;
    assert_eq!(
        body["instances"],
        json!([{
            "prompt": "A slow aerial shot over a calm alpine lake at sunrise",
            "image": {"bytesBase64Encoded": b64(PNG_HEAD), "mimeType": "image/png"},
            "lastFrame": {"bytesBase64Encoded": b64(JPEG_HEAD), "mimeType": "image/jpeg"}
        }])
    );
    assert_eq!(body["parameters"]["durationSeconds"], 6);

    let mut first_only = request(LITE, &[]);
    first_only.first_frame = Some(image(InputRole::FirstFrame, "image/jpeg", JPEG_HEAD));
    let body = sent_body(&first_only).await;
    assert!(body["instances"][0].get("lastFrame").is_none());
    assert_eq!(body["instances"][0]["image"]["mimeType"], "image/jpeg");
}

#[tokio::test]
async fn reference_images_are_sent_as_asset_references_in_order() {
    let mut req = request(FAST, &[]);
    req.references = vec![
        image(InputRole::Reference, "image/png", PNG_HEAD),
        image(InputRole::Reference, "image/jpeg", JPEG_HEAD),
    ];
    let body = sent_body(&req).await;
    assert_eq!(
        body["instances"][0]["referenceImages"],
        json!([
            {"image": {"bytesBase64Encoded": b64(PNG_HEAD), "mimeType": "image/png"}, "referenceType": "ASSET"},
            {"image": {"bytesBase64Encoded": b64(JPEG_HEAD), "mimeType": "image/jpeg"}, "referenceType": "ASSET"}
        ])
    );
    assert!(body["instances"][0].get("image").is_none());
    assert_eq!(body["parameters"]["durationSeconds"], 8);
}

#[tokio::test]
async fn unmapped_options_and_unsafe_model_ids_fail_before_sending() {
    let server = MockServer::start().await;
    mount_submit(&server, LITE, accepted()).await;
    for (name, value) in
        [("audio", OptionValue::Bool(false)), ("seed", OptionValue::Int(1)), ("count", OptionValue::Int(2))]
    {
        let err = submit(&server, &request(LITE, &[(name, value)])).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError, "{name}");
    }
    let err = submit(&server, &request("veo/../../v1/models/x", &[])).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn requests_of_one_hundred_megabytes_fail_locally_with_zero_requests() {
    let server = MockServer::start().await;
    mount_submit(&server, FAST, accepted()).await;
    let mut req = request(FAST, &[]);
    let mut big = PNG_HEAD.to_vec();
    big.resize(25_000_000, 0);
    req.references = (0..3).map(|_| image(InputRole::Reference, "image/png", &big)).collect();
    let err = submit(&server, &req).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.details["limit_bytes"], 100_000_000);
    assert!(requests(&server).await.is_empty());
}

fn google_error(code: u16, status: &str, message: &str) -> Value {
    json!({"error": {"code": code, "message": message, "status": status}})
}

#[tokio::test]
async fn a_rate_limited_submit_is_retried_and_then_accepted() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(submit_path(LITE)))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({"error": {
            "code": 429, "message": "Resource has been exhausted", "status": "RESOURCE_EXHAUSTED",
            "details": [{"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "0.01s"}]
        }})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(submit_path(LITE)))
        .respond_with(accepted())
        .with_priority(2)
        .mount(&server)
        .await;
    let op = submit(&server, &request(LITE, &[])).await.unwrap();
    assert_eq!(op.remote_id, OPERATION);
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 2);
    assert_header_auth(&reqs);
    assert_eq!(reqs[0].body, reqs[1].body, "the retry resends the identical request");
    for r in &reqs {
        assert_eq!(r.headers.get("content-type").unwrap().to_str().unwrap(), "application/json");
    }
}

async fn uncertain_case(template: ResponseTemplate) -> (IrisError, usize) {
    let server = MockServer::start().await;
    mount_submit(&server, LITE, template).await;
    let err = submit(&server, &request(LITE, &[])).await.unwrap_err();
    (err, requests(&server).await.len())
}

fn assert_uncertain(err: &IrisError) {
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{}", err.message);
    assert_eq!(err.exit_code(), 5);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.provider, Some(ProviderId::Gemini));
    assert_eq!(err.details["charge_possible"], true);
    let hint = err.hint.as_deref().unwrap();
    assert!(hint.contains("Google AI Studio") && hint.contains("will not resubmit"), "{hint}");
}

#[tokio::test]
async fn server_errors_on_submit_are_uncertain_and_sent_exactly_once() {
    for status in [500u16, 502, 503, 504, 408] {
        let (err, sent) = uncertain_case(
            ResponseTemplate::new(status).set_body_json(google_error(status, "INTERNAL", "boom")),
        )
        .await;
        assert_uncertain(&err);
        assert_eq!(sent, 1, "HTTP {status} must never be resubmitted");
        assert_eq!(err.provider_status, Some(status));
    }
    let (err, _) = uncertain_case(ResponseTemplate::new(503).set_body_json(google_error(
        503,
        "UNAVAILABLE",
        "overloaded",
    )))
    .await;
    assert_eq!(err.provider_code.as_deref(), Some("UNAVAILABLE"));
    assert_eq!(err.details["provider_message"], "overloaded");
}

#[tokio::test]
async fn a_submit_timeout_after_sending_is_uncertain() {
    let server = MockServer::start().await;
    mount_submit(&server, LITE, accepted().set_delay(Duration::from_millis(1500))).await;
    let err = GeminiProvider::new()
        .submit(&request(LITE, &[]), &ctx_with(&server, Duration::from_millis(300)))
        .await
        .unwrap_err();
    assert_uncertain(&err);
    assert_eq!(err.details["transport"], "timeout");
    assert_eq!(requests(&server).await.len(), 1);
}

/// One scripted raw HTTP exchange per connection: read the request (headers and
/// `Content-Length` body), write `response` (possibly nothing), close. Returns the
/// base URL and the number of connections accepted.
fn raw_server(responses: Vec<Vec<u8>>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((stream, _)) = listener.accept() else { return };
            seen.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);
            let mut stream = stream;
            let _ = stream.write_all(&response);
            let _ = stream.flush();
        }
    });
    (base, count)
}

#[tokio::test]
async fn a_reset_or_truncated_answer_after_sending_is_uncertain_and_never_resent() {
    let accepted = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
        json!({"name": OPERATION}).to_string().len(),
        json!({"name": OPERATION})
    )
    .into_bytes();
    let truncated =
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"name\":"
            .to_vec();
    let closed_without_answer = Vec::new();
    for first in [truncated, closed_without_answer] {
        // A resend would reach the second, accepting exchange and succeed.
        let (base, connections) = raw_server(vec![first, accepted.clone()]);
        let err = GeminiProvider::new()
            .submit(&request(LITE, &[]), &ctx_for(&base, Duration::from_secs(5)))
            .await
            .unwrap_err();
        assert_uncertain(&err);
        assert_eq!(err.details["transport"], "other", "{}", err.message);
        assert_eq!(connections.load(Ordering::SeqCst), 1, "a paid submit is never resent after sending");
    }
}

#[tokio::test]
async fn unusable_success_bodies_are_uncertain() {
    let cases = [
        ResponseTemplate::new(200).set_body_string("<html>gateway</html>"),
        ResponseTemplate::new(200).set_body_json(json!({"done": false})),
        ResponseTemplate::new(200).set_body_json(json!({"name": "operations/abc"})),
        ResponseTemplate::new(200).set_body_json(json!({"name": "models/veo/operations/../../files/x"})),
        ResponseTemplate::new(200).set_body_json(json!({"name": "models/veo/operations/abc?alt=1"})),
        ResponseTemplate::new(200).set_body_json(json!({"name": "models/veo/operations/abc:cancel"})),
        ResponseTemplate::new(200).set_body_json(json!({"name": "models/veo/operations/.."})),
    ];
    for template in cases {
        let (err, sent) = uncertain_case(template).await;
        assert_uncertain(&err);
        assert_eq!(sent, 1);
    }
}

#[tokio::test]
async fn definite_rejections_on_submit_map_like_image_errors() {
    let rows = [
        (400, google_error(400, "INVALID_ARGUMENT", "bad durationSeconds"), ErrorCode::InvalidArgument),
        (
            400,
            json!({"error": {"code": 400, "message": "API key not valid.", "status": "INVALID_ARGUMENT",
                    "details": [{"@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "API_KEY_INVALID"}]}}),
            ErrorCode::AuthenticationFailed,
        ),
        (401, google_error(401, "UNAUTHENTICATED", "no"), ErrorCode::AuthenticationFailed),
        (402, google_error(402, "RESOURCE_EXHAUSTED", "prepay depleted"), ErrorCode::QuotaExceeded),
        (403, google_error(403, "PERMISSION_DENIED", "no"), ErrorCode::PermissionDenied),
        (404, google_error(404, "NOT_FOUND", "no such model"), ErrorCode::PermissionDenied),
    ];
    for (status, body, code) in rows {
        let (err, sent) = uncertain_case(ResponseTemplate::new(status).set_body_json(body)).await;
        assert_eq!(err.code, code, "HTTP {status}");
        assert_eq!(sent, 1);
        assert_eq!(err.provider_status, Some(status));
    }
    let (err, sent) = uncertain_case(ResponseTemplate::new(429).set_body_json(google_error(
        429,
        "RESOURCE_EXHAUSTED",
        "slow",
    )))
    .await;
    assert_eq!(err.code, ErrorCode::RateLimited, "a 429 is a definite rejection");
    assert_eq!(sent, 3);
}

#[tokio::test]
async fn a_zero_quota_on_submit_is_a_definite_quota_rejection_sent_once() {
    let (err, sent) = uncertain_case(ResponseTemplate::new(429).set_body_json(json!({"error": {
        "code": 429,
        "message": "You exceeded your current quota, please check your plan and billing details.",
        "status": "RESOURCE_EXHAUSTED",
        "details": [
            {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{
                "quotaId": "PredictLongRunningRequestsPerDayPerProjectPerModel-FreeTier",
                "quotaValue": "0"
            }]},
            {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "0.01s"}
        ]
    }})))
    .await;
    assert_eq!(err.code, ErrorCode::QuotaExceeded, "{}", err.message);
    assert_eq!(sent, 1);
    assert_eq!(err.exit_code(), 3);
    assert!(err.details.get("charge_possible").is_none(), "a rejection is not an uncertain submit");
    assert!(err.hint.as_deref().unwrap().contains("no free tier"));
}

// ----- polling -------------------------------------------------------------------

async fn poll_with(
    template: ResponseTemplate,
) -> (Result<RemoteStatus, IrisError>, Vec<Request>, MockServer) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(template)
        .mount(&server)
        .await;
    let status = GeminiProvider::new().poll(OPERATION, &ctx(&server)).await;
    let reqs = requests(&server).await;
    (status, reqs, server)
}

async fn poll_json(body: Value) -> RemoteStatus {
    poll_with(ResponseTemplate::new(200).set_body_json(body)).await.0.unwrap()
}

fn failed(status: RemoteStatus) -> IrisError {
    match status {
        RemoteStatus::Failed { error } => error,
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn a_running_operation_reports_progress_when_present() {
    let (status, reqs, _server) =
        poll_with(ResponseTemplate::new(200).set_body_json(json!({"name": OPERATION, "done": false}))).await;
    assert!(matches!(status.unwrap(), RemoteStatus::Running { progress: None }));
    assert_eq!(reqs.len(), 1);
    assert_header_auth(&reqs);
    assert_eq!(reqs[0].url.path(), format!("/v1beta/{OPERATION}"));

    let status = poll_json(json!({"name": OPERATION, "metadata": {"progressPercent": 42}})).await;
    assert!(matches!(status, RemoteStatus::Running { progress: Some(p) } if (p - 42.0).abs() < f32::EPSILON));
    let status = poll_json(json!({"name": OPERATION, "metadata": {"progressPercent": 250}})).await;
    assert!(matches!(status, RemoteStatus::Running { progress: None }));
}

#[tokio::test]
async fn a_succeeded_operation_returns_validated_download_uris() {
    let server = MockServer::start().await;
    let uri = format!("{}/v1beta/files/abc-123:download?alt=media", server.uri());
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": OPERATION,
            "done": true,
            "response": {
                "@type": "type.googleapis.com/google.ai.generativelanguage.v1beta.PredictLongRunningResponse",
                "generateVideoResponse": {"generatedSamples": [{"video": {"uri": uri}}]}
            }
        })))
        .mount(&server)
        .await;
    match GeminiProvider::new().poll(OPERATION, &ctx(&server)).await.unwrap() {
        RemoteStatus::Succeeded { outputs, usage, warnings } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].uri, uri);
            assert_eq!(outputs[0].media_type.as_deref(), Some("video/mp4"));
            assert!(usage.is_none());
            assert!(warnings.is_empty());
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }
}

#[tokio::test]
async fn a_success_with_filtered_outputs_carries_a_content_filtered_warning() {
    let server = MockServer::start().await;
    let uri = format!("{}/v1beta/files/abc-123:download?alt=media", server.uri());
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": OPERATION,
            "done": true,
            "response": {"generateVideoResponse": {
                "generatedSamples": [{"video": {"uri": uri}}],
                "raiMediaFilteredCount": 1,
                "raiMediaFilteredReasons": ["One output was blocked for safety reasons."]
            }}
        })))
        .mount(&server)
        .await;
    match GeminiProvider::new().poll(OPERATION, &ctx(&server)).await.unwrap() {
        RemoteStatus::Succeeded { outputs, warnings, .. } => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].uri, uri);
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].code, "content_filtered");
            assert!(warnings[0].message.contains("filtered 1 output"), "{}", warnings[0].message);
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }
}

#[tokio::test]
async fn a_filtered_video_is_content_blocked_and_not_charged() {
    let err = failed(
        poll_json(json!({
            "name": OPERATION,
            "done": true,
            "response": {"generateVideoResponse": {
                "raiMediaFilteredCount": 1,
                "raiMediaFilteredReasons": ["Video generation was blocked for safety reasons."]
            }}
        }))
        .await,
    );
    assert_eq!(err.code, ErrorCode::ContentBlocked);
    assert_eq!(err.details["reasons"], json!(["Video generation was blocked for safety reasons."]));
    assert!(err.hint.as_deref().unwrap().contains("not charged"));
    assert_eq!(err.remote_operation_id.as_deref(), Some(OPERATION));
}

#[tokio::test]
async fn an_operation_error_is_a_failed_remote_job() {
    let err = failed(
        poll_json(json!({
            "name": OPERATION,
            "done": true,
            "error": {"code": 13, "message": "Internal error while generating the video."}
        }))
        .await,
    );
    assert_eq!(err.code, ErrorCode::RemoteJobFailed);
    assert_eq!(err.provider_code.as_deref(), Some("INTERNAL"));
    assert_eq!(err.details["provider_message"], "Internal error while generating the video.");
    assert_eq!(err.provider, Some(ProviderId::Gemini));
}

#[tokio::test]
async fn done_without_video_or_error_is_a_bad_response() {
    let err = failed(poll_json(json!({"name": OPERATION, "done": true})).await);
    assert_eq!(err.code, ErrorCode::ProviderBadResponse);
    let err = failed(
        poll_json(json!({"name": OPERATION, "done": true,
            "response": {"generateVideoResponse": {"generatedSamples": [{"video": {"encodedVideo": "AAAA"}}]}}}))
        .await,
    );
    assert_eq!(err.code, ErrorCode::ProviderBadResponse);
    assert!(err.message.contains("inline video bytes"));
}

#[tokio::test]
async fn only_a_google_not_found_answer_reports_the_operation_gone() {
    let (status, reqs, _server) =
        poll_with(ResponseTemplate::new(404).set_body_json(google_error(404, "NOT_FOUND", "not found")))
            .await;
    let RemoteStatus::Gone { error } = status.unwrap() else { panic!("expected Gone") };
    assert_eq!(reqs.len(), 1);
    // The provider's evidence travels with it, phrased for the not-yet-expired case.
    assert_eq!(error.provider, Some(ProviderId::Gemini));
    assert_eq!(error.provider_status, Some(404));
    assert_eq!(error.provider_code.as_deref(), Some("NOT_FOUND"));
    assert_eq!(error.remote_operation_id.as_deref(), Some(OPERATION));
    assert_eq!(error.retryable, Some(false));
    let hint = error.hint.as_deref().unwrap();
    assert!(hint.contains("GEMINI_API_KEY") && hint.contains("base URL"), "{hint}");

    // Any other 404 (a proxy's or web server's page, an empty body) is an error, not "gone".
    for template in [
        ResponseTemplate::new(404).set_body_raw("<html><body>Not Found</body></html>", "text/html"),
        ResponseTemplate::new(404),
        ResponseTemplate::new(404).set_body_json(json!({"error": {"code": 404, "message": "nope"}})),
    ] {
        let (status, reqs, _server) = poll_with(template).await;
        let err = status.unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(err.provider_status, Some(404));
        assert_eq!(err.provider, Some(ProviderId::Gemini));
        assert_eq!(err.remote_operation_id.as_deref(), Some(OPERATION));
        assert!(err.hint.as_deref().unwrap().contains("base URL"), "{err:?}");
        assert_eq!(reqs.len(), 1, "a 404 is not retried");
    }
}

#[tokio::test]
async fn transient_poll_failures_are_retried() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(ResponseTemplate::new(503).set_body_json(google_error(503, "UNAVAILABLE", "busy")))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": OPERATION, "done": false})))
        .with_priority(2)
        .mount(&server)
        .await;
    let status = GeminiProvider::new().poll(OPERATION, &ctx(&server)).await.unwrap();
    assert!(matches!(status, RemoteStatus::Running { .. }));
    assert_eq!(requests(&server).await.len(), 3);
}

#[tokio::test]
async fn poll_errors_never_claim_the_job_failed() {
    let (status, reqs, _server) =
        poll_with(ResponseTemplate::new(500).set_body_json(google_error(500, "INTERNAL", "down"))).await;
    let err = status.unwrap_err();
    assert_eq!(err.code, ErrorCode::ProviderError);
    assert_eq!(err.remote_operation_id.as_deref(), Some(OPERATION));
    assert_eq!(reqs.len(), 5, "IdempotentRead retries 5xx up to 5 attempts");

    let (status, _, _server) =
        poll_with(ResponseTemplate::new(403).set_body_json(google_error(403, "PERMISSION_DENIED", "no")))
            .await;
    assert_eq!(status.unwrap_err().code, ErrorCode::PermissionDenied);

    let (status, _, _server) = poll_with(ResponseTemplate::new(200).set_body_string("not json")).await;
    assert_eq!(status.unwrap_err().code, ErrorCode::ProviderBadResponse);
}

#[tokio::test]
async fn operation_names_are_validated_before_any_url_is_built() {
    let server = MockServer::start().await;
    for bad in [
        "models/veo/operations/../../../v1/models/x",
        "https://evil.example/v1beta/models/veo/operations/x",
        "models/veo/operations/x?key=1",
        "models/veo/operations/x#y",
        "models/veo/operations/",
        "files/abc",
        "",
    ] {
        let err = GeminiProvider::new().poll(bad, &ctx(&server)).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{bad}");
        assert!(!is_operation_name(bad));
    }
    assert!(requests(&server).await.is_empty());
    assert!(is_operation_name(OPERATION));
}

#[tokio::test]
async fn any_unreserved_operation_id_is_accepted_and_polled_verbatim() {
    // The grammar is wider than the documented lowercase examples so that a real id
    // with other URL-safe characters is not reported as an uncertain submit.
    let name = "models/veo-3.1-lite-generate-preview/operations/_Op.id~2-X";
    let server = MockServer::start().await;
    mount_submit(&server, LITE, ResponseTemplate::new(200).set_body_json(json!({"name": name}))).await;
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{name}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": name, "done": false})))
        .mount(&server)
        .await;
    let op = submit(&server, &request(LITE, &[])).await.unwrap();
    assert_eq!(op.remote_id, name);
    let status = GeminiProvider::new().poll(&op.remote_id, &ctx(&server)).await.unwrap();
    assert!(matches!(status, RemoteStatus::Running { .. }));
    let reqs = requests(&server).await;
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].url.path(), format!("/v1beta/{name}"));
    assert!(reqs[1].url.query().is_none());
}

/// A done operation answered `uri` as its only output.
async fn poll_output(server: &MockServer, uri: &str) -> RemoteStatus {
    Mock::given(method("GET"))
        .and(path(format!("/v1beta/{OPERATION}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": OPERATION, "done": true,
            "response": {"generateVideoResponse": {"generatedSamples": [{"video": {"uri": uri}}]}}
        })))
        .up_to_n_times(1)
        .mount(server)
        .await;
    GeminiProvider::new().poll(OPERATION, &ctx(server)).await.unwrap()
}

#[tokio::test]
async fn untrusted_output_uris_still_mean_success_and_are_refused_only_at_download_time() {
    let server = MockServer::start().await;
    let base = server.uri();
    let base_url = url::Url::parse(&base).unwrap();
    let other_port = base_url.port().unwrap().wrapping_add(1);
    let untrusted = [
        format!("http://127.0.0.1:{other_port}/v1beta/files/abc:download?alt=media"),
        "https://generativelanguage.googleapis.com/v1beta/files/abc:download?alt=media".to_string(),
        "https://storage.googleapis.com/veo-out/abc.mp4?X-Goog-Signature=deadbeef".to_string(),
        format!("{base}/v1/files/abc:download"),
        format!("{base}/v1beta/files/abc"),
        format!("{base}/v1beta/files/ABC:download"),
        format!("{base}/v1beta/files/-abc:download"),
        format!("{base}/v1beta/files/a_b:download"),
        format!("{base}/v1beta/files/{}:download", "a".repeat(41)),
        format!("{base}/v1beta/files/../models/x:download"),
    ];
    for uri in &untrusted {
        // The job finished: its output is recorded exactly as sent (the record is
        // where the raw URI lives), whatever Iris later decides about fetching it.
        match poll_output(&server, uri).await {
            RemoteStatus::Succeeded { outputs, .. } => {
                assert_eq!(outputs.len(), 1, "{uri}");
                assert_eq!(&outputs[0].uri, uri);
            }
            other => panic!("{uri}: expected Succeeded, got {other:?}"),
        }
        // Fetching it is refused against this base URL: download_failed, redacted.
        let err = GeminiProvider::new().check_output_uri(uri, &base_url).unwrap_err();
        assert_eq!(err.code, ErrorCode::DownloadFailed, "{uri}");
        assert_eq!(err.retryable, Some(false));
        assert_eq!(err.provider, Some(ProviderId::Gemini));
        let shown = err.details["uri"].as_str().unwrap();
        assert!(!shown.contains("deadbeef"), "{shown}");
        assert!(err.hint.as_deref().unwrap().contains("proxy"), "{err:?}");
    }
    // A Files API URI under the configured base is fetched.
    let good = format!("{base}/v1beta/files/abc-123:download?alt=media");
    assert!(GeminiProvider::new().check_output_uri(&good, &base_url).is_ok());
    // A URI on Google's origin is trusted once the base URL is Google's again.
    let google = url::Url::parse("https://generativelanguage.googleapis.com").unwrap();
    assert!(GeminiProvider::new().check_output_uri(&untrusted[1], &google).is_ok());
}

#[tokio::test]
async fn structurally_unusable_output_uris_are_a_bad_response() {
    let server = MockServer::start().await;
    let base = server.uri();
    for uri in [
        "not a url".to_string(),
        "ftp://generativelanguage.googleapis.com/v1beta/files/abc:download".to_string(),
        "file:///etc/passwd".to_string(),
        format!("{}/v1beta/files/abc:download#frag", base),
        base.replace("http://", "http://user:pw@") + "/v1beta/files/abc:download",
    ] {
        let err = failed(poll_output(&server, &uri).await);
        assert_eq!(err.code, ErrorCode::ProviderBadResponse, "{uri}");
        assert!(err.details.contains_key("uri"), "{uri}");
        assert!(!err.details["uri"].as_str().unwrap().contains("pw@"), "{err:?}");
        assert_eq!(err.remote_operation_id.as_deref(), Some(OPERATION));
    }
}

#[test]
fn output_uri_validation_accepts_only_files_downloads_under_the_base() {
    let base = url::Url::parse("https://generativelanguage.googleapis.com").unwrap();
    let good = "https://generativelanguage.googleapis.com/v1beta/files/abc-123:download?alt=media";
    assert_eq!(validate_output_uri(good, &base).unwrap().as_str(), good);
    let forty = format!("https://generativelanguage.googleapis.com/v1beta/files/{}:download", "a".repeat(40));
    assert!(validate_output_uri(&forty, &base).is_ok());
    assert!(
        validate_output_uri("https://generativelanguage.googleapis.com:443/v1beta/files/x:download", &base)
            .is_ok()
    );
    for bad in [
        "http://generativelanguage.googleapis.com/v1beta/files/x:download",
        "https://user:pw@generativelanguage.googleapis.com/v1beta/files/x:download",
        "https://generativelanguage.googleapis.com.evil.example/v1beta/files/x:download",
        "https://generativelanguage.googleapis.com/v1beta/files/x%2Fy:download",
        "https://generativelanguage.googleapis.com/v1beta/files/x:download/more",
        "not a url",
    ] {
        assert!(validate_output_uri(bad, &base).is_err(), "{bad}");
    }
    // A configured base path (e.g. a proxy prefix) is part of the expected path.
    let proxied = url::Url::parse("https://proxy.example/gemini").unwrap();
    assert!(validate_output_uri("https://proxy.example/gemini/v1beta/files/x:download", &proxied).is_ok());
    assert!(validate_output_uri("https://proxy.example/v1beta/files/x:download", &proxied).is_err());
}
