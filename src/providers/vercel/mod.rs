//! Vercel AI Gateway adapter: image-only models through the gateway's
//! OpenAI-compatible Images endpoint (see docs/contributing/architecture.md and the
//! model catalog, `catalog::vercel`).
//!
//! A thin REST client over the shared HTTP layer:
//!
//! * `image.generate` → `POST {base}/images/generations` (JSON, `response_format:
//!   "b64_json"`, so images come back inline);
//! * `check_access` → `GET {base}/models?include_availability` (free, authenticated:
//!   the team's eligibility for each model).
//!
//! The endpoint is synchronous: there is no job id and no documented idempotency
//! key, so the paid call runs under the `PaidSubmit` retry class, and an answer lost
//! after sending is `submission_uncertain`, never resent.
//!
//! Not implemented, deliberately (docs/contributing/decisions.md, "Vercel AI Gateway"):
//! image edits, which the cataloged models are not documented to accept, and video,
//! whose asynchronous start and status calls the gateway documents only through its
//! JavaScript SDK, not as an HTTP API.

mod client;
mod wire;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;

use super::{
    AccountAccess, CredentialHeader, GeneratedImage, ImageFailure, ImageOutput, ImageProvider, ImageRequest,
    Provider, ProviderContext, UnusableOutput,
};
use crate::artifacts::media;
use crate::domain::{ProviderId, Warning, WarningCode};
use crate::error::IrisError;
use crate::http::{AuthHeader, HttpResponse};
use wire::{GenerateBody, WireImagesResponse};

/// The gateway's image generation guide.
pub const DOCS_URL: &str = crate::catalog::vercel::DOCS_URL;

const CREDENTIAL_HEADER: CredentialHeader = CredentialHeader { name: "authorization", prefix: "Bearer " };

/// The Vercel AI Gateway provider: image generation.
#[derive(Debug, Default)]
pub struct VercelProvider;

impl VercelProvider {
    pub fn new() -> Self {
        VercelProvider
    }
}

#[async_trait]
impl Provider for VercelProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Vercel
    }

    fn credential_header(&self) -> CredentialHeader {
        CREDENTIAL_HEADER
    }

    fn docs_url(&self) -> &'static str {
        DOCS_URL
    }

    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError> {
        client::check_access(model_id, ctx).await
    }

    fn image(&self) -> Option<&dyn ImageProvider> {
        Some(self)
    }
}

#[async_trait]
impl ImageProvider for VercelProvider {
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        // Input images would be dropped from a generation body: never send one silently.
        if !req.images.is_empty() || req.mask.is_some() {
            return Err(IrisError::internal(
                "the Vercel adapter's generate() received input images or a mask; nothing was sent",
            )
            .into());
        }
        let body = GenerateBody::new(&req.model, &req.prompt, &req.options)?;
        let body = serde_json::to_vec(&body)
            .map_err(|e| IrisError::internal(format!("could not encode the Vercel request: {e}")))?;
        let auth = AuthHeader::new(CREDENTIAL_HEADER.name, CREDENTIAL_HEADER.prefix, &ctx.credential)?;
        let resp = client::post_paid(ctx, &auth, "images/generations", body.into(), &req.model).await?;
        decode_images(&resp)
    }

    /// No cataloged gateway model declares `image.edit`, so validation rejects an
    /// edit before it reaches the adapter.
    async fn edit(&self, _req: &ImageRequest, _ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        Err(IrisError::internal(
            "the Vercel adapter does not implement image edits; validation should have rejected the request \
             (nothing was sent)",
        )
        .into())
    }
}

/// Decode an images response: every `data[].b64_json`, typed by its magic bytes.
/// Paid output is never discarded because of another item: an item that cannot be
/// used is skipped with `output_item_unusable`, and its content handed to the app.
/// Only a response with no usable item is `provider_bad_response`. A number of items
/// other than one is reported with `unexpected_output_count`.
fn decode_images(resp: &HttpResponse) -> Result<ImageOutput, ImageFailure> {
    let parsed = WireImagesResponse::parse(&resp.body).map_err(|why| client::bad_response(resp, &why))?;
    if parsed.data.is_empty() {
        return Err(client::bad_response(resp, "it contains no images").into());
    }
    let returned = parsed.data.len();
    let mut images = Vec::with_capacity(returned);
    let mut kept = Vec::new();
    let mut reasons = Vec::new();
    for (index, item) in parsed.data.into_iter().enumerate() {
        let Some(b64) = item.b64_json else {
            reasons.push(if item.has_url {
                format!(
                    "response item {index} is a URL instead of inline base64 data; Iris does not fetch it"
                )
            } else {
                format!("response item {index} has no b64_json data")
            });
            continue;
        };
        let bytes = match STANDARD_PAD_INDIFFERENT.decode(b64.trim()) {
            Ok(bytes) => bytes,
            Err(_) => {
                reasons.push(format!("response item {index} is not valid base64"));
                kept.push(UnusableOutput { item: index, bytes: b64.into_bytes() });
                continue;
            }
        };
        match media::sniff(&bytes) {
            Some(actual) if media::is_image(actual) => {
                images.push(GeneratedImage { item: index, media_type: actual.to_string(), bytes })
            }
            other => {
                reasons.push(format!(
                    "response item {index} should be an image but its content is {}",
                    other.unwrap_or("not a recognized image")
                ));
                kept.push(UnusableOutput { item: index, bytes });
            }
        }
    }
    if images.is_empty() {
        return Err(ImageFailure { error: client::bad_response(resp, &reasons.join("; ")), unusable: kept });
    }
    let mut warnings: Vec<Warning> = reasons
        .into_iter()
        .map(|why| {
            Warning::new(
                WarningCode::OutputItemUnusable,
                format!("{why}; it was skipped and every usable image was kept"),
            )
        })
        .collect();
    if returned != 1 {
        warnings.push(Warning::new(
            WarningCode::UnexpectedOutputCount,
            format!(
                "the Vercel AI Gateway returned {returned} items ({} usable) for a request of 1; every usable image \
                 was kept",
                images.len()
            ),
        ));
    }
    Ok(ImageOutput {
        images,
        unusable: kept,
        text: None,
        usage: None,
        provider_request_id: resp.request_id.clone(),
        warnings,
    })
}
