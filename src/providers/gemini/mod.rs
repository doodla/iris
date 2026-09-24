//! Google Gemini API adapter (native image generation "Nano Banana" and Veo video).
//! Implemented by task T-09 per contracts C-01/C-04/C-06.

use async_trait::async_trait;

use super::{
    AccountAccess, CredentialHeader, ImageOutput, ImageProvider, ImageRequest, Provider, ProviderContext,
    RemoteStatus, SubmittedOperation, VideoProvider, VideoRequest,
};
use crate::domain::ProviderId;
use crate::error::IrisError;

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
    fn default_base_url(&self) -> &'static str {
        "https://generativelanguage.googleapis.com/v1beta"
    }
    fn credential_header(&self) -> CredentialHeader {
        CredentialHeader { name: "x-goog-api-key", prefix: "" }
    }
    fn docs_url(&self) -> &'static str {
        "https://ai.google.dev/gemini-api/docs"
    }
    async fn check_access(
        &self,
        _model_id: &str,
        _ctx: &ProviderContext,
    ) -> Result<AccountAccess, IrisError> {
        Err(IrisError::internal("gemini adapter not implemented yet"))
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
    async fn generate(&self, _req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        Err(IrisError::internal("gemini adapter not implemented yet"))
    }
    async fn edit(&self, _req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        Err(IrisError::internal("gemini adapter not implemented yet"))
    }
}

#[async_trait]
impl VideoProvider for GeminiProvider {
    async fn submit(
        &self,
        _req: &VideoRequest,
        _ctx: &ProviderContext,
    ) -> Result<SubmittedOperation, IrisError> {
        Err(IrisError::internal("veo adapter not implemented yet"))
    }
    async fn poll(&self, _remote_id: &str, _ctx: &ProviderContext) -> Result<RemoteStatus, IrisError> {
        Err(IrisError::internal("veo adapter not implemented yet"))
    }
    fn output_retention(&self) -> Option<std::time::Duration> {
        None
    }
}
