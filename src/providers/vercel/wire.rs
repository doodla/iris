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
use crate::providers::openai_compat::{ErrorBody, error_member, text_field};

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

/// A gateway error body, read defensively: the OpenAI-style object, or a bare
/// message string, which some answers carry instead.
pub(super) fn parse_error(body: &[u8]) -> ErrorBody {
    match error_member(body) {
        Some(Value::String(s)) => {
            ErrorBody { message: text_field(Some(&Value::String(s))), ..ErrorBody::default() }
        }
        Some(error) => ErrorBody::from_member(&error),
        None => ErrorBody::default(),
    }
}

/// Model availability from `GET /models?include_availability`.
pub(super) enum Availability {
    Eligible,
    Ineligible,
    Unknown,
}

/// An annotated model list (`GET /models?include_availability`), parsed once for
/// every model it answers.
pub(super) struct AvailabilityList {
    /// The `model_eligibility` of each requested model that the list names, by id;
    /// `None` when the body is not a readable list.
    rows: Option<std::collections::HashMap<String, Value>>,
    /// `request_context_availability.http.status`.
    account: Option<String>,
}

impl AvailabilityList {
    /// Parse the list, keeping only the rows of the `wanted` models. The gateway lists
    /// every model it routes to, so the other rows are dropped, and the kept values are
    /// moved out of the parsed document, never copied.
    pub fn parse(body: &[u8], wanted: &[&str]) -> AvailabilityList {
        let mut value = serde_json::from_slice::<Value>(body).ok();
        let account = value
            .as_ref()
            .and_then(|v| v.pointer("/request_context_availability/http/status"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let rows =
            value.as_mut().and_then(|v| v.get_mut("data")).and_then(Value::as_array_mut).map(|models| {
                models
                    .iter_mut()
                    .filter_map(|m| {
                        let id = m.get("id")?.as_str().filter(|id| wanted.contains(id))?.to_string();
                        Some((id, m.get_mut("model_eligibility").map(Value::take).unwrap_or(Value::Null)))
                    })
                    .collect()
            });
        AvailabilityList { rows, account }
    }

    /// The availability of `model`, one of the `wanted` models: it must be listed, its `model_eligibility.status`
    /// `eligible`, and `request_context_availability.http.status` `available` (the
    /// REST API reference's rule). A missing model is ineligible; a transient
    /// ineligibility, a verdict for another runtime than `http`, or anything
    /// unreadable is unknown.
    pub fn of(&self, model: &str) -> Availability {
        let Some(rows) = &self.rows else {
            return Availability::Unknown;
        };
        let Some(eligibility) = rows.get(model) else {
            return Availability::Ineligible;
        };
        let field = |key: &str| eligibility.get(key).and_then(Value::as_str);
        // The verdict must be for HTTP requests; a row without a runtime is read as one.
        if field("evaluated_runtime").is_some_and(|runtime| runtime != "http") {
            return Availability::Unknown;
        }
        // An account that can't make requests decides the answer for every model.
        if self.account.as_deref() == Some("unavailable") {
            return Availability::Ineligible;
        }
        match (field("status"), self.account.as_deref()) {
            (Some("eligible"), Some("available")) => Availability::Eligible,
            // A transient ineligibility is a passing state, not an answer.
            (Some("ineligible"), _) if field("category") == Some("transient") => Availability::Unknown,
            (Some("ineligible"), _) => Availability::Ineligible,
            _ => Availability::Unknown,
        }
    }
}

/// [`AvailabilityList::of`] for one model of a body.
#[cfg(test)]
fn availability(body: &[u8], model: &str) -> Availability {
    AvailabilityList::parse(body, &[model]).of(model)
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
        let e = parse_error(b"<html>bad gateway</html>");
        assert!(e.message.is_none() && e.kind.is_none());
        let e =
            parse_error(br#"{"error": {"message": " Rate limit exceeded ", "type": "rate_limit_exceeded"}}"#);
        assert_eq!(e.message.as_deref(), Some("Rate limit exceeded"));
        assert_eq!(e.kind.as_deref(), Some("rate_limit_exceeded"));
        let e = parse_error(br#"{"error": "plain"}"#);
        assert_eq!(e.message.as_deref(), Some("plain"));
        let e = parse_error(br#"{"error": {"message": "m", "code": 7}}"#);
        assert_eq!(e.code.as_deref(), Some("7"));
        assert!(e.is("7"));
    }

    #[test]
    fn an_account_that_cannot_make_requests_decides_before_a_transient_verdict() {
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [{"id": "m", "model_eligibility": {"status": "ineligible", "category": "transient"}}],
            "request_context_availability": {"http": {"status": "unavailable"}}
        }))
        .unwrap();
        assert!(matches!(availability(&body, "m"), Availability::Ineligible));
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
    fn only_the_requested_models_are_kept_and_each_is_answered() {
        let body = serde_json::to_vec(&serde_json::json!({
            "data": [
                {"id": "a", "model_eligibility": {"status": "eligible"}},
                {"id": "b", "model_eligibility": {"status": "ineligible", "category": "policy"}},
                {"id": "c", "model_eligibility": {"status": "eligible"}},
                {"id": 7}, {"model_eligibility": {}}
            ],
            "request_context_availability": {"http": {"status": "available"}}
        }))
        .unwrap();
        let list = AvailabilityList::parse(&body, &["a", "b", "missing"]);
        assert_eq!(list.rows.as_ref().map(|rows| rows.len()), Some(2), "c is not kept");
        assert!(matches!(list.of("a"), Availability::Eligible));
        assert!(matches!(list.of("b"), Availability::Ineligible));
        assert!(matches!(list.of("missing"), Availability::Ineligible));
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
