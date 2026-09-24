//! Streaming artifact downloads (see docs/jobs.md "Downloads").
//!
//! Redirects are followed here, by hand, because reqwest's automatic redirects would
//! forward custom credential headers such as `x-goog-api-key` to other hosts.

use std::io::SeekFrom;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, LOCATION};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use url::Url;

use super::retry::{
    Failure, PROVIDER_TEXT_MAX, RetryClass, TransportError, TransportKind, failure_from_reqwest, full_jitter,
    over_cap, retry_after_from_headers, safe_snippet,
};
use super::{AuthHeader, HttpClient, same_origin};
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::redact;

/// Maximum redirect hops followed for one download attempt.
pub const MAX_REDIRECTS: u32 = 5;

/// Bytes of an error body read for diagnostics (the rest is discarded).
const SNIPPET_READ_LIMIT: usize = 16 * 1024;

/// What to download and where.
#[derive(Debug, Clone)]
pub struct DownloadRequest<'a> {
    /// Artifact URL as reported by the provider.
    pub url: &'a str,
    /// Open, writable file that receives the bytes: normally the caller's temp file
    /// (`.<name>.iris-part-*`, created with `O_EXCL`), e.g.
    /// `PartFile::file_mut()` / `NamedTempFile::as_file()`.
    ///
    /// The download writes only through this handle, never by path, so a path that
    /// is replaced by a symlink or deleted meanwhile cannot redirect the bytes
    /// (temp files are never reached through a symlinked path). The file is
    /// emptied (`set_len(0)`, rewound) at the start of every attempt, so a retried
    /// download never appends to a partial body, and it is left empty on failure.
    /// The handle's position is shared: afterwards it is at the end of the data.
    pub dest: &'a std::fs::File,
    /// The provider's configured base URL. Its origin is the only origin that ever
    /// receives `auth`; if its scheme is `http` (local mock servers), `http` hops
    /// are allowed, otherwise every hop must be `https`.
    pub base_url: &'a Url,
    /// Credential header, attached only to hops whose origin equals `base_url`'s.
    pub auth: Option<&'a AuthHeader>,
    /// Longest wait for response headers or for the next body chunk (60s).
    pub idle_timeout: Duration,
    /// Provider attached to transport errors.
    pub provider: Option<ProviderId>,
}

/// A completed download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Bytes written to `dest`.
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the bytes written.
    pub sha256_hex: String,
    /// Media type from the final response's `Content-Type` (lowercase, no parameters).
    pub content_type: Option<String>,
    /// Redirect hops followed in the successful attempt.
    pub redirects: u32,
    /// Attempts made, including the successful one.
    pub attempts: u32,
}

/// Why a download failed. On every failure `dest` is left empty.
#[derive(Debug, Clone)]
pub enum DownloadError {
    /// The file host answered with a non-success status (after retries where the
    /// status is retryable). [`DownloadError::into_iris`] maps 410 to
    /// `artifact_expired` and 403/404 to a retryable `download_failed`: whether a
    /// 403/404 means the output is gone depends on the provider's retention, which
    /// only the caller knows.
    Status {
        /// HTTP status of the failing hop.
        status: u16,
        /// Bounded, secret-scrubbed, URL-redacted excerpt of the error body.
        body_snippet: String,
        /// Delay requested by the host, if any.
        retry_after: Option<Duration>,
        /// Set when retrying stopped because `retry_after` exceeded this limit
        /// ([`RetryPolicy::max_retry_after`](super::RetryPolicy::max_retry_after));
        /// [`DownloadError::into_iris`] then reports `rate_limited` whatever the
        /// status (see docs/jobs.md), as [`HttpClient::execute`] does.
        retry_after_limit: Option<Duration>,
        /// Attempts made.
        attempts: u32,
        /// Redacted URL of the failing hop.
        url: String,
    },
    /// A success status whose `Content-Type` says it is an error document
    /// (`application/json`, `*+json`, `application/xml`, `text/*`), not media.
    /// Nothing was written to `dest`.
    InvalidMedia {
        /// The media type the host declared (lowercase, no parameters).
        content_type: String,
        /// Bounded, secret-scrubbed, URL-redacted excerpt of the body.
        body_snippet: String,
        /// Redacted URL of the hop that served it.
        url: String,
    },
    /// Refused by policy: invalid URL, non-https hop, redirect without a usable
    /// `Location`, or more than [`MAX_REDIRECTS`] hops.
    Refused {
        /// Display-safe reason.
        message: String,
    },
    /// No usable response after the allowed attempts.
    Transport(TransportError),
    /// Writing `dest` failed.
    Io {
        /// Display-safe description of the file error.
        message: String,
    },
    /// A local programming or setup error, never retried: the client was not built
    /// by [`HttpClient::new`] (its redirect policy is unknown), or reqwest refused
    /// to build the request.
    Internal {
        /// Display-safe description.
        message: String,
    },
}

impl DownloadError {
    /// Map to the public taxonomy (see docs/json-contract.md and docs/jobs.md):
    /// 410 → `artifact_expired`; 403/404 → `download_failed`, retryable (callers
    /// that know the output's retention report `artifact_expired` once it has
    /// passed); 401 → `authentication_failed`;
    /// 429, or any status whose `Retry-After` exceeded the automatic-wait limit →
    /// `rate_limited` with `retry_after`; other statuses, policy refusals and
    /// transport failures → `download_failed` (retryable unless the failure is
    /// permanent); error documents served as media → `invalid_media`; file errors →
    /// `io_error`; [`DownloadError::Internal`] → `internal_error`.
    pub fn into_iris(self) -> IrisError {
        match self {
            DownloadError::Status { status, body_snippet, retry_after, retry_after_limit, attempts, url } => {
                let (code, retryable, message) = match status {
                    410 => (
                        ErrorCode::ArtifactExpired,
                        Some(false),
                        format!("the file host no longer serves this artifact (HTTP {status})"),
                    ),
                    403 | 404 => (
                        ErrorCode::DownloadFailed,
                        Some(true),
                        format!("the file host refused the download (HTTP {status})"),
                    ),
                    401 => (
                        ErrorCode::AuthenticationFailed,
                        Some(false),
                        "the file host rejected the credentials (HTTP 401)".to_string(),
                    ),
                    429 => (
                        ErrorCode::RateLimited,
                        Some(true),
                        "the file host is rate limiting downloads (HTTP 429)".to_string(),
                    ),
                    408 | 500..=599 => (
                        ErrorCode::DownloadFailed,
                        Some(true),
                        format!("the file host returned a server error (HTTP {status})"),
                    ),
                    _ => (
                        ErrorCode::DownloadFailed,
                        Some(false),
                        format!("the file host refused the download (HTTP {status})"),
                    ),
                };
                let mut err = IrisError::new(code, message)
                    .with_retryable(retryable)
                    .with_provider_status(status)
                    .with_detail("url", url)
                    .with_detail("attempts", attempts);
                if code == ErrorCode::ArtifactExpired {
                    err = err
                        .with_hint("the file host says the output is gone; it can no longer be downloaded");
                }
                if let Some(after) = retry_after {
                    err = err.with_retry_after(after);
                }
                if !body_snippet.is_empty() {
                    err = err.with_detail("provider_message", body_snippet);
                }
                if let (Some(after), Some(limit)) = (retry_after, retry_after_limit) {
                    err = over_cap(err, after, limit);
                }
                err
            }
            DownloadError::InvalidMedia { content_type, body_snippet, url } => {
                let mut err = IrisError::new(
                    ErrorCode::InvalidMedia,
                    format!(
                        "the file host returned '{content_type}' content instead of media; nothing was saved"
                    ),
                )
                .with_detail("content_type", content_type)
                .with_detail("url", url);
                if !body_snippet.is_empty() {
                    err = err.with_detail("provider_message", body_snippet);
                }
                err
            }
            DownloadError::Refused { message } => {
                IrisError::new(ErrorCode::DownloadFailed, message).with_retryable(Some(false))
            }
            DownloadError::Transport(t) => {
                let base = t.to_iris();
                let mut err =
                    IrisError::new(ErrorCode::DownloadFailed, format!("download failed: {}", base.message))
                        .with_retryable(Some(true));
                err.details = base.details.clone();
                err.provider = base.provider;
                err.provider_status = base.provider_status;
                err
            }
            DownloadError::Io { message } => IrisError::new(ErrorCode::IoError, message),
            DownloadError::Internal { message } => IrisError::internal(message),
        }
    }
}

impl From<DownloadError> for IrisError {
    fn from(e: DownloadError) -> Self {
        e.into_iris()
    }
}

impl std::fmt::Display for DownloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadError::Status { status, url, .. } => write!(f, "HTTP {status} from {url}"),
            DownloadError::InvalidMedia { content_type, url, .. } => {
                write!(f, "'{content_type}' error document from {url}")
            }
            DownloadError::Refused { message }
            | DownloadError::Io { message }
            | DownloadError::Internal { message } => f.write_str(message),
            DownloadError::Transport(t) => write!(f, "{t}"),
        }
    }
}

impl std::error::Error for DownloadError {}

/// Whether a download hop to `url` is allowed: `https` always; `http` only when the
/// configured `base_url` is itself `http` (local mock servers). Other schemes never.
pub fn hop_allowed(url: &Url, base_url: &Url) -> bool {
    if url.host().is_none() {
        return false;
    }
    match url.scheme() {
        "https" => true,
        "http" => base_url.scheme() == "http",
        _ => false,
    }
}

/// `Content-Type` values that are error documents, never media.
fn is_error_document(media_type: &str) -> bool {
    media_type.starts_with("text/")
        || media_type == "application/json"
        || media_type.ends_with("+json")
        || media_type == "application/xml"
}

enum AttemptError {
    Final(DownloadError),
    Retryable { error: DownloadError, delay: Option<Duration> },
}

/// Stream `req.url` into `req.dest` with the `Download` retry class, hashing and
/// counting bytes as they arrive. See [`DownloadRequest`] for the credential and
/// redirect rules. Dropping the future aborts the download (the file may then hold
/// a partial body; callers discard their temp file).
///
/// Fails with [`DownloadError::Internal`] before sending anything if `client` was
/// not built by [`HttpClient::new`]: only that client is known not to follow
/// redirects by itself (see [`HttpClient::from_reqwest`]).
pub async fn download(client: &HttpClient, req: &DownloadRequest<'_>) -> Result<Downloaded, DownloadError> {
    client.require_manual_redirects().map_err(|e| DownloadError::Internal { message: e.message.clone() })?;
    let start = Url::parse(req.url)
        .map_err(|_| DownloadError::Refused { message: "the artifact URL is not a valid URL".to_string() })?;
    // A second handle to the caller's open file: every write, truncation, and seek
    // goes to the file the caller created, whatever happens to its path.
    let mut file = req.dest.try_clone().map(tokio::fs::File::from_std).map_err(dest_io_error)?;
    let class = RetryClass::Download;
    let mut schedule = client.retry.schedule();
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let result = attempt_once(client, req, &start, attempt, &mut file).await;
        let (error, delay) = match result {
            Ok(mut done) => {
                done.attempts = attempt;
                return Ok(done);
            }
            Err(AttemptError::Final(error)) => return Err(fail(&mut file, error).await),
            Err(AttemptError::Retryable { error, delay }) => (error, delay),
        };
        if attempt >= class.max_attempts() {
            return Err(fail(&mut file, error).await);
        }
        let limit = client.retry.max_retry_after;
        let delay = match delay {
            Some(d) if d > limit => {
                let error = match error {
                    DownloadError::Status { status, body_snippet, retry_after, attempts, url, .. } => {
                        DownloadError::Status {
                            status,
                            body_snippet,
                            retry_after,
                            retry_after_limit: Some(limit),
                            attempts,
                            url,
                        }
                    }
                    other => other,
                };
                return Err(fail(&mut file, error).await);
            }
            Some(d) => d,
            None => full_jitter(schedule.next().unwrap_or(client.retry.cap).min(client.retry.cap)),
        };
        tracing::debug!(attempt, delay_ms = delay.as_millis() as u64, "retrying download");
        tokio::time::sleep(delay).await;
    }
}

fn dest_io_error(e: std::io::Error) -> DownloadError {
    DownloadError::Io { message: format!("cannot write the downloaded file: {e}") }
}

/// Empty `file` and rewind it, through the handle.
async fn empty(file: &mut tokio::fs::File) -> std::io::Result<()> {
    file.set_len(0).await?;
    file.seek(SeekFrom::Start(0)).await?;
    Ok(())
}

/// Leave `dest` empty after a failed download (best effort) and return the error.
async fn fail(file: &mut tokio::fs::File, error: DownloadError) -> DownloadError {
    let _ = file.flush().await;
    let _ = empty(file).await;
    error
}

/// One attempt: empty the file, then fetch into it.
async fn attempt_once(
    client: &HttpClient,
    req: &DownloadRequest<'_>,
    start: &Url,
    attempt: u32,
    file: &mut tokio::fs::File,
) -> Result<Downloaded, AttemptError> {
    let io_error = |e: std::io::Error| AttemptError::Final(dest_io_error(e));
    empty(file).await.map_err(io_error)?;
    let result = fetch_into(client, req, start, attempt, file).await;
    // tokio's File completes queued writes on a blocking thread. Flushing waits for
    // them, so no write of this attempt can land after a retry empties the file
    // or after a failure empties it.
    let flushed = file.flush().await;
    let done = result?;
    flushed.map_err(io_error)?;
    file.sync_all().await.map_err(io_error)?;
    Ok(done)
}

async fn fetch_into(
    client: &HttpClient,
    req: &DownloadRequest<'_>,
    start: &Url,
    attempt: u32,
    file: &mut tokio::fs::File,
) -> Result<Downloaded, AttemptError> {
    let transport = |f: Failure, url: &Url| TransportError {
        kind: f.kind,
        after_send: f.after_send,
        status: f.status,
        attempts: attempt,
        class: RetryClass::Download,
        provider: req.provider,
        url: redact::redact_url(url.as_str()),
        message: f.message,
    };
    let idle_timeout = |url: &Url, status: Option<u16>| {
        transport(
            Failure {
                kind: TransportKind::Timeout,
                after_send: true,
                status,
                message: format!("no data for {}s", req.idle_timeout.as_secs_f64()),
                local: false,
            },
            url,
        )
    };
    let io_error = |e: std::io::Error| AttemptError::Final(dest_io_error(e));

    let mut url = start.clone();
    let mut redirects: u32 = 0;
    loop {
        let shown = redact::redact_url(url.as_str());
        if !hop_allowed(&url, req.base_url) {
            return Err(AttemptError::Final(DownloadError::Refused {
                message: format!(
                    "refusing to download from {shown}: only https URLs are allowed{}",
                    if redirects > 0 { " (reached through a redirect)" } else { "" }
                ),
            }));
        }
        let mut builder = client.inner.get(url.clone());
        let with_credential = req.auth.is_some() && same_origin(&url, req.base_url);
        if let Some(auth) = req.auth.filter(|_| with_credential) {
            builder = auth.apply(builder);
        }
        let started = Instant::now();
        let response = match tokio::time::timeout(req.idle_timeout, builder.send()).await {
            Err(_) => return Err(retryable(DownloadError::Transport(idle_timeout(&url, None)))),
            Ok(Err(e)) => {
                let failure = failure_from_reqwest(e, None);
                if failure.local {
                    return Err(AttemptError::Final(DownloadError::Internal {
                        message: format!(
                            "could not send the download request to {shown}: {}",
                            failure.message
                        ),
                    }));
                }
                return Err(retryable(DownloadError::Transport(transport(failure, &url))));
            }
            Ok(Ok(r)) => r,
        };
        let status = response.status();
        tracing::debug!(
            method = "GET",
            url = %shown,
            status = status.as_u16(),
            attempt,
            redirects,
            credential = with_credential,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "download response"
        );

        if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
            let next = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|loc| url.join(loc).ok())
                .ok_or_else(|| {
                    AttemptError::Final(DownloadError::Refused {
                        message: format!("{shown} redirected without a usable Location header"),
                    })
                })?;
            redirects += 1;
            if redirects > MAX_REDIRECTS {
                return Err(AttemptError::Final(DownloadError::Refused {
                    message: format!("too many redirects (more than {MAX_REDIRECTS}) while downloading"),
                }));
            }
            url = next;
            continue;
        }

        if !status.is_success() {
            let retry_after = retry_after_from_headers(response.headers(), jiff::Timestamp::now());
            let no_retry = response
                .headers()
                .get("x-should-retry")
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("false"));
            let body_snippet = read_snippet(response, req.idle_timeout).await;
            let error = DownloadError::Status {
                status: status.as_u16(),
                body_snippet,
                retry_after,
                retry_after_limit: None,
                attempts: attempt,
                url: shown,
            };
            let transient = matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504);
            return Err(if transient && !no_retry {
                AttemptError::Retryable { error, delay: retry_after }
            } else {
                AttemptError::Final(error)
            });
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty());
        if let Some(ct) = content_type.as_deref().filter(|ct| is_error_document(ct)) {
            let content_type = ct.to_string();
            let body_snippet = read_snippet(response, req.idle_timeout).await;
            return Err(AttemptError::Final(DownloadError::InvalidMedia {
                content_type,
                body_snippet,
                url: shown,
            }));
        }

        let mut hasher = Sha256::new();
        let mut bytes: u64 = 0;
        let mut stream = response.bytes_stream();
        loop {
            match tokio::time::timeout(req.idle_timeout, stream.next()).await {
                Err(_) => {
                    return Err(retryable(DownloadError::Transport(idle_timeout(
                        &url,
                        Some(status.as_u16()),
                    ))));
                }
                Ok(None) => break,
                Ok(Some(Err(e))) => {
                    let failure = failure_from_reqwest(e, Some(status.as_u16()));
                    return Err(retryable(DownloadError::Transport(transport(failure, &url))));
                }
                Ok(Some(Ok(chunk))) => {
                    hasher.update(&chunk);
                    bytes += chunk.len() as u64;
                    file.write_all(&chunk).await.map_err(io_error)?;
                }
            }
        }
        return Ok(Downloaded {
            bytes,
            sha256_hex: hex::encode(hasher.finalize()),
            content_type,
            redirects,
            attempts: attempt,
        });
    }
}

fn retryable(error: DownloadError) -> AttemptError {
    AttemptError::Retryable { error, delay: None }
}

/// Read at most [`SNIPPET_READ_LIMIT`] bytes of an error body and make them safe to show.
async fn read_snippet(response: reqwest::Response, idle: Duration) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = response.bytes_stream();
    while buf.len() < SNIPPET_READ_LIMIT {
        match tokio::time::timeout(idle, stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                let take = chunk.len().min(SNIPPET_READ_LIMIT - buf.len());
                buf.extend_from_slice(&chunk[..take]);
            }
            _ => break,
        }
    }
    safe_snippet(&buf, PROVIDER_TEXT_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn https_only_unless_the_base_url_is_http() {
        let https_base = u("https://generativelanguage.googleapis.com");
        let http_base = u("http://127.0.0.1:9000");
        assert!(hop_allowed(&u("https://storage.googleapis.com/x"), &https_base));
        assert!(!hop_allowed(&u("http://storage.googleapis.com/x"), &https_base));
        assert!(hop_allowed(&u("http://127.0.0.1:9001/x"), &http_base));
        assert!(hop_allowed(&u("https://example.com/x"), &http_base));
        assert!(!hop_allowed(&u("ftp://example.com/x"), &http_base));
        assert!(!hop_allowed(&u("file:///etc/passwd"), &http_base));
        assert!(!hop_allowed(&u("data:text/plain,hi"), &http_base));
    }

    #[test]
    fn error_documents_are_recognized() {
        for ct in
            ["application/json", "application/problem+json", "text/html", "text/plain", "application/xml"]
        {
            assert!(is_error_document(ct), "{ct}");
        }
        for ct in ["video/mp4", "image/png", "application/octet-stream", "image/svg+xml"] {
            assert!(!is_error_document(ct), "{ct}");
        }
    }

    #[test]
    fn status_errors_map_to_the_public_taxonomy() {
        let status = |s: u16| DownloadError::Status {
            status: s,
            body_snippet: String::new(),
            retry_after: None,
            retry_after_limit: None,
            attempts: 1,
            url: "https://x/".into(),
        };
        assert_eq!(status(410).into_iris().code, ErrorCode::ArtifactExpired);
        for s in [403, 404] {
            let e = status(s).into_iris();
            assert_eq!(
                (e.code, e.retryable, e.provider_status),
                (ErrorCode::DownloadFailed, Some(true), Some(s))
            );
        }
        assert_eq!(status(401).into_iris().code, ErrorCode::AuthenticationFailed);
        assert_eq!(status(429).into_iris().code, ErrorCode::RateLimited);
        let e = status(503).into_iris();
        assert_eq!(e.code, ErrorCode::DownloadFailed);
        assert_eq!(e.retryable, Some(true));
        assert_eq!(status(400).into_iris().retryable, Some(false));
        let e = DownloadError::Refused { message: "no".into() }.into_iris();
        assert_eq!((e.code, e.retryable), (ErrorCode::DownloadFailed, Some(false)));
        assert_eq!(
            DownloadError::Internal { message: "bug".into() }.into_iris().code,
            ErrorCode::InternalError
        );
    }

    #[test]
    fn a_retry_after_beyond_the_limit_reports_rate_limited_for_any_status() {
        let over = |s: u16| DownloadError::Status {
            status: s,
            body_snippet: String::new(),
            retry_after: Some(Duration::from_secs(600)),
            retry_after_limit: Some(Duration::from_secs(60)),
            attempts: 1,
            url: "https://x/".into(),
        };
        for s in [408u16, 429, 500, 503] {
            let e = over(s).into_iris();
            assert_eq!(e.code, ErrorCode::RateLimited, "{s}");
            assert_eq!((e.retryable, e.retry_after), (Some(true), Some(Duration::from_secs(600))), "{s}");
            assert_eq!(e.provider_status, Some(s));
        }
    }
}
