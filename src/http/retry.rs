//! Operation-aware retry executor (C-04 retry classes, D-05 paid-submit policy).
//!
//! The executor owns *when* to retry; the calling adapter owns *what a response
//! means* (it knows the provider's error bodies) and says so through a [`Verdict`].
//!
//! # Paid submissions: what the adapter still has to decide
//!
//! For [`RetryClass::PaidSubmit`] the executor never resends after a possible
//! delivery, but it cannot know how each operation must *report* an uncertain
//! outcome. Two different failures can leave a paid request in an unknown state:
//!
//! * no usable response arrived after the request may have been sent (timeout,
//!   reset, truncated body): [`HttpError::is_ambiguous`] is true;
//! * the provider answered 408 or 5xx, which does not prove the request was not
//!   processed. [`Verdict::default_for`] classifies these as [`Verdict::Transient`],
//!   and the executor then returns the classifier's error (`provider_error`,
//!   retryable) *as an [`HttpError::Error`]*, where `is_ambiguous()` is false.
//!
//! Synchronous image calls report both as documented by D-05 (`request_timeout` +
//! `charge_possible`, or `provider_error` retryable). Video submissions (D-08) must
//! treat both as `submission_uncertain` so the job is recorded as
//! `submission_unknown` and never resubmitted: the Veo submit classifier returns
//! `Verdict::Final(<submission_uncertain error>)` for 408 and every 5xx (never
//! `Transient`), except a provider-documented "not processed" overload rejection,
//! which stays a [`Verdict::RetryableRejection`]; and the adapter maps
//! `Err(e) if e.is_ambiguous()` to `submission_uncertain` too. For example:
//!
//! ```
//! # use iris::http::{HttpResponse, Verdict};
//! # use iris::domain::ProviderId;
//! # use iris::error::{ErrorCode, IrisError};
//! fn classify_veo_submit(r: &HttpResponse) -> Verdict {
//!     if r.status.as_u16() == 408 || r.status.is_server_error() {
//!         // The operation may exist and be billed (D-08): never "failed".
//!         return Verdict::Final(IrisError::new(
//!             ErrorCode::SubmissionUncertain,
//!             "the provider may have accepted this video job",
//!         ));
//!     }
//!     Verdict::default_for(r, Some(ProviderId::Gemini))
//! }
//! ```

use std::fmt;
use std::time::{Duration, Instant};

use backon::BackoffBuilder;
use bytes::Bytes;
use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;

use super::HttpClient;
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::redact;

/// Maximum characters of provider text kept in messages and details (C-03).
pub(crate) const PROVIDER_TEXT_MAX: usize = 500;

/// How a call may be retried (C-04 "Retry classes").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetryClass {
    /// Paid, non-idempotent submission (image generate/edit, video submit). Retried
    /// only when the request provably was not processed: a connection failure before
    /// sending, or a [`Verdict::RetryableRejection`]. At most 3 attempts. See the
    /// module documentation for how video submissions report 408/5xx answers.
    PaidSubmit,
    /// Idempotent read (poll, model metadata). Retries connect errors, timeouts,
    /// body read errors, [`Verdict::RetryableRejection`] and [`Verdict::Transient`].
    /// At most 5 attempts.
    IdempotentRead,
    /// Artifact download: same rules as `IdempotentRead`; each attempt restarts the
    /// stream into an emptied file. At most 5 attempts.
    Download,
}

impl RetryClass {
    /// Total attempts allowed, including the first.
    pub const fn max_attempts(self) -> u32 {
        match self {
            RetryClass::PaidSubmit => 3,
            RetryClass::IdempotentRead | RetryClass::Download => 5,
        }
    }

    /// Stable lowercase name (used in logs and error details).
    pub const fn as_str(self) -> &'static str {
        match self {
            RetryClass::PaidSubmit => "paid_submit",
            RetryClass::IdempotentRead => "idempotent_read",
            RetryClass::Download => "download",
        }
    }

    /// Whether a transport failure may be retried under this class. Paid submissions
    /// retry only failures that happened before the request could be sent.
    pub fn retries_transport(self, kind: TransportKind, after_send: bool) -> bool {
        match self {
            RetryClass::PaidSubmit => kind == TransportKind::Connect && !after_send,
            RetryClass::IdempotentRead | RetryClass::Download => true,
        }
    }

    /// Whether a non-success response with this verdict may be retried under this class.
    pub fn retries_verdict(self, verdict: &Verdict) -> bool {
        match verdict {
            Verdict::RetryableRejection { .. } => true,
            Verdict::Transient { .. } => self != RetryClass::PaidSubmit,
            Verdict::Final(_) => false,
        }
    }
}

impl fmt::Display for RetryClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Backoff schedule: bounded exponential backoff with full jitter.
///
/// The un-jittered ceiling for retry `n` (0-based) is `min(cap, base * factor^n)`,
/// computed with `backon`'s exponential schedule; the actual delay is uniform in
/// `[0, ceiling]` ("full jitter"). backon's own jitter is additive (`[d, 2d)`), which
/// would exceed the cap, so Iris applies full jitter itself.
#[derive(Debug, Clone, PartialEq)]
pub struct RetryPolicy {
    /// First backoff ceiling (C-04: 1s).
    pub base: Duration,
    /// Growth factor per retry (C-04: 2).
    pub factor: f32,
    /// Maximum backoff ceiling (C-04: 30s).
    pub cap: Duration,
    /// Longest provider-requested delay (`Retry-After`, `RetryInfo`) Iris waits
    /// automatically (C-04: 60s). Longer requests stop retrying with `rate_limited`.
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            base: Duration::from_secs(1),
            factor: 2.0,
            cap: Duration::from_secs(30),
            max_retry_after: Duration::from_secs(60),
        }
    }
}

impl RetryPolicy {
    /// A fresh un-jittered schedule (`base`, `base*factor`, … capped at `cap`).
    pub(crate) fn schedule(&self) -> backon::ExponentialBackoff {
        backon::ExponentialBuilder::new()
            .with_min_delay(self.base)
            .with_factor(self.factor)
            .with_max_delay(self.cap)
            .without_max_times()
            .build()
    }

    /// Un-jittered ceiling of the delay before retry number `retry` (0-based).
    pub fn backoff_ceiling(&self, retry: u32) -> Duration {
        self.schedule().nth(retry as usize).unwrap_or(self.cap).min(self.cap)
    }

    /// A full-jitter delay before retry number `retry`: uniform in `[0, ceiling]`.
    pub fn backoff_delay(&self, retry: u32) -> Duration {
        full_jitter(self.backoff_ceiling(retry))
    }
}

pub(crate) fn full_jitter(ceiling: Duration) -> Duration {
    ceiling.mul_f64(fastrand::f64())
}

/// The caller's interpretation of one non-success HTTP response.
#[derive(Debug, Clone)]
pub enum Verdict {
    /// The provider definitely did not process the request and asks to try again:
    /// HTTP 429 *rate* limits (never quota/billing exhaustion) and documented
    /// overload rejections such as OpenAI 503 `server_is_overloaded` (D-05).
    /// Retried by every class. `error` is returned if retries run out.
    /// `retry_after` is a provider-body delay (e.g. Google `RetryInfo.retryDelay`);
    /// `Retry-After`/`retry-after-ms` headers are read by the executor itself.
    RetryableRejection {
        /// Returned (enriched) when retries run out or are not allowed.
        error: IrisError,
        /// Delay the provider asked for in the response body, if any.
        retry_after: Option<Duration>,
    },
    /// A transient server-side failure where the request may have been processed
    /// (408, 500, 502, 503, 504). Retried by `IdempotentRead`/`Download`; returned
    /// immediately for `PaidSubmit` as [`HttpError::Error`] carrying `error`.
    /// A video submission classifier must not return this: it returns
    /// `Final(submission_uncertain)` instead (module docs, D-08).
    Transient {
        /// Returned (enriched) when retries run out or are not allowed.
        error: IrisError,
        /// Delay the provider asked for in the response body, if any.
        retry_after: Option<Duration>,
    },
    /// Not retryable (400, 401, 403, 404, 409, 413, 422, quota, content blocks, …).
    Final(IrisError),
}

impl Verdict {
    /// The C-04 default for a status: 429 → `RetryableRejection`,
    /// 408/500/502/503/504 → `Transient`, everything else → `Final`.
    ///
    /// Adapters must check quota/billing codes before calling this: a 429 that
    /// means "quota exhausted" is `Final`.
    pub fn for_status(status: StatusCode, error: IrisError) -> Verdict {
        match status.as_u16() {
            429 => Verdict::RetryableRejection { error, retry_after: None },
            408 | 500 | 502 | 503 | 504 => Verdict::Transient { error, retry_after: None },
            _ => Verdict::Final(error),
        }
    }

    /// [`Verdict::for_status`] with [`HttpResponse::fallback_error`]: a classifier for
    /// endpoints whose error bodies need no provider-specific interpretation.
    pub fn default_for(resp: &HttpResponse, provider: Option<ProviderId>) -> Verdict {
        Verdict::for_status(resp.status, resp.fallback_error(provider))
    }

    /// The error carried by this verdict.
    pub fn error(&self) -> &IrisError {
        match self {
            Verdict::RetryableRejection { error, .. } | Verdict::Transient { error, .. } => error,
            Verdict::Final(error) => error,
        }
    }

    fn into_parts(self) -> (IrisError, Option<Duration>) {
        match self {
            Verdict::RetryableRejection { error, retry_after }
            | Verdict::Transient { error, retry_after } => (error, retry_after),
            Verdict::Final(error) => (error, None),
        }
    }
}

/// Per-call parameters for [`HttpClient::execute`].
#[derive(Debug, Clone, Copy)]
pub struct Call {
    /// Retry rules for this call.
    pub class: RetryClass,
    /// Total time limit of one attempt, from connecting until the body is read
    /// (`Timeouts::generate` / `submit` / `poll`).
    pub timeout: Duration,
    /// Response header carrying the provider's request id (e.g. `x-request-id`).
    pub request_id_header: Option<&'static str>,
    /// Provider attached to errors produced by the executor.
    pub provider: Option<ProviderId>,
}

impl Call {
    /// A call with retry class `class` and per-attempt time limit `timeout`, no
    /// request id header, and no provider.
    pub fn new(class: RetryClass, timeout: Duration) -> Self {
        Call { class, timeout, request_id_header: None, provider: None }
    }

    /// Capture the provider request id from response header `header` (sanitized
    /// with [`sanitize_request_id`]).
    pub fn with_request_id_header(mut self, header: &'static str) -> Self {
        self.request_id_header = Some(header);
        self
    }

    /// Attach `provider` to errors the executor produces or enriches.
    pub fn with_provider(mut self, provider: ProviderId) -> Self {
        self.provider = Some(provider);
        self
    }
}

/// A complete HTTP response (body fully read).
#[derive(Clone)]
pub struct HttpResponse {
    /// HTTP status.
    pub status: StatusCode,
    /// Response headers (never logged).
    pub headers: HeaderMap,
    /// The complete body (never logged).
    pub body: Bytes,
    /// Sanitized provider request id from [`Call::request_id_header`].
    pub request_id: Option<String>,
    /// Attempts made, including the one that produced this response.
    pub attempts: u32,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Bodies can hold megabytes of media or echo request content; never dump them.
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("request_id", &self.request_id)
            .field("attempts", &self.attempts)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl HttpResponse {
    /// Deserialize the body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// A response header as text, if present and valid UTF-8.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    /// The body as display-safe text: lossy UTF-8, URLs redacted, secrets scrubbed,
    /// truncated to `max_chars`.
    pub fn body_snippet(&self, max_chars: usize) -> String {
        safe_snippet(&self.body, max_chars)
    }

    /// A generic mapping of this (non-success) response to an [`IrisError`], for
    /// adapters that have nothing more specific to say:
    /// 401 → `authentication_failed`, 403 → `permission_denied`, 429 → `rate_limited`,
    /// 5xx/408 → `provider_error` (retryable), other 4xx → `provider_error`
    /// (not retryable). The scrubbed body excerpt goes to `details.provider_message`.
    pub fn fallback_error(&self, provider: Option<ProviderId>) -> IrisError {
        let status = self.status.as_u16();
        let who = provider.map(|p| p.display_name()).unwrap_or("the server");
        let (code, retryable, message) = match status {
            401 => (
                ErrorCode::AuthenticationFailed,
                Some(false),
                format!("{who} rejected the credentials (HTTP 401)"),
            ),
            403 => (ErrorCode::PermissionDenied, Some(false), format!("{who} denied access (HTTP 403)")),
            429 => {
                (ErrorCode::RateLimited, Some(true), format!("{who} is rate limiting requests (HTTP 429)"))
            }
            408 | 500..=599 => (
                ErrorCode::ProviderError,
                Some(true),
                format!("{who} returned a server error (HTTP {status})"),
            ),
            400..=499 => {
                (ErrorCode::ProviderError, Some(false), format!("{who} rejected the request (HTTP {status})"))
            }
            _ => (ErrorCode::ProviderError, None, format!("{who} returned an unexpected HTTP {status}")),
        };
        let mut err = IrisError::new(code, message)
            .with_retryable(retryable)
            .with_provider_status(status)
            .with_provider_request_id(self.request_id.clone());
        if let Some(p) = provider {
            err = err.with_provider(p);
        }
        if status == 401
            && let Some(p) = provider
        {
            err = err.with_hint(format!("check that {} holds a valid key", p.credential_env()));
        }
        let snippet = self.body_snippet(PROVIDER_TEXT_MAX);
        if !snippet.trim().is_empty() {
            err = err.with_detail("provider_message", snippet);
        }
        err
    }
}

/// Which way a request failed without producing a usable response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// The connection could not be established; the request was not sent.
    Connect,
    /// The time limit expired (the request may have been sent).
    Timeout,
    /// Any other transport or body error (reset, protocol error, truncated body).
    Other,
}

impl TransportKind {
    /// Stable lowercase name (`connect`, `timeout`, `other`), used in logs and in
    /// `details.transport`.
    pub const fn as_str(self) -> &'static str {
        match self {
            TransportKind::Connect => "connect",
            TransportKind::Timeout => "timeout",
            TransportKind::Other => "other",
        }
    }
}

/// No usable response was received.
#[derive(Debug, Clone)]
pub struct TransportError {
    /// How the attempt failed.
    pub kind: TransportKind,
    /// True when the request may have reached the provider (everything except a
    /// failed connection). For a paid submission this is the uncertainty window.
    pub after_send: bool,
    /// Status of the response, if its headers arrived before the body failed.
    pub status: Option<u16>,
    /// Attempts made, including the failed one.
    pub attempts: u32,
    /// Retry class of the call.
    pub class: RetryClass,
    /// Provider of the call, if known.
    pub provider: Option<ProviderId>,
    /// Redacted URL of the failed request.
    pub url: String,
    /// Sanitized description (no URLs with query values, no secrets).
    pub message: String,
}

impl TransportError {
    /// True when a paid request may have been accepted (and billed) by the provider.
    pub fn charge_possible(&self) -> bool {
        self.class == RetryClass::PaidSubmit && self.after_send
    }

    /// Map to the public taxonomy: `network_error` for connect/other failures,
    /// `request_timeout` for timeouts; paid submissions carry
    /// `details.charge_possible` and a hint that Iris did not retry automatically.
    /// Video adapters turn `after_send` failures into `submission_uncertain` instead.
    pub fn to_iris(&self) -> IrisError {
        let attempts =
            if self.attempts > 1 { format!(" (after {} attempts)", self.attempts) } else { String::new() };
        let (code, message) = match self.kind {
            TransportKind::Connect => (
                ErrorCode::NetworkError,
                format!("could not connect to {}{attempts}: {}", self.url, self.message),
            ),
            TransportKind::Timeout => (
                ErrorCode::RequestTimeout,
                format!(
                    "no complete response from {} within the time limit{attempts}: {}",
                    self.url, self.message
                ),
            ),
            TransportKind::Other => (
                ErrorCode::NetworkError,
                format!("the connection to {} failed{attempts}: {}", self.url, self.message),
            ),
        };
        let mut err = IrisError::new(code, message)
            .with_detail("transport", self.kind.as_str())
            .with_detail("attempts", self.attempts);
        if let Some(p) = self.provider {
            err = err.with_provider(p);
        }
        if let Some(status) = self.status {
            err = err.with_provider_status(status);
        }
        if self.class == RetryClass::PaidSubmit {
            err = err.with_detail("charge_possible", self.charge_possible());
            if self.charge_possible() {
                err = err.with_hint(
                    "Iris did not retry automatically because the provider may already have processed \
                     (and billed) this request; check your provider usage before running it again",
                );
            } else {
                err = err.with_hint("nothing was sent; check network connectivity and proxy settings");
            }
        }
        err
    }
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} error for {}: {}", self.kind.as_str(), self.url, self.message)
    }
}

/// Failure of [`HttpClient::execute`].
#[derive(Debug, Clone)]
pub enum HttpError {
    /// A definite outcome already mapped to an [`IrisError`]: the classifier's error
    /// for a non-success response that was not (or no longer) retried — enriched
    /// with status, request id, provider, and retry delay — or a local error while
    /// building the request.
    Error(IrisError),
    /// No usable response was received. Check [`TransportError::after_send`] (or
    /// [`HttpError::is_ambiguous`]) before deciding how to report a paid call.
    Transport(TransportError),
}

impl HttpError {
    /// True when a request may have been processed although no answer arrived
    /// (a transport failure after sending).
    ///
    /// This does not cover a 408/5xx *answer* to a paid submission, which the
    /// executor returns as [`HttpError::Error`] with the classifier's error. Video
    /// submissions must make their classifier return `submission_uncertain` for
    /// those (see the module documentation and D-08).
    pub fn is_ambiguous(&self) -> bool {
        matches!(self, HttpError::Transport(t) if t.after_send)
    }

    /// Map to an [`IrisError`] (transport failures via [`TransportError::to_iris`]).
    pub fn into_iris(self) -> IrisError {
        match self {
            HttpError::Error(e) => e,
            HttpError::Transport(t) => t.to_iris(),
        }
    }
}

impl From<HttpError> for IrisError {
    fn from(e: HttpError) -> Self {
        e.into_iris()
    }
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Error(e) => write!(f, "{e}"),
            HttpError::Transport(t) => write!(f, "{t}"),
        }
    }
}

impl std::error::Error for HttpError {}

/// Outcome of a single attempt before classification.
pub(crate) struct Failure {
    pub kind: TransportKind,
    pub after_send: bool,
    pub status: Option<u16>,
    pub message: String,
    /// The request could not be built for a deterministic local reason (a reqwest
    /// builder error at send time, e.g. a disallowed URL scheme or an invalid
    /// header): nothing was sent, and retrying cannot help. Reported as
    /// `internal_error`, never retried, never "check your network".
    pub local: bool,
}

pub(crate) fn failure_from_reqwest(e: reqwest::Error, status: Option<u16>) -> Failure {
    let local = e.is_builder();
    let (kind, after_send) = if e.is_connect() {
        (TransportKind::Connect, false)
    } else if e.is_timeout() {
        (TransportKind::Timeout, true)
    } else {
        (TransportKind::Other, !local)
    };
    Failure { kind, after_send, status, message: describe_reqwest_error(e), local }
}

impl HttpClient {
    /// Run one logical API call with the retry rules of `call.class`.
    ///
    /// * `build` is called once per attempt with the shared reqwest client and must
    ///   return a fresh request (bodies are rebuilt, never reused). The executor adds
    ///   the per-attempt timeout `call.timeout`.
    /// * `classify` is called for every non-2xx response and returns a [`Verdict`].
    /// * `Retry-After` (seconds or HTTP-date) and `retry-after-ms` headers, and a
    ///   verdict's own `retry_after`, are honored up to
    ///   [`RetryPolicy::max_retry_after`]; a longer requested delay stops with
    ///   `rate_limited` carrying `retry_after`. `x-should-retry: false` stops retries.
    /// * Returns the first 2xx response (with its attempt count), or an [`HttpError`].
    /// * Redirects are never followed: a 3xx response goes to `classify` like any
    ///   other non-2xx response.
    /// * Fails with `internal_error`, before building or sending anything, on a
    ///   client that was not built by [`HttpClient::new`] (see
    ///   [`HttpClient::from_reqwest`]). A request that reqwest refuses to send for a
    ///   local reason (e.g. an unsupported URL scheme) is also `internal_error` and
    ///   is never retried.
    pub async fn execute<B, C>(
        &self,
        call: &Call,
        mut build: B,
        mut classify: C,
    ) -> Result<HttpResponse, HttpError>
    where
        B: FnMut(&reqwest::Client) -> Result<reqwest::RequestBuilder, IrisError>,
        C: FnMut(&HttpResponse) -> Verdict,
    {
        self.require_manual_redirects().map_err(HttpError::Error)?;
        let max_attempts = call.class.max_attempts();
        let mut schedule = self.retry.schedule();
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            let builder = build(&self.inner).map_err(HttpError::Error)?;
            let (client, request) = builder.timeout(call.timeout).build_split();
            let request = request.map_err(|e| {
                HttpError::Error(IrisError::internal(format!(
                    "could not build the HTTP request: {}",
                    describe_reqwest_error(e)
                )))
            })?;
            let method = request.method().clone();
            let url = redact::redact_url(request.url().as_str());
            let started = Instant::now();

            match send_and_read(&client, request).await {
                Ok((status, headers, body)) => {
                    let request_id = call
                        .request_id_header
                        .and_then(|h| headers.get(h))
                        .and_then(|v| v.to_str().ok())
                        .and_then(sanitize_request_id);
                    tracing::debug!(
                        method = %method,
                        url = %url,
                        status = status.as_u16(),
                        attempt,
                        class = call.class.as_str(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        request_id = request_id.as_deref().unwrap_or("-"),
                        "http response"
                    );
                    let resp = HttpResponse { status, headers, body, request_id, attempts: attempt };
                    if status.is_success() {
                        return Ok(resp);
                    }

                    let verdict = classify(&resp);
                    let retry_allowed = call.class.retries_verdict(&verdict);
                    let (error, body_delay) = verdict.into_parts();
                    let header_delay = retry_after_from_headers(&resp.headers, jiff::Timestamp::now());
                    let requested = match (header_delay, body_delay) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
                    let error = enrich(error, &resp, call.provider, requested);
                    let should_retry_false =
                        resp.header("x-should-retry").is_some_and(|v| v.trim().eq_ignore_ascii_case("false"));

                    if !retry_allowed || should_retry_false || attempt >= max_attempts {
                        return Err(HttpError::Error(error));
                    }
                    let delay = match requested {
                        Some(d) if d > self.retry.max_retry_after => {
                            return Err(HttpError::Error(over_cap(error, d, self.retry.max_retry_after)));
                        }
                        Some(d) => d,
                        None => full_jitter(schedule.next().unwrap_or(self.retry.cap).min(self.retry.cap)),
                    };
                    tracing::debug!(
                        method = %method,
                        url = %url,
                        attempt,
                        delay_ms = delay.as_millis() as u64,
                        "retrying after HTTP {}",
                        status.as_u16()
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(failure) if failure.local => {
                    return Err(HttpError::Error(IrisError::internal(format!(
                        "could not send the HTTP request to {url}: {}",
                        failure.message
                    ))));
                }
                Err(failure) => {
                    tracing::debug!(
                        method = %method,
                        url = %url,
                        attempt,
                        class = call.class.as_str(),
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        transport = failure.kind.as_str(),
                        after_send = failure.after_send,
                        "http transport failure"
                    );
                    if !call.class.retries_transport(failure.kind, failure.after_send)
                        || attempt >= max_attempts
                    {
                        return Err(HttpError::Transport(TransportError {
                            kind: failure.kind,
                            after_send: failure.after_send,
                            status: failure.status,
                            attempts: attempt,
                            class: call.class,
                            provider: call.provider,
                            url,
                            message: failure.message,
                        }));
                    }
                    let delay = full_jitter(schedule.next().unwrap_or(self.retry.cap).min(self.retry.cap));
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

/// Send the request and read the full body. A non-success response whose body
/// cannot be read is still a definite answer and is returned with an empty body.
async fn send_and_read(
    client: &reqwest::Client,
    request: reqwest::Request,
) -> Result<(StatusCode, HeaderMap, Bytes), Failure> {
    let response = client.execute(request).await.map_err(|e| failure_from_reqwest(e, None))?;
    let status = response.status();
    let headers = response.headers().clone();
    match response.bytes().await {
        Ok(body) => Ok((status, headers, body)),
        Err(_) if !status.is_success() => Ok((status, headers, Bytes::new())),
        Err(e) => Err(failure_from_reqwest(e, Some(status.as_u16()))),
    }
}

fn enrich(
    mut error: IrisError,
    resp: &HttpResponse,
    provider: Option<ProviderId>,
    retry_after: Option<Duration>,
) -> IrisError {
    if error.provider_status.is_none() {
        error.provider_status = Some(resp.status.as_u16());
    }
    if error.provider_request_id.is_none() {
        error.provider_request_id = resp.request_id.clone();
    }
    if error.provider.is_none() {
        error.provider = provider;
    }
    if error.retry_after.is_none() {
        error.retry_after = retry_after;
    }
    error
}

/// The provider asked for a longer pause than Iris waits automatically.
pub(crate) fn over_cap(mut error: IrisError, requested: Duration, cap: Duration) -> IrisError {
    error.code = ErrorCode::RateLimited;
    error.retryable = Some(true);
    error.retry_after = Some(requested);
    if error.hint.is_none() {
        error.hint = Some(format!(
            "the provider asked to wait {}s before retrying, longer than the {}s Iris waits automatically; \
             run the command again later",
            requested.as_secs().max(1),
            cap.as_secs()
        ));
    }
    error
}

/// Delay requested by response headers: `retry-after-ms` (milliseconds, OpenAI) or
/// `Retry-After` (delta-seconds or an IMF-fixdate HTTP-date, relative to `now`).
/// A date in the past yields zero. Unparseable values are ignored.
pub fn retry_after_from_headers(headers: &HeaderMap, now: jiff::Timestamp) -> Option<Duration> {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(ms) = text("retry-after-ms").and_then(|v| v.parse::<f64>().ok())
        && let Ok(d) = Duration::try_from_secs_f64(ms / 1000.0)
    {
        return Some(d);
    }
    let value = text("retry-after")?;
    if let Ok(secs) = value.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    if let Some(d) = value.parse::<f64>().ok().and_then(|s| Duration::try_from_secs_f64(s).ok()) {
        return Some(d);
    }
    let when = jiff::fmt::rfc2822::DateTimeParser::new().parse_timestamp(value).ok()?;
    let delta = when.duration_since(now);
    if delta.is_negative() {
        return Some(Duration::ZERO);
    }
    Duration::try_from(delta).ok()
}

/// Parse a protobuf JSON `Duration` string such as Google's `RetryInfo.retryDelay`
/// (`"12s"`, `"1.5s"`).
pub fn parse_protobuf_duration(text: &str) -> Option<Duration> {
    let secs = text.trim().strip_suffix('s')?;
    Duration::try_from_secs_f64(secs.parse::<f64>().ok()?).ok()
}

/// Accept a provider request id only if it looks like one: 1–128 characters from
/// `[A-Za-z0-9._:/=+-]`. Anything else is dropped rather than echoed.
pub fn sanitize_request_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    let ok = !id.is_empty()
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "._:/=+-".contains(c));
    ok.then(|| redact::scrub(id).into_owned())
}

/// Replace every URL-looking token (`scheme://…`) in free text with its
/// [`redact::redact_url`] form, so signed query values and userinfo never leak
/// through provider or library messages.
pub fn redact_urls_in_text(text: &str) -> String {
    if !text.contains("://") {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                push_redacted_token(&mut out, &text[s..i]);
            }
            out.push(c);
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        push_redacted_token(&mut out, &text[s..]);
    }
    out
}

fn push_redacted_token(out: &mut String, token: &str) {
    let Some(sep) = token.find("://") else {
        out.push_str(token);
        return;
    };
    let scheme_start = token[..sep]
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_ascii_alphanumeric() || "+.-".contains(*c))
        .last()
        .map(|(i, _)| i)
        .unwrap_or(sep);
    let core_end = token.trim_end_matches(|c: char| ")]}>\"'`,;.".contains(c)).len().max(sep + 3);
    out.push_str(&token[..scheme_start]);
    out.push_str(&redact::redact_url(&token[scheme_start..core_end]));
    out.push_str(&token[core_end..]);
}

/// Lossy UTF-8, URLs redacted, secrets scrubbed, truncated.
pub(crate) fn safe_snippet(bytes: &[u8], max_chars: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = redact_urls_in_text(&text);
    let text = redact::scrub(&text);
    redact::truncate(text.trim(), max_chars)
}

/// A reqwest error as display-safe text: the URL removed, the source chain appended,
/// embedded URLs redacted, secrets scrubbed.
pub(crate) fn describe_reqwest_error(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut parts: Vec<String> = vec![e.to_string()];
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        let text = s.to_string();
        if parts.last() != Some(&text) {
            parts.push(text);
        }
        source = s.source();
    }
    let text = redact_urls_in_text(&parts.join(": "));
    redact::truncate(&redact::scrub(&text), PROVIDER_TEXT_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn ts(s: &str) -> jiff::Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn backoff_ceilings_follow_base_factor_and_cap() {
        let p = RetryPolicy::default();
        let secs: Vec<u64> = (0..8).map(|n| p.backoff_ceiling(n).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn full_jitter_stays_within_zero_and_the_ceiling() {
        let p = RetryPolicy::default();
        for n in 0..8 {
            let ceiling = p.backoff_ceiling(n);
            let samples: Vec<Duration> = (0..500).map(|_| p.backoff_delay(n)).collect();
            assert!(samples.iter().all(|d| *d <= ceiling), "retry {n}");
            // Full jitter spreads over the whole range, not just the upper half.
            assert!(samples.iter().any(|d| *d < ceiling / 2), "retry {n}: no low samples");
            assert!(samples.iter().any(|d| *d > ceiling / 2), "retry {n}: no high samples");
        }
    }

    #[test]
    fn retry_after_parses_seconds_milliseconds_and_http_dates() {
        let now = ts("2026-09-24T12:00:00Z");
        assert_eq!(
            retry_after_from_headers(&headers(&[("retry-after", "7")]), now),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after_from_headers(&headers(&[("retry-after-ms", "1500")]), now),
            Some(Duration::from_millis(1500))
        );
        // retry-after-ms wins over Retry-After when both are present.
        assert_eq!(
            retry_after_from_headers(&headers(&[("retry-after-ms", "250"), ("retry-after", "9")]), now),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            retry_after_from_headers(&headers(&[("retry-after", "Thu, 24 Sep 2026 12:00:45 GMT")]), now),
            Some(Duration::from_secs(45))
        );
        assert_eq!(
            retry_after_from_headers(&headers(&[("retry-after", "Thu, 24 Sep 2026 11:00:00 GMT")]), now),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after_from_headers(&headers(&[("retry-after", "soon")]), now), None);
        assert_eq!(retry_after_from_headers(&headers(&[("retry-after", "-3")]), now), None);
        assert_eq!(retry_after_from_headers(&HeaderMap::new(), now), None);
    }

    #[test]
    fn protobuf_durations_parse() {
        assert_eq!(parse_protobuf_duration("12s"), Some(Duration::from_secs(12)));
        assert_eq!(parse_protobuf_duration("1.5s"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_protobuf_duration("12"), None);
        assert_eq!(parse_protobuf_duration("-1s"), None);
    }

    #[test]
    fn request_ids_are_sanitized() {
        assert_eq!(sanitize_request_id(" req_abc-123 "), Some("req_abc-123".to_string()));
        assert_eq!(sanitize_request_id("bad id with spaces"), None);
        assert_eq!(sanitize_request_id("<script>"), None);
        assert_eq!(sanitize_request_id(&"a".repeat(129)), None);
        assert_eq!(sanitize_request_id(""), None);
    }

    #[test]
    fn urls_inside_text_are_redacted() {
        let t = redact_urls_in_text(
            "error for url (https://u:p@storage.example.com/o.mp4?X-Goog-Signature=abc&alt=media), retry",
        );
        assert!(!t.contains("abc"), "{t}");
        assert!(!t.contains("u:p"), "{t}");
        assert!(t.contains("X-Goog-Signature=REDACTED"), "{t}");
        assert!(t.starts_with("error for url (https://storage.example.com/"), "{t}");
        assert!(t.ends_with("), retry"), "{t}");
        assert_eq!(redact_urls_in_text("no urls here"), "no urls here");
        let t = redact_urls_in_text("proxy=http://user:pw@proxy.local:3128\nnext line");
        assert!(!t.contains("pw"), "{t}");
        assert!(t.ends_with("\nnext line"), "{t}");
    }

    #[test]
    fn class_rules_match_c04() {
        use RetryClass::*;
        use TransportKind::*;
        assert_eq!(PaidSubmit.max_attempts(), 3);
        assert_eq!(IdempotentRead.max_attempts(), 5);
        assert_eq!(Download.max_attempts(), 5);
        assert!(PaidSubmit.retries_transport(Connect, false));
        assert!(!PaidSubmit.retries_transport(Timeout, true));
        assert!(!PaidSubmit.retries_transport(Other, true));
        assert!(IdempotentRead.retries_transport(Timeout, true));
        assert!(Download.retries_transport(Other, true));
        let e = || IrisError::internal("x");
        let transient = Verdict::for_status(StatusCode::INTERNAL_SERVER_ERROR, e());
        let rejection = Verdict::for_status(StatusCode::TOO_MANY_REQUESTS, e());
        let bad = Verdict::for_status(StatusCode::BAD_REQUEST, e());
        assert!(matches!(transient, Verdict::Transient { .. }));
        assert!(matches!(rejection, Verdict::RetryableRejection { .. }));
        assert!(matches!(bad, Verdict::Final(_)));
        assert!(!PaidSubmit.retries_verdict(&transient));
        assert!(IdempotentRead.retries_verdict(&transient));
        assert!(PaidSubmit.retries_verdict(&rejection));
        assert!(!IdempotentRead.retries_verdict(&bad));
        for s in [400u16, 401, 403, 404, 409, 413, 422] {
            let v = Verdict::for_status(StatusCode::from_u16(s).unwrap(), e());
            assert!(matches!(v, Verdict::Final(_)), "{s}");
        }
    }

    #[test]
    fn paid_transport_errors_flag_charge_possible_only_after_send() {
        let base = TransportError {
            kind: TransportKind::Timeout,
            after_send: true,
            status: None,
            attempts: 1,
            class: RetryClass::PaidSubmit,
            provider: Some(ProviderId::OpenAi),
            url: "https://api.openai.com/v1/images/generations".into(),
            message: "operation timed out".into(),
        };
        let e = base.to_iris();
        assert_eq!(e.code, ErrorCode::RequestTimeout);
        assert_eq!(e.details.get("charge_possible"), Some(&serde_json::Value::Bool(true)));
        assert!(e.hint.as_deref().unwrap().contains("did not retry"));

        let connect =
            TransportError { kind: TransportKind::Connect, after_send: false, attempts: 3, ..base.clone() };
        let e = connect.to_iris();
        assert_eq!(e.code, ErrorCode::NetworkError);
        assert_eq!(e.details.get("charge_possible"), Some(&serde_json::Value::Bool(false)));

        let read = TransportError { class: RetryClass::IdempotentRead, ..base };
        assert!(!read.charge_possible());
        assert!(read.to_iris().details.get("charge_possible").is_none());
    }
}
