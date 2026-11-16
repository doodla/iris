//! The OpenAI Images response shape (`{"data": [{"b64_json": …}, …]}`), shared by
//! the adapters whose endpoints return it: OpenAI's Images API and the Vercel AI
//! Gateway's OpenAI-compatible Images endpoint. Private to `providers` (see
//! docs/contributing/architecture.md): only adapters read it.
//!
//! This module parses the body and decodes each item. What a provider expected of
//! the items (a requested format, a count, usage) stays in its adapter, which turns
//! the decoded items into an `ImageOutput` or `ImageFailure`.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD_PAD_INDIFFERENT;
use serde::Deserialize;
use serde_json::Value;

use crate::artifacts::media;
use crate::domain::{Warning, WarningCode};
use crate::providers::{GeneratedImage, UnusableOutput};

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

/// Decode every item into an image typed by its magic bytes, or say why it cannot
/// be used. Paid output is never discarded because of another item. `expected`
/// phrases the reason for content that is not an image ("should be image/png"), and
/// `url_note` is added to the reason for an item that is a URL.
pub(crate) fn decode_items(items: Vec<ImageItem>, expected: &str, url_note: &str) -> DecodedItems {
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
            Some(actual) if media::is_image(actual) => {
                out.images.push(GeneratedImage { item: index, media_type: actual.to_string(), bytes })
            }
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
    /// than `wanted`. `who` names the provider ("OpenAI"), and `asked` how the count
    /// was requested (" (n)", or empty).
    pub fn count_warning(&self, who: &str, wanted: i64, asked: &str) -> Option<Warning> {
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
