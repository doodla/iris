//! Gemini native image generation and editing (`generateContent`, synchronous).
//!
//! One paid request per call (`PaidSubmit` retry class): only connection failures
//! before sending and 429 rate limits are retried. Images come back inline as
//! base64; there is no job id and nothing to recover later.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_PAD_INDIFFERENT, URL_SAFE_PAD_INDIFFERENT};

use super::client::{self, API_V1};
use super::wire::{
    Blob, Content, GenerateContentRequest, GenerateContentResponse, GenerationConfig, ImageConfig,
    RequestPart, ThinkingConfig,
};
use crate::artifacts::media;
use crate::catalog::gemini::MAX_REQUEST_BYTES;
use crate::domain::{Operation, ProviderId, Usage, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpError, RetryClass};
use crate::providers::{GeneratedImage, ImageOutput, ImageRequest, ProviderContext};
use crate::redact;

/// `finishReason` values meaning the output was blocked (per the Gemini API's error codes).
const BLOCKING_FINISH_REASONS: &[&str] = &[
    "SAFETY",
    "IMAGE_SAFETY",
    "PROHIBITED_CONTENT",
    "IMAGE_PROHIBITED_CONTENT",
    "BLOCKLIST",
    "SPII",
    "RECITATION",
    "IMAGE_RECITATION",
    "LANGUAGE",
];

/// Warning code: the model returned text parts next to (or instead of) images.
pub const WARNING_TEXT_OUTPUT: &str = "provider_text_output";
/// Warning code: the provider returned more images than requested (all are kept).
pub const WARNING_OUTPUT_COUNT: &str = "unexpected_output_count";

/// Run `image.generate` or `image.edit` (`op`) against `generateContent`.
pub async fn run(op: Operation, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, IrisError> {
    check_request_shape(op, req)?;
    client::validate_model_id(&req.model)?;
    let body = encode_request(req)?;
    let auth = client::auth(ctx)?;
    let url = client::endpoint(ctx, API_V1, &format!("models/{}:generateContent", req.model));
    let call = client::call(RetryClass::PaidSubmit, ctx.timeouts.generate);

    let resp = ctx
        .http
        .execute(
            &call,
            |c| {
                Ok(auth
                    .apply(c.post(&url))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone()))
            },
            client::classify,
        )
        .await
        .map_err(paid_call_error)?;

    let request_id = client::request_id(&resp);
    let status = resp.status.as_u16();
    let parsed: GenerateContentResponse = resp.json().map_err(|_| {
        charged_bad_response(
            "the Gemini API answered with a body Iris could not parse as a generateContent response",
            status,
        )
        .with_provider_request_id(request_id.clone())
    })?;
    interpret(parsed, status, requested_count(req), request_id)
}

/// Requests reaching the adapter were validated against the catalog; anything the
/// adapter cannot express is an internal error, never silently dropped.
fn check_request_shape(op: Operation, req: &ImageRequest) -> Result<(), IrisError> {
    if req.operation != op {
        return Err(IrisError::internal(format!(
            "the Gemini image adapter was asked to run {op} with a {} request",
            req.operation
        )));
    }
    if req.mask.is_some() {
        return Err(IrisError::internal(
            "Gemini image models take no mask; the request should have been rejected",
        ));
    }
    match op {
        Operation::ImageGenerate if !req.images.is_empty() => {
            Err(IrisError::internal("image.generate request carries input images"))
        }
        Operation::ImageEdit if req.images.is_empty() => {
            Err(IrisError::internal("image.edit request carries no input images"))
        }
        Operation::VideoGenerate => Err(IrisError::internal("video.generate is not an image operation")),
        _ => Ok(()),
    }
}

fn requested_count(req: &ImageRequest) -> usize {
    req.options.get("count").and_then(|v| v.as_int()).map_or(1, |n| n.max(1) as usize)
}

/// Build the JSON body: the prompt text part first, then one `inlineData` part per
/// input image in order; `responseModalities: ["IMAGE"]`, `store: false`, and
/// `imageConfig`/`thinkingConfig` only for options the user set. Fails with
/// `invalid_argument` (nothing sent) if the encoded body exceeds the 20 MB cap.
pub fn encode_request(req: &ImageRequest) -> Result<bytes::Bytes, IrisError> {
    let mut image_config = ImageConfig::default();
    let mut thinking_config = None;
    for (name, value) in req.options.iter() {
        match name.as_str() {
            // Validated only (always 1); candidateCount is not verified for image models.
            "count" => {
                if value.as_int() != Some(1) {
                    return Err(IrisError::internal(format!(
                        "the Gemini image adapter can only request one image (count={value})"
                    )));
                }
            }
            "aspect_ratio" => image_config.aspect_ratio = Some(str_option(name, value)?),
            "resolution" => image_config.image_size = Some(str_option(name, value)?),
            "thinking_level" => {
                let level = match str_option(name, value)? {
                    "minimal" => "MINIMAL",
                    "high" => "HIGH",
                    other => {
                        return Err(IrisError::internal(format!(
                            "the Gemini image adapter does not map thinking_level '{other}'"
                        )));
                    }
                };
                thinking_config = Some(ThinkingConfig { thinking_level: level });
            }
            other => {
                return Err(IrisError::internal(format!(
                    "the Gemini image adapter does not map option '{other}'"
                )));
            }
        }
    }
    let image_config =
        (image_config.aspect_ratio.is_some() || image_config.image_size.is_some()).then_some(image_config);

    // The inline images alone are a lower bound of the body size: reject early
    // without encoding them when that bound is already over the cap.
    let images_b64: usize = req.images.iter().map(|i| client::base64_len(i.bytes.len())).sum();
    if images_b64 > MAX_REQUEST_BYTES {
        return Err(request_too_large(images_b64, "at least "));
    }
    let mut parts = Vec::with_capacity(1 + req.images.len());
    parts.push(RequestPart { text: Some(&req.prompt), inline_data: None });
    for image in &req.images {
        parts.push(RequestPart {
            text: None,
            inline_data: Some(Blob { mime_type: &image.media_type, data: STANDARD.encode(&image.bytes) }),
        });
    }
    let body = GenerateContentRequest {
        contents: vec![Content { role: "user", parts }],
        generation_config: GenerationConfig { response_modalities: ["IMAGE"], image_config, thinking_config },
        store: false,
    };
    let bytes = serde_json::to_vec(&body)
        .map_err(|e| IrisError::internal(format!("could not encode the Gemini request: {e}")))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(request_too_large(bytes.len(), ""));
    }
    Ok(bytes::Bytes::from(bytes))
}

/// `invalid_argument` for a body over the 20 MB inline cap (nothing is sent).
fn request_too_large(size: usize, qualifier: &str) -> IrisError {
    IrisError::invalid(format!(
        "the encoded request is {qualifier}{size} bytes, above the {MAX_REQUEST_BYTES}-byte limit for inline \
         images on the Gemini API"
    ))
    .with_hint("use fewer or smaller input images (base64 adds about a third to each image)")
    .with_detail("request_bytes", size as u64)
    .with_detail("limit_bytes", MAX_REQUEST_BYTES as u64)
}

fn str_option<'a>(name: &str, value: &'a crate::catalog::OptionValue) -> Result<&'a str, IrisError> {
    value
        .as_str()
        .ok_or_else(|| IrisError::internal(format!("option '{name}' must be a string, got {value}")))
}

/// Paid synchronous call failures (see docs/json-contract.md). Neither case below is retried.
/// * A transport failure after sending is `request_timeout` with `charge_possible`.
/// * An HTTP 408 or 504 answer (`request_timeout`) arrived after
///   the request was sent. It gets the same `charge_possible` and no-retry hint.
///   Google's billing page says a request that "fails with a 400 or 500 error" is
///   not charged, and it does not name 408 or 504. docs/json-contract.md treats every timeout
///   after sending the same way, and a Veo submit that gets 408 or 504 is
///   `submission_uncertain` too.
fn paid_call_error(err: HttpError) -> IrisError {
    match err {
        HttpError::Transport(t) if t.after_send => {
            let mut e = t.to_iris();
            e.code = ErrorCode::RequestTimeout;
            e.retryable = ErrorCode::RequestTimeout.default_retryable();
            e
        }
        HttpError::Error(e) if e.code == ErrorCode::RequestTimeout => {
            e.with_detail("charge_possible", true).with_hint(
                "Iris did not retry automatically because the provider may already have processed (and \
                 billed) this request; check usage in Google AI Studio before running it again",
            )
        }
        other => other.into_iris(),
    }
}

/// A 2xx answer Iris cannot use. The request was processed, so it may be billed.
fn charged_bad_response(message: &str, status: u16) -> IrisError {
    IrisError::new(ErrorCode::ProviderBadResponse, message)
        .with_provider(ProviderId::Gemini)
        .with_provider_status(status)
        .with_detail("charge_possible", true)
        .with_hint("the provider may have billed this request; Iris did not retry automatically")
}

/// Interpret a successful `generateContent` answer.
fn interpret(
    resp: GenerateContentResponse,
    status: u16,
    requested: usize,
    header_request_id: Option<String>,
) -> Result<ImageOutput, IrisError> {
    let request_id =
        header_request_id.or_else(|| resp.response_id.as_deref().and_then(crate::http::sanitize_request_id));
    let mut images = Vec::new();
    let mut texts = Vec::new();
    let candidates = resp.candidates.as_deref().unwrap_or_default();
    for part in candidates.iter().filter_map(|c| c.content.as_ref()).flat_map(|c| c.parts.iter().flatten()) {
        if part.thought == Some(true) {
            continue;
        }
        if let Some(text) = part.text.as_deref().filter(|t| !t.trim().is_empty()) {
            texts.push(text.to_string());
        }
        let Some(blob) = &part.inline_data else { continue };
        let declared = blob.mime_type.as_deref().unwrap_or("");
        if !media::is_image(declared) {
            continue;
        }
        images.push(decode_image(declared, blob.data.as_deref().unwrap_or(""), status, &request_id)?);
    }

    let text = (!texts.is_empty()).then(|| texts.join("\n"));
    let usage = resp.usage_metadata.as_ref().and_then(usage_from_metadata);

    if images.is_empty() {
        return Err(no_image_error(&resp, text.as_deref())
            .with_provider_status(status)
            .with_provider_request_id(request_id));
    }

    let mut warnings = Vec::new();
    if text.is_some() {
        warnings.push(Warning::new(
            WARNING_TEXT_OUTPUT,
            "the model also returned text; it is reported in the result's `text` field",
        ));
    }
    if images.len() > requested {
        warnings.push(Warning::new(
            WARNING_OUTPUT_COUNT,
            format!("the model returned {} images for a request of {requested}; all were kept", images.len()),
        ));
    }
    Ok(ImageOutput { images, text, usage, provider_request_id: request_id, warnings })
}

/// Decode base64 image data and check its magic bytes against the declared type.
fn decode_image(
    declared: &str,
    data: &str,
    status: u16,
    request_id: &Option<String>,
) -> Result<GeneratedImage, IrisError> {
    let bad = |message: String| {
        charged_bad_response(&message, status)
            .with_provider_request_id(request_id.clone())
            .with_detail("declared_media_type", client::safe_text(declared))
    };
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(data)
        .or_else(|_| URL_SAFE_PAD_INDIFFERENT.decode(data))
        .map_err(|_| bad("the Gemini API returned image data that is not valid base64".to_string()))?;
    let Some(sniffed) = media::sniff(&bytes) else {
        return Err(bad(format!(
            "the Gemini API returned {} bytes labeled {} that are not a recognized image",
            bytes.len(),
            client::safe_text(declared)
        )));
    };
    if !media::is_image(sniffed) || !media::accepts(&[declared], sniffed) {
        return Err(bad(format!(
            "the Gemini API labeled an image {} but its content is {sniffed}",
            client::safe_text(declared)
        ))
        .with_detail("sniffed_media_type", sniffed));
    }
    Ok(GeneratedImage { media_type: sniffed.to_string(), bytes })
}

/// Normalize `usageMetadata`: input = prompt tokens; output = candidate plus
/// thinking tokens (both billed as output); total as reported.
fn usage_from_metadata(meta: &serde_json::Value) -> Option<Usage> {
    let provider_usage = client::sanitize_usage(meta)?;
    let get = |k: &str| meta.get(k).and_then(serde_json::Value::as_u64);
    let output = match (get("candidatesTokenCount"), get("thoughtsTokenCount")) {
        (None, None) => None,
        (c, t) => Some(c.unwrap_or(0) + t.unwrap_or(0)),
    };
    Some(Usage {
        input_tokens: get("promptTokenCount"),
        output_tokens: output,
        total_tokens: get("totalTokenCount"),
        provider_usage: Some(provider_usage),
    })
}

/// Zero final images: blocked prompt/output → `content_blocked`; account limited →
/// `permission_denied`; anything else → `remote_job_failed`.
fn no_image_error(resp: &GenerateContentResponse, text: Option<&str>) -> IrisError {
    let block_reason = resp
        .prompt_feedback
        .as_ref()
        .and_then(|f| f.block_reason.as_deref())
        .filter(|r| !r.is_empty() && *r != "BLOCK_REASON_UNSPECIFIED");
    let candidates = resp.candidates.as_deref().unwrap_or_default();
    let finish_reasons: Vec<&str> = candidates.iter().filter_map(|c| c.finish_reason.as_deref()).collect();
    let finish_message = candidates
        .iter()
        .filter_map(|c| c.finish_message.as_deref())
        .chain(resp.prompt_feedback.as_ref().and_then(|f| f.block_reason_message.as_deref()))
        .next();

    let mut err = if let Some(reason) = block_reason {
        IrisError::new(ErrorCode::ContentBlocked, format!("the Gemini API blocked the prompt ({reason})"))
            .with_provider_code(client::safe_text(reason))
            .with_detail("block_reason", client::safe_text(reason))
            .with_hint("change the prompt or input images; the provider may still bill input tokens")
    } else if let Some(reason) = finish_reasons.iter().find(|r| BLOCKING_FINISH_REASONS.contains(r)) {
        IrisError::new(
            ErrorCode::ContentBlocked,
            format!("the Gemini API blocked the generated image ({reason})"),
        )
        .with_provider_code(*reason)
        .with_hint("change the prompt or input images; the provider may still bill input and thinking tokens")
    } else if finish_reasons.contains(&"PUP_LIMITED_DISABLED") {
        IrisError::new(
            ErrorCode::PermissionDenied,
            "the Gemini API refused the request: the account is limited or disabled for Prohibited Use \
             Policy violations (PUP_LIMITED_DISABLED)",
        )
        .with_provider_code("PUP_LIMITED_DISABLED")
        .with_hint("review the account status in Google AI Studio")
    } else {
        let reason = finish_reasons.first().copied();
        let described = reason.map(client::safe_text).unwrap_or_else(|| "no candidates".to_string());
        let mut e = IrisError::new(
            ErrorCode::RemoteJobFailed,
            format!("the Gemini API returned no image ({described})"),
        )
        .with_hint(
            "running the command again may succeed, but it is billed again; the provider may bill input and \
             thinking tokens for this attempt",
        );
        if let Some(r) = reason {
            e = e.with_provider_code(client::safe_text(r));
        }
        e
    };
    err = err.with_provider(ProviderId::Gemini);
    if !finish_reasons.is_empty() {
        let reasons: Vec<String> = finish_reasons.iter().map(|r| client::safe_text(r)).collect();
        err = err.with_detail("finish_reasons", reasons);
    }
    if let Some(m) = finish_message {
        err = err.with_detail("provider_message", client::safe_text(m));
    }
    if let Some(t) = text {
        err = err.with_detail("model_text", redact::truncate(&redact::scrub(t), client::PROVIDER_TEXT_MAX));
    }
    err
}
