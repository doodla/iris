//! OpenAI Images API adapter (see docs/contributing/architecture.md, docs/concepts/video-jobs.md, and the model catalog).
//!
//! A thin REST client over the shared HTTP layer:
//!
//! * `image.generate` → `POST {base}/images/generations` (JSON);
//! * `image.edit` → `POST {base}/images/edits` (JSON body with every input image and
//!   the optional mask as a `data:<mime>;base64,…` URL);
//! * `check_access` → `GET {base}/models/{model}` (free metadata read).
//!
//! The Images API is synchronous: images come back inline as base64, there is no job
//! id, no retrieval endpoint, and no idempotency key (API reference, OpenAPI spec). Paid calls
//! therefore run under the `PaidSubmit` retry class, and an answer lost after sending
//! (or an HTTP 408/5xx answer other than the documented overload rejection) is reported
//! as `submission_uncertain` with `details.charge_possible`, never resent.
//!
//! Every request sends `model` explicitly (the documented default is a removed
//! model), `Authorization: Bearer <key>`, and a fresh ULID in `X-Client-Request-Id`;
//! OpenAI's `x-request-id` is captured on success and on errors. Only options the
//! adapter maps are sent; anything else is an `internal_error`, never dropped.

mod client;
mod wire;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::openai_compat::{ExpectedItem, ImagesApi, ImagesResponse, decode_items};
use super::{
    AccountAccess, CredentialHeader, ImageFailure, ImageOutput, ImageProvider, ImageRequest, InputImage,
    Provider, ProviderContext, TypeExpectation, with_reported_usage,
};
use crate::artifacts::media;
use crate::domain::{ProviderId, Warning, WarningCode};
use crate::error::{ErrorCode, IrisError};
use crate::http::{AuthHeader, HttpResponse};
use wire::{EditBody, GenerateBody, ImageRef, OUTPUT_FORMATS, WireOptions};

/// Official image generation guide.
pub const DOCS_URL: &str = crate::catalog::openai::DOCS_URL;
/// Longest data URL the JSON edit body accepts per image (`image_url` maxLength).
pub const DATA_URL_MAX_CHARS: usize = 20_971_520;
/// Largest mask Iris sends: "less than 4MB" in OpenAI's spec, read as 4,000,000 bytes
/// (the stricter reading, like the catalog's decimal byte limits).
pub const MASK_MAX_BYTES: usize = 4_000_000;
/// Most input images one edit accepts (`images` maxItems).
pub const MAX_EDIT_IMAGES: usize = 16;

const CREDENTIAL_HEADER: CredentialHeader = CredentialHeader { name: "authorization", prefix: "Bearer " };

/// Input and mask media types the Images API accepts.
const INPUT_MEDIA_TYPES: &[&str] = &[media::PNG, media::JPEG, media::WEBP];

/// The OpenAI provider: image generation and editing through the Images API.
#[derive(Debug, Default)]
pub struct OpenAiProvider;

impl OpenAiProvider {
    pub fn new() -> Self {
        OpenAiProvider
    }

    fn auth(&self, ctx: &ProviderContext) -> Result<AuthHeader, IrisError> {
        AuthHeader::new(CREDENTIAL_HEADER.name, CREDENTIAL_HEADER.prefix, &ctx.credential)
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> ProviderId {
        ProviderId::OpenAi
    }

    fn credential_header(&self) -> CredentialHeader {
        CREDENTIAL_HEADER
    }

    fn docs_url(&self) -> &'static str {
        DOCS_URL
    }

    /// `GET {base}/models/{model}` (free): 200 → available, 404 → unavailable,
    /// 401 → `authentication_failed`, other answers → unknown; transport failures are
    /// errors (`network_error` / `request_timeout`).
    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError> {
        client::check_access(model_id, ctx).await
    }

    fn image(&self) -> Option<&dyn ImageProvider> {
        Some(self)
    }
}

#[async_trait]
impl ImageProvider for OpenAiProvider {
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        // Input images would be dropped from a generation body: never send one silently.
        if !req.images.is_empty() || req.mask.is_some() {
            return Err(IrisError::internal(
                "the OpenAI adapter's generate() received input images or a mask; nothing was sent",
            )
            .into());
        }
        let options = WireOptions::from_resolved(&req.options)?;
        let expect = Expected::of(&options);
        let body = GenerateBody { model: &req.model, prompt: &req.prompt, options };
        let body = serde_json::to_vec(&body)
            .map_err(|e| IrisError::internal(format!("could not encode the OpenAI request: {e}")))?;
        let auth = self.auth(ctx)?;
        let resp = client::post_paid(ctx, &auth, "images/generations", body.into(), &req.model).await?;
        decode_images(&resp, &expect)
    }

    async fn edit(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure> {
        if req.images.is_empty() {
            return Err(IrisError::usage("image edit requires at least one --image").into());
        }
        if req.images.len() > MAX_EDIT_IMAGES {
            return Err(IrisError::invalid(format!(
                "OpenAI accepts at most {MAX_EDIT_IMAGES} input images per edit; got {}",
                req.images.len()
            ))
            .into());
        }
        let options = WireOptions::from_resolved(&req.options)?;
        let expect = Expected::of(&options);
        if let Some(mask) = &req.mask {
            check_mask(mask, &req.images[0])?;
        }
        let images = req
            .images
            .iter()
            .map(|image| {
                data_url(image, "input image", INPUT_MEDIA_TYPES).map(|image_url| ImageRef { image_url })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mask = match &req.mask {
            Some(mask) => Some(ImageRef { image_url: data_url(mask, "mask image", &[media::PNG])? }),
            None => None,
        };
        let body = EditBody { model: &req.model, prompt: &req.prompt, images, mask, options };
        let body = serde_json::to_vec(&body)
            .map_err(|e| IrisError::internal(format!("could not encode the OpenAI request: {e}")))?;
        let auth = self.auth(ctx)?;
        let resp = client::post_paid(ctx, &auth, "images/edits", body.into(), &req.model).await?;
        decode_images(&resp, &expect)
    }
}

fn input_invalid(image: &InputImage, message: String) -> IrisError {
    IrisError::new(ErrorCode::InputFileInvalid, message).with_detail("path", image.path.display().to_string())
}

/// `data:<mime>;base64,<bytes>` for an input image or mask. The media type is sniffed
/// from the bytes (never taken from the file name) and must be in `accepted`; the
/// URL must fit the API's 20,971,520-character limit. Nothing is sent on failure.
fn data_url(image: &InputImage, role: &str, accepted: &[&str]) -> Result<String, IrisError> {
    let shown = image.path.display();
    let Some(media_type) = media::sniff(&image.bytes).filter(|t| accepted.contains(t)) else {
        return Err(input_invalid(
            image,
            format!("{role} {shown} is not {}; OpenAI does not accept it", accepted.join(", ")),
        ));
    };
    let prefix = format!("data:{media_type};base64,");
    let chars = base64::encoded_len(image.bytes.len(), true)
        .and_then(|n| n.checked_add(prefix.len()))
        .unwrap_or(usize::MAX);
    if chars > DATA_URL_MAX_CHARS {
        return Err(input_invalid(
            image,
            format!(
                "{role} {shown} is {} bytes, which encodes to {chars} characters; OpenAI accepts at most \
                 {DATA_URL_MAX_CHARS} characters per image (about 15.7 MB of image data)",
                image.bytes.len()
            ),
        ));
    }
    let mut url = String::with_capacity(chars);
    url.push_str(&prefix);
    STANDARD.encode_string(&image.bytes, &mut url);
    Ok(url)
}

/// Mask rules the catalog cannot express: a PNG with an alpha channel, at most
/// 4 MB, with the same dimensions as the FIRST input image. Checked by decoding,
/// before anything is sent.
fn check_mask(mask: &InputImage, first: &InputImage) -> Result<(), IrisError> {
    let shown = mask.path.display();
    if mask.bytes.len() > MASK_MAX_BYTES {
        return Err(input_invalid(
            mask,
            format!(
                "mask image {shown} is {} bytes; OpenAI accepts masks smaller than 4 MB ({MASK_MAX_BYTES} bytes)",
                mask.bytes.len()
            ),
        ));
    }
    let info = media::png_info(&mask.bytes).map_err(|e| {
        input_invalid(mask, format!("mask image {shown} must be a PNG with an alpha channel: {}", e.message))
    })?;
    if !info.has_alpha {
        return Err(input_invalid(
            mask,
            format!(
                "mask image {shown} has no alpha channel; OpenAI edits the areas where the mask is fully \
                 transparent, so the mask needs transparency"
            ),
        ));
    }
    let base = media::inspect_image(&first.bytes).map_err(|e| {
        input_invalid(
            first,
            format!("cannot read the dimensions of input image {}: {}", first.path.display(), e.message),
        )
    })?;
    if (info.width, info.height) != (base.width, base.height) {
        return Err(input_invalid(
            mask,
            format!(
                "mask image {shown} is {}x{} but the first input image {} is {}x{}; the mask applies to the \
                 first image and must have the same dimensions",
                info.width,
                info.height,
                first.path.display(),
                base.width,
                base.height
            ),
        ));
    }
    Ok(())
}

/// Media type of an `output_format` value.
fn media_type_for_format(format: &str) -> Option<&'static str> {
    match format {
        "png" => Some(media::PNG),
        "jpeg" => Some(media::JPEG),
        "webp" => Some(media::WEBP),
        _ => None,
    }
}

/// What the Images API returns: the count is sent as `n`, and GPT Image models
/// return base64 only.
const IMAGES_API: ImagesApi =
    ImagesApi { who: client::WORDING.who, count_field: Some("n"), base64_only: Some("GPT image models") };

/// What a paid request asked for, to check the response against.
struct Expected {
    /// `output_format` sent; `None` means the provider default (png).
    format: Option<String>,
    /// `n` sent; `None` means the provider default (1).
    count: Option<i64>,
}

impl Expected {
    fn of(options: &WireOptions) -> Expected {
        Expected { format: options.output_format.clone(), count: options.n }
    }
}

/// Decode an `ImagesResponse`: every `data[].b64_json` (standard base64), typed by
/// its magic bytes and checked against the requested and echoed `output_format`
/// (with neither, the provider default png). Items are decoded one by one, and
/// paid output is never discarded because of another item:
///
/// * A valid image of another type is kept under its real media type, with the
///   difference in [`GeneratedImage::format_mismatch`], which the app reports as
///   `output_format_mismatch`: the request completed and may have been billed (see
///   `iris --help`).
/// * An item that cannot be used (a URL instead of inline data, missing or invalid
///   base64, content that is not a recognized image) is skipped with warning
///   `output_item_unusable` naming it; every usable item is kept, and the content of
///   a skipped item is handed to the app (`unusable`) to be saved as received.
/// * Warnings name items by their position in `data[]` ("response item N"), which
///   differs from the artifact index once an earlier item was skipped.
/// * Only a response with no usable item at all is `provider_bad_response` (with
///   that content in the failure, and the usage the response reported). OpenAI does
///   not document whether such a response is billed, so it is `charge_possible`, not
///   `charged`.
/// * A number of items other than the requested `n` is reported with warning
///   `unexpected_output_count`.
fn decode_images(resp: &HttpResponse, expect: &Expected) -> Result<ImageOutput, ImageFailure> {
    let parsed = ImagesResponse::parse(&resp.body).map_err(|why| client::bad_response(resp, &why))?;
    let usage = parsed.usage.as_ref().and_then(wire::parse_usage);
    if parsed.data.is_empty() {
        let err = client::bad_response(resp, "it contains no images");
        return Err(with_reported_usage(err, usage.as_ref()).into());
    }
    let format_media_type = |format: &str| {
        media_type_for_format(format)
            .ok_or_else(|| IrisError::internal(format!("unexpected output format '{format}'")))
    };
    // Every statement about the type the images should have, with its media type.
    let mut expectations: Vec<(TypeExpectation, &'static str)> = Vec::new();
    if let Some(format) = expect.format.as_deref() {
        expectations.push((TypeExpectation::Requested(format.to_string()), format_media_type(format)?));
    }
    if let Some(format) = parsed.output_format.as_deref().filter(|f| OUTPUT_FORMATS.contains(f)) {
        expectations.push((TypeExpectation::Declared(format.to_string()), format_media_type(format)?));
    }
    if expectations.is_empty() {
        expectations.push((TypeExpectation::ProviderDefault("png".to_string()), media::PNG));
    }
    // The echo (pushed last) is the most specific statement.
    let expected = expectations.last().map_or(media::PNG, |(_, media_type)| media_type);

    let mut decoded = decode_items(parsed.data, ExpectedItem::MediaType(expected), &IMAGES_API);
    if decoded.images.is_empty() {
        let mut err = client::bad_response(resp, &decoded.reasons());
        if let Some(actual) = decoded.problems.first().and_then(|p| p.actual) {
            err = err.with_detail("expected_media_type", expected).with_detail("actual_media_type", actual);
        }
        let err = with_reported_usage(err, usage.as_ref());
        return Err(ImageFailure { error: err, unusable: decoded.unusable });
    }

    for image in &mut decoded.images {
        image.format_mismatch = expectations
            .iter()
            .filter(|(_, media_type)| *media_type != image.media_type)
            .map(|(expectation, _)| expectation.clone())
            .collect();
    }
    let mut warnings = decoded.unusable_warnings();
    warnings.extend(decoded.count_warning(&IMAGES_API, expect.count.unwrap_or(1)));

    // Text in the result is always reported with `provider_text_output`
    // (docs/reference/json-output.md), as the Gemini adapter does.
    let text = (!decoded.revised_prompts.is_empty()).then(|| decoded.revised_prompts.join("\n\n"));
    if text.is_some() {
        warnings.push(Warning::new(
            WarningCode::ProviderTextOutput,
            "OpenAI also returned a revised prompt; it is reported in the result's `text` field",
        ));
    }

    Ok(ImageOutput {
        images: decoded.images,
        unusable: decoded.unusable,
        text,
        usage,
        provider_request_id: resp.request_id.clone(),
        warnings,
    })
}
