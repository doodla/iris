//! OpenAI Images API wire format (request bodies, response and error bodies).
//! Everything here is private to the adapter (see docs/contributing/architecture.md).
//!
//! Requests are typed structs, so fields the adapter must never send
//! (`response_format`, `style`, `input_fidelity`, `user`, `stream`) cannot appear.
//! Responses and errors are read defensively from `serde_json::Value`, because
//! their optional fields are loosely documented (e.g. `usage` is described as
//! "gpt-image-1 only" but returned by newer models).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{OptionValue, ResolvedOptions};
use crate::domain::Usage;
use crate::error::IrisError;
use crate::providers::{USAGE_MAX_DEPTH, USAGE_MAX_ENTRIES};

/// Option fields shared by generation and edit bodies (see the model catalog's wire mapping). Only
/// options the user set explicitly are present; omitted ones take the provider default.
#[derive(Debug, Default, Serialize, PartialEq)]
pub(super) struct WireOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_compression: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moderation: Option<String>,
}

/// Output formats the Images API can produce (`output_format`).
pub(super) const OUTPUT_FORMATS: &[&str] = &["png", "jpeg", "webp"];

impl WireOptions {
    /// Translate validated options to wire fields. An option this adapter does not
    /// map, or a value of the wrong type, is an `internal_error`: validation against
    /// the catalog should have rejected it, and it must never be dropped silently.
    pub fn from_resolved(options: &ResolvedOptions) -> Result<WireOptions, IrisError> {
        let mut wire = WireOptions::default();
        for (name, value) in options.iter() {
            match name.as_str() {
                "count" => wire.n = Some(int(name, value)?),
                "size" => wire.size = Some(text(name, value)?),
                "quality" => wire.quality = Some(text(name, value)?),
                "format" => {
                    let format = text(name, value)?;
                    if !OUTPUT_FORMATS.contains(&format.as_str()) {
                        return Err(IrisError::internal(format!(
                            "the OpenAI adapter cannot request output format '{format}'; validation should \
                             have rejected it"
                        )));
                    }
                    wire.output_format = Some(format);
                }
                "compression" => wire.output_compression = Some(int(name, value)?),
                "background" => wire.background = Some(text(name, value)?),
                "moderation" => wire.moderation = Some(text(name, value)?),
                other => {
                    return Err(IrisError::internal(format!(
                        "the OpenAI adapter does not map option '{other}'; validation should have rejected it \
                         (nothing was sent)"
                    ))
                    .with_detail("option", other.to_string()));
                }
            }
        }
        Ok(wire)
    }
}

fn int(name: &str, value: &OptionValue) -> Result<i64, IrisError> {
    value.as_int().ok_or_else(|| wrong_type(name, "an integer"))
}

fn text(name: &str, value: &OptionValue) -> Result<String, IrisError> {
    value.as_str().map(str::to_string).ok_or_else(|| wrong_type(name, "a string"))
}

fn wrong_type(name: &str, expected: &str) -> IrisError {
    IrisError::internal(format!(
        "option '{name}' reached the OpenAI adapter with a value that is not {expected} (nothing was sent)"
    ))
    .with_detail("option", name.to_string())
}

/// `POST /images/generations` body. No `Debug`: it holds the prompt.
#[derive(Serialize)]
pub(super) struct GenerateBody<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    #[serde(flatten)]
    pub options: WireOptions,
}

/// One image reference in a JSON edit body (`{"image_url": "data:…"}`). No `Debug`:
/// it holds the image contents.
#[derive(Serialize)]
pub(super) struct ImageRef {
    pub image_url: String,
}

/// `POST /images/edits` JSON body (`EditImageBodyJsonParam`). No `Debug`: it holds
/// the prompt and the image contents.
#[derive(Serialize)]
pub(super) struct EditBody<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub images: Vec<ImageRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<ImageRef>,
    #[serde(flatten)]
    pub options: WireOptions,
}

/// One decoded element of `data[]` in an `ImagesResponse`.
pub(super) struct WireImage {
    /// `b64_json`, if present.
    pub b64_json: Option<String>,
    /// True if the element carries a `url` (not supported for GPT image models).
    pub has_url: bool,
    pub revised_prompt: Option<String>,
}

/// The parts of an `ImagesResponse` Iris uses.
pub(super) struct WireImagesResponse {
    pub data: Vec<WireImage>,
    /// Echoed `output_format` (`png` / `jpeg` / `webp`), if present.
    pub output_format: Option<String>,
    pub usage: Option<Usage>,
}

/// Top level of an `ImagesResponse`; unknown fields (`created`, `size`, …) are skipped
/// without being materialized, and every used field is read leniently.
#[derive(Deserialize)]
struct RawImagesResponse {
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    output_format: Option<Value>,
    #[serde(default)]
    usage: Option<Value>,
}

impl WireImagesResponse {
    /// Parse a success body. `Err` carries a short reason when the body is not the
    /// documented shape (not a JSON object, no `data` array). Base64 strings are moved
    /// out of the parsed document, never copied (responses can be tens of MB).
    pub fn parse(body: &[u8]) -> Result<WireImagesResponse, String> {
        let raw: RawImagesResponse = serde_json::from_slice(body)
            .map_err(|_| "the response body is not the documented JSON object".to_string())?;
        let items = match raw.data {
            Some(Value::Array(items)) => items,
            Some(Value::Null) | None => return Err("the response has no `data` array".to_string()),
            Some(_) => return Err("the response's `data` field is not an array".to_string()),
        };
        let data = items
            .into_iter()
            .map(|mut item| {
                let b64_json = match item.get_mut("b64_json").map(Value::take) {
                    Some(Value::String(s)) => Some(s),
                    _ => None,
                };
                WireImage {
                    b64_json,
                    has_url: item.get("url").and_then(Value::as_str).is_some_and(|u| !u.trim().is_empty()),
                    revised_prompt: item
                        .get("revised_prompt")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                }
            })
            .collect();
        Ok(WireImagesResponse {
            data,
            output_format: raw.output_format.as_ref().and_then(Value::as_str).map(str::to_ascii_lowercase),
            usage: raw.usage.as_ref().and_then(parse_usage),
        })
    }
}

/// Read an `ImagesUsage` object defensively: token counts that are not
/// non-negative integers are ignored, never an error.
pub(super) fn parse_usage(value: &Value) -> Option<Usage> {
    let obj = value.as_object()?;
    let count = |key: &str| obj.get(key).and_then(Value::as_u64);
    let usage = Usage {
        input_tokens: count("input_tokens"),
        output_tokens: count("output_tokens"),
        total_tokens: count("total_tokens"),
        provider_usage: numbers_only(value, 0),
    };
    let empty = usage.input_tokens.is_none()
        && usage.output_tokens.is_none()
        && usage.total_tokens.is_none()
        && usage.provider_usage.is_none();
    (!empty).then_some(usage)
}

/// A sanitized copy of a provider usage object: only objects and numbers survive
/// (strings, booleans, arrays, and nulls are dropped), keys must be short
/// `[a-z0-9_]` identifiers, and depth and width are bounded. `None` if nothing is left.
pub(super) fn numbers_only(value: &Value, depth: usize) -> Option<Value> {
    match value {
        Value::Number(n) => Some(Value::Number(n.clone())),
        Value::Object(map) if depth < USAGE_MAX_DEPTH => {
            let kept: serde_json::Map<String, Value> = map
                .iter()
                .filter(|(k, _)| {
                    !k.is_empty()
                        && k.len() <= 64
                        && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                })
                .filter_map(|(k, v)| numbers_only(v, depth + 1).map(|v| (k.clone(), v)))
                .take(USAGE_MAX_ENTRIES)
                .collect();
            (!kept.is_empty()).then_some(Value::Object(kept))
        }
        _ => None,
    }
}

/// The documented error body `{"error": {"message", "type", "param", "code", …}}`,
/// read defensively (any field may be missing; gateways may send HTML).
#[derive(Debug, Default)]
pub(super) struct WireError {
    pub message: Option<String>,
    pub kind: Option<String>,
    pub code: Option<String>,
    /// True if the error carries a `moderation_details` object (documented only for
    /// `code = moderation_blocked`).
    pub moderation_details: bool,
    /// `moderation_details.moderation_stage` (`input` / `output` / `unknown`).
    pub moderation_stage: Option<String>,
    /// `moderation_details.categories`.
    pub categories: Vec<String>,
}

impl WireError {
    /// Parse an error body; a body that is not the documented JSON shape yields an
    /// empty `WireError` (all fields `None`).
    pub fn parse(body: &[u8]) -> WireError {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return WireError::default();
        };
        let Some(error) = value.get("error").filter(|e| e.is_object()) else {
            return WireError::default();
        };
        let string = |v: Option<&Value>| match v {
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        let details = error.get("moderation_details").filter(|d| d.is_object());
        WireError {
            message: string(error.get("message")),
            kind: string(error.get("type")),
            code: string(error.get("code")),
            moderation_details: details.is_some(),
            moderation_stage: string(details.and_then(|d| d.get("moderation_stage"))),
            categories: details
                .and_then(|d| d.get("categories"))
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }

    /// True if `code` or `type` equals `value`.
    pub fn is(&self, value: &str) -> bool {
        self.code.as_deref() == Some(value) || self.kind.as_deref() == Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_keeps_numbers_and_drops_everything_else() {
        let usage = parse_usage(&json!({
            "input_tokens": 50, "output_tokens": 196, "total_tokens": 246,
            "input_tokens_details": {"text_tokens": 50, "image_tokens": 0, "note": "x"},
            "Weird-Key": 3, "list": [1, 2], "flag": true, "nested": {"a": {"b": {"c": 1}}}
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, Some(50));
        assert_eq!(usage.output_tokens, Some(196));
        assert_eq!(usage.total_tokens, Some(246));
        assert_eq!(
            usage.provider_usage.unwrap(),
            json!({
                "input_tokens": 50, "output_tokens": 196, "total_tokens": 246,
                "input_tokens_details": {"text_tokens": 50, "image_tokens": 0}
            })
        );
    }

    #[test]
    fn malformed_usage_is_ignored_not_an_error() {
        assert!(parse_usage(&json!("lots")).is_none());
        assert!(parse_usage(&json!({"input_tokens": "12", "details": {"x": "y"}})).is_none());
        let u = parse_usage(&json!({"output_tokens": 7.5, "input_tokens": -1, "total_tokens": 9})).unwrap();
        assert_eq!(u.output_tokens, None);
        assert_eq!(u.input_tokens, None);
        assert_eq!(u.total_tokens, Some(9));
    }

    #[test]
    fn error_bodies_are_read_defensively() {
        let e = WireError::parse(b"<html>bad gateway</html>");
        assert!(e.message.is_none() && e.code.is_none() && e.kind.is_none());
        let e = WireError::parse(br#"{"error": {"message": " m ", "type": "t", "code": 42}}"#);
        assert_eq!(e.message.as_deref(), Some("m"));
        assert_eq!(e.code.as_deref(), Some("42"));
        assert!(e.is("t") && e.is("42") && !e.is("m"));
        assert!(!e.moderation_details);
        let e = WireError::parse(br#"{"error": {"code": "x", "moderation_details": {}}}"#);
        assert!(e.moderation_details && e.moderation_stage.is_none() && e.categories.is_empty());
        let e = WireError::parse(br#"{"error": {"code": "x", "moderation_details": "input"}}"#);
        assert!(!e.moderation_details, "only an object counts as moderation details");
    }
}
