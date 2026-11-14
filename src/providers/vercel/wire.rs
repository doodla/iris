//! Vercel AI Gateway wire format for the OpenAI-compatible Images endpoint (request
//! body, response and error bodies). Everything here is private to the adapter (see
//! docs/contributing/architecture.md).
//!
//! The request is a typed struct, so only the fields the adapter maps can appear.
//! Responses and errors are read defensively from `serde_json::Value`: the gateway
//! documents the shape by example (`data[].b64_json`), and its error body as
//! `{"error": {"message", "type"}}`, sometimes with `code`.

use serde::Serialize;
use serde_json::Value;

use crate::catalog::ResolvedOptions;
use crate::error::IrisError;

/// `POST /images/generations` body. No `Debug`: it holds the prompt.
#[derive(Serialize)]
pub(super) struct GenerateBody<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    /// Always `b64_json`, so images come back inline (the documented examples).
    pub response_format: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
}

impl<'a> GenerateBody<'a> {
    /// Translate validated options to wire fields. `count` is validated as 1 by the
    /// catalog and never sent; an option this adapter does not map, or a value of the
    /// wrong type, is an `internal_error`: it must never be dropped silently.
    pub fn new(model: &'a str, prompt: &'a str, options: &ResolvedOptions) -> Result<Self, IrisError> {
        let mut body = GenerateBody { model, prompt, response_format: "b64_json", size: None };
        for (name, value) in options.iter() {
            match name.as_str() {
                "count" if value.as_int() == Some(1) => {}
                "size" => {
                    body.size = Some(value.as_str().map(str::to_string).ok_or_else(|| {
                        IrisError::internal(
                            "option 'size' reached the Vercel adapter with a value that is not a string \
                             (nothing was sent)",
                        )
                    })?)
                }
                other => {
                    return Err(IrisError::internal(format!(
                        "the Vercel adapter does not map option '{other}' with value {value}; validation should \
                         have rejected it (nothing was sent)"
                    ))
                    .with_detail("option", other.to_string()));
                }
            }
        }
        Ok(body)
    }
}

/// A gateway error body, read defensively.
#[derive(Debug, Default)]
pub(super) struct WireError {
    pub message: Option<String>,
    /// `error.type`, e.g. `rate_limit_exceeded`, `model_not_found`.
    pub kind: Option<String>,
    /// `error.code`, when present.
    pub code: Option<String>,
}

impl WireError {
    pub fn parse(body: &[u8]) -> WireError {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return WireError::default();
        };
        let Some(error) = value.get("error") else {
            return WireError::default();
        };
        let text = |key: &str| match error.get(key) {
            Some(Value::String(s)) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        match error {
            // Some answers carry the message as a bare string.
            Value::String(s) => WireError { message: Some(s.trim().to_string()), ..WireError::default() },
            _ => WireError { message: text("message"), kind: text("type"), code: text("code") },
        }
    }
}

/// Model availability from `GET /models?include_availability`.
pub(super) enum Availability {
    Eligible,
    Ineligible,
    Unknown,
}

/// Read the availability of `model` from an annotated model list: the model must be
/// listed, its `model_eligibility.status` `eligible`, and
/// `request_context_availability.http.status` `available` (the REST API reference's
/// rule). A missing model is ineligible; anything unreadable is unknown.
pub(super) fn availability(body: &[u8], model: &str) -> Availability {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Availability::Unknown;
    };
    let Some(models) = value.get("data").and_then(Value::as_array) else {
        return Availability::Unknown;
    };
    let account = value.pointer("/request_context_availability/http/status").and_then(Value::as_str);
    let Some(row) = models.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(model)) else {
        return Availability::Ineligible;
    };
    let eligibility = row.get("model_eligibility");
    let field = |key: &str| eligibility.and_then(|e| e.get(key)).and_then(Value::as_str);
    // The verdict must be for HTTP requests; a row without a runtime is read as one.
    if field("evaluated_runtime").is_some_and(|runtime| runtime != "http") {
        return Availability::Unknown;
    }
    match (field("status"), account) {
        (Some("eligible"), Some("available")) => Availability::Eligible,
        // A transient ineligibility is a passing state, not an answer.
        (Some("ineligible"), _) if field("category") == Some("transient") => Availability::Unknown,
        (Some("ineligible"), _) | (_, Some("unavailable")) => Availability::Ineligible,
        _ => Availability::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::OptionValue;

    #[test]
    fn body_sends_size_and_b64_json_but_never_count() {
        let mut options = ResolvedOptions::new();
        options.insert("count", OptionValue::Int(1));
        options.insert("size", OptionValue::Str("1024x1024".into()));
        let body = GenerateBody::new("bfl/flux-3-image-klein", "a fox", &options).unwrap();
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            serde_json::json!({"model": "bfl/flux-3-image-klein", "prompt": "a fox", "response_format": "b64_json",
                               "size": "1024x1024"})
        );
    }

    #[test]
    fn unmapped_options_are_internal_errors() {
        let mut options = ResolvedOptions::new();
        options.insert("quality", OptionValue::Str("high".into()));
        assert!(GenerateBody::new("m", "p", &options).is_err());
        let mut options = ResolvedOptions::new();
        options.insert("count", OptionValue::Int(2));
        assert!(GenerateBody::new("m", "p", &options).is_err(), "a count other than 1 is never dropped");
    }

    #[test]
    fn error_bodies_are_read_defensively() {
        let e = WireError::parse(b"<html>bad gateway</html>");
        assert!(e.message.is_none() && e.kind.is_none());
        let e = WireError::parse(
            br#"{"error": {"message": " Rate limit exceeded ", "type": "rate_limit_exceeded"}}"#,
        );
        assert_eq!(e.message.as_deref(), Some("Rate limit exceeded"));
        assert_eq!(e.kind.as_deref(), Some("rate_limit_exceeded"));
        let e = WireError::parse(br#"{"error": "plain"}"#);
        assert_eq!(e.message.as_deref(), Some("plain"));
    }

    #[test]
    fn a_transient_or_non_http_verdict_is_unknown() {
        let body = |eligibility: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "data": [{"id": "m", "model_eligibility": eligibility}],
                "request_context_availability": {"http": {"status": "available"}}
            }))
            .unwrap()
        };
        let transient = serde_json::json!({"status": "ineligible", "category": "transient", "reason": "x"});
        assert!(matches!(availability(&body(transient), "m"), Availability::Unknown));
        let policy = serde_json::json!({"status": "ineligible", "category": "policy"});
        assert!(matches!(availability(&body(policy), "m"), Availability::Ineligible));
        let websocket = serde_json::json!({"status": "eligible", "evaluated_runtime": "realtime_websocket"});
        assert!(matches!(availability(&body(websocket), "m"), Availability::Unknown));
        let http = serde_json::json!({"status": "eligible", "evaluated_runtime": "http"});
        assert!(matches!(availability(&body(http), "m"), Availability::Eligible));
    }

    #[test]
    fn availability_follows_the_documented_rule() {
        let body = |status: &str, account: &str| {
            serde_json::to_vec(&serde_json::json!({
                "data": [{"id": "bfl/flux-3-image-klein", "model_eligibility": {"status": status}}],
                "request_context_availability": {"http": {"status": account}}
            }))
            .unwrap()
        };
        assert!(matches!(
            availability(&body("eligible", "available"), "bfl/flux-3-image-klein"),
            Availability::Eligible
        ));
        assert!(matches!(
            availability(&body("ineligible", "available"), "bfl/flux-3-image-klein"),
            Availability::Ineligible
        ));
        assert!(matches!(
            availability(&body("eligible", "unavailable"), "bfl/flux-3-image-klein"),
            Availability::Ineligible
        ));
        assert!(matches!(
            availability(&body("unknown", "available"), "bfl/flux-3-image-klein"),
            Availability::Unknown
        ));
        assert!(matches!(
            availability(&body("eligible", "available"), "bfl/other"),
            Availability::Ineligible
        ));
        assert!(matches!(availability(b"nope", "bfl/flux-3-image-klein"), Availability::Unknown));
    }
}
