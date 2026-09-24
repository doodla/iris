//! Provider abstraction: traits the application uses, and the built-in registry.
//!
//! Synchronous image generation and provider-native asynchronous video jobs are
//! deliberately different traits: image calls return media in the response;
//! video calls return an operation id that must be persisted and polled.
//!
//! Provider wire types live only inside `providers/<name>/`.

pub mod gemini;
pub mod openai;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

use crate::catalog::ResolvedOptions;
use crate::domain::{ProviderId, Usage, Warning};
use crate::error::IrisError;
use crate::http::{HttpClient, Timeouts};
use crate::secret::Secret;

/// Role of an input image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRole {
    /// Image to edit / reference for `image.edit`.
    Image,
    /// Mask for `image.edit`.
    Mask,
    /// First frame for image-to-video.
    FirstFrame,
    /// Last frame for interpolation.
    LastFrame,
    /// Reference (subject/style/asset) image for video.
    Reference,
}

/// A local input image, already read and validated (size, sniffed media type).
#[derive(Clone)]
pub struct InputImage {
    pub role: InputRole,
    pub path: PathBuf,
    /// File name without directories, for multipart uploads.
    pub file_name: String,
    /// Media type sniffed from content (e.g. `image/png`).
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for InputImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputImage")
            .field("role", &self.role)
            .field("file_name", &self.file_name)
            .field("media_type", &self.media_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Validated request for `image.generate` / `image.edit`. The operation is the
/// [`ImageProvider`] method it is passed to.
#[derive(Debug, Clone)]
pub struct ImageRequest {
    /// Model id to send.
    pub model: String,
    pub prompt: String,
    /// Empty for generate; at least one for edit.
    pub images: Vec<InputImage>,
    pub mask: Option<InputImage>,
    pub options: ResolvedOptions,
}

/// Validated request for `video.generate`.
#[derive(Debug, Clone)]
pub struct VideoRequest {
    pub model: String,
    pub prompt: String,
    pub first_frame: Option<InputImage>,
    pub last_frame: Option<InputImage>,
    pub references: Vec<InputImage>,
    pub options: ResolvedOptions,
}

/// One generated image returned inline by a synchronous provider.
#[derive(Clone)]
pub struct GeneratedImage {
    /// Media type sniffed from the bytes (the provider's label may differ; see
    /// `output_format_mismatch`).
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for GeneratedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeneratedImage")
            .field("media_type", &self.media_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Result of a synchronous image call.
#[derive(Debug, Clone, Default)]
pub struct ImageOutput {
    pub images: Vec<GeneratedImage>,
    /// Text returned alongside images (Gemini text parts, OpenAI revised prompt).
    pub text: Option<String>,
    pub usage: Option<Usage>,
    pub provider_request_id: Option<String>,
    pub warnings: Vec<Warning>,
}

/// A provider accepted an asynchronous job.
#[derive(Debug, Clone)]
pub struct SubmittedOperation {
    /// Provider operation id/name used for polling (e.g. `models/…/operations/…`).
    pub remote_id: String,
    pub provider_request_id: Option<String>,
}

/// A remote output reference of a finished async job.
#[derive(Debug, Clone)]
pub struct RemoteArtifact {
    pub uri: String,
    pub media_type: Option<String>,
}

/// Normalized remote status of an async job.
#[derive(Debug, Clone)]
pub enum RemoteStatus {
    Running {
        progress: Option<f32>,
    },
    Succeeded {
        outputs: Vec<RemoteArtifact>,
        usage: Option<Usage>,
        warnings: Vec<Warning>,
    },
    /// The provider reports the job failed (or was blocked). `error.code` is e.g.
    /// `remote_job_failed` or `content_blocked`.
    Failed {
        error: IrisError,
    },
    /// The provider no longer knows the operation (retention passed).
    Gone,
}

/// Whether the current account can use a model, from a free metadata call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccess {
    NotChecked,
    Available,
    Unavailable,
    Unknown,
}

/// How a provider authenticates: header name and value prefix.
#[derive(Debug, Clone, Copy)]
pub struct CredentialHeader {
    /// Lowercase header name, e.g. `authorization` or `x-goog-api-key`.
    pub name: &'static str,
    /// Prefix before the key, e.g. `Bearer `; empty for raw keys.
    pub prefix: &'static str,
}

/// Everything an adapter needs for one call. Built by the app from config.
#[derive(Debug, Clone)]
pub struct ProviderContext {
    pub http: HttpClient,
    /// Configured API base URL (e.g. `https://api.openai.com/v1`). Credentials are
    /// only ever sent to this origin.
    pub base_url: url::Url,
    pub credential: Secret,
    pub timeouts: Timeouts,
}

/// Common provider surface.
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn default_base_url(&self) -> &'static str;
    fn credential_header(&self) -> CredentialHeader;
    fn docs_url(&self) -> &'static str;

    /// Free metadata call (model lookup) to check whether this account can see `model_id`.
    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError>;

    fn image(&self) -> Option<&dyn ImageProvider> {
        None
    }

    fn video(&self) -> Option<&dyn VideoProvider> {
        None
    }
}

/// Synchronous image generation/editing. Paid, non-idempotent: implementations use the
/// `PaidSubmit` retry class and never retry ambiguous failures.
#[async_trait]
pub trait ImageProvider: Send + Sync {
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, IrisError>;
    async fn edit(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, IrisError>;
}

/// Provider-native asynchronous video jobs.
#[async_trait]
pub trait VideoProvider: Send + Sync {
    /// Paid, non-idempotent submission. On an ambiguous failure (sent but no usable
    /// answer) returns `submission_uncertain`; on a definite rejection returns the
    /// mapped error. Never retries except as the `PaidSubmit` retry class allows.
    async fn submit(
        &self,
        req: &VideoRequest,
        ctx: &ProviderContext,
    ) -> Result<SubmittedOperation, IrisError>;

    /// Idempotent status read (`IdempotentRead` retry class).
    async fn poll(&self, remote_id: &str, ctx: &ProviderContext) -> Result<RemoteStatus, IrisError>;

    /// Documented server-side retention of generated outputs, if any.
    fn output_retention(&self) -> Option<std::time::Duration>;

    /// Whether Iris should fetch `uri`, an output of a succeeded job, given the base
    /// URL configured now. Called before every download attempt (never at poll time,
    /// so a refusal never changes the job's status). A refusal is returned as the
    /// error to record on that output (normally `download_failed`, not retryable as
    /// is, with the redacted URI and a hint). The default accepts every URI: the
    /// downloader's credential-origin rule applies either way.
    fn check_output_uri(&self, uri: &str, base_url: &url::Url) -> Result<(), IrisError> {
        let _ = (uri, base_url);
        Ok(())
    }
}

/// Built-in providers. Adding a provider = one line here + adapter + catalog + tests.
#[derive(Clone)]
pub struct Registry {
    providers: Vec<Arc<dyn Provider>>,
}

impl Registry {
    pub fn builtin() -> Self {
        Registry {
            providers: vec![Arc::new(openai::OpenAiProvider::new()), Arc::new(gemini::GeminiProvider::new())],
        }
    }

    /// A registry with explicit providers (tests inject fakes through this).
    pub fn with_providers(providers: Vec<Arc<dyn Provider>>) -> Self {
        Registry { providers }
    }

    pub fn get(&self, id: ProviderId) -> Option<&dyn Provider> {
        self.providers.iter().find(|p| p.id() == id).map(|p| p.as_ref())
    }

    pub fn all(&self) -> impl Iterator<Item = &dyn Provider> {
        self.providers.iter().map(|p| p.as_ref())
    }
}
