//! Gemini native image generation and editing (`generateContent`, synchronous).
//!
//! One paid request per call (`PaidSubmit` retry class): only connection failures
//! before sending and 429 rate limits are retried. Images come back inline as
//! base64; there is no job id and nothing to recover later, so a request that may
//! have been processed without an answer reaching Iris is `submission_uncertain`.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_PAD_INDIFFERENT, URL_SAFE_PAD_INDIFFERENT};

use super::client::{self, API_V1};
use super::wire::{
    Blob, Content, GenerateContentRequest, GenerateContentResponse, GenerationConfig, ImageConfig,
    RequestPart, ThinkingConfig,
};
use crate::artifacts::media;
use crate::catalog::gemini::MAX_REQUEST_BYTES;
use crate::domain::{Operation, ProviderId, Usage, Warning, WarningCode};
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpError, RetryClass, TransportKind};
use crate::providers::{
    self, GeneratedImage, ImageFailure, ImageOutput, ImageRequest, ProviderContext, UnusableOutput,
};
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

/// Run `image.generate` or `image.edit` (`op`) against `generateContent`.
pub async fn run(
    op: Operation,
    req: &ImageRequest,
    ctx: &ProviderContext,
) -> Result<ImageOutput, ImageFailure> {
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
        possibly_billed_bad_response(
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

/// Paid synchronous call failures (see docs/reference/json-output.md). None is retried.
///
/// * A transport failure after sending (timeout, reset, truncated body, or an
///   answer over the size limit) is `submission_uncertain`: the request may have
///   been processed and billed, and Iris cannot find out. It keeps
///   `details.transport` and `details.charge_possible`.
/// * An HTTP error answer keeps its mapped code (e.g. `provider_error`, retryable, for
///   a 5xx) without `charge_possible`: Google's billing documentation says requests
///   that fail with 400 or 500 errors are not charged.
fn paid_call_error(err: HttpError) -> IrisError {
    match err {
        HttpError::Transport(t) if t.after_send => {
            let mut e = t.to_iris();
            e.code = ErrorCode::SubmissionUncertain;
            e.retryable = Some(false);
            let what = match (t.kind, t.status) {
                (TransportKind::Timeout, _) => "the time limit passed",
                // The status line arrived: the answer was cut off or longer than Iris reads.
                (_, Some(_)) => "the answer could not be read in full",
                _ => "the connection failed after the request was sent",
            };
            e.message = format!(
                "the Gemini API may have received this image request, but no complete answer arrived \
                 ({what}; {}): {}",
                t.url, t.message
            );
            e.hint = Some(
                "Iris did not retry automatically because the provider may already have processed (and \
                 billed) this request; check usage in Google AI Studio before running the command again"
                    .to_string(),
            );
            e.with_detail("charge_possible", true)
        }
        HttpError::Error(e)
            if e.hint.is_none() && e.provider_status.is_some_and(|s| s == 408 || s >= 500) =>
        {
            e.with_hint(
                "Iris did not retry automatically; Google's billing documentation says requests that fail \
                 with 400 or 500 errors are not charged",
            )
        }
        other => other.into_iris(),
    }
}

/// A 2xx answer Iris cannot use. The request was processed, so it may be billed
/// (`details.charge_possible`, which keeps the error from being called retryable).
fn possibly_billed_bad_response(message: &str, status: u16) -> IrisError {
    IrisError::new(ErrorCode::ProviderBadResponse, message)
        .with_provider(ProviderId::Gemini)
        .with_provider_status(status)
        .with_detail("charge_possible", true)
        .with_hint("the provider may have billed this request; Iris did not retry automatically")
}

/// `err`, built from a completed `generateContent` answer (HTTP 200) that holds no
/// usable image. Google bills such an answer by the tokens it reports, image or not,
/// so the error says so (`details.charged: true`) and carries the sanitized usage
/// the answer reported (`details.usage`), from which the app estimates the cost.
fn billed_answer(err: IrisError, usage: Option<&Usage>) -> IrisError {
    providers::with_reported_usage(err.with_detail("charged", true), usage)
}

/// Why one returned inline item is not a usable image.
struct Unusable {
    /// Human reason naming the item.
    why: String,
    /// The provider's label (scrubbed), if any.
    declared: Option<String>,
    /// Sniffed type of content that is not an image (e.g. a video), if any.
    sniffed: Option<&'static str>,
    /// The item's content as received: decoded bytes, or the data text when it is
    /// not base64; `None` when it carried no data.
    content: Option<Vec<u8>>,
}

/// Interpret a successful `generateContent` answer.
///
/// Every non-thought part with `inlineData` is a returned item, whatever its label.
/// Each is judged by its bytes, never by its label, and paid output is never
/// discarded because of another item: a valid image is kept under its sniffed type
/// (with `output_format_mismatch` when the label differs or is missing); an item that
/// is not a usable image is skipped with `output_item_unusable`, its content handed
/// to the app in `unusable` to be saved as received. Only an answer without any
/// usable image is an error (with that content in the failure); it completed, so it
/// is billed ([`billed_answer`]) whatever the reason. Warnings name items
/// by their position among the returned inline items ("response item N"), which
/// differs from the artifact index once an earlier item was skipped.
fn interpret(
    resp: GenerateContentResponse,
    status: u16,
    requested: usize,
    header_request_id: Option<String>,
) -> Result<ImageOutput, ImageFailure> {
    let request_id =
        header_request_id.or_else(|| resp.response_id.as_deref().and_then(crate::http::sanitize_request_id));
    let mut images = Vec::new();
    let mut texts = Vec::new();
    let mut mismatches = Vec::new();
    let mut unusable = Vec::new();
    let mut returned = 0usize;
    let candidates = resp.candidates.as_deref().unwrap_or_default();
    for part in candidates.iter().filter_map(|c| c.content.as_ref()).flat_map(|c| c.parts.iter().flatten()) {
        if part.thought == Some(true) {
            continue;
        }
        if let Some(text) = part.text.as_deref().filter(|t| !t.trim().is_empty()) {
            texts.push(text.to_string());
        }
        let Some(blob) = &part.inline_data else { continue };
        let index = returned;
        returned += 1;
        let declared = blob.mime_type.as_deref().map(str::trim).filter(|m| !m.is_empty());
        match decode_image(index, declared, blob.data.as_deref().unwrap_or("")) {
            Ok((image, mismatch)) => {
                images.push(image);
                mismatches.extend(mismatch);
            }
            Err(problem) => unusable.push((index, problem)),
        }
    }
    let kept: Vec<UnusableOutput> = unusable
        .iter_mut()
        .filter_map(|(item, problem)| {
            problem.content.take().map(|bytes| UnusableOutput { item: *item, bytes })
        })
        .collect();
    let unusable: Vec<Unusable> = unusable.into_iter().map(|(_, problem)| problem).collect();

    let text = (!texts.is_empty()).then(|| texts.join("\n"));
    let usage = resp.usage_metadata.as_ref().and_then(usage_from_metadata);

    if images.is_empty() {
        let err = match unusable.first() {
            // Inline items came back, none of them a usable image: their content
            // goes back to the app to be kept.
            Some(first) => {
                let reasons: Vec<&str> = unusable.iter().map(|u| u.why.as_str()).collect();
                let mut err = possibly_billed_bad_response(
                    &format!("the Gemini API returned no usable image: {}", reasons.join("; ")),
                    status,
                )
                .with_hint(
                    "the request completed, so the provider bills the tokens it reports in details.usage; \
                     Iris did not retry automatically",
                );
                if let Some(declared) = &first.declared {
                    err = err.with_detail("declared_media_type", declared.clone());
                }
                if let Some(sniffed) = first.sniffed {
                    err = err.with_detail("sniffed_media_type", sniffed);
                }
                if let Some(t) = &text {
                    err = err.with_detail(
                        "model_text",
                        redact::truncate(&redact::scrub(t), client::PROVIDER_TEXT_MAX),
                    );
                }
                err
            }
            None => no_image_error(&resp, text.as_deref()).with_provider_status(status),
        };
        let err = billed_answer(err.with_provider_request_id(request_id), usage.as_ref());
        return Err(ImageFailure { error: err, unusable: kept });
    }

    let mut warnings = Vec::new();
    if text.is_some() {
        warnings.push(Warning::new(
            WarningCode::ProviderTextOutput,
            "the model also returned text; it is reported in the result's `text` field",
        ));
    }
    warnings.extend(mismatches);
    for problem in &unusable {
        warnings.push(Warning::new(
            WarningCode::OutputItemUnusable,
            format!("{}; it was skipped and every usable image was kept", problem.why),
        ));
    }
    if returned > requested {
        let usable = images.len();
        warnings.push(Warning::new(
            WarningCode::UnexpectedOutputCount,
            format!(
                "the model returned {returned} items ({usable} usable) for a request of {requested}; every \
                 usable image was kept"
            ),
        ));
    }
    Ok(ImageOutput { images, unusable: kept, text, usage, provider_request_id: request_id, warnings })
}

/// Decode one returned inline item (item `index`, labeled `declared`) and type it by
/// its magic bytes. A recognized image is kept under its sniffed type, with an
/// `output_format_mismatch` warning when the label is missing or names another type;
/// anything else (no data, not base64, not an image) is [`Unusable`].
fn decode_image(
    index: usize,
    declared: Option<&str>,
    data: &str,
) -> Result<(GeneratedImage, Option<Warning>), Unusable> {
    let label = declared.map(client::safe_text);
    let unusable = |why: String, sniffed: Option<&'static str>, content: Option<Vec<u8>>| Unusable {
        why,
        declared: label.clone(),
        sniffed,
        content,
    };
    let labeled = match &label {
        Some(l) => format!("labeled {l}"),
        None => "without a media type".to_string(),
    };
    if data.trim().is_empty() {
        return Err(unusable(format!("response item {index} ({labeled}) has no data"), None, None));
    }
    let bytes = STANDARD_PAD_INDIFFERENT
        .decode(data)
        .or_else(|_| URL_SAFE_PAD_INDIFFERENT.decode(data))
        .map_err(|_| {
            let why = format!("response item {index} ({labeled}) is not valid base64");
            unusable(why, None, Some(data.as_bytes().to_vec()))
        })?;
    let sniffed = match media::sniff(&bytes) {
        Some(t) if media::is_image(t) => t,
        other => {
            let why = format!(
                "response item {index} ({} bytes {labeled}) is {}",
                bytes.len(),
                other.map_or("not a recognized image".to_string(), |t| format!("{t}, not an image"))
            );
            return Err(unusable(why, other, Some(bytes)));
        }
    };
    let matches_label = declared.is_some_and(|d| media::is_image(d) && media::accepts(&[d], sniffed));
    let warning = (!matches_label).then(|| {
        Warning::new(
            WarningCode::OutputFormatMismatch,
            format!(
                "the Gemini API returned response item {index} {labeled} but its content is {sniffed}; it \
                 is kept as {sniffed} because the request completed and may have been billed"
            ),
        )
    });
    Ok((GeneratedImage { item: index, media_type: sniffed.to_string(), bytes }, warning))
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
/// `permission_denied`; anything else → `provider_error`, retryable (a synchronous
/// call has no remote job, and the model may produce an image on another try).
///
/// The answer is a completed HTTP 200, which Google bills by the usage it reports
/// (only requests that fail with 400 or 500 errors are not charged): the caller adds
/// `details.charged: true` and the sanitized `details.usage`. That is a known
/// outcome, not an uncertain one, so it does not by itself make the error
/// non-retryable; running the command again is a new, separately billed request.
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
            .with_hint(
                "change the prompt or input images; the request completed, so the provider bills the tokens it \
                 reports in details.usage",
            )
    } else if let Some(reason) = finish_reasons.iter().find(|r| BLOCKING_FINISH_REASONS.contains(r)) {
        IrisError::new(
            ErrorCode::ContentBlocked,
            format!("the Gemini API blocked the generated image ({reason})"),
        )
        .with_provider_code(*reason)
        .with_hint(
            "change the prompt or input images; the request completed, so the provider bills the input and \
             thinking tokens it reports in details.usage",
        )
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
            ErrorCode::ProviderError,
            format!("the Gemini API returned no image ({described})"),
        )
        .with_retryable(Some(true))
        .with_hint(
            "running the command again may succeed, but it is billed again; this attempt completed, so the \
             provider bills the input and thinking tokens it reports in details.usage",
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
