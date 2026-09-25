//! Size and time limits of the HTTP executor: bounded response bodies and the
//! upload allowance of request time limits. Most tests use raw 127.0.0.1 socket
//! servers, which can stream an endless body or read a request slowly; some go
//! through the Veo and image adapters, or the `iris` binary, to check how a paid
//! submission reports them. Offline; fake keys only.

mod support;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use iris::catalog::ResolvedOptions;
use iris::domain::ProviderId;
use iris::error::{ErrorCode, IrisError};
use iris::http::{
    Call, HttpClient, HttpError, HttpResponse, HttpSettings, JSON_BODY_LIMIT, MEDIA_BODY_LIMIT, RetryClass,
    RetryPolicy, Timeouts, TransportKind, Verdict,
};
use iris::providers::gemini::GeminiProvider;
use iris::providers::openai::OpenAiProvider;
use iris::providers::{
    ImageProvider, ImageRequest, InputImage, InputRole, ProviderContext, VideoProvider, VideoRequest,
};
use iris::secret::Secret;
use serde_json::{Value, json};

const KEY: &str = "test-gemini-key-000";
const OPERATION: &str = "models/veo-3.1-lite-generate-preview/operations/abc123xyz";
const MIB: usize = 1024 * 1024;

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

fn classify(resp: &HttpResponse) -> Verdict {
    Verdict::default_for(resp, Some(ProviderId::Gemini))
}

// ----- raw server ------------------------------------------------------------------

/// What the server writes after reading a request.
enum Reply {
    /// These exact bytes (head and body), then close.
    Bytes(Vec<u8>),
    /// A `200` head with `Transfer-Encoding: chunked`, then `prefix` and 64 KiB
    /// chunks of padding until `max_total` body bytes are written or the client
    /// goes away (a write fails). The body never ends properly if cut short.
    Endless { prefix: Vec<u8>, max_total: usize },
    /// `head` (which declares a length it never sends), then hold the connection
    /// open for `hold` without writing anything else.
    DeclaredOnly { head: Vec<u8>, hold: Duration },
}

/// One scripted exchange per connection. The request body is read in `read_chunk`
/// pieces with `pause` after each (a slow upload), then the reply is written.
struct Script {
    reply: Reply,
    read_chunk: usize,
    pause: Duration,
}

impl Script {
    fn reply(reply: Reply) -> Script {
        Script { reply, read_chunk: 64 * 1024, pause: Duration::ZERO }
    }
}

struct Server {
    base: String,
    connections: Arc<AtomicUsize>,
    /// Response body bytes written (for `Reply::Endless`).
    written: Arc<AtomicUsize>,
    /// Request body bytes read.
    received: Arc<AtomicUsize>,
}

fn raw_server(scripts: Vec<Script>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let written = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(AtomicUsize::new(0));
    let (conns, wrote, got) = (connections.clone(), written.clone(), received.clone());
    std::thread::spawn(move || {
        for script in scripts {
            let Ok((stream, _)) = listener.accept() else { return };
            conns.fetch_add(1, Ordering::SeqCst);
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
            let mut left = content_length;
            let mut buf = vec![0u8; script.read_chunk];
            while left > 0 {
                let want = left.min(script.read_chunk);
                if reader.read_exact(&mut buf[..want]).is_err() {
                    break;
                }
                left -= want;
                got.fetch_add(want, Ordering::SeqCst);
                std::thread::sleep(script.pause);
            }
            let mut stream = stream;
            match script.reply {
                Reply::Bytes(bytes) => {
                    let _ = stream.write_all(&bytes);
                }
                Reply::Endless { prefix, max_total } => {
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n";
                    if stream.write_all(head.as_bytes()).is_err() {
                        continue;
                    }
                    let padding = vec![b'a'; 64 * 1024];
                    let mut total = 0usize;
                    let mut piece: &[u8] = &prefix;
                    while total < max_total {
                        let chunk = [format!("{:x}\r\n", piece.len()).as_bytes(), piece, b"\r\n"].concat();
                        if stream.write_all(&chunk).is_err() {
                            break;
                        }
                        total += piece.len();
                        wrote.store(total, Ordering::SeqCst);
                        piece = &padding;
                    }
                    if total >= max_total {
                        let _ = stream.write_all(b"0\r\n\r\n");
                    }
                }
                Reply::DeclaredOnly { head, hold } => {
                    let _ = stream.write_all(&head);
                    let _ = stream.flush();
                    std::thread::sleep(hold);
                }
            }
            let _ = stream.flush();
        }
    });
    Server { base, connections, written, received }
}

fn response(status: &str, body: &[u8]) -> Vec<u8> {
    [
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
        body,
    ]
    .concat()
}

async fn get(server: &Server, call: &Call) -> Result<HttpResponse, HttpError> {
    let url = format!("{}/v1beta/models/x/operations/y", server.base);
    client().execute(call, |c| Ok(c.get(&url)), classify).await
}

async fn post(server: &Server, call: &Call, body: Vec<u8>) -> Result<HttpResponse, HttpError> {
    let url = format!("{}/v1/images/generations", server.base);
    client().execute(call, |c| Ok(c.post(&url).body(body.clone())), classify).await
}

fn read_call() -> Call {
    Call::new(RetryClass::IdempotentRead, Duration::from_secs(20)).with_provider(ProviderId::Gemini)
}

// ----- adapter fixtures --------------------------------------------------------------

fn adapter_ctx(base: &str, submit: Duration) -> ProviderContext {
    ProviderContext {
        http: client(),
        base_url: url::Url::parse(base).unwrap(),
        credential: Secret::new(KEY),
        timeouts: Timeouts {
            connect: Duration::from_secs(2),
            generate: Duration::from_secs(20),
            submit,
            poll: Duration::from_secs(20),
            download_idle: Duration::from_secs(20),
        },
    }
}

fn veo_request(reference_bytes: usize) -> VideoRequest {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(reference_bytes.max(8), 0x42);
    let references = if reference_bytes == 0 {
        Vec::new()
    } else {
        vec![InputImage {
            role: InputRole::Reference,
            path: PathBuf::from("/tmp/ref.png"),
            file_name: "ref.png".to_string(),
            media_type: "image/png".to_string(),
            bytes,
        }]
    };
    VideoRequest {
        model: "veo-3.1-lite-generate-preview".to_string(),
        prompt: "A slow aerial shot over a calm alpine lake at sunrise".to_string(),
        first_frame: None,
        last_frame: None,
        references,
        options: ResolvedOptions::new(),
    }
}

// ----- response body limits ----------------------------------------------------------------

#[test]
fn each_class_has_a_body_limit_and_calls_can_tighten_it() {
    assert_eq!(Call::new(RetryClass::IdempotentRead, Duration::from_secs(1)).max_body, JSON_BODY_LIMIT);
    assert_eq!(Call::new(RetryClass::Download, Duration::from_secs(1)).max_body, JSON_BODY_LIMIT);
    assert_eq!(Call::new(RetryClass::PaidSubmit, Duration::from_secs(1)).max_body, MEDIA_BODY_LIMIT);
    assert_eq!(JSON_BODY_LIMIT, 16 * MIB as u64);
    assert_eq!(MEDIA_BODY_LIMIT, 512 * MIB as u64);
    let call = Call::new(RetryClass::PaidSubmit, Duration::from_secs(1)).with_max_body(1024);
    assert_eq!(call.max_body, 1024);
}

/// A status read answered with an endless body stops at the limit instead of
/// buffering it all, and is not retried.
#[tokio::test]
async fn an_endless_read_answer_stops_at_the_limit_as_provider_bad_response() {
    let server = raw_server(vec![Script::reply(Reply::Endless {
        prefix: b"{\"name\":\"".to_vec(),
        max_total: 256 * MIB,
    })]);
    let err = get(&server, &read_call()).await.unwrap_err();
    let HttpError::Error(e) = err else { panic!("expected a mapped error, got {err:?}") };
    assert_eq!(e.code, ErrorCode::ProviderBadResponse, "{e:?}");
    assert_eq!(e.provider, Some(ProviderId::Gemini));
    assert_eq!(e.provider_status, Some(200));
    assert_eq!(e.details.get("limit_bytes"), Some(&json!(JSON_BODY_LIMIT)));
    assert_eq!(e.details.get("attempts"), Some(&json!(1)));
    assert!(e.details.get("declared_bytes").is_none());
    assert!(e.message.contains("16 MiB"), "{}", e.message);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1, "never retried");
    // Iris stopped reading at the limit; the server could write little more than
    // that (socket buffers) before its writes failed.
    let written = server.written.load(Ordering::SeqCst);
    assert!(written < 128 * MIB, "the server wrote {written} bytes");
}

/// A declared `Content-Length` over the limit is refused before reading the body,
/// without waiting for it.
#[tokio::test]
async fn a_declared_length_over_the_limit_is_refused_without_reading() {
    let head =
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 5000000000\r\n\r\n".to_vec();
    let server = raw_server(vec![Script::reply(Reply::DeclaredOnly { head, hold: Duration::from_secs(15) })]);
    let started = Instant::now();
    let err = get(&server, &read_call()).await.unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10), "waited for the body: {:?}", started.elapsed());
    let HttpError::Error(e) = err else { panic!("expected a mapped error, got {err:?}") };
    assert_eq!(e.code, ErrorCode::ProviderBadResponse);
    assert_eq!(e.details.get("declared_bytes"), Some(&json!(5_000_000_000u64)));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

/// A paid submission whose answer is too long was processed but its answer is
/// lost: an ambiguous outcome after sending (charge possible), never resent.
#[tokio::test]
async fn a_paid_answer_over_the_limit_is_ambiguous_and_never_resent() {
    let server = raw_server(vec![
        Script::reply(Reply::Endless { prefix: b"{\"data\":\"".to_vec(), max_total: 64 * MIB }),
        Script::reply(Reply::Bytes(response("200 OK", b"{}"))),
    ]);
    let call = Call::new(RetryClass::PaidSubmit, Duration::from_secs(20))
        .with_provider(ProviderId::OpenAi)
        .with_max_body(MIB as u64);
    let err = post(&server, &call, b"{\"prompt\":\"a lighthouse\"}".to_vec()).await.unwrap_err();
    let HttpError::Transport(t) = err else { panic!("expected a transport error, got {err:?}") };
    assert_eq!(t.kind, TransportKind::Other);
    assert!(t.after_send);
    assert_eq!(t.status, Some(200));
    assert_eq!(t.attempts, 1);
    assert!(t.charge_possible());
    assert!(t.message.contains("1 MiB"), "{}", t.message);
    let e = t.to_iris();
    assert_eq!(e.details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1, "a paid request is never resent");
}

/// Error answers are definite whatever their body: a long one is cut at 1 MiB and
/// still classified by its status.
#[tokio::test]
async fn a_long_error_body_is_cut_and_still_classified() {
    let body = vec![b'x'; 3 * MIB];
    let server = raw_server(vec![Script::reply(Reply::Bytes(response("400 Bad Request", &body)))]);
    let url = format!("{}/v1beta/models/x/operations/y", server.base);
    let mut seen = 0usize;
    let err = client()
        .execute(
            &read_call(),
            |c| Ok(c.get(&url)),
            |resp| {
                seen = resp.body.len();
                classify(resp)
            },
        )
        .await
        .unwrap_err();
    assert_eq!(seen, MIB, "the classifier sees the first 1 MiB");
    let e = err.into_iris();
    assert_eq!((e.code, e.provider_status), (ErrorCode::ProviderError, Some(400)));
}

/// Answers within the limit are read whole, however they arrive.
#[tokio::test]
async fn answers_within_the_limit_are_read_whole() {
    let server =
        raw_server(vec![Script::reply(Reply::Endless { prefix: b"x".to_vec(), max_total: 3 * MIB })]);
    let call = read_call().with_max_body(4 * MIB as u64);
    let resp = get(&server, &call).await.unwrap();
    assert_eq!(resp.body.len(), server.written.load(Ordering::SeqCst));
    assert!(resp.body.len() >= 3 * MIB);
}

/// Through the Veo adapter: an oversized answer to the paid submission is
/// `submission_uncertain` (the job may exist and be billed); one to a status read
/// is `provider_bad_response` (the job is unaffected). Neither is retried.
#[tokio::test]
async fn veo_answers_over_the_limit_are_uncertain_on_submit_and_bad_on_poll() {
    let prefix = format!("{{\"name\":\"{OPERATION}\",\"padding\":\"").into_bytes();
    let server =
        raw_server(vec![Script::reply(Reply::Endless { prefix: prefix.clone(), max_total: 256 * MIB })]);
    let ctx = adapter_ctx(&server.base, Duration::from_secs(20));
    let err = GeminiProvider::new().submit(&veo_request(0), &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{err:?}");
    assert_eq!(err.retryable, Some(false));
    assert_eq!(err.details.get("charge_possible"), Some(&json!(true)));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    assert!(
        server.written.load(Ordering::SeqCst) < 128 * MIB,
        "the submit answer is small JSON: 16 MiB limit"
    );

    let server = raw_server(vec![Script::reply(Reply::Endless { prefix, max_total: 256 * MIB })]);
    let ctx = adapter_ctx(&server.base, Duration::from_secs(20));
    let err = GeminiProvider::new().poll(OPERATION, &ctx).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::ProviderBadResponse, "{err:?}");
    assert_eq!(err.provider_status, Some(200));
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

// ----- upload allowance ----------------------------------------------------------------

/// A server that reads the request body in 32 KiB pieces, 50 ms apart (about
/// 640 KiB/s), before answering `reply`.
fn slow_reader(reply: Vec<u8>) -> Server {
    raw_server(vec![Script {
        reply: Reply::Bytes(reply),
        read_chunk: 32 * 1024,
        pause: Duration::from_millis(50),
    }])
}

/// An attempt's time limit grows with its request body: a 1 MiB body read in about
/// 1.6 s fits in a 1 s timeout plus its 4 s upload allowance, while a small body
/// the server takes as long to answer does not.
#[tokio::test]
async fn a_slow_upload_gets_time_in_proportion_to_its_body() {
    let call = Call::new(RetryClass::PaidSubmit, Duration::from_secs(1)).with_provider(ProviderId::OpenAi);
    assert_eq!(iris::http::upload_allowance(MIB as u64), Duration::from_secs(4));

    let server = slow_reader(response("200 OK", b"{}"));
    let started = Instant::now();
    let resp = post(&server, &call, vec![b'x'; MIB]).await.unwrap();
    assert_eq!(resp.status.as_u16(), 200);
    assert!(started.elapsed() > Duration::from_secs(1), "the upload was not slow: {:?}", started.elapsed());
    assert_eq!(server.received.load(Ordering::SeqCst), MIB);

    // 32 KiB earn 125 ms: the server's 1.5 s pause after reading it is too long.
    let server = raw_server(vec![Script {
        reply: Reply::Bytes(response("200 OK", b"{}")),
        read_chunk: 32 * 1024,
        pause: Duration::from_millis(1500),
    }]);
    let err = post(&server, &call, vec![b'x'; 32 * 1024]).await.unwrap_err();
    let HttpError::Transport(t) = err else { panic!("expected a timeout, got {err:?}") };
    assert_eq!((t.kind, t.after_send, t.attempts), (TransportKind::Timeout, true, 1));
}

/// Through the Veo adapter: a job with a large reference image is not cut off by
/// the 1 s submit timeout while it uploads slowly (it would otherwise become
/// `submission_uncertain`, a job that may exist but cannot be followed).
#[tokio::test]
async fn a_slow_veo_upload_is_not_cut_off_by_the_submit_timeout() {
    let server = slow_reader(response("200 OK", format!("{{\"name\":\"{OPERATION}\"}}").as_bytes()));
    let ctx = adapter_ctx(&server.base, Duration::from_secs(1));
    // 768 KiB of image, 1 MiB as base64: about 1.6 s at the server's pace.
    let op = GeminiProvider::new().submit(&veo_request(768 * 1024), &ctx).await.unwrap();
    assert_eq!(op.remote_id, OPERATION);
    assert!(server.received.load(Ordering::SeqCst) > MIB);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
}

// ----- paid image answers that cannot be read in full -------------------------------------

/// A `200` head declaring a body far over the 512 MiB image-answer limit (it is
/// refused before anything of the body is read).
fn oversized_answer() -> Reply {
    let head = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 5000000000\r\n\r\n";
    Reply::DeclaredOnly { head: head.to_vec(), hold: Duration::from_secs(5) }
}

/// A `200` head declaring 5000 bytes, then only the start of the body.
fn cut_off_answer() -> Reply {
    let head = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 5000\r\n\r\n";
    Reply::Bytes([head.as_slice(), b"{\"data\":[{\"b64_json\":\"iVBOR"].concat())
}

/// One image generation through `provider`'s adapter against `server`.
async fn generate_image(provider: ProviderId, server: &Server) -> IrisError {
    let (model, base) = match provider {
        ProviderId::OpenAi => ("gpt-image-2.5-sunburst", format!("{}/v1", server.base)),
        _ => ("gemini-3.1-flash-image", server.base.clone()),
    };
    let req = ImageRequest {
        model: model.to_string(),
        prompt: "a lighthouse at dusk, watercolor".to_string(),
        images: Vec::new(),
        mask: None,
        options: ResolvedOptions::new(),
    };
    let ctx = adapter_ctx(&base, Duration::from_secs(20));
    let result = match provider {
        ProviderId::OpenAi => OpenAiProvider::new().generate(&req, &ctx).await,
        _ => GeminiProvider::new().generate(&req, &ctx).await,
    };
    result.expect_err("expected an error")
}

/// One way a paid answer fails to arrive whole, and what the error says happened.
#[derive(Clone, Copy)]
struct Unread {
    name: &'static str,
    reply: fn() -> Reply,
    what: &'static str,
    /// The status that arrived (and is reported as `provider_status`).
    status: Option<u16>,
    /// More the message must say.
    also: Option<&'static str>,
}

/// Both image adapters say what happened to a paid call whose answer never arrived
/// whole: no answer at all is a connection that failed; an answer whose status line
/// arrived but whose body was cut off, or was longer than Iris reads, "could not be
/// read in full" (the provider answered; the connection did not fail). Each is
/// `submission_uncertain` with the status that arrived, and is never resent.
#[tokio::test]
async fn image_answers_that_never_arrive_whole_are_uncertain_and_say_what_happened() {
    let read_in_full = "the answer could not be read in full";
    let cases = [
        Unread {
            name: "no answer",
            reply: || Reply::Bytes(Vec::new()),
            what: "the connection failed after the request was sent",
            status: None,
            also: None,
        },
        Unread {
            name: "a cut-off answer",
            reply: cut_off_answer,
            what: read_in_full,
            status: Some(200),
            also: None,
        },
        Unread {
            name: "an answer over the limit",
            reply: oversized_answer,
            what: read_in_full,
            status: Some(200),
            also: Some("over Iris's 512 MiB limit"),
        },
    ];
    for provider in [ProviderId::OpenAi, ProviderId::Gemini] {
        for Unread { name, reply, what, status, also } in cases {
            let case = format!("{}, {name}", provider.display_name());
            let server = raw_server(vec![Script::reply(reply())]);
            let err = generate_image(provider, &server).await;
            assert_eq!(err.code, ErrorCode::SubmissionUncertain, "{case}: {err:?}");
            assert_eq!(err.retryable, Some(false), "{case}");
            assert_eq!(err.provider, Some(provider), "{case}");
            assert_eq!(err.provider_status, status, "{case}");
            assert_eq!(err.details.get("transport"), Some(&json!("other")), "{case}");
            assert_eq!(err.details.get("charge_possible"), Some(&json!(true)), "{case}");
            assert!(err.message.contains(&format!("({what}; ")), "{case}: {}", err.message);
            if status.is_some() {
                assert!(!err.message.contains("connection failed"), "{case}: {}", err.message);
            }
            if let Some(text) = also {
                assert!(err.message.contains(text), "{case}: {}", err.message);
            }
            assert_eq!(server.connections.load(Ordering::SeqCst), 1, "{case}: never resent");
        }
    }
}

/// End to end, as docs/json-contract.md describes it: a paid image call whose answer
/// is longer than Iris reads exits 5 with `submission_uncertain`, the 2xx status,
/// `details.transport: "other"` and `charge_possible`, no retry delay and no job.
/// The request is sent once, and nothing is saved.
#[test]
fn an_image_answer_over_the_limit_is_reported_as_the_contract_says() {
    let server = raw_server(vec![Script::reply(oversized_answer())]);
    let sb = support::Sandbox::new();
    let out = sb
        .iris()
        .keys()
        .env("IRIS_OPENAI_BASE_URL", format!("{}/v1", server.base))
        .args(["image", "generate", "a lighthouse at dusk", "--json"])
        .run();
    let v = out.err(5, "submission_uncertain");
    let e = &v["error"];
    assert_eq!(e["provider"], "openai", "{v}");
    assert_eq!(e["provider_status"], 200, "{v}");
    assert_eq!(e["retryable"], false, "{v}");
    assert_eq!(e["retry_after_seconds"], Value::Null, "{v}");
    assert_eq!(e["job_id"], Value::Null, "{v}");
    assert_eq!(e["details"]["transport"], "other", "{v}");
    assert_eq!(e["details"]["charge_possible"], true, "{v}");
    let message = e["message"].as_str().unwrap();
    assert!(message.contains("the answer could not be read in full"), "{message}");
    assert!(message.contains("over Iris's 512 MiB limit"), "{message}");
    assert_eq!(server.connections.load(Ordering::SeqCst), 1, "never resent");
    let saved = support::files_in(&sb.work());
    assert!(saved.is_empty(), "nothing is saved: {saved:?}");
}
