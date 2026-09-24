//! Shared HTTP plumbing: client construction, operation-aware retries, error
//! classification helpers, streaming downloads with credential-origin rules, and
//! redaction. Implemented by task T-06 per contract C-04.

use std::time::Duration;

/// Shared HTTP client (cheap to clone).
#[derive(Debug, Clone)]
pub struct HttpClient {
    pub(crate) inner: reqwest::Client,
}

impl HttpClient {
    /// Wrap an existing reqwest client (T-06 adds the configured constructor).
    pub fn from_reqwest(inner: reqwest::Client) -> Self {
        HttpClient { inner }
    }

    /// The underlying reqwest client.
    pub fn reqwest(&self) -> &reqwest::Client {
        &self.inner
    }
}

/// Per-provider timeouts (C-04).
#[derive(Debug, Clone, Copy)]
pub struct Timeouts {
    pub connect: Duration,
    /// Synchronous paid generation (image) request.
    pub generate: Duration,
    /// Async job submission request.
    pub submit: Duration,
    /// Status poll request.
    pub poll: Duration,
    /// Download read-idle timeout.
    pub download_idle: Duration,
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
