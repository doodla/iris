//! Retry executor behavior against localhost mock servers (see docs/contributing/architecture.md
//! "Where invariants live" for retry classes).
//! No network access beyond 127.0.0.1; no credentials.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iris::domain::ProviderId;
use iris::error::ErrorCode;
use iris::http::{
    Call, HttpClient, HttpError, HttpResponse, HttpSettings, RetryClass, RetryPolicy, TransportKind, Verdict,
};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Millisecond backoff so tests are fast; Retry-After handling keeps the 60s cap.
/// No system proxy: mock traffic to 127.0.0.1 must never leave the machine.
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

/// A failure after which a paid request may have been processed although no answer
/// arrived: a transport failure after sending.
fn sent_without_answer(err: &HttpError) -> bool {
    matches!(err, HttpError::Transport(t) if t.after_send)
}

fn call(class: RetryClass) -> Call {
    Call::new(class, Duration::from_secs(5))
        .with_request_id_header("x-request-id")
        .with_provider(ProviderId::OpenAi)
}

fn classify(resp: &HttpResponse) -> Verdict {
    Verdict::default_for(resp, Some(ProviderId::OpenAi))
}

async fn post(server_uri: &str, class: RetryClass) -> Result<HttpResponse, HttpError> {
    let url = format!("{server_uri}/v1/images/generations");
    client()
        .execute(&call(class), |c| Ok(c.post(&url).json(&json!({"prompt": "a lighthouse"}))), classify)
        .await
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

/// `n` responses with `status` (plus headers), then 200 `{"ok":true}`.
async fn failing_then_ok(server: &MockServer, status: u16, n: u64, headers: &[(&str, &str)]) {
    let mut failing = ResponseTemplate::new(status)
        .set_body_json(json!({"error": {"message": "slow down"}}))
        .insert_header("x-request-id", "req_failing_1");
    for (k, v) in headers {
        failing = failing.insert_header(*k, *v);
    }
    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .respond_with(failing)
        .up_to_n_times(n)
        .with_priority(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/images/generations"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"ok": true}))
                .insert_header("x-request-id", "req_ok_2"),
        )
        .with_priority(2)
        .mount(server)
        .await;
}

fn expect_error(result: Result<HttpResponse, HttpError>) -> iris::error::IrisError {
    match result {
        Err(HttpError::Error(e)) => e,
        other => panic!("expected a classified error, got {other:?}"),
    }
}

#[tokio::test]
async fn reads_retry_429_until_success() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 2, &[]).await;
    let resp = post(&server.uri(), RetryClass::IdempotentRead).await.unwrap();
    assert_eq!(resp.status.as_u16(), 200);
    assert_eq!(resp.attempts, 3);
    assert_eq!(resp.request_id.as_deref(), Some("req_ok_2"));
    assert_eq!(resp.json::<serde_json::Value>().unwrap(), json!({"ok": true}));
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn paid_submit_retries_a_rate_limit_rejection() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 1, &[]).await;
    let resp = post(&server.uri(), RetryClass::PaidSubmit).await.unwrap();
    assert_eq!(resp.attempts, 2);
    assert_eq!(requests(&server).await, 2);
}

#[tokio::test]
async fn paid_submit_stops_after_three_attempts_with_the_classifier_error() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 10, &[]).await;
    let err = expect_error(post(&server.uri(), RetryClass::PaidSubmit).await);
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(err.provider_status, Some(429));
    assert_eq!(err.provider, Some(ProviderId::OpenAi));
    assert_eq!(err.provider_request_id.as_deref(), Some("req_failing_1"));
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn reads_retry_500_but_paid_submit_does_not() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 500, 1, &[]).await;
    let resp = post(&server.uri(), RetryClass::IdempotentRead).await.unwrap();
    assert_eq!(resp.attempts, 2);

    let server = MockServer::start().await;
    failing_then_ok(&server, 500, 1, &[]).await;
    let err = expect_error(post(&server.uri(), RetryClass::PaidSubmit).await);
    assert_eq!(err.code, ErrorCode::ProviderError);
    assert_eq!(err.retryable, Some(true), "reported retryable for the caller to decide");
    assert_eq!(err.provider_status, Some(500));
    assert_eq!(requests(&server).await, 1, "a paid submission must never be resent after a 500");
}

#[tokio::test]
async fn reads_retry_every_transient_status() {
    for status in [408u16, 502, 503, 504] {
        let server = MockServer::start().await;
        failing_then_ok(&server, status, 1, &[]).await;
        let resp = post(&server.uri(), RetryClass::IdempotentRead).await.unwrap();
        assert_eq!(resp.attempts, 2, "status {status}");
    }
}

#[tokio::test]
async fn final_statuses_are_never_retried() {
    for status in [400u16, 401, 403, 404, 409, 413, 422] {
        let server = MockServer::start().await;
        failing_then_ok(&server, status, 1, &[]).await;
        let err = expect_error(post(&server.uri(), RetryClass::IdempotentRead).await);
        assert_eq!(err.provider_status, Some(status));
        assert_eq!(requests(&server).await, 1, "status {status}");
    }
}

#[tokio::test]
async fn x_should_retry_false_stops_retries() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 5, &[("x-should-retry", "false")]).await;
    let err = expect_error(post(&server.uri(), RetryClass::IdempotentRead).await);
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn classifier_can_mark_a_429_final_for_quota_exhaustion() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 5, &[]).await;
    let url = format!("{}/v1/images/generations", server.uri());
    let err = expect_error(
        client()
            .execute(
                &call(RetryClass::PaidSubmit),
                |c| Ok(c.post(&url)),
                |_resp| {
                    Verdict::Final(iris::error::IrisError::new(
                        ErrorCode::QuotaExceeded,
                        "insufficient_quota",
                    ))
                },
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::QuotaExceeded);
    assert_eq!(err.provider_status, Some(429), "the executor enriches the classifier's error");
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn retry_after_seconds_is_honored() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 1, &[("retry-after", "1")]).await;
    let started = Instant::now();
    let resp = post(&server.uri(), RetryClass::PaidSubmit).await.unwrap();
    assert_eq!(resp.attempts, 2);
    assert!(started.elapsed() >= Duration::from_millis(950), "waited only {:?}", started.elapsed());
}

#[tokio::test]
async fn retry_after_http_date_is_honored() {
    let server = MockServer::start().await;
    let when = jiff::Timestamp::now() + jiff::SignedDuration::from_secs(2);
    let date = jiff::fmt::rfc2822::DateTimePrinter::new().timestamp_to_rfc9110_string(&when).unwrap();
    failing_then_ok(&server, 503, 1, &[("retry-after", &date)]).await;
    let started = Instant::now();
    let resp = post(&server.uri(), RetryClass::IdempotentRead).await.unwrap();
    assert_eq!(resp.attempts, 2);
    // HTTP dates have one-second resolution: the wait is between 1s and 2s.
    assert!(started.elapsed() >= Duration::from_millis(900), "waited only {:?}", started.elapsed());
}

#[tokio::test]
async fn retry_after_ms_is_honored() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 1, &[("retry-after-ms", "300")]).await;
    let started = Instant::now();
    post(&server.uri(), RetryClass::IdempotentRead).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(290), "waited only {:?}", started.elapsed());
}

#[tokio::test]
async fn retry_after_beyond_the_cap_stops_with_rate_limited() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 5, &[("retry-after", "120")]).await;
    let started = Instant::now();
    let err = expect_error(post(&server.uri(), RetryClass::IdempotentRead).await);
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::from_secs(120)));
    assert_eq!(err.retryable, Some(true));
    assert_eq!(requests(&server).await, 1);
    assert!(started.elapsed() < Duration::from_secs(5));

    // Same for an HTTP date an hour away, on a transient status (converted to rate_limited).
    let server = MockServer::start().await;
    let when = jiff::Timestamp::now() + jiff::SignedDuration::from_hours(1);
    let date = jiff::fmt::rfc2822::DateTimePrinter::new().timestamp_to_rfc9110_string(&when).unwrap();
    failing_then_ok(&server, 503, 5, &[("retry-after", &date)]).await;
    let err = expect_error(post(&server.uri(), RetryClass::IdempotentRead).await);
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert!(err.retry_after.unwrap() > Duration::from_secs(3500));
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn provider_supplied_delay_is_honored_and_capped() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 1, &[]).await;
    let url = format!("{}/v1/images/generations", server.uri());
    let with_delay = |d: Duration| {
        move |resp: &HttpResponse| Verdict::RetryableRejection {
            error: resp.fallback_error(Some(ProviderId::Gemini)),
            retry_after: Some(d),
        }
    };
    let started = Instant::now();
    let resp = client()
        .execute(
            &call(RetryClass::IdempotentRead),
            |c| Ok(c.post(&url)),
            with_delay(Duration::from_millis(250)),
        )
        .await
        .unwrap();
    assert_eq!(resp.attempts, 2);
    assert!(started.elapsed() >= Duration::from_millis(240));

    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 5, &[]).await;
    let url = format!("{}/v1/images/generations", server.uri());
    let err = expect_error(
        client()
            .execute(
                &call(RetryClass::IdempotentRead),
                |c| Ok(c.post(&url)),
                with_delay(Duration::from_secs(90)),
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::RateLimited);
    assert_eq!(err.retry_after, Some(Duration::from_secs(90)));
    assert_eq!(requests(&server).await, 1);
}

/// A `Retry-After` is kept only on an error that retrying could help: never on a
/// final verdict or an error marked not retryable, whoever set the delay.
#[tokio::test]
async fn retry_after_is_kept_only_on_errors_worth_retrying() {
    let run = |status: u16, class: RetryClass, verdict: fn(&HttpResponse) -> Verdict| async move {
        let server = MockServer::start().await;
        failing_then_ok(&server, status, 5, &[("retry-after", "30")]).await;
        let url = format!("{}/v1/images/generations", server.uri());
        let err = expect_error(client().execute(&call(class), |c| Ok(c.post(&url)), verdict).await);
        (err, requests(&server).await)
    };

    // A final verdict: a quota that will not clear by waiting, an uncertain paid
    // submission. The classifier's own delay is dropped too.
    let (err, sent) = run(429, RetryClass::PaidSubmit, |_| {
        Verdict::Final(
            iris::error::IrisError::new(ErrorCode::QuotaExceeded, "daily quota used up")
                .with_retry_after(Duration::from_secs(20)),
        )
    })
    .await;
    assert_eq!((err.code, err.retry_after, sent), (ErrorCode::QuotaExceeded, None, 1));
    let (err, sent) = run(503, RetryClass::PaidSubmit, |_| {
        Verdict::Final(
            iris::error::IrisError::new(ErrorCode::SubmissionUncertain, "the job may exist")
                .with_detail("charge_possible", true),
        )
    })
    .await;
    assert_eq!(
        (err.code, err.retry_after, err.retryable, sent),
        (ErrorCode::SubmissionUncertain, None, Some(false), 1)
    );

    // A transient verdict whose error is not retryable, returned at once for a paid call.
    let (err, sent) = run(503, RetryClass::PaidSubmit, |resp| Verdict::Transient {
        error: resp.fallback_error(Some(ProviderId::OpenAi)).with_retryable(Some(false)),
        retry_after: Some(Duration::from_secs(5)),
    })
    .await;
    assert_eq!((err.retry_after, sent), (None, 1));

    // A retryable error keeps the delay: a Gemini 5xx the caller may run again, or
    // a rate limit that outlasted the attempts.
    let (err, sent) = run(503, RetryClass::PaidSubmit, |resp| Verdict::Transient {
        error: resp.fallback_error(Some(ProviderId::Gemini)),
        retry_after: None,
    })
    .await;
    assert_eq!((err.code, err.retryable, sent), (ErrorCode::ProviderError, Some(true), 1));
    assert_eq!(err.retry_after, Some(Duration::from_secs(30)));
    let server = MockServer::start().await;
    failing_then_ok(&server, 429, 5, &[("retry-after", "0")]).await;
    let err = expect_error(post(&server.uri(), RetryClass::PaidSubmit).await);
    assert_eq!((err.code, err.retry_after), (ErrorCode::RateLimited, Some(Duration::ZERO)));
    assert_eq!(requests(&server).await, 3);
}

#[tokio::test]
async fn the_request_is_rebuilt_for_every_attempt() {
    let server = MockServer::start().await;
    failing_then_ok(&server, 503, 2, &[]).await;
    let url = format!("{}/v1/images/generations", server.uri());
    let builds = AtomicUsize::new(0);
    let resp = client()
        .execute(
            &call(RetryClass::IdempotentRead),
            |c| {
                builds.fetch_add(1, Ordering::SeqCst);
                let form = reqwest::multipart::Form::new().text("prompt", "a lighthouse");
                Ok(c.post(&url).multipart(form))
            },
            classify,
        )
        .await
        .unwrap();
    assert_eq!(resp.attempts, 3);
    assert_eq!(builds.load(Ordering::SeqCst), 3);
    for req in server.received_requests().await.unwrap() {
        assert!(
            String::from_utf8_lossy(&req.body).contains("a lighthouse"),
            "every attempt carries the full body"
        );
    }
}

#[tokio::test]
async fn a_build_error_is_returned_without_sending() {
    let err = expect_error(
        client()
            .execute(
                &call(RetryClass::PaidSubmit),
                |_c| Err(iris::error::IrisError::invalid("bad input")),
                classify,
            )
            .await,
    );
    assert_eq!(err.code, ErrorCode::InvalidArgument);

    // An unparseable URL fails while building: a local internal error, nothing sent.
    let err = client().execute(&call(RetryClass::PaidSubmit), |c| Ok(c.post("not a url")), classify).await;
    assert_eq!(expect_error(err).code, ErrorCode::InternalError);

    // A scheme reqwest refuses fails at send time: a deterministic local error,
    // reported as internal (not "check your network"), never sent, never retried.
    for class in [RetryClass::PaidSubmit, RetryClass::IdempotentRead] {
        let builds = AtomicUsize::new(0);
        let err = client()
            .execute(
                &call(class),
                |c| {
                    builds.fetch_add(1, Ordering::SeqCst);
                    Ok(c.post("ftp://127.0.0.1/x"))
                },
                classify,
            )
            .await
            .unwrap_err();
        assert!(!sent_without_answer(&err), "{err:?}");
        let e = err.into_iris();
        assert_eq!(e.code, ErrorCode::InternalError, "{class}");
        assert!(e.details.get("charge_possible").is_none(), "{class}");
        assert_eq!(builds.load(Ordering::SeqCst), 1, "{class}: a local error is not retried");
    }
}

#[tokio::test]
async fn redirects_go_to_the_classifier_and_requests_carry_the_iris_user_agent() {
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/v1/images/generations", second.uri())),
        )
        .mount(&first)
        .await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&second).await;
    let url = format!("{}/v1/images/generations", first.uri());
    let seen = Mutex::new(Vec::new());
    let err = expect_error(
        client()
            .execute(
                &call(RetryClass::IdempotentRead),
                |c| Ok(c.post(&url).header("x-goog-api-key", "test-gemini-key-000")),
                |r| {
                    seen.lock().unwrap().push(r.status.as_u16());
                    classify(r)
                },
            )
            .await,
    );
    assert_eq!(*seen.lock().unwrap(), vec![302], "the 3xx is handed to the classifier");
    assert_eq!(err.provider_status, Some(302));
    assert_eq!(requests(&second).await, 0, "the executor must never follow a redirect");
    let received = first.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    let ua = received[0].headers.get("user-agent").and_then(|v| v.to_str().ok());
    assert_eq!(ua, Some(format!("iris/{}", env!("CARGO_PKG_VERSION")).as_str()));
}

#[tokio::test]
async fn a_client_not_built_by_iris_is_refused_before_building_or_sending() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    // reqwest's default redirect policy would follow redirects and keep custom
    // credential headers on cross-origin hops.
    let foreign = HttpClient::from_reqwest(reqwest::Client::builder().no_proxy().build().unwrap());
    assert!(!foreign.follows_redirects_manually());
    assert!(client().follows_redirects_manually());
    let url = format!("{}/v1/images/generations", server.uri());
    let builds = AtomicUsize::new(0);
    let err = foreign
        .execute(
            &call(RetryClass::PaidSubmit),
            |c| {
                builds.fetch_add(1, Ordering::SeqCst);
                Ok(c.post(&url))
            },
            classify,
        )
        .await;
    assert_eq!(expect_error(err).code, ErrorCode::InternalError);
    assert_eq!(builds.load(Ordering::SeqCst), 0);
    assert_eq!(requests(&server).await, 0);
}

/// The origin of a port nothing listens on. Port 9 (discard) lies below every OS's
/// ephemeral port range, so no mock server started by a test running in parallel can
/// be assigned it (a bound-then-released ephemeral port could be reused by one, and
/// the paid requests of the refused-connection test would then land in its mock).
const DEAD_URL: &str = "http://127.0.0.1:9";

#[tokio::test]
async fn connection_refused_is_retried_for_paid_submit_and_reported_as_not_sent() {
    let err = post(DEAD_URL, RetryClass::PaidSubmit).await.unwrap_err();
    assert!(!sent_without_answer(&err));
    let HttpError::Transport(t) = err else { panic!("expected a transport error") };
    assert_eq!(t.kind, TransportKind::Connect);
    assert!(!t.after_send);
    assert_eq!(t.attempts, 3, "connect failures happen before sending, so PaidSubmit retries them");
    let e = t.to_iris();
    assert_eq!(e.code, ErrorCode::NetworkError);
    assert_eq!(e.details.get("charge_possible"), Some(&json!(false)));
    assert!(!e.message.contains("?"), "{}", e.message);
}

#[tokio::test]
async fn a_timeout_after_sending_is_not_retried_for_paid_submit_and_flags_charge_possible() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let url = format!("{}/v1/images/generations", server.uri());
    let short =
        Call::new(RetryClass::PaidSubmit, Duration::from_millis(300)).with_provider(ProviderId::OpenAi);
    let err = client().execute(&short, |c| Ok(c.post(&url).body("{}")), classify).await.unwrap_err();
    assert!(sent_without_answer(&err));
    let HttpError::Transport(t) = err.clone() else { panic!("expected a transport error") };
    assert_eq!(t.kind, TransportKind::Timeout);
    assert!(t.after_send);
    assert_eq!(t.attempts, 1);
    assert!(t.charge_possible());
    let e = err.into_iris();
    assert_eq!(e.code, ErrorCode::RequestTimeout);
    assert_eq!(e.details.get("charge_possible"), Some(&json!(true)));
    assert!(e.hint.as_deref().unwrap_or_default().contains("did not retry"));
    assert_eq!(requests(&server).await, 1);
}

#[tokio::test]
async fn timeouts_are_retried_for_reads() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let url = format!("{}/v1/operations/x", server.uri());
    let short = Call::new(RetryClass::IdempotentRead, Duration::from_millis(150));
    let err = client().execute(&short, |c| Ok(c.get(&url)), classify).await.unwrap_err();
    let HttpError::Transport(t) = err else { panic!("expected a transport error") };
    assert_eq!(t.kind, TransportKind::Timeout);
    assert_eq!(t.attempts, 5);
    assert!(!t.charge_possible());
    assert_eq!(requests(&server).await, 5);
}

/// One scripted raw HTTP exchange per connection: read the request (headers and
/// `Content-Length` body), write `response`, close.
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
async fn a_truncated_success_body_is_ambiguous_for_paid_submit() {
    let truncated =
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"data\":"
            .to_vec();
    let (base, connections) = raw_server(vec![truncated]);
    let err = post(&base, RetryClass::PaidSubmit).await.unwrap_err();
    let HttpError::Transport(t) = err else { panic!("expected a transport error") };
    assert_eq!(t.kind, TransportKind::Other);
    assert!(t.after_send);
    assert_eq!(t.status, Some(200));
    assert_eq!(t.attempts, 1);
    assert_eq!(t.to_iris().details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_truncated_body_is_retried_for_reads() {
    let truncated = b"HTTP/1.1 200 OK\r\ncontent-length: 100\r\n\r\npartial".to_vec();
    let ok = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok".to_vec();
    let (base, connections) = raw_server(vec![truncated, ok]);
    let url = format!("{base}/v1/operations/x");
    let resp =
        client().execute(&call(RetryClass::IdempotentRead), |c| Ok(c.get(&url)), classify).await.unwrap();
    assert_eq!(&resp.body[..], b"ok");
    assert_eq!(resp.attempts, 2);
    assert_eq!(connections.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fallback_errors_carry_a_scrubbed_bounded_provider_message() {
    let server = MockServer::start().await;
    let long = format!("see https://files.example.com/x?sig=SECRETSIG {}", "y".repeat(2000));
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400).set_body_string(long).insert_header("x-request-id", "bad id!"),
        )
        .mount(&server)
        .await;
    let err = expect_error(post(&server.uri(), RetryClass::PaidSubmit).await);
    assert_eq!(err.code, ErrorCode::ProviderError);
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.provider_request_id, None, "malformed request ids are dropped");
    let msg = err.details.get("provider_message").and_then(|v| v.as_str()).unwrap();
    assert!(!msg.contains("SECRETSIG"), "{msg}");
    assert!(msg.chars().count() <= 501, "{}", msg.len());
}
