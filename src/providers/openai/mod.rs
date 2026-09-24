//! OpenAI Images API adapter. Implemented by task T-08 per contracts C-01/C-04/C-06.

use async_trait::async_trait;

use super::{
    AccountAccess, CredentialHeader, ImageOutput, ImageProvider, ImageRequest, Provider, ProviderContext,
};
use crate::domain::ProviderId;
use crate::error::IrisError;

#[derive(Debug, Default)]
pub struct OpenAiProvider;

impl OpenAiProvider {
    pub fn new() -> Self {
        OpenAiProvider
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenAi
    }
    fn default_base_url(&self) -> &'static str {
        "https://api.openai.com/v1"
    }
    fn credential_header(&self) -> CredentialHeader {
        CredentialHeader { name: "authorization", prefix: "Bearer " }
    }
    fn docs_url(&self) -> &'static str {
        "https://platform.openai.com/docs/guides/image-generation"
    }
    async fn check_access(
        &self,
        _model_id: &str,
        _ctx: &ProviderContext,
    ) -> Result<AccountAccess, IrisError> {
        Err(IrisError::internal("openai adapter not implemented yet"))
    }
    fn image(&self) -> Option<&dyn ImageProvider> {
        Some(self)
    }
}

#[async_trait]
impl ImageProvider for OpenAiProvider {
    async fn generate(&self, _req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        Err(IrisError::internal("openai adapter not implemented yet"))
    }
    async fn edit(&self, _req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
        Err(IrisError::internal("openai adapter not implemented yet"))
    }
}
