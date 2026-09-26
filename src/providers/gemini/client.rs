//! Shared REST plumbing for the Gemini API: endpoint URLs, the credential header,
//! identifier validation, and the mapping of `google.rpc.Status` error bodies to the
//! public error taxonomy (see docs/reference/json-output.md).

use std::time::Duration;

use serde_json::Value;

use super::wire::{ErrorBody, ErrorStatus};
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::http::{
    AuthHeader, Call, HttpResponse, RetryClass, Verdict, parse_protobuf_duration, redact_urls_in_text,
    sanitize_request_id,
};
use crate::providers::{CredentialHeader, ProviderContext, USAGE_MAX_DEPTH, USAGE_MAX_ENTRIES};
use crate::redact;

/// API version for `generateContent` (images) and image-model metadata.
pub const API_V1: &str = "v1";
/// API version for Veo `predictLongRunning`, operations, and files (v1 lacks them).
pub const API_V1BETA: &str = "v1beta";

/// The Gemini API key goes in this header; never in a `?key=` query parameter.
pub const CREDENTIAL_HEADER: CredentialHeader = CredentialHeader { name: "x-goog-api-key", prefix: "" };

/// Response headers that may carry a request id. None is documented for the Gemini
/// API; Iris records one if a proxy or future API version adds it.
const REQUEST_ID_HEADERS: &[&str] = &["x-request-id", "x-goog-request-id"];

/// Maximum characters of provider text kept in messages and details: the limit
/// shared by every adapter (see docs/reference/json-output.md).
pub(crate) use crate::http::PROVIDER_TEXT_MAX;

/// The credential header for this call.
pub fn auth(ctx: &ProviderContext) -> Result<AuthHeader, IrisError> {
    AuthHeader::new(CREDENTIAL_HEADER.name, CREDENTIAL_HEADER.prefix, &ctx.credential)
}

/// `{base}/{version}/{path}`. Built by concatenation so a configured base path (for
/// a proxy) is kept; `path` must already be validated.
pub fn endpoint(ctx: &ProviderContext, version: &str, path: &str) -> String {
    format!("{}/{version}/{path}", ctx.base_url.as_str().trim_end_matches('/'))
}

/// A call description for this provider with request-id capture.
pub fn call(class: RetryClass, timeout: Duration) -> Call {
    Call::new(class, timeout).with_request_id_header(REQUEST_ID_HEADERS[0]).with_provider(ProviderId::Gemini)
}

/// Sanitized request id from any known request-id header.
pub fn request_id(resp: &HttpResponse) -> Option<String> {
    resp.request_id
        .clone()
        .or_else(|| REQUEST_ID_HEADERS.iter().filter_map(|h| resp.header(h)).find_map(sanitize_request_id))
}

/// A model id is placed in a URL path segment (`models/{id}:method`), so only a
/// conservative character set is accepted: ASCII letters, digits, `.`, `_`, `-`,
/// starting with a letter or digit, at most 128 characters. Anything else (a `/`,
/// `:`, `?`, `%`, …) could change which endpoint is called.
pub fn validate_model_id(id: &str) -> Result<(), IrisError> {
    if is_path_segment(id, 128) {
        return Ok(());
    }
    Err(IrisError::invalid(format!(
        "model id '{}' cannot be sent to the Gemini API: use letters, digits, '.', '_', and '-' only",
        redact::truncate(&redact::scrub(id), 80)
    )))
}

/// `[A-Za-z0-9][A-Za-z0-9._-]*`, 1..=`max` bytes.
pub fn is_path_segment(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Length of the padded standard base64 encoding of `n` bytes. Used to reject
/// oversized requests before building (and allocating) their bodies.
pub fn base64_len(n: usize) -> usize {
    n.div_ceil(3).saturating_mul(4)
}

/// Provider text made safe to show: URLs redacted, secrets scrubbed, truncated.
pub fn safe_text(text: &str) -> String {
    redact::truncate(&redact::scrub(&redact_urls_in_text(text)), PROVIDER_TEXT_MAX)
}

/// Facts extracted from a Google error body.
#[derive(Debug, Default, Clone)]
pub struct GoogleError {
    /// Canonical status (`INVALID_ARGUMENT`, …).
    pub status: Option<String>,
    /// `google.rpc.ErrorInfo.reason` (e.g. `API_KEY_INVALID`).
    pub reason: Option<String>,
    /// `google.rpc.RetryInfo.retryDelay`.
    pub retry_delay: Option<Duration>,
    /// Scrubbed, truncated provider message.
    pub message: Option<String>,
    /// `google.rpc.QuotaFailure` violations, in body order.
    pub quota_violations: Vec<QuotaViolation>,
}

/// One `google.rpc.QuotaFailure.violations[]` entry.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QuotaViolation {
    /// `quotaId`, e.g. `GenerateRequestsPerDayPerProjectPerModel-FreeTier` (kept only
    /// when it is identifier-shaped).
    pub quota_id: Option<String>,
    /// `quotaValue`: the limit that was reached (a proto `int64`, so JSON sends it as
    /// a string).
    pub quota_value: Option<u64>,
}

/// Why a 429 is quota exhaustion rather than a rate limit that clears within the
/// retry window. Quota exhaustion is never retried; only rate-limit 429s are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaExhaustion<'a> {
    /// The limit is 0: the project has no quota for this model at all. This is the
    /// expected answer for a project without billing, because the image and Veo
    /// models have no free tier (inferred from research; not yet seen live).
    ZeroLimit(&'a QuotaViolation),
    /// A per-day quota is used up. It resets at midnight Pacific time, so retrying
    /// within the executor's retry window cannot succeed.
    Daily(&'a QuotaViolation),
}

impl GoogleError {
    /// `STATUS:REASON`, `STATUS`, or `REASON` (informational `provider_code`).
    pub fn provider_code(&self) -> Option<String> {
        match (&self.status, &self.reason) {
            (Some(s), Some(r)) => Some(format!("{s}:{r}")),
            (Some(s), None) => Some(s.clone()),
            (None, Some(r)) => Some(r.clone()),
            (None, None) => None,
        }
    }

    /// Quota exhaustion shown by the `QuotaFailure` details, if any. Without such
    /// details, a 429 stays a rate limit.
    pub fn quota_exhaustion(&self) -> Option<QuotaExhaustion<'_>> {
        let violations = &self.quota_violations;
        if let Some(v) = violations.iter().find(|v| v.quota_value == Some(0)) {
            return Some(QuotaExhaustion::ZeroLimit(v));
        }
        violations
            .iter()
            .find(|v| v.quota_id.as_deref().is_some_and(|id| id.contains("PerDay")))
            .map(QuotaExhaustion::Daily)
    }
}

fn detail_type(detail: &Value) -> &str {
    detail.get("@type").and_then(Value::as_str).unwrap_or("")
}

/// Only short, enum-like identifiers are kept from provider status/reason fields.
fn identifier(text: &str) -> Option<String> {
    let t = text.trim();
    let ok = !t.is_empty()
        && t.len() <= 64
        && t.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    ok.then(|| t.to_string())
}

/// Quota ids mix cases and dashes (`GenerateRequestsPerDayPerProjectPerModel-FreeTier`);
/// only ASCII letters, digits, `-`, `_`, and `.` are kept, up to 128 characters.
fn quota_identifier(text: &str) -> Option<String> {
    let t = text.trim();
    let ok = !t.is_empty()
        && t.len() <= 128
        && t.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    ok.then(|| t.to_string())
}

/// A non-negative integer sent as a JSON number or as a decimal string (proto3 JSON
/// encodes `int64` as a string).
fn json_u64(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Parse a `google.rpc.Status` error body. Missing or unparseable bodies give an
/// empty result (the HTTP status still drives the mapping).
pub fn parse_google_error(resp: &HttpResponse) -> GoogleError {
    let status: ErrorStatus = resp.json::<ErrorBody>().ok().and_then(|b| b.error).unwrap_or_default();
    let details = status.details.unwrap_or_default();
    let reason = details
        .iter()
        .filter(|d| detail_type(d).ends_with("google.rpc.ErrorInfo"))
        .find_map(|d| d.get("reason").and_then(Value::as_str).and_then(identifier));
    let retry_delay = details
        .iter()
        .filter(|d| detail_type(d).ends_with("google.rpc.RetryInfo"))
        .find_map(|d| d.get("retryDelay").and_then(Value::as_str).and_then(parse_protobuf_duration));
    let quota_violations = details
        .iter()
        .filter(|d| detail_type(d).ends_with("google.rpc.QuotaFailure"))
        .filter_map(|d| d.get("violations").and_then(Value::as_array))
        .flatten()
        .map(|v| QuotaViolation {
            quota_id: v.get("quotaId").and_then(Value::as_str).and_then(quota_identifier),
            quota_value: v.get("quotaValue").and_then(json_u64),
        })
        .collect();
    GoogleError {
        status: status.status.as_deref().and_then(identifier),
        reason,
        retry_delay,
        message: status.message.as_deref().map(safe_text).filter(|m| !m.trim().is_empty()),
        quota_violations,
    }
}

/// Map a non-success response to the public taxonomy (see docs/reference/json-output.md).
/// Returns the error and the provider-requested retry delay (`RetryInfo`).
pub fn map_error(resp: &HttpResponse) -> (IrisError, Option<Duration>) {
    let google = parse_google_error(resp);
    let http = resp.status.as_u16();
    let key_env = ProviderId::Gemini.credential_env();
    let reason_is_key = google.reason.as_deref().is_some_and(|r| r.starts_with("API_KEY_"));
    let status_is = |s: &str| google.status.as_deref() == Some(s);
    let exhausted = if http == 429 { google.quota_exhaustion() } else { None };

    let (code, retryable, message, hint): (ErrorCode, Option<bool>, String, Option<String>) = match http {
        400 if reason_is_key => (
            ErrorCode::AuthenticationFailed,
            Some(false),
            format!(
                "the Gemini API rejected the API key (HTTP 400 {})",
                google.reason.as_deref().unwrap_or("")
            ),
            Some(format!(
                "check that {key_env} holds a valid Gemini API key; Google says standard keys will be \
                 rejected from September 2026, so use an auth key created in Google AI Studio"
            )),
        ),
        400 if status_is("FAILED_PRECONDITION") => (
            ErrorCode::PermissionDenied,
            Some(false),
            "the Gemini API refused the request for this project (HTTP 400 FAILED_PRECONDITION)".to_string(),
            Some(
                "the project may need billing enabled (image and video models have no free tier), or the \
                 API may not be available in your region"
                    .to_string(),
            ),
        ),
        400 => (
            ErrorCode::InvalidArgument,
            Some(false),
            "the Gemini API rejected the request as invalid (HTTP 400)".to_string(),
            Some(
                "check the prompt, options, and input images against `iris models show <model>`; the \
                 provider message is in details.provider_message"
                    .to_string(),
            ),
        ),
        401 => (
            ErrorCode::AuthenticationFailed,
            Some(false),
            "the Gemini API rejected the credentials (HTTP 401)".to_string(),
            Some(format!("check that {key_env} holds a valid Gemini API key")),
        ),
        402 => (
            ErrorCode::QuotaExceeded,
            Some(false),
            "the Gemini API refused the request because the Prepay credit balance is depleted (HTTP 402)"
                .to_string(),
            Some(
                "add credit to the project's billing account in Google AI Studio before retrying".to_string(),
            ),
        ),
        403 => (
            ErrorCode::PermissionDenied,
            Some(false),
            "the Gemini API key is not permitted to make this request (HTTP 403)".to_string(),
            Some(
                "check the key's API restrictions and that the Gemini API is enabled for its project; \
                 Google says standard keys will be rejected from September 2026, so use an auth key"
                    .to_string(),
            ),
        ),
        404 => (
            ErrorCode::PermissionDenied,
            Some(false),
            "the Gemini API returned not found (HTTP 404)".to_string(),
            Some("model not found or not available to this project/key".to_string()),
        ),
        408 => (
            ErrorCode::RequestTimeout,
            Some(true),
            "the Gemini API timed out waiting for the request (HTTP 408)".to_string(),
            None,
        ),
        413 => (
            ErrorCode::InvalidArgument,
            Some(false),
            "the Gemini API rejected the request as too large (HTTP 413)".to_string(),
            Some("use fewer or smaller input images".to_string()),
        ),
        // A 429 is a rate limit unless its QuotaFailure proves the quota is exhausted
        // (quota exhaustion is never retried). Both forms stay definite
        // rejections: nothing was processed or billed.
        429 if matches!(exhausted, Some(QuotaExhaustion::ZeroLimit(_))) => (
            ErrorCode::QuotaExceeded,
            Some(false),
            "the Gemini API refused the request: this project's quota for the model is 0 (HTTP 429 \
             RESOURCE_EXHAUSTED)"
                .to_string(),
            Some(
                "Gemini image and Veo models have no free tier, so a project without billing has no quota \
                 for them; link a billing account to the key's project in Google AI Studio. Retrying \
                 will not help"
                    .to_string(),
            ),
        ),
        429 if matches!(exhausted, Some(QuotaExhaustion::Daily(_))) => (
            ErrorCode::QuotaExceeded,
            Some(false),
            "the Gemini API refused the request: the project's daily quota for this model is used up \
             (HTTP 429 RESOURCE_EXHAUSTED)"
                .to_string(),
            Some(
                "daily quotas reset at midnight Pacific time; retrying sooner will not help. Check the \
                 project's limits and tier in Google AI Studio"
                    .to_string(),
            ),
        ),
        429 => (
            ErrorCode::RateLimited,
            Some(true),
            "the Gemini API is rate limiting this project (HTTP 429 RESOURCE_EXHAUSTED)".to_string(),
            Some(
                "rate and spend limits apply per project; wait and run the command again, or check the \
                 project's limits in Google AI Studio. Gemini image and Veo models have no free tier: if \
                 every call gets HTTP 429, the key's project probably has no billing account"
                    .to_string(),
            ),
        ),
        504 => (
            ErrorCode::RequestTimeout,
            Some(true),
            "the Gemini API did not finish the request in time (HTTP 504 DEADLINE_EXCEEDED)".to_string(),
            None,
        ),
        500..=599 => (
            ErrorCode::ProviderError,
            Some(true),
            format!("the Gemini API returned a server error (HTTP {http})"),
            None,
        ),
        _ => {
            let mut err = resp.fallback_error(Some(ProviderId::Gemini));
            if let Some(code) = google.provider_code() {
                err = err.with_provider_code(code);
            }
            err = err.with_provider_request_id(request_id(resp));
            return (err, google.retry_delay);
        }
    };

    let mut err = IrisError::new(code, message)
        .with_retryable(retryable)
        .with_provider(ProviderId::Gemini)
        .with_provider_status(http)
        .with_provider_request_id(request_id(resp));
    if let Some(hint) = hint {
        err = err.with_hint(hint);
    }
    if let Some(code) = google.provider_code() {
        err = err.with_provider_code(code);
    }
    match &google.message {
        Some(m) => err = err.with_detail("provider_message", m.clone()),
        None => {
            let snippet = resp.body_snippet(PROVIDER_TEXT_MAX);
            if !snippet.trim().is_empty() {
                err = err.with_detail("provider_message", snippet);
            }
        }
    }
    if let Some(QuotaExhaustion::ZeroLimit(v) | QuotaExhaustion::Daily(v)) = exhausted {
        if let Some(id) = &v.quota_id {
            err = err.with_detail("quota_id", id.clone());
        }
        if let Some(limit) = v.quota_value {
            err = err.with_detail("quota_limit", limit);
        }
        // A RetryInfo delay next to an exhausted quota would tell the caller that
        // waiting a few seconds helps; it does not.
        return (err, None);
    }
    if let Some(delay) = google.retry_delay {
        err = err.with_retry_after(delay);
    }
    (err, google.retry_delay)
}

/// Retry verdict for a non-success response (see docs/contributing/architecture.md "Where
/// invariants live" for retry classes):
/// 429 → retryable rejection honoring `RetryInfo`, unless its `QuotaFailure` shows
/// an exhausted quota (`quota_exceeded`, final; Gemini's billing exhaustion is the
/// 402); 408/5xx → transient (retried by reads only); everything else → final.
pub fn classify(resp: &HttpResponse) -> Verdict {
    let (error, retry_after) = map_error(resp);
    match resp.status.as_u16() {
        429 if error.code == ErrorCode::QuotaExceeded => Verdict::Final(error),
        429 => Verdict::RetryableRejection { error, retry_after },
        408 | 500..=599 => Verdict::Transient { error, retry_after },
        _ => Verdict::Final(error),
    }
}

/// `google.rpc.Code` number → canonical name (for `provider_code`).
pub fn rpc_code_name(code: i64) -> String {
    let name = match code {
        0 => "OK",
        1 => "CANCELLED",
        2 => "UNKNOWN",
        3 => "INVALID_ARGUMENT",
        4 => "DEADLINE_EXCEEDED",
        5 => "NOT_FOUND",
        6 => "ALREADY_EXISTS",
        7 => "PERMISSION_DENIED",
        8 => "RESOURCE_EXHAUSTED",
        9 => "FAILED_PRECONDITION",
        10 => "ABORTED",
        11 => "OUT_OF_RANGE",
        12 => "UNIMPLEMENTED",
        13 => "INTERNAL",
        14 => "UNAVAILABLE",
        15 => "DATA_LOSS",
        16 => "UNAUTHENTICATED",
        other => return format!("CODE_{other}"),
    };
    name.to_string()
}

/// Keep only numbers, booleans, and short enum-like strings from a provider usage
/// object (`Usage::provider_usage` is informational and must not carry free text).
/// Nesting and width are bounded like every adapter's usage object
/// ([`USAGE_MAX_DEPTH`] levels of objects or arrays, [`USAGE_MAX_ENTRIES`] entries
/// each); anything deeper is dropped.
pub fn sanitize_usage(value: &Value) -> Option<Value> {
    sanitize_usage_at(value, 0)
}

fn sanitize_usage_at(value: &Value, depth: usize) -> Option<Value> {
    match value {
        Value::Number(_) | Value::Bool(_) => Some(value.clone()),
        Value::String(s) => identifier(s).map(Value::String),
        Value::Array(items) if depth < USAGE_MAX_DEPTH => Some(Value::Array(
            items.iter().filter_map(|v| sanitize_usage_at(v, depth + 1)).take(USAGE_MAX_ENTRIES).collect(),
        )),
        Value::Object(map) if depth < USAGE_MAX_DEPTH => Some(Value::Object(
            map.iter()
                .filter(|(k, _)| k.len() <= 64)
                .filter_map(|(k, v)| sanitize_usage_at(v, depth + 1).map(|v| (k.clone(), v)))
                .take(USAGE_MAX_ENTRIES)
                .collect(),
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segments_reject_anything_that_changes_the_url() {
        for ok in ["gemini-3.1-flash-image", "veo-3.1-lite-generate-preview", "a", "x_y.z-1"] {
            assert!(is_path_segment(ok, 128), "{ok}");
        }
        for bad in ["", "../x", ".hidden", "-x", "a/b", "a:b", "a?b", "a#b", "a%2Fb", "a b", "é"] {
            assert!(!is_path_segment(bad, 128), "{bad}");
        }
        assert!(!is_path_segment(&"a".repeat(129), 128));
    }

    #[test]
    fn base64_length_is_padded_standard_encoding() {
        use base64::Engine as _;
        for n in [0usize, 1, 2, 3, 4, 5, 6, 100, 1001] {
            let encoded = base64::engine::general_purpose::STANDARD.encode(vec![0u8; n]);
            assert_eq!(base64_len(n), encoded.len(), "{n}");
        }
    }

    #[test]
    fn only_a_zero_limit_or_a_daily_quota_counts_as_exhausted() {
        let violation = |id: &str, value: Option<u64>| QuotaViolation {
            quota_id: quota_identifier(id),
            quota_value: value,
        };
        let with = |violations: Vec<QuotaViolation>| GoogleError {
            quota_violations: violations,
            ..GoogleError::default()
        };

        let per_minute = violation("GenerateRequestsPerMinutePerProjectPerModel", Some(10));
        let daily = violation("GenerateRequestsPerDayPerProjectPerModel", Some(250));
        let zero = violation("GenerateRequestsPerMinutePerProjectPerModel-FreeTier", Some(0));

        assert_eq!(with(vec![]).quota_exhaustion(), None);
        assert_eq!(with(vec![per_minute.clone()]).quota_exhaustion(), None);
        assert_eq!(
            with(vec![per_minute.clone(), daily.clone()]).quota_exhaustion(),
            Some(QuotaExhaustion::Daily(&daily))
        );
        assert_eq!(
            with(vec![daily.clone(), zero.clone()]).quota_exhaustion(),
            Some(QuotaExhaustion::ZeroLimit(&zero)),
            "a zero limit is reported first: billing, not waiting, fixes it"
        );
        assert_eq!(with(vec![violation("", Some(5))]).quota_exhaustion(), None);
    }

    #[test]
    fn quota_values_parse_from_proto_json_strings_and_numbers() {
        assert_eq!(json_u64(&serde_json::json!("0")), Some(0));
        assert_eq!(json_u64(&serde_json::json!(" 250 ")), Some(250));
        assert_eq!(json_u64(&serde_json::json!(7)), Some(7));
        assert_eq!(json_u64(&serde_json::json!(-1)), None);
        assert_eq!(json_u64(&serde_json::json!("many")), None);
        assert_eq!(
            quota_identifier("GenerateRequestsPerDayPerProjectPerModel-FreeTier").as_deref(),
            Some("GenerateRequestsPerDayPerProjectPerModel-FreeTier")
        );
        assert_eq!(quota_identifier("has space"), None);
        assert_eq!(quota_identifier(&"a".repeat(129)), None);
    }

    #[test]
    fn rpc_codes_have_canonical_names() {
        assert_eq!(rpc_code_name(3), "INVALID_ARGUMENT");
        assert_eq!(rpc_code_name(13), "INTERNAL");
        assert_eq!(rpc_code_name(99), "CODE_99");
    }

    #[test]
    fn usage_sanitizer_keeps_numbers_and_enum_strings_only() {
        let raw = serde_json::json!({
            "promptTokenCount": 14,
            "candidatesTokensDetails": [{"modality": "IMAGE", "tokenCount": 747}],
            "note": "free text that should go",
            "serviceTier": "STANDARD",
            "nothing": null
        });
        let clean = sanitize_usage(&raw).unwrap();
        assert_eq!(
            clean,
            serde_json::json!({
                "promptTokenCount": 14,
                "candidatesTokensDetails": [{"modality": "IMAGE", "tokenCount": 747}],
                "serviceTier": "STANDARD"
            })
        );
    }

    #[test]
    fn usage_sanitizer_bounds_depth_and_width_like_the_other_adapters() {
        let deep = serde_json::json!({"a": {"b": {"c": {"d": 1}}, "n": 2}, "list": [[[1]], 3]});
        assert_eq!(
            sanitize_usage(&deep).unwrap(),
            serde_json::json!({"a": {"b": {}, "n": 2}, "list": [[], 3]}),
            "levels beyond the shared depth are dropped"
        );
        let wide: serde_json::Map<String, Value> =
            (0..100).map(|i| (format!("k{i:03}"), Value::from(i))).collect();
        let clean = sanitize_usage(&Value::Object(wide)).unwrap();
        assert_eq!(clean.as_object().unwrap().len(), USAGE_MAX_ENTRIES);
        let long = Value::Array((0..100).map(Value::from).collect());
        assert_eq!(sanitize_usage(&long).unwrap().as_array().unwrap().len(), USAGE_MAX_ENTRIES);
    }
}
