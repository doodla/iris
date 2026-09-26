//! Google Gemini Developer API adapter: native image generation and editing
//! ("Nano Banana", `generateContent`, synchronous) and Veo video generation
//! (`predictLongRunning`, provider-native asynchronous operations).
//!
//! A thin REST client: Google publishes no Rust SDK, and no community crate
//! was verified to cover `imageConfig`, `thinkingLevel`, and Veo on these API
//! versions; the surface is three calls. All Google wire types stay in
//! this module. See docs/contributing/architecture.md for the shared traits and, in "Where
//! invariants live", the retry classes; the model catalog declares option values
//! and defaults. Wire mapping and response/error rules live in this adapter.
//!
//! * The configured base URL is the origin (`https://generativelanguage.googleapis.com`);
//!   the adapter appends `/v1` (images, image-model metadata) or `/v1beta`
//!   (Veo, operations, files).
//! * The key is sent only in the `x-goog-api-key` header, never as `?key=`.
//! * Paid calls use the `PaidSubmit` retry class; polls and metadata use
//!   `IdempotentRead`.

mod client;
mod image;
mod veo;
mod wire;

use std::time::Duration;

use async_trait::async_trait;

pub use client::{API_V1, API_V1BETA};
pub use veo::{check_output_uri, is_operation_name, validate_output_uri};

use super::{
    AccountAccess, CredentialHeader, ImageFailure, ImageOutput, ImageProvider, ImageRequest, Provider,
    ProviderContext, RemoteStatus, SubmittedOperation, VideoProvider, VideoRequest,
};
use crate::catalog;
use crate::domain::{Operation, ProviderId};
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpError, RetryClass};

/// The Gemini API adapter (images and Veo).
#[derive(Debug, Default)]
pub struct GeminiProvider;

impl GeminiProvider {
    pub fn new() -> Self {
        GeminiProvider
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Gemini
    }

    fn credential_header(&self) -> CredentialHeader {
        client::CREDENTIAL_HEADER
    }

    fn docs_url(&self) -> &'static str {
        "https://ai.google.dev/gemini-api/docs"
    }

    /// `models.get` (free, `IdempotentRead`): 200 → available, 404 → unavailable,
    /// rejected credentials (401, 403, 400 `API_KEY_*`) → the error, anything else
    /// (rate limits, server or network errors) → unknown.
    ///
    /// Image models are looked up on `v1` (the version their generation call uses),
    /// Veo and unknown models on `v1beta` (Veo exists only there).
    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError> {
        client::validate_model_id(model_id)?;
        let version = match catalog::find(model_id) {
            Some(spec) if spec.provider == ProviderId::Gemini && !spec.supports(Operation::VideoGenerate) => {
                API_V1
            }
            _ => API_V1BETA,
        };
        let auth = client::auth(ctx)?;
        let url = client::endpoint(ctx, version, &format!("models/{model_id}"));
        let call = client::call(RetryClass::IdempotentRead, ctx.timeouts.poll);
        match ctx.http.execute(&call, |c| Ok(auth.apply(c.get(&url))), client::classify).await {
            Ok(_) => Ok(AccountAccess::Available),
            Err(HttpError::Error(e)) if e.provider_status == Some(404) => Ok(AccountAccess::Unavailable),
            Err(HttpError::Error(e))
                if e.code == ErrorCode::AuthenticationFailed || e.provider_status == Some(403) =>
            {
                Err(e)
            }
            Err(other) => {
                let err = other.into_iris();
                tracing::debug!(code = %err.code, "gemini access check inconclusive");
                Ok(AccountAccess::Unknown)
            }
        }
    }

    fn image(&self) -> Option<&dyn ImageProvider> {
        Some(self)
    }

    fn video(&self) -> Option<&dyn VideoProvider> {
        Some(self)
    }
}

#[async_trait]
impl ImageProvider for GeminiProvider {
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        image::run(Operation::ImageGenerate, req, ctx).await
    }

    async fn edit(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        image::run(Operation::ImageEdit, req, ctx).await
    }
}

#[async_trait]
impl VideoProvider for GeminiProvider {
    fn validate(&self, req: &VideoRequest) -> Result<(), IrisError> {
        veo::validate(req)
    }

    async fn submit(
        &self,
        req: &VideoRequest,
        ctx: &ProviderContext,
    ) -> Result<SubmittedOperation, IrisError> {
        veo::submit(req, ctx).await
    }

    async fn poll(&self, remote_id: &str, ctx: &ProviderContext) -> Result<RemoteStatus, IrisError> {
        veo::poll(remote_id, ctx).await
    }

    /// "Generated videos are stored on the server for 2 days" (Veo guide).
    fn output_retention(&self) -> Option<Duration> {
        Some(Duration::from_secs(catalog::veo::OUTPUT_RETENTION_HOURS * 3600))
    }

    /// Files API download URLs under the configured base URL only.
    fn check_output_uri(&self, uri: &str, base_url: &url::Url) -> Result<(), IrisError> {
        veo::check_output_uri(uri, base_url)
    }
}
