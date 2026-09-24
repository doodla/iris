//! 127.0.0.1 mock servers (wiremock) and provider wire fixtures for OpenAI, the
//! Gemini API, and Veo.
//!
//! Each [`MockApi`] is a fresh, non-pooled server on its own port (its own origin),
//! so a test can use two of them as the API origin and a separate file host, and
//! no request from one test can reach another test's server.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::media::{b64, mp4};

/// A running mock server with a small synchronous API (the server runs on its own
/// thread; this runtime only drives registration and request inspection).
pub struct MockApi {
    server: MockServer,
    rt: tokio::runtime::Runtime,
}

impl MockApi {
    pub fn start() -> MockApi {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let server = rt.block_on(MockServer::builder().start());
        MockApi { server, rt }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub fn mount(&self, mock: Mock) {
        self.rt.block_on(mock.mount(&self.server));
    }

    /// Answer `METHOD path` with `responder`.
    pub fn on(&self, verb: &str, route: &str, responder: impl Respond + 'static) {
        self.mount(Mock::given(method(verb)).and(path(route)).respond_with(responder));
    }

    /// Every request received so far, in order.
    pub fn requests(&self) -> Vec<Request> {
        self.rt.block_on(self.server.received_requests()).unwrap_or_default()
    }

    /// Requests received for `METHOD path`.
    pub fn hits(&self, verb: &str, route: &str) -> Vec<Request> {
        self.requests()
            .into_iter()
            .filter(|r| r.method.as_str().eq_ignore_ascii_case(verb) && r.url.path() == route)
            .collect()
    }

    pub fn count(&self, verb: &str, route: &str) -> usize {
        self.hits(verb, route).len()
    }

    pub fn total(&self) -> usize {
        self.requests().len()
    }

    /// Poll the recorded requests until `METHOD path` was received `n` times
    /// (no fixed sleeps). Panics after `timeout`.
    pub fn wait_for(&self, verb: &str, route: &str, n: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while self.count(verb, route) < n {
            assert!(
                Instant::now() < deadline,
                "{verb} {route} was not received {n} times within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// A request header as a string (`None` if absent).
pub fn header(req: &Request, name: &str) -> Option<String> {
    req.headers.get(name).map(|v| v.to_str().unwrap().to_string())
}

/// A request body parsed as JSON.
pub fn body_json(req: &Request) -> Value {
    serde_json::from_slice(&req.body).expect("request body is JSON")
}

/// Answers with its current script: the templates in turn, then the last one for
/// every further request. Tests switch the script at any time (e.g. running → done).
#[derive(Clone)]
pub struct Switch(Arc<Mutex<VecDeque<ResponseTemplate>>>);

impl Switch {
    /// Always answer `template`.
    pub fn new(template: ResponseTemplate) -> Switch {
        Switch::sequence(vec![template])
    }

    /// Answer each template in turn, then keep repeating the last.
    pub fn sequence(templates: Vec<ResponseTemplate>) -> Switch {
        assert!(!templates.is_empty());
        Switch(Arc::new(Mutex::new(templates.into())))
    }

    /// From now on, always answer `template`.
    pub fn set(&self, template: ResponseTemplate) {
        self.set_sequence(vec![template]);
    }

    /// From now on, answer each template in turn, then keep repeating the last.
    pub fn set_sequence(&self, templates: Vec<ResponseTemplate>) {
        assert!(!templates.is_empty());
        *self.0.lock().unwrap() = templates.into();
    }
}

impl Respond for Switch {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let mut queue = self.0.lock().unwrap();
        if queue.len() > 1 { queue.pop_front().unwrap() } else { queue[0].clone() }
    }
}

/// A JSON response.
pub fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.to_string().into_bytes(), "application/json")
}

// ----- OpenAI Images API ------------------------------------------------------------------

pub const OPENAI_GENERATIONS: &str = "/v1/images/generations";
pub const OPENAI_EDITS: &str = "/v1/images/edits";
pub const OPENAI_DEFAULT_MODEL: &str = "gpt-image-2.5-sunburst";

/// The usage every OpenAI success reports: 50 text input tokens, 196 output tokens.
/// Estimate: (50 × $5 + 196 × $30) / 1M = $0.00613.
pub const OPENAI_USAGE_ESTIMATE_USD: f64 = 0.00613;

/// A successful `ImagesResponse` with `images` as `b64_json` and OpenAI's request id.
pub fn openai_images(images: &[&[u8]], request_id: &str) -> ResponseTemplate {
    let data: Vec<Value> = images.iter().map(|b| json!({ "b64_json": b64(b) })).collect();
    json_response(
        200,
        json!({
            "created": 1_790_000_000,
            "data": data,
            "output_format": "png",
            "usage": {
                "input_tokens": 50,
                "input_tokens_details": { "text_tokens": 50, "image_tokens": 0 },
                "output_tokens": 196,
                "total_tokens": 246
            }
        }),
    )
    .insert_header("x-request-id", request_id)
}

/// An OpenAI error body (`{"error": {message, type, code}}`).
pub fn openai_error(status: u16, kind: &str, code: Option<&str>, message: &str) -> ResponseTemplate {
    json_response(
        status,
        json!({ "error": { "message": message, "type": kind, "param": null, "code": code } }),
    )
    .insert_header("x-request-id", "req_e2e_error")
}

// ----- Gemini API (images) ------------------------------------------------------------------

pub const GEMINI_DEFAULT_IMAGE_MODEL: &str = "gemini-3.1-flash-image";

/// `/v1/models/<model>:generateContent`.
pub fn gemini_generate_path(model: &str) -> String {
    format!("/v1/models/{model}:generateContent")
}

/// A `generateContent` answer with one candidate holding `parts`.
pub fn gemini_parts(parts: Value) -> ResponseTemplate {
    json_response(
        200,
        json!({
            "candidates": [ { "content": { "role": "model", "parts": parts }, "finishReason": "STOP", "index": 0 } ],
            "usageMetadata": {
                "promptTokenCount": 12,
                "candidatesTokenCount": 1120,
                "thoughtsTokenCount": 40,
                "totalTokenCount": 1172
            },
            "modelVersion": GEMINI_DEFAULT_IMAGE_MODEL,
            "responseId": "e2e-response-1"
        }),
    )
}

/// An `inlineData` part.
pub fn inline_part(mime: &str, bytes: &[u8]) -> Value {
    json!({ "inlineData": { "mimeType": mime, "data": b64(bytes) } })
}

/// A `google.rpc.Status` error body.
pub fn google_error(http: u16, status: &str, message: &str, details: Value) -> ResponseTemplate {
    json_response(
        http,
        json!({ "error": { "code": http, "message": message, "status": status, "details": details } }),
    )
}

// ----- Veo (v1beta predictLongRunning, operations, Files API) ---------------------------------

pub const VEO_LITE: &str = "veo-3.1-lite-generate-preview";
pub const VEO_FILE_ID: &str = "e2e-video-3k9q";
/// Signature of the signed file-host URL the Files API redirects to. It must never
/// appear in any output (printed URLs are redacted).
pub const SIGNATURE: &str = "e2e-signature-5f3c9a77d1";
/// The video the file host serves (4 seconds).
pub fn veo_video() -> Vec<u8> {
    mp4(4)
}

/// A Gemini API origin serving one Veo operation, plus a second origin (the
/// signed-URL file host the Files API download redirects to).
///
/// * `POST /v1beta/models/<VEO_LITE>:predictLongRunning` → [`VeoMock::submit`] (an
///   operation name by default);
/// * `GET /v1beta/<operation>` → [`VeoMock::operation`] (running until
///   [`VeoMock::succeed`]);
/// * `GET /v1beta/files/<id>:download` → 302 to `<file host>/bucket/<id>.mp4?…signature…`;
/// * file host `GET /bucket/<id>.mp4` → [`VeoMock::file`] (the MP4 by default).
pub struct VeoMock {
    pub api: MockApi,
    pub files: MockApi,
    pub submit: Switch,
    pub operation: Switch,
    pub file: Switch,
    pub op_name: String,
}

impl VeoMock {
    pub fn start() -> VeoMock {
        let api = MockApi::start();
        let files = MockApi::start();
        let op_name = format!("models/{VEO_LITE}/operations/e2e-op-7h2k");
        let submit = Switch::new(json_response(200, json!({ "name": op_name })));
        let operation = Switch::new(veo_running(&op_name));
        let file = Switch::new(ResponseTemplate::new(200).set_body_raw(veo_video(), "video/mp4"));
        api.on("POST", &veo_submit_path(VEO_LITE), submit.clone());
        api.on("GET", &format!("/v1beta/{op_name}"), operation.clone());
        let signed = format!(
            "{}/bucket/{VEO_FILE_ID}.mp4?X-Goog-Algorithm=GOOG4-RSA-SHA256&X-Goog-Expires=600&X-Goog-Signature={SIGNATURE}",
            files.uri()
        );
        api.on(
            "GET",
            &Self::download_path(),
            ResponseTemplate::new(302).insert_header("location", signed.as_str()),
        );
        files.on("GET", &format!("/bucket/{VEO_FILE_ID}.mp4"), file.clone());
        VeoMock { api, files, submit, operation, file, op_name }
    }

    /// `/v1beta/files/<id>:download` on the API origin.
    pub fn download_path() -> String {
        format!("/v1beta/files/{VEO_FILE_ID}:download")
    }

    /// The output URI the finished operation reports (on the API origin).
    pub fn output_uri(&self) -> String {
        format!("{}{}?alt=media", self.api.uri(), Self::download_path())
    }

    /// The operation is done with one video.
    pub fn succeed(&self) {
        self.operation.set(json_response(
            200,
            json!({
                "name": self.op_name,
                "done": true,
                "response": {
                    "@type": "type.googleapis.com/google.ai.generativelanguage.v1beta.PredictLongRunningResponse",
                    "generateVideoResponse": { "generatedSamples": [ { "video": { "uri": self.output_uri() } } ] }
                }
            }),
        ));
    }

    pub fn submits(&self) -> usize {
        self.api.count("POST", &veo_submit_path(VEO_LITE))
    }

    pub fn polls(&self) -> usize {
        self.api.count("GET", &format!("/v1beta/{}", self.op_name))
    }

    /// Download requests to the Files API on the API origin.
    pub fn api_downloads(&self) -> usize {
        self.api.count("GET", &Self::download_path())
    }

    /// Requests to the file host (the second origin).
    pub fn file_fetches(&self) -> usize {
        self.files.total()
    }
}

/// `/v1beta/models/<model>:predictLongRunning`.
pub fn veo_submit_path(model: &str) -> String {
    format!("/v1beta/models/{model}:predictLongRunning")
}

/// An operation that is still running.
pub fn veo_running(name: &str) -> ResponseTemplate {
    json_response(200, json!({ "name": name, "metadata": {} }))
}
