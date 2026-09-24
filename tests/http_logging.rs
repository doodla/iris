//! Debug logging of the HTTP layer: request metadata only (method, redacted URL,
//! status, attempt, elapsed, request id), never credentials, bodies, prompts, or
//! signed query values (C-04 "Redaction", SPEC §6).
//!
//! This binary deliberately holds a single test: tracing caches callsite interest
//! globally, and a thread-scoped subscriber can miss events whose callsites other
//! tests register concurrently.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iris::http::{
    AuthHeader, Call, DownloadRequest, HttpClient, HttpSettings, RetryClass, RetryPolicy, Verdict, download,
};
use iris::secret::Secret;
use serde_json::json;
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAKE_OPENAI: &str = "test-openai-key-000";
const FAKE_GEMINI: &str = "test-gemini-key-000";

/// Captures formatted tracing output.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

fn client() -> HttpClient {
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

#[tokio::test]
async fn debug_logs_carry_request_metadata_but_never_secrets_bodies_or_signed_values() {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter("iris=trace")
        .with_writer(capture.clone())
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // An API call: rate limited once, then accepted.
    let api = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({"error": {"message": "slow down BODYTEXT"}}))
                .insert_header("x-request-id", "req_first"),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&api)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true}))
                .insert_header("x-request-id", "req_second"),
        )
        .with_priority(2)
        .mount(&api)
        .await;
    let url = format!("{}/v1/images/generations?key=QUERYSECRET", api.uri());
    let auth = AuthHeader::new("authorization", "Bearer ", &Secret::new(FAKE_OPENAI)).unwrap();
    let call =
        Call::new(RetryClass::PaidSubmit, Duration::from_secs(5)).with_request_id_header("x-request-id");
    client()
        .execute(
            &call,
            |c| Ok(auth.apply(c.post(&url)).json(&json!({"prompt": "PROMPTTEXT"}))),
            |resp| Verdict::default_for(resp, None),
        )
        .await
        .unwrap();

    // A download: redirected to a signed URL on another origin, which fails once.
    let files = MockServer::start().await;
    let storage = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/abc:download"))
        .respond_with(
            ResponseTemplate::new(302).insert_header(
                "location",
                format!("{}/signed/abc.mp4?X-Goog-Signature=SIGVALUE", storage.uri()),
            ),
        )
        .mount(&files)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&storage)
        .await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"\0\0\0\x18ftypmp42MEDIABYTES".to_vec(), "video/mp4"),
        )
        .with_priority(2)
        .mount(&storage)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = std::fs::File::create(dir.path().join("part")).unwrap();
    let base = Url::parse(&files.uri()).unwrap();
    let gemini_auth = AuthHeader::new("x-goog-api-key", "", &Secret::new(FAKE_GEMINI)).unwrap();
    let artifact = format!("{}/v1beta/files/abc:download?alt=media&token=TOKENVALUE", files.uri());
    let req = DownloadRequest {
        url: &artifact,
        dest: &dest,
        base_url: &base,
        auth: Some(&gemini_auth),
        idle_timeout: Duration::from_secs(5),
        provider: None,
    };
    let done = download(&client(), &req).await.unwrap();
    assert_eq!(done.attempts, 2);

    let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    // Metadata is there.
    assert!(logs.contains("http response") && logs.contains("method=POST"), "{logs}");
    assert!(logs.contains("status=429") && logs.contains("attempt=2"), "{logs}");
    assert!(logs.contains("request_id=\"req_second\""), "{logs}");
    assert!(logs.contains("elapsed_ms="), "{logs}");
    assert!(logs.contains("key=REDACTED"), "{logs}");
    assert!(logs.contains("download response") && logs.contains("X-Goog-Signature=REDACTED"), "{logs}");
    assert!(logs.contains("alt=media"), "allow-listed query values stay readable: {logs}");
    // Secrets, bodies, and prompts are not.
    for leaked in [
        FAKE_OPENAI,
        FAKE_GEMINI,
        "QUERYSECRET",
        "SIGVALUE",
        "TOKENVALUE",
        "PROMPTTEXT",
        "BODYTEXT",
        "MEDIABYTES",
    ] {
        assert!(!logs.contains(leaked), "{leaked} leaked into logs:\n{logs}");
    }
}
