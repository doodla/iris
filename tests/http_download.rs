//! Streaming downloads against localhost mock servers: hashing, retries from an
//! empty file, manual redirects, credential origin rule, https-only rule, and
//! error documents served as media (C-04). No network beyond 127.0.0.1.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use iris::error::ErrorCode;
use iris::http::{
    AuthHeader, DownloadError, DownloadRequest, Downloaded, HttpClient, HttpSettings, RetryPolicy, download,
};
use iris::secret::Secret;
use sha2::{Digest, Sha256};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAKE_KEY: &str = "test-gemini-key-000";

fn client() -> HttpClient {
    HttpClient::new(&HttpSettings {
        connect_timeout: Duration::from_secs(2),
        retry: RetryPolicy {
            base: Duration::from_millis(5),
            factor: 2.0,
            cap: Duration::from_millis(20),
            max_retry_after: Duration::from_secs(60),
        },
    })
    .unwrap()
}

fn auth() -> AuthHeader {
    AuthHeader::new("x-goog-api-key", "", &Secret::new(FAKE_KEY)).unwrap()
}

fn media(len: usize) -> Vec<u8> {
    let mut rng = fastrand::Rng::with_seed(7);
    let mut v = b"\0\0\0\x18ftypmp42".to_vec();
    v.extend((v.len()..len).map(|_| rng.u8(..)));
    v
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

async fn fetch(url: &str, base: &Url, dest: &Path, idle: Duration) -> Result<Downloaded, DownloadError> {
    let auth = auth();
    let req =
        DownloadRequest { url, dest, base_url: base, auth: Some(&auth), idle_timeout: idle, provider: None };
    download(&client(), &req).await
}

fn has_key(req: &wiremock::Request) -> bool {
    req.headers.get("x-goog-api-key").is_some()
}

#[tokio::test]
async fn streams_to_the_file_and_reports_size_hash_and_type() {
    let server = MockServer::start().await;
    let body = media(3 * 1024 * 1024 + 17);
    Mock::given(method("GET"))
        .and(path("/v1beta/files/abc:download"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "video/mp4"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join(".out.mp4.iris-part-1");
    let base = Url::parse(&server.uri()).unwrap();
    let url = format!("{}/v1beta/files/abc:download?alt=media", server.uri());
    let done = fetch(&url, &base, &dest, Duration::from_secs(10)).await.unwrap();
    assert_eq!(done.bytes, body.len() as u64);
    assert_eq!(done.sha256_hex, sha256_hex(&body));
    assert_eq!(done.content_type.as_deref(), Some("video/mp4"));
    assert_eq!((done.redirects, done.attempts), (0, 1));
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    let reqs = server.received_requests().await.unwrap();
    assert!(has_key(&reqs[0]), "same-origin request carries the credential");
}

#[tokio::test]
async fn the_credential_is_dropped_on_a_cross_origin_redirect_and_restored_on_return() {
    let api = MockServer::start().await; // configured base URL origin
    let storage = MockServer::start().await; // another origin (different port)
    let body = media(4096);
    Mock::given(method("GET"))
        .and(path("/v1beta/files/abc:download"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/signed/abc.mp4?X-Goog-Signature=s1", storage.uri())),
        )
        .mount(&api)
        .await;
    Mock::given(method("GET"))
        .and(path("/signed/abc.mp4"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("location", format!("{}/final/abc.mp4", api.uri())),
        )
        .mount(&storage)
        .await;
    Mock::given(method("GET"))
        .and(path("/final/abc.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "video/mp4"))
        .mount(&api)
        .await;

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("part");
    let base = Url::parse(&format!("{}/", api.uri())).unwrap();
    let url = format!("{}/v1beta/files/abc:download?alt=media", api.uri());
    let done = fetch(&url, &base, &dest, Duration::from_secs(10)).await.unwrap();
    assert_eq!(done.redirects, 2);
    assert_eq!(done.sha256_hex, sha256_hex(&body));

    let api_reqs = api.received_requests().await.unwrap();
    let storage_reqs = storage.received_requests().await.unwrap();
    assert_eq!(api_reqs.len(), 2);
    assert_eq!(storage_reqs.len(), 1);
    assert!(api_reqs.iter().all(has_key), "hops to the base origin carry the credential");
    assert!(!has_key(&storage_reqs[0]), "the credential must never reach another origin");
}

#[tokio::test]
async fn no_credential_is_sent_when_the_url_is_not_on_the_base_origin() {
    let files = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(media(100), "video/mp4"))
        .mount(&files)
        .await;
    let dir = tempfile::tempdir().unwrap();
    // Same host, different port: a different origin.
    let base = Url::parse("http://127.0.0.1:9/").unwrap();
    fetch(&format!("{}/x.mp4", files.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
        .await
        .unwrap();
    assert!(!has_key(&files.received_requests().await.unwrap()[0]));
}

#[tokio::test]
async fn http_urls_are_refused_when_the_base_url_is_https() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let base = Url::parse("https://generativelanguage.googleapis.com").unwrap();
    let err = fetch(&format!("{}/x.mp4", server.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Refused { .. }), "{err:?}");
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        0,
        "nothing may be requested over plain http"
    );
    let e = err.into_iris();
    assert_eq!((e.code, e.retryable), (ErrorCode::DownloadFailed, Some(false)));
}

#[tokio::test]
async fn every_redirect_hop_is_checked_against_the_scheme_rule() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "ftp://files.example.com/x.mp4"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let base = Url::parse(&server.uri()).unwrap();
    let err = fetch(&format!("{}/x", server.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
        .await
        .unwrap_err();
    let DownloadError::Refused { message } = err else { panic!("expected refusal") };
    assert!(message.contains("redirect"), "{message}");
}

#[tokio::test]
async fn redirect_loops_stop_after_five_hops() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/loop"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let base = Url::parse(&server.uri()).unwrap();
    let err = fetch(&format!("{}/loop", server.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(matches!(err, DownloadError::Refused { .. }), "{err:?}");
    assert_eq!(server.received_requests().await.unwrap().len(), 6, "initial request + 5 followed redirects");
}

#[tokio::test]
async fn json_and_html_error_documents_served_as_200_are_rejected_and_not_written() {
    for (body, content_type) in [
        (
            format!(
                r#"{{"error":{{"code":403,"message":"expired, see https://x.example/y?sig=SIGVALUE","key":"{FAKE_KEY}"}}}}"#
            ),
            "application/json; charset=utf-8",
        ),
        ("<html><body>Access denied</body></html>".to_string(), "text/html"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone().into_bytes(), content_type))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("p");
        let base = Url::parse(&server.uri()).unwrap();
        let err = fetch(&format!("{}/x.mp4", server.uri()), &base, &dest, Duration::from_secs(5))
            .await
            .unwrap_err();
        let DownloadError::InvalidMedia { content_type: ct, body_snippet, .. } = &err else {
            panic!("expected invalid media, got {err:?}")
        };
        assert!(content_type.starts_with(ct.as_str()), "{ct}");
        assert!(!body_snippet.contains("SIGVALUE"), "{body_snippet}");
        assert_eq!(
            std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0),
            0,
            "no error bytes saved as media"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1, "an error document is not retried");
        assert_eq!(err.into_iris().code, ErrorCode::InvalidMedia);
    }
}

#[tokio::test]
async fn gone_artifacts_map_to_artifact_expired_without_retry() {
    for status in [403u16, 404, 410] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_string("x".repeat(100_000)))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let base = Url::parse(&server.uri()).unwrap();
        let err =
            fetch(&format!("{}/x.mp4", server.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
                .await
                .unwrap_err();
        let DownloadError::Status { status: s, body_snippet, .. } = &err else { panic!("{err:?}") };
        assert_eq!(*s, status);
        assert!(body_snippet.chars().count() <= 501, "snippet is bounded");
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let e = err.into_iris();
        assert_eq!(e.code, ErrorCode::ArtifactExpired);
        assert_eq!(e.provider_status, Some(status));
    }
}

#[tokio::test]
async fn server_errors_are_retried_and_x_should_retry_false_is_respected() {
    let server = MockServer::start().await;
    let body = media(1000);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "video/mp4"))
        .with_priority(2)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let base = Url::parse(&server.uri()).unwrap();
    let done =
        fetch(&format!("{}/x.mp4", server.uri()), &base, &dir.path().join("p"), Duration::from_secs(5))
            .await
            .unwrap();
    assert_eq!(done.attempts, 3);

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).insert_header("x-should-retry", "false"))
        .mount(&server)
        .await;
    let err = fetch(
        &format!("{}/x.mp4", server.uri()),
        &base_of(&server),
        &dir.path().join("q"),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, DownloadError::Status { status: 503, .. }));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn retry_after_beyond_the_cap_stops_immediately() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "600"))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let err = fetch(
        &format!("{}/x.mp4", server.uri()),
        &base_of(&server),
        &dir.path().join("p"),
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();
    let DownloadError::Status { status: 429, retry_after, .. } = &err else { panic!("{err:?}") };
    assert_eq!(*retry_after, Some(Duration::from_secs(600)));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let e = err.into_iris();
    assert_eq!((e.code, e.retry_after), (ErrorCode::RateLimited, Some(Duration::from_secs(600))));
}

fn base_of(server: &MockServer) -> Url {
    Url::parse(&server.uri()).unwrap()
}

/// Raw HTTP responses, one per connection: write `head`, then `body`, optionally
/// stall (keeping the connection open), then close.
struct Script {
    bytes: Vec<u8>,
    stall: Option<Duration>,
}

fn raw_server(scripts: Vec<Script>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for script in scripts {
            let Ok((stream, _)) = listener.accept() else { return };
            seen.fetch_add(1, Ordering::SeqCst);
            // One thread per connection so a stalled response does not block the retry.
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let mut stream = stream;
                let _ = stream.write_all(&script.bytes);
                let _ = stream.flush();
                if let Some(d) = script.stall {
                    std::thread::sleep(d);
                }
            });
        }
    });
    (base, count)
}

fn response(declared_len: usize, body: &[u8]) -> Vec<u8> {
    let mut v = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: video/mp4\r\ncontent-length: {declared_len}\r\nconnection: close\r\n\r\n"
    )
    .into_bytes();
    v.extend_from_slice(body);
    v
}

#[tokio::test]
async fn an_interrupted_download_restarts_into_an_empty_file() {
    let body = media(1000);
    let (base, connections) = raw_server(vec![
        Script { bytes: response(1000, &body[..400]), stall: None },
        Script { bytes: response(1000, &body), stall: None },
    ]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("p");
    std::fs::write(&dest, b"stale bytes from an earlier run").unwrap();
    let base_url = Url::parse(&base).unwrap();
    let done = fetch(&format!("{base}/x.mp4"), &base_url, &dest, Duration::from_secs(5)).await.unwrap();
    assert_eq!(done.attempts, 2);
    assert_eq!(done.bytes, 1000);
    assert_eq!(done.sha256_hex, sha256_hex(&body));
    assert_eq!(std::fs::read(&dest).unwrap(), body, "the retry must not append to the partial file");
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_stalled_body_hits_the_idle_timeout_and_is_retried() {
    let body = media(2000);
    let (base, connections) = raw_server(vec![
        Script { bytes: response(2000, &body[..100]), stall: Some(Duration::from_secs(3)) },
        Script { bytes: response(2000, &body), stall: None },
    ]);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("p");
    let base_url = Url::parse(&base).unwrap();
    let done = fetch(&format!("{base}/x.mp4"), &base_url, &dest, Duration::from_millis(300)).await.unwrap();
    assert_eq!(done.attempts, 2);
    assert_eq!(std::fs::read(&dest).unwrap(), body);
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn persistent_interruptions_fail_as_download_failed_and_leave_the_file_empty() {
    let body = media(1000);
    let scripts = (0..5).map(|_| Script { bytes: response(1000, &body[..10]), stall: None }).collect();
    let (base, connections) = raw_server(scripts);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("p");
    let base_url = Url::parse(&base).unwrap();
    let err = fetch(&format!("{base}/x.mp4"), &base_url, &dest, Duration::from_secs(5)).await.unwrap_err();
    let DownloadError::Transport(t) = &err else { panic!("{err:?}") };
    assert_eq!(t.attempts, 5);
    assert_eq!(connections.load(Ordering::SeqCst), 5);
    assert_eq!(std::fs::metadata(&dest).unwrap().len(), 0);
    let e = err.into_iris();
    assert_eq!((e.code, e.retryable), (ErrorCode::DownloadFailed, Some(true)));
}
