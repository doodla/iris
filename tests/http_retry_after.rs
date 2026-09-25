//! `Retry-After` through the Gemini and Veo adapters: a delay the provider asks
//! for reaches the caller (`retry_after`, shown as `retry_after_seconds`) only on
//! an error that is worth retrying. An agent that waits and resubmits whenever it
//! sees a delay must never do so after an uncertain paid submission (it could pay
//! twice) or an exhausted daily quota (it cannot succeed). Offline: wiremock on
//! 127.0.0.1, fake key.

use std::time::Duration;

use iris::catalog::ResolvedOptions;
use iris::error::{ErrorCode, IrisError};
use iris::http::{HttpClient, HttpSettings, RetryPolicy, Timeouts};
use iris::providers::gemini::GeminiProvider;
use iris::providers::{ImageProvider, ImageRequest, ProviderContext, VideoProvider, VideoRequest};
use iris::secret::Secret;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "test-gemini-key-000";
const IMAGE_MODEL: &str = "gemini-3.1-flash-image";
const VIDEO_MODEL: &str = "veo-3.1-lite-generate-preview";

fn ctx(server: &MockServer) -> ProviderContext {
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
            generate: Duration::from_secs(5),
            submit: Duration::from_secs(5),
            poll: Duration::from_secs(5),
            download_idle: Duration::from_secs(5),
        },
    }
}

fn google_error(code: u16, status: &str, message: &str, details: Value) -> Value {
    json!({"error": {"code": code, "message": message, "status": status, "details": details}})
}

/// A used-up per-day quota (`google.rpc.QuotaFailure`), plus a `RetryInfo` delay.
fn daily_quota() -> Value {
    google_error(
        429,
        "RESOURCE_EXHAUSTED",
        "Quota exceeded.",
        json!([
            {"@type": "type.googleapis.com/google.rpc.QuotaFailure", "violations": [{
                "quotaMetric": "generativelanguage.googleapis.com/generate_requests",
                "quotaId": "GenerateRequestsPerDayPerProjectPerModel",
                "quotaValue": "250"
            }]},
            {"@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "20s"}
        ]),
    )
}

/// Every request to `route` gets `status` + `body` with `Retry-After: <seconds>`.
async fn answering(route: &str, status: u16, body: Value, seconds: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(status).set_body_json(body).insert_header("retry-after", seconds))
        .mount(&server)
        .await;
    server
}

async fn sent(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

fn video_request() -> VideoRequest {
    VideoRequest {
        model: VIDEO_MODEL.to_string(),
        prompt: "A slow aerial shot over a calm alpine lake at sunrise".to_string(),
        first_frame: None,
        last_frame: None,
        references: vec![],
        options: ResolvedOptions::new(),
    }
}

fn image_request() -> ImageRequest {
    ImageRequest {
        model: IMAGE_MODEL.to_string(),
        prompt: "A red kite over green hills, watercolor".to_string(),
        images: vec![],
        mask: None,
        options: ResolvedOptions::new(),
    }
}

async fn submit(server: &MockServer) -> IrisError {
    GeminiProvider::new().submit(&video_request(), &ctx(server)).await.unwrap_err()
}

async fn generate(server: &MockServer) -> IrisError {
    GeminiProvider::new().generate(&image_request(), &ctx(server)).await.unwrap_err()
}

const SUBMIT: &str = "/v1beta/models/veo-3.1-lite-generate-preview:predictLongRunning";
const GENERATE: &str = "/v1/models/gemini-3.1-flash-image:generateContent";

/// A Veo submission answered 503 may have created (and billed) a job: the answer's
/// `Retry-After` must not read as "resubmit in 30 seconds".
#[tokio::test]
async fn an_uncertain_veo_submission_carries_no_retry_delay() {
    let body = google_error(503, "UNAVAILABLE", "The service is currently unavailable.", json!([]));
    let server = answering(SUBMIT, 503, body, "30").await;
    let err = submit(&server).await;
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{err:?}");
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(err.retry_after, None, "{err:?}");
    assert_eq!(err.provider_status, Some(503));
    assert_eq!(sent(&server).await, 1);
}

/// A used-up daily quota resets at midnight Pacific time: neither the header nor
/// the body's `RetryInfo` delay reaches the caller, for images or video.
#[tokio::test]
async fn an_exhausted_daily_quota_carries_no_retry_delay() {
    let server = answering(GENERATE, 429, daily_quota(), "20").await;
    let err = generate(&server).await;
    assert_eq!(err.code, ErrorCode::QuotaExceeded, "{err:?}");
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.retry_after, None, "{err:?}");
    assert_eq!(sent(&server).await, 1);

    let server = answering(SUBMIT, 429, daily_quota(), "20").await;
    let err = submit(&server).await;
    assert_eq!(err.code, ErrorCode::QuotaExceeded, "{err:?}");
    assert_eq!(err.retry_after, None, "{err:?}");
    assert_eq!(sent(&server).await, 1);
}

/// Errors worth retrying keep the delay: a Gemini image 503 (not charged, reported
/// for the caller to run again) and a rate limit that outlasted the attempts.
#[tokio::test]
async fn retryable_gemini_errors_keep_the_retry_delay() {
    let body = google_error(503, "UNAVAILABLE", "The service is currently unavailable.", json!([]));
    let server = answering(GENERATE, 503, body, "30").await;
    let err = generate(&server).await;
    assert_eq!((err.code, err.retryable), (ErrorCode::ProviderError, Some(true)), "{err:?}");
    assert_eq!(err.retry_after, Some(Duration::from_secs(30)));
    assert_eq!(sent(&server).await, 1, "a paid image call is not retried after a 503");

    let body = google_error(429, "RESOURCE_EXHAUSTED", "Slow down.", json!([]));
    let server = answering(SUBMIT, 429, body, "0").await;
    let err = submit(&server).await;
    assert_eq!((err.code, err.retryable), (ErrorCode::RateLimited, Some(true)), "{err:?}");
    assert_eq!(err.retry_after, Some(Duration::ZERO));
    assert_eq!(sent(&server).await, 3, "a rate limit is retried; nothing was processed");
}
