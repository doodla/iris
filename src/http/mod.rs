//! Shared HTTP plumbing: client construction, operation-aware retries, error
//! classification helpers, streaming downloads with credential-origin rules, and
//! redaction. See docs/architecture.md "Where invariants live" for the retry
//! classes, the paid-submit retry policy, and the credential-origin rule.
//!
//! Provider adapters use three entry points:
//!
//! * [`HttpClient::new`] builds the shared client (no automatic redirects, fixed
//!   `User-Agent`, system proxy, no default credentials).
//! * [`HttpClient::execute`] runs one logical API call under a [`RetryClass`],
//!   rebuilding the request on every attempt and asking a caller-supplied classifier
//!   how to treat each non-success response (see [`Verdict`]). Response bodies are
//!   read up to a per-call limit ([`Call::max_body`]), never without bound.
//! * [`download()`] streams an artifact to a file while hashing it, following
//!   redirects manually and attaching the credential only to the configured origin.
//!
//! Nothing in this module logs or formats headers, bodies, prompts, or keys; URLs
//! are always passed through [`crate::redact::redact_url`] before they are logged.
//!
//! Cancellation: every future here can be dropped at any point (the application
//! races it against Ctrl-C); dropping aborts the in-flight request or backoff sleep.

#![warn(missing_docs)]

mod download;
mod retry;

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};

pub use download::{
    DownloadError, DownloadRequest, Downloaded, MAX_DOWNLOAD_BYTES, MAX_REDIRECTS, download,
    download_limited, hop_allowed,
};
pub(crate) use retry::PROVIDER_TEXT_MAX;
pub use retry::{
    Call, HttpError, HttpResponse, JSON_BODY_LIMIT, MEDIA_BODY_LIMIT, RetryClass, RetryPolicy,
    TransportError, TransportKind, Verdict, parse_protobuf_duration, redact_urls_in_text,
    retry_after_from_headers, sanitize_request_id,
};

use crate::error::{ErrorCode, IrisError};
use crate::secret::Secret;

/// `User-Agent` sent with every request.
pub const USER_AGENT: &str = concat!("iris/", env!("CARGO_PKG_VERSION"));

/// Settings for [`HttpClient::new`].
#[derive(Debug, Clone)]
pub struct HttpSettings {
    /// TCP/TLS connect timeout (15s). This is a client-level setting: it is
    /// fixed when the client is built, and [`Timeouts::connect`] reaches requests only
    /// through this field (the application derives it from there).
    pub connect_timeout: Duration,
    /// Backoff schedule used by [`HttpClient::execute`] and [`download()`].
    pub retry: RetryPolicy,
    /// Honor the system proxy configuration (`HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`,
    /// …). Default `true`. Tests against 127.0.0.1 mock servers set `false`:
    /// loopback is not exempt from a configured proxy unless `NO_PROXY` covers it, so
    /// mock traffic (including fake credential headers) would otherwise go to the proxy.
    pub system_proxy: bool,
}

impl Default for HttpSettings {
    fn default() -> Self {
        HttpSettings {
            connect_timeout: Timeouts::default().connect,
            retry: RetryPolicy::default(),
            system_proxy: true,
        }
    }
}

/// Shared HTTP client (cheap to clone).
#[derive(Debug, Clone)]
pub struct HttpClient {
    pub(crate) inner: reqwest::Client,
    /// The same client without any proxy, for loopback destinations: a proxy
    /// cannot reach this machine's loopback, and sending a request there through a
    /// system proxy would hand its credential header to the proxy in clear text
    /// when the base URL is plain http.
    direct: reqwest::Client,
    pub(crate) retry: RetryPolicy,
    /// True only for clients built by [`HttpClient::new`], whose redirect policy is
    /// known to be `none`. [`HttpClient::execute`] and [`download()`] refuse to run
    /// on any other client (see [`HttpClient::from_reqwest`]).
    manual_redirects: bool,
}

impl HttpClient {
    /// Build the client Iris uses for every provider call:
    ///
    /// * redirect policy `none` — redirects are followed manually by [`download()`] so
    ///   credentials never follow a cross-origin hop;
    /// * connect timeout from `settings`;
    /// * `User-Agent: iris/<version>`;
    /// * system proxy configuration (reqwest default: `HTTPS_PROXY`, `NO_PROXY`, …)
    ///   unless [`HttpSettings::system_proxy`] is `false`;
    /// * no default headers, in particular no credentials.
    ///
    /// Requests to loopback hosts ([`is_loopback`]) never use a proxy.
    pub fn new(settings: &HttpSettings) -> Result<Self, IrisError> {
        let build = |proxy: bool| {
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(settings.connect_timeout)
                .user_agent(USER_AGENT);
            if !proxy {
                builder = builder.no_proxy();
            }
            builder.build().map_err(|e| {
                IrisError::internal(format!(
                    "could not initialize the HTTP client: {}",
                    retry::describe_reqwest_error(e)
                ))
            })
        };
        let inner = build(settings.system_proxy)?;
        let direct = build(false)?;
        Ok(HttpClient { inner, direct, retry: settings.retry.clone(), manual_redirects: true })
    }

    /// The client to send a request to `url` with: [`HttpClient::new`]'s client, or
    /// its proxy-free twin when `url` is a loopback destination.
    pub(crate) fn client_for(&self, url: &url::Url) -> &reqwest::Client {
        if is_loopback(url) { &self.direct } else { &self.inner }
    }

    /// Wrap an existing reqwest client, using the default [`RetryPolicy`], for direct
    /// use through [`HttpClient::reqwest`].
    ///
    /// Iris cannot inspect a foreign client's redirect policy, and reqwest's default
    /// policy follows redirects itself while keeping custom credential headers such
    /// as `x-goog-api-key` on cross-origin hops (and skipping the https-only and
    /// 5-hop rules). [`HttpClient::execute`] and [`download()`] therefore refuse to
    /// run on a client built here and fail with `internal_error` before sending
    /// anything. Use [`HttpClient::new`] for every client that talks to a provider.
    pub fn from_reqwest(inner: reqwest::Client) -> Self {
        HttpClient { direct: inner.clone(), inner, retry: RetryPolicy::default(), manual_redirects: false }
    }

    /// Whether this client was built by [`HttpClient::new`] (redirects are never
    /// followed automatically), which [`HttpClient::execute`] and [`download()`]
    /// require.
    pub fn follows_redirects_manually(&self) -> bool {
        self.manual_redirects
    }

    /// `internal_error` unless this client was built by [`HttpClient::new`].
    pub(crate) fn require_manual_redirects(&self) -> Result<(), IrisError> {
        if self.manual_redirects {
            return Ok(());
        }
        Err(IrisError::internal(
            "this HTTP client was not built by HttpClient::new, so its redirect policy is unknown; \
             refusing to send a request whose credentials could follow a redirect to another origin",
        ))
    }

    /// Replace the backoff schedule (tests use millisecond delays).
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// The backoff schedule in use.
    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.retry
    }

    /// The underlying reqwest client.
    pub fn reqwest(&self) -> &reqwest::Client {
        &self.inner
    }
}

/// Upload rate the time limit of a request with a body allows for sending it:
/// 256 KiB/s (about 2 Mbit/s), a slow uplink. See [`upload_allowance`].
pub const UPLOAD_ALLOWANCE_RATE: u64 = 256 * 1024;

/// Longest [`upload_allowance`]: 10 minutes, enough to send 150 MiB at
/// [`UPLOAD_ALLOWANCE_RATE`] (a Veo request is below 100 MB; Gemini image requests
/// below 20 MB). It bounds how long one attempt can last, which the stale-submission
/// rule relies on ([`Timeouts::max_submit_attempt`]).
pub const MAX_UPLOAD_ALLOWANCE: Duration = Duration::from_secs(600);

/// Extra time [`HttpClient::execute`] adds to an attempt's time limit for sending a
/// request body of `body_bytes`: the body's size at [`UPLOAD_ALLOWANCE_RATE`], at
/// most [`MAX_UPLOAD_ALLOWANCE`]. A request with inline images (a Veo job with
/// reference images, an image edit) can take minutes to upload on a slow link, and
/// a paid request cut off mid-upload is an uncertain outcome; a small JSON body
/// adds nothing noticeable.
pub fn upload_allowance(body_bytes: u64) -> Duration {
    let millis = u128::from(body_bytes) * 1000 / u128::from(UPLOAD_ALLOWANCE_RATE);
    Duration::from_millis(u64::try_from(millis).unwrap_or(u64::MAX)).min(MAX_UPLOAD_ALLOWANCE)
}

/// Per-provider timeouts. The time limit of one attempt of a call with a request
/// body is its timeout here plus the body's [`upload_allowance`].
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    /// TCP/TLS connect timeout. Client-level: [`HttpClient::execute`] and
    /// [`download()`] do not read it; it takes effect only through
    /// [`HttpSettings::connect_timeout`] when the client is built
    /// (`config::Settings::http_settings` derives it from here).
    pub connect: Duration,
    /// One attempt of a synchronous paid generation (image) request, before the
    /// upload allowance (`request_timeout`).
    pub generate: Duration,
    /// One attempt of an async job submission (Veo), before the upload allowance
    /// (`submit_timeout`).
    pub submit: Duration,
    /// Status poll request.
    pub poll: Duration,
    /// Download read-idle timeout.
    pub download_idle: Duration,
}

impl Timeouts {
    /// The longest one job-submission attempt can last, whatever its body:
    /// [`Timeouts::submit`] plus [`MAX_UPLOAD_ALLOWANCE`] (the connect timeout comes
    /// on top).
    pub fn max_submit_attempt(&self) -> Duration {
        self.submit.saturating_add(MAX_UPLOAD_ALLOWANCE)
    }
}

impl Default for Timeouts {
    fn default() -> Self {
        Timeouts {
            connect: Duration::from_secs(15),
            generate: Duration::from_secs(300),
            submit: Duration::from_secs(60),
            poll: Duration::from_secs(30),
            download_idle: Duration::from_secs(60),
        }
    }
}

/// A credential header (`name: prefix + secret`) ready to attach to a request.
///
/// The header value is marked sensitive and `Debug` never shows it. Build one per
/// call from the provider's credential header description and the `Secret` in the
/// provider context.
#[derive(Clone)]
pub struct AuthHeader {
    name: HeaderName,
    value: HeaderValue,
}

impl AuthHeader {
    /// `name` is the header name (e.g. `authorization`, `x-goog-api-key`), `prefix`
    /// is prepended to the secret (e.g. `"Bearer "`, or `""`).
    ///
    /// Fails with `config_invalid` if the key contains characters that cannot appear
    /// in an HTTP header (the value is never included in the message), or with
    /// `internal_error` if `name` is not a valid header name.
    pub fn new(name: &str, prefix: &str, secret: &Secret) -> Result<Self, IrisError> {
        let header_name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| IrisError::internal(format!("invalid credential header name '{name}'")))?;
        let mut value = HeaderValue::from_str(&format!("{prefix}{}", secret.expose())).map_err(|_| {
            IrisError::new(
                ErrorCode::ConfigInvalid,
                "the API key contains characters that cannot be sent in an HTTP header",
            )
            .with_hint("check the credential environment variable for stray quotes or line breaks")
        })?;
        value.set_sensitive(true);
        Ok(AuthHeader { name: header_name, value })
    }

    /// The header name.
    pub fn name(&self) -> &HeaderName {
        &self.name
    }

    /// Attach this header to a request.
    pub fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        builder.header(self.name.clone(), self.value.clone())
    }
}

impl fmt::Debug for AuthHeader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthHeader").field("name", &self.name).field("value", &"***").finish()
    }
}

/// True when both URLs have the same origin (scheme, host, and effective port).
/// Credentials are only ever sent to the configured base URL's origin.
pub fn same_origin(a: &url::Url, b: &url::Url) -> bool {
    let (oa, ob) = (a.origin(), b.origin());
    oa.is_tuple() && oa == ob
}

/// True when `url`'s host is this machine's loopback interface: `localhost`, an
/// IPv4 address in 127.0.0.0/8, or `::1`. Plain `http` is allowed only for such
/// hosts (a local mock server or proxy), because traffic to any other host
/// crosses a network where an unencrypted API key could be read.
pub fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_names_iris_and_version() {
        assert_eq!(USER_AGENT, format!("iris/{}", env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn auth_header_debug_hides_the_secret_and_marks_it_sensitive() {
        let h = AuthHeader::new("authorization", "Bearer ", &Secret::new("sk-test-secret-123456")).unwrap();
        let dbg = format!("{h:?}");
        assert!(!dbg.contains("secret-123456"), "{dbg}");
        assert!(h.value.is_sensitive());
        assert_eq!(h.name().as_str(), "authorization");
    }

    #[test]
    fn auth_header_rejects_control_characters_without_echoing_the_value() {
        let err = AuthHeader::new("x-goog-api-key", "", &Secret::new("abc\ndef-secret")).unwrap_err();
        assert_eq!(err.code, ErrorCode::ConfigInvalid);
        assert!(!err.message.contains("def-secret"));
    }

    #[test]
    fn same_origin_compares_scheme_host_and_effective_port() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(same_origin(&u("https://api.openai.com/v1"), &u("https://api.openai.com:443/files/x")));
        assert!(!same_origin(&u("https://api.openai.com/v1"), &u("http://api.openai.com/v1")));
        assert!(!same_origin(&u("https://api.openai.com/v1"), &u("https://files.openai.com/v1")));
        assert!(!same_origin(&u("http://127.0.0.1:8080"), &u("http://127.0.0.1:8081")));
    }

    #[test]
    fn the_upload_allowance_grows_with_the_body_and_is_capped() {
        assert_eq!(upload_allowance(0), Duration::ZERO);
        assert_eq!(upload_allowance(2_000), Duration::from_millis(7));
        assert_eq!(upload_allowance(256 * 1024), Duration::from_secs(1));
        // A 23 MB Veo request with a large reference image: about 88 seconds more.
        assert_eq!(upload_allowance(23_000_000).as_secs(), 87);
        // The largest Veo request (below 100 MB) stays under the cap.
        assert!(upload_allowance(100_000_000) < MAX_UPLOAD_ALLOWANCE);
        assert_eq!(upload_allowance(u64::MAX), MAX_UPLOAD_ALLOWANCE);
        let t = Timeouts::default();
        assert_eq!(t.max_submit_attempt(), Duration::from_secs(660));
    }

    #[test]
    fn loopback_hosts_are_localhost_127_0_0_0_8_and_ipv6_one() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        for yes in [
            "http://localhost:8080/v1",
            "http://LOCALHOST",
            "http://127.0.0.1:9",
            "http://127.1.2.3",
            "http://127.1/x",
            "http://[::1]:8080",
        ] {
            assert!(is_loopback(&u(yes)), "{yes}");
        }
        for no in [
            "http://api.example.invalid",
            "http://localhost.example.com",
            "http://mylocalhost",
            "http://10.0.0.1",
            "http://128.0.0.1",
            "http://0.0.0.0",
            "http://[::ffff:127.0.0.1]",
            "http://[::2]",
            "http://192.168.1.10",
        ] {
            assert!(!is_loopback(&u(no)), "{no}");
        }
    }

    #[test]
    fn client_builds_with_default_settings() {
        let c = HttpClient::new(&HttpSettings::default()).unwrap();
        assert_eq!(c.retry_policy().base, Duration::from_secs(1));
    }
}
