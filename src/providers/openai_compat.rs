//! The OpenAI Images response shape (`{"data": [{"b64_json": …}, …]}`) and the
//! OpenAI-style error body (`{"error": {"message", "type", "code"}}`), shared by the
//! adapters whose endpoints return them: OpenAI's Images API and the Vercel AI
//! Gateway's OpenAI-compatible Images endpoint. Private to `providers` (see
//! docs/contributing/architecture.md): only adapters read it.
//!
//! This module parses the bodies and decodes each item. What a provider expected of
//! the items (a requested format, a count, usage), and what an error status means,
//! stay in its adapter, which turns the decoded items into an `ImageOutput` or
//! `ImageFailure`, and an error body into an `IrisError`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use serde::Deserialize;
use serde_json::Value;

use crate::artifacts::media;
use crate::domain::{ProviderId, Warning, WarningCode};
use crate::error::{ErrorCode, IrisError};
use crate::http::HttpResponse;
use crate::providers::errors;
use crate::providers::{GeneratedImage, UnusableOutput};

/// The `error` member of an error body, or `None` when the body is not a JSON object
/// with one (gateways may send HTML).
pub(crate) fn error_member(body: &[u8]) -> Option<Value> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(mut top)) => top.remove("error"),
        _ => None,
    }
}

/// A non-empty string field, trimmed, or a number as text.
pub(crate) fn text_field(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// The fields every OpenAI-style error body may carry, read defensively: any of them
/// may be missing.
#[derive(Debug, Default)]
pub(crate) struct ErrorBody {
    pub message: Option<String>,
    /// `error.type`.
    pub kind: Option<String>,
    pub code: Option<String>,
}

impl ErrorBody {
    /// The fields of an `error` object; anything else yields an empty `ErrorBody`.
    pub fn from_member(error: &Value) -> ErrorBody {
        ErrorBody {
            message: text_field(error.get("message")),
            kind: text_field(error.get("type")),
            code: text_field(error.get("code")),
        }
    }

    /// True if `code` or `type` equals `value`.
    pub fn is(&self, value: &str) -> bool {
        self.code.as_deref() == Some(value) || self.kind.as_deref() == Some(value)
    }

    /// The provider code to report: `error.code`, else `error.type`.
    fn provider_code(&self) -> Option<&str> {
        self.code.as_deref().or(self.kind.as_deref())
    }

    /// An error for a non-2xx answer: provider, status, request id, provider code,
    /// and the scrubbed provider message ([`errors::error_from`]).
    pub fn error(
        &self,
        resp: &HttpResponse,
        provider: ProviderId,
        code: ErrorCode,
        message: String,
    ) -> IrisError {
        errors::error_from(resp, provider, code, message, self.provider_code(), self.message.as_deref())
    }

    /// Attach the provider code and message to an error ([`errors::annotate`]).
    pub fn annotate(&self, e: &mut IrisError, resp: &HttpResponse) {
        errors::annotate(e, resp, self.provider_code(), self.message.as_deref());
    }

    /// `": <provider message>"` for human messages, or empty ([`errors::provider_suffix`]).
    pub fn suffix(&self) -> String {
        errors::provider_suffix(self.message.as_deref())
    }
}

/// One element of `data[]`.
pub(crate) struct ImageItem {
    /// `b64_json`, if present.
    pub b64_json: Option<String>,
    /// True if the element carries a `url` instead of inline data.
    pub has_url: bool,
    /// `revised_prompt`, trimmed, if present and not empty.
    pub revised_prompt: Option<String>,
}

/// The parts of an images response the adapters use.
pub(crate) struct ImagesResponse {
    pub data: Vec<ImageItem>,
    /// Echoed `output_format`, lowercased, if present.
    pub output_format: Option<String>,
    /// The raw `usage` object, for the adapter to read.
    pub usage: Option<Value>,
}

/// Top level of an images response; unknown fields are skipped without being
/// materialized, and every used field is read leniently.
#[derive(Deserialize)]
struct RawImagesResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    output_format: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
}

impl ImagesResponse {
    /// Parse a success body. `Err` carries a short reason when the body is not the
    /// documented shape (not a JSON object, no `data` array). Base64 strings are moved
    /// out of the parsed document, never copied (responses can be tens of MB).
    pub fn parse(body: &[u8]) -> Result<ImagesResponse, String> {
        let raw: RawImagesResponse = serde_json::from_slice(body)
            .map_err(|_| "the response body is not the documented JSON object".to_string())?;
        let items = match raw.data {
            Some(Value::Array(items)) => items,
            Some(Value::Null) | None => return Err("the response has no `data` array".to_string()),
            Some(_) => return Err("the response's `data` field is not an array".to_string()),
        };
        let data = items
            .into_iter()
            .map(|mut item| ImageItem {
                b64_json: match item.get_mut("b64_json").map(Value::take) {
                    Some(Value::String(s)) => Some(s),
                    _ => None,
                },
                has_url: item.get("url").and_then(Value::as_str).is_some_and(|u| !u.trim().is_empty()),
                revised_prompt: item
                    .get("revised_prompt")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            })
            .collect();
        Ok(ImagesResponse {
            data,
            output_format: raw.output_format.as_ref().and_then(Value::as_str).map(str::to_ascii_lowercase),
            usage: raw.usage,
        })
    }
}

/// Why one item cannot be used.
pub(crate) struct ItemProblem {
    /// Human reason naming the item by its position, e.g. "response item 1 is not
    /// valid base64".
    pub why: String,
    /// Sniffed type of content that is not an image (e.g. `video/mp4`), if any.
    pub actual: Option<&'static str>,
}

/// Every item of a response, decoded: usable images, the content of items that are
/// not usable images (kept as received, so that the app can save it), and why each
/// such item cannot be used.
pub(crate) struct DecodedItems {
    pub images: Vec<GeneratedImage>,
    pub unusable: Vec<UnusableOutput>,
    pub problems: Vec<ItemProblem>,
    /// Distinct revised prompts, in order.
    pub revised_prompts: Vec<String>,
    /// Number of items in the response.
    pub returned: usize,
}

/// What an adapter's Images endpoint returns, declared once per adapter: the facts
/// that [`decode_items`] and [`DecodedItems::count_warning`] phrase their reasons and
/// warnings from.
pub(crate) struct ImagesApi {
    /// The provider as a sentence subject, e.g. "OpenAI" (its `PaidWording::who`).
    pub who: &'static str,
    /// The request field that sets the number of images (`n`), when the adapter sends
    /// one.
    pub count_field: Option<&'static str>,
    /// The models that the provider documents as returning base64 only (e.g. "GPT
    /// image models"), which the reason for a URL item names.
    pub base64_only: Option<&'static str>,
}

/// The type every item of a response should have.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ExpectedItem {
    /// Any image type: the provider chooses the format.
    AnyImage,
    /// This media type, e.g. `image/png`.
    MediaType(&'static str),
}

impl std::fmt::Display for ExpectedItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExpectedItem::AnyImage => f.write_str("an image"),
            ExpectedItem::MediaType(media_type) => f.write_str(media_type),
        }
    }
}

/// Decode every item into an image typed by its magic bytes, or say why it cannot
/// be used. Paid output is never discarded because of another item. The reason for
/// content that is not an image names `expected`, and the reason for an item that is
/// a URL names `api.base64_only`.
pub(crate) fn decode_items(items: Vec<ImageItem>, expected: ExpectedItem, api: &ImagesApi) -> DecodedItems {
    let url_note = api.base64_only.map(|models| format!(" ({models} return base64)")).unwrap_or_default();
    let returned = items.len();
    let mut out = DecodedItems {
        images: Vec::with_capacity(returned),
        unusable: Vec::new(),
        problems: Vec::new(),
        revised_prompts: Vec::new(),
        returned,
    };
    for (index, item) in items.into_iter().enumerate() {
        if let Some(text) = item.revised_prompt
            && !out.revised_prompts.contains(&text)
        {
            out.revised_prompts.push(text);
        }
        let problem = |why: String| ItemProblem { why, actual: None };
        let Some(b64) = item.b64_json else {
            out.problems.push(problem(if item.has_url {
                format!("response item {index} is a URL instead of inline base64 data{url_note}; Iris does not fetch it")
            } else {
                format!("response item {index} has no b64_json data")
            }));
            continue;
        };
        let bytes = match STANDARD_PAD_INDIFFERENT.decode(b64.trim()) {
            Ok(bytes) => bytes,
            Err(_) => {
                out.problems.push(problem(format!("response item {index} is not valid base64")));
                out.unusable.push(UnusableOutput { item: index, bytes: b64.into_bytes() });
                continue;
            }
        };
        drop(b64);
        match media::sniff(&bytes) {
            Some(actual) if media::is_image(actual) => out.images.push(GeneratedImage {
                item: index,
                media_type: actual.to_string(),
                bytes,
                format_mismatch: None,
            }),
            other => {
                out.problems.push(ItemProblem {
                    why: format!(
                        "response item {index} should be {expected} but its content is {}",
                        other.unwrap_or("not a recognized image")
                    ),
                    actual: Some(other.unwrap_or("unknown")),
                });
                out.unusable.push(UnusableOutput { item: index, bytes });
            }
        }
    }
    out
}

impl DecodedItems {
    /// One `output_item_unusable` warning per item that could not be used (only
    /// meaningful when at least one image was usable).
    pub fn unusable_warnings(&self) -> Vec<Warning> {
        self.problems
            .iter()
            .map(|p| {
                Warning::new(
                    WarningCode::OutputItemUnusable,
                    format!("{}; it was skipped and every usable image was kept", p.why),
                )
            })
            .collect()
    }

    /// `unexpected_output_count` when the response held a number of items other
    /// than `wanted`.
    pub fn count_warning(&self, api: &ImagesApi, wanted: i64) -> Option<Warning> {
        let who = api.who;
        let asked = api.count_field.map(|field| format!(" ({field})")).unwrap_or_default();
        let returned = self.returned;
        if i64::try_from(returned).ok() == Some(wanted) {
            return None;
        }
        let noun = if returned == 1 { "item" } else { "items" };
        Some(Warning::new(
            WarningCode::UnexpectedOutputCount,
            format!(
                "{who} returned {returned} {noun} ({} usable) for a request of {wanted}{asked}; every usable \
                 image was kept",
                self.images.len()
            ),
        ))
    }

    /// The reasons of every problem, joined, for an error when no image was usable.
    pub fn reasons(&self) -> String {
        self.problems.iter().map(|p| p.why.as_str()).collect::<Vec<_>>().join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(body: &'static [u8]) -> HttpResponse {
        HttpResponse {
            status: reqwest::StatusCode::BAD_REQUEST,
            headers: reqwest::header::HeaderMap::new(),
            body: bytes::Bytes::from_static(body),
            request_id: Some("req_1".to_string()),
            attempts: 1,
        }
    }

    fn item(b64: Option<&str>, url: bool) -> ImageItem {
        ImageItem { b64_json: b64.map(str::to_string), has_url: url, revised_prompt: None }
    }

    #[test]
    fn reasons_and_warnings_are_phrased_from_the_declared_facts() {
        let documented =
            ImagesApi { who: "OpenAI", count_field: Some("n"), base64_only: Some("GPT image models") };
        let plain = ImagesApi { who: "OpenAI", count_field: None, base64_only: None };
        let text = base64::engine::general_purpose::STANDARD.encode(b"not an image");
        let items = || vec![item(None, true), item(Some(&text), false)];

        let decoded = decode_items(items(), ExpectedItem::MediaType("image/png"), &documented);
        assert_eq!(
            decoded.reasons(),
            "response item 0 is a URL instead of inline base64 data (GPT image models return base64); Iris does \
             not fetch it; response item 1 should be image/png but its content is not a recognized image"
        );
        let count = decoded.count_warning(&documented, 1).unwrap().message;
        assert!(count.starts_with("OpenAI returned 2 items (0 usable) for a request of 1 (n);"), "{count}");
        assert!(decoded.count_warning(&documented, 2).is_none());

        let decoded = decode_items(items(), ExpectedItem::AnyImage, &plain);
        assert_eq!(
            decoded.reasons(),
            "response item 0 is a URL instead of inline base64 data; Iris does not fetch it; response item 1 \
             should be an image but its content is not a recognized image"
        );
        let count = decoded.count_warning(&plain, 1).unwrap().message;
        assert!(count.starts_with("OpenAI returned 2 items (0 usable) for a request of 1;"), "{count}");
    }

    #[test]
    fn an_error_member_is_read_only_from_a_json_object() {
        assert!(error_member(b"<html>bad gateway</html>").is_none());
        assert!(error_member(br#"[{"error": {}}]"#).is_none());
        assert_eq!(error_member(br#"{"error": "plain"}"#), Some(Value::String("plain".into())));
        let e = ErrorBody::from_member(&Value::String("plain".into()));
        assert!(e.message.is_none() && e.kind.is_none() && e.code.is_none());
    }

    #[test]
    fn errors_carry_the_code_else_the_type_and_the_cleaned_message() {
        let resp = response(br#"{"error": {"message": " too big ", "type": "invalid_request_error"}}"#);
        let body = ErrorBody::from_member(&error_member(&resp.body).unwrap());
        assert_eq!(body.suffix(), ": too big");
        let e = body.error(&resp, ProviderId::Vercel, ErrorCode::InvalidArgument, "rejected".into());
        assert_eq!(e.provider_code.as_deref(), Some("invalid_request_error"));
        assert_eq!(e.provider_status, Some(400));
        assert_eq!(e.provider_request_id.as_deref(), Some("req_1"));
        assert_eq!(e.details.get("provider_message"), Some(&Value::String("too big".into())));

        let body = ErrorBody { code: Some("c".into()), kind: Some("t".into()), message: None };
        let mut e = IrisError::new(ErrorCode::ProviderError, "x");
        body.annotate(&mut e, &resp);
        assert_eq!(e.provider_code.as_deref(), Some("c"));
        assert!(body.is("c") && body.is("t") && !body.is("x"));
        assert_eq!(body.suffix(), "");
    }
}
