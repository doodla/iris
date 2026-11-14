//! Error helpers every adapter shares: making provider text display-safe, attaching
//! the provider's code and message to an error, and removing the adapter's own
//! credential from an error before it leaves the adapter.
//!
//! These rules are the same for every provider, so they live here once: a fix to
//! secret handling then reaches every adapter (see docs/contributing/adding-a-provider.md,
//! "Redact before an error can leak").

use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpResponse, PROVIDER_TEXT_MAX, TransportError, TransportKind};
use crate::providers::ProviderContext;
use crate::redact;

/// Remove the context's own credential from every string of an error.
///
/// Provider text is already passed through [`redact::scrub`], which covers the
/// credentials in the environment (the only source the CLI uses; see
/// docs/reference/configuration.md). This pass also covers a key handed to the
/// adapter another way, e.g. by a library caller.
pub(crate) fn scrub_credential(mut e: IrisError, ctx: &ProviderContext) -> IrisError {
    let secrets = [ctx.credential.expose().trim().to_string()];
    let scrub = |s: &mut String| {
        if let std::borrow::Cow::Owned(clean) = redact::scrub_with(s, &secrets) {
            *s = clean;
        }
    };
    let data = &mut *e;
    scrub(&mut data.message);
    for field in [&mut data.hint, &mut data.provider_code, &mut data.provider_request_id] {
        if let Some(s) = field.as_mut() {
            scrub(s);
        }
    }
    let mut stack: Vec<&mut serde_json::Value> = data.details.values_mut().collect();
    while let Some(value) = stack.pop() {
        match value {
            serde_json::Value::String(s) => scrub(s),
            serde_json::Value::Array(items) => stack.extend(items.iter_mut()),
            serde_json::Value::Object(map) => stack.extend(map.values_mut()),
            _ => {}
        }
    }
    e
}

/// Provider text made display-safe: URLs redacted, secrets scrubbed, truncated.
pub(crate) fn clean(text: &str, max: usize) -> String {
    let text = crate::http::redact_urls_in_text(text);
    redact::truncate(redact::scrub(&text).trim(), max)
}

/// Accept a provider error code only if it looks like an identifier.
pub(crate) fn provider_code(raw: &str) -> Option<String> {
    let ok = !raw.is_empty()
        && raw.len() <= 100
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || "_.-:".contains(c));
    ok.then(|| redact::scrub(raw).into_owned())
}

/// Attach `provider_code` (from `code`, when it looks like an identifier) and
/// `details.provider_message` (the cleaned `message`, or a scrubbed body excerpt when
/// the body carried no message).
pub(crate) fn annotate(e: &mut IrisError, resp: &HttpResponse, code: Option<&str>, message: Option<&str>) {
    if let Some(code) = code.and_then(provider_code) {
        e.provider_code = Some(code);
    }
    let message = match message {
        Some(m) => clean(m, PROVIDER_TEXT_MAX),
        None => resp.body_snippet(PROVIDER_TEXT_MAX),
    };
    if !message.trim().is_empty() {
        e.details.insert("provider_message".to_string(), message.into());
    }
}

/// `": <cleaned provider message>"` for human messages, or empty when there is no
/// message, or nothing is left of it once cleaned.
pub(crate) fn provider_suffix(message: Option<&str>) -> String {
    match message.map(|m| clean(m, PROVIDER_TEXT_MAX)) {
        Some(m) if !m.is_empty() => format!(": {m}"),
        _ => String::new(),
    }
}

/// How a synchronous paid adapter names itself in the errors of a paid request.
pub(crate) struct PaidWording {
    pub provider: ProviderId,
    /// The provider as a sentence subject, e.g. "OpenAI" or "the Vercel AI Gateway".
    pub who: &'static str,
    /// Where to check usage, e.g. "your OpenAI usage".
    pub usage: &'static str,
    /// Whom to quote a request id to, e.g. "OpenAI support".
    pub support: &'static str,
}

/// A paid request may have reached the provider, but no complete answer arrived
/// (timeout, reset, truncated body, or an answer over the size limit):
/// `submission_uncertain` (not retryable) with `details.charge_possible` and
/// `details.transport` (`timeout` or `other`, never relabeled). Never retried (see
/// docs/concepts/paid-requests.md). `hint_suffix` is added to the hint, e.g. the
/// request id to quote.
pub(crate) fn uncertain_transport(t: &TransportError, w: &PaidWording, hint_suffix: &str) -> IrisError {
    let mut err = t.to_iris();
    err.code = ErrorCode::SubmissionUncertain;
    err.retryable = Some(false);
    let what = match (t.kind, t.status) {
        (TransportKind::Timeout, _) => "the time limit passed",
        // The status line arrived: the answer was cut off or longer than Iris reads.
        (_, Some(_)) => "the answer could not be read in full",
        _ => "the connection failed after the request was sent",
    };
    err.message = format!(
        "{} may have received this image request, but no complete response arrived ({what}; {}): {}",
        w.who, t.url, t.message
    );
    err.hint = Some(format!(
        "Iris did not retry automatically; the provider may have billed this request. Check {} before running \
         the command again{hint_suffix}",
        w.usage
    ));
    err.with_detail("charge_possible", true)
}

/// A 2xx answer to a paid request that Iris cannot use: `provider_bad_response`. The
/// request completed, so it may have been billed (`details.charge_possible`).
pub(crate) fn bad_response(resp: &HttpResponse, why: &str, w: &PaidWording) -> IrisError {
    IrisError::new(ErrorCode::ProviderBadResponse, format!("{} returned an unusable response: {why}", w.who))
        .with_provider(w.provider)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(resp.request_id.clone())
        .with_detail("charge_possible", true)
        .with_hint(format!(
            "the request completed, so {} may have billed it; Iris did not retry automatically. Quote the provider \
             request id if you contact {}",
            w.who, w.support
        ))
}

/// An error for a non-2xx answer: provider, status, request id, and the provider's
/// code and message ([`annotate`]).
pub(crate) fn error_from(
    resp: &HttpResponse,
    provider: ProviderId,
    code: ErrorCode,
    message: String,
    provider_code: Option<&str>,
    provider_message: Option<&str>,
) -> IrisError {
    let mut e = IrisError::new(code, message)
        .with_provider(provider)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(resp.request_id.clone());
    annotate(&mut e, resp, provider_code, provider_message);
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_codes_must_look_like_identifiers() {
        assert_eq!(provider_code("rate_limit_exceeded").as_deref(), Some("rate_limit_exceeded"));
        assert_eq!(provider_code("a b"), None);
        assert_eq!(provider_code(""), None);
        assert_eq!(provider_code(&"x".repeat(101)), None);
    }

    #[test]
    fn an_empty_or_blank_message_gives_no_suffix() {
        assert_eq!(provider_suffix(None), "");
        assert_eq!(provider_suffix(Some("   ")), "");
        assert_eq!(provider_suffix(Some(" no ")), ": no");
    }
}
