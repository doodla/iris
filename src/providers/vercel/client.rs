//! HTTP calls to the Vercel AI Gateway and the classification of its answers (see
//! docs/contributing/architecture.md "Where invariants live" for retry classes and the
//! paid-submit policy).
//!
//! Paid image requests run under [`RetryClass::PaidSubmit`]: they are resent only
//! when the gateway provably did not process them: a connection that failed before
//! sending, or a 429, which the rate-limit page documents as "not a failure of your
//! request" that "can succeed later without changes". The gateway documents no
//! idempotency key for its Images endpoint, and no 5xx answer as "not processed", so
//! any other open outcome (HTTP 408 or 5xx, a timeout or a lost connection after
//! sending) is `submission_uncertain`: reported, never retried.

use bytes::Bytes;
use reqwest::header::CONTENT_TYPE;

use super::wire::{self, Availability, WireError};
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::http::{
    AuthHeader, Call, HttpError, HttpResponse, PROVIDER_TEXT_MAX, RetryClass, TransportError, Verdict,
};
use crate::providers::{AccountAccess, ProviderContext};
use crate::redact;

/// Response header Vercel sets on every answer, identifying the request.
pub(super) const REQUEST_ID_HEADER: &str = "x-vercel-id";

const VERCEL: ProviderId = ProviderId::Vercel;

/// `{base}/{path}`, keeping the base URL's path (e.g. `/v1`).
fn endpoint(ctx: &ProviderContext, path: &str) -> String {
    format!("{}/{path}", ctx.base_url.as_str().trim_end_matches('/'))
}

/// POST a JSON body to a paid endpoint under `PaidSubmit`; errors are mapped by
/// [`classify_paid`], and a transport failure after sending is `submission_uncertain`.
pub(super) async fn post_paid(
    ctx: &ProviderContext,
    auth: &AuthHeader,
    path: &str,
    body: Bytes,
    model: &str,
) -> Result<HttpResponse, IrisError> {
    let url = endpoint(ctx, path);
    let call = Call::new(RetryClass::PaidSubmit, ctx.timeouts.generate)
        .with_request_id_header(REQUEST_ID_HEADER)
        .with_provider(VERCEL);
    let result = ctx
        .http
        .execute(
            &call,
            |c| Ok(auth.apply(c.post(&url)).header(CONTENT_TYPE, "application/json").body(body.clone())),
            |resp| classify_paid(resp, model),
        )
        .await;
    let error = match result {
        Ok(response) => return Ok(response),
        Err(HttpError::Transport(t)) if t.after_send => uncertain_transport(&t),
        Err(e) => {
            let mut e = e.into_iris();
            if e.retryable == Some(false) {
                e.retry_after = None;
            }
            e
        }
    };
    Err(scrub_credential(error, ctx))
}

/// Remove the context's own credential from every string of an error (provider text
/// is already scrubbed of the configured credentials by [`redact::scrub`]).
fn scrub_credential(mut e: IrisError, ctx: &ProviderContext) -> IrisError {
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

/// A paid request may have reached the gateway but no complete answer arrived:
/// `submission_uncertain` (not retryable) with `details.charge_possible`.
fn uncertain_transport(t: &TransportError) -> IrisError {
    let mut err = t.to_iris();
    err.code = ErrorCode::SubmissionUncertain;
    err.retryable = Some(false);
    let what = match (t.kind, t.status) {
        (crate::http::TransportKind::Timeout, _) => "the time limit passed",
        (_, Some(_)) => "the answer could not be read in full",
        _ => "the connection failed after the request was sent",
    };
    err.message = format!(
        "the Vercel AI Gateway may have received this image request, but no complete response arrived ({what}; \
         {}): {}",
        t.url, t.message
    );
    err.hint = Some(
        "Iris did not retry automatically; the gateway may have billed this request. Check the AI Gateway \
         usage page of your Vercel team before running the command again"
            .to_string(),
    );
    err.with_detail("charge_possible", true)
}

/// Classify a non-2xx answer to a paid image request:
///
/// | answer | code | retried |
/// |---|---|---|
/// | 429 | `rate_limited` | yes, honoring `Retry-After` |
/// | 402 (budget or credits) | `quota_exceeded` | no |
/// | 400, 409, 422, 413 | `invalid_argument` | no |
/// | 401 | `authentication_failed` | no |
/// | 403 | `permission_denied` | no |
/// | 404 | `permission_denied` (model not found) | no |
/// | 408, 500–599 | `submission_uncertain` (`charge_possible`, not retryable) | no |
fn classify_paid(resp: &HttpResponse, model: &str) -> Verdict {
    let status = resp.status.as_u16();
    let wire = WireError::parse(&resp.body);
    let says = provider_suffix(&wire);
    let err = |code: ErrorCode, message: String| error_from(resp, &wire, code, message);
    match status {
        429 => Verdict::RetryableRejection {
            error: err(ErrorCode::RateLimited, format!("the Vercel AI Gateway is rate limiting requests (HTTP 429){says}"))
                .with_hint(
                    "wait and run the command again; the free tier has lower per-model limits, and buying AI \
                     Gateway Credits removes the gateway's own limits",
                ),
            retry_after: None,
        },
        402 => Verdict::Final(
            err(
                ErrorCode::QuotaExceeded,
                format!("the Vercel AI Gateway refused the request because of a budget or credit limit (HTTP 402){says}"),
            )
            .with_hint(
                "check the team's AI Gateway Credits balance and budgets in the Vercel dashboard; retrying will \
                 not help until the limit changes",
            ),
        ),
        400 | 409 | 422 => Verdict::Final(
            err(ErrorCode::InvalidArgument, format!("the Vercel AI Gateway rejected the request (HTTP {status}){says}"))
                .with_hint("check the prompt and the options against `iris models show <MODEL>`"),
        ),
        413 => Verdict::Final(err(
            ErrorCode::InvalidArgument,
            format!("the Vercel AI Gateway rejected the request as too large (HTTP 413){says}"),
        )),
        401 => Verdict::Final(
            err(ErrorCode::AuthenticationFailed, format!("the Vercel AI Gateway rejected the API key (HTTP 401){says}"))
                .with_hint("check that AI_GATEWAY_API_KEY holds a valid AI Gateway API key"),
        ),
        403 => Verdict::Final(
            err(ErrorCode::PermissionDenied, format!("the Vercel AI Gateway denied access (HTTP 403){says}"))
                .with_hint(
                    "the team's plan, model allowlist, or a regional endpoint may not allow this request; check the \
                     AI Gateway settings of your Vercel team",
                ),
        ),
        404 => Verdict::Final(
            err(
                ErrorCode::PermissionDenied,
                format!("model '{model}' is not available on the Vercel AI Gateway (HTTP 404){says}"),
            )
            .about_model(model)
            .with_hint("`iris models show <MODEL> --check-access` checks availability for free"),
        ),
        408 | 500..=599 => Verdict::Final(
            err(
                ErrorCode::SubmissionUncertain,
                format!("the Vercel AI Gateway answered HTTP {status} and may still have processed this image request{says}"),
            )
            .with_retryable(Some(false))
            .with_detail("charge_possible", true)
            .with_hint(
                "Iris did not retry automatically because the gateway may already have processed (and billed) \
                 this request; check the AI Gateway usage page before running the command again",
            ),
        ),
        _ => {
            let mut e = resp.fallback_error(Some(VERCEL));
            annotate(&mut e, resp, &wire);
            Verdict::Final(e)
        }
    }
}

/// An error for a non-2xx answer: provider, status, request id, provider code, and
/// the scrubbed provider message.
fn error_from(resp: &HttpResponse, wire: &WireError, code: ErrorCode, message: String) -> IrisError {
    let mut e = IrisError::new(code, message)
        .with_provider(VERCEL)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(resp.request_id.clone());
    annotate(&mut e, resp, wire);
    e
}

/// Attach `provider_code` (`error.code`, else `error.type`) and
/// `details.provider_message` (the scrubbed message, or a scrubbed body excerpt).
fn annotate(e: &mut IrisError, resp: &HttpResponse, wire: &WireError) {
    if let Some(code) = wire.code.as_deref().or(wire.kind.as_deref()).and_then(provider_code) {
        e.provider_code = Some(code);
    }
    let message = match &wire.message {
        Some(m) => clean(m, PROVIDER_TEXT_MAX),
        None => resp.body_snippet(PROVIDER_TEXT_MAX),
    };
    if !message.trim().is_empty() {
        e.details.insert("provider_message".to_string(), message.into());
    }
}

fn provider_suffix(wire: &WireError) -> String {
    match &wire.message {
        Some(m) => format!(": {}", clean(m, PROVIDER_TEXT_MAX)),
        None => String::new(),
    }
}

/// Provider text made display-safe: URLs redacted, secrets scrubbed, truncated.
fn clean(text: &str, max: usize) -> String {
    let text = crate::http::redact_urls_in_text(text);
    redact::truncate(redact::scrub(&text).trim(), max)
}

/// Accept a provider error code only if it looks like an identifier.
fn provider_code(raw: &str) -> Option<String> {
    let ok = !raw.is_empty()
        && raw.len() <= 100
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || "_.-:".contains(c));
    ok.then(|| redact::scrub(raw).into_owned())
}

/// A 2xx answer Iris cannot use. The request completed, so it may have been billed.
pub(super) fn bad_response(resp: &HttpResponse, why: &str) -> IrisError {
    IrisError::new(
        ErrorCode::ProviderBadResponse,
        format!("the Vercel AI Gateway returned an unusable response: {why}"),
    )
    .with_provider(VERCEL)
    .with_provider_status(resp.status.as_u16())
    .with_provider_request_id(resp.request_id.clone())
    .with_detail("charge_possible", true)
    .with_hint(
        "the request completed, so the gateway may have billed it; Iris did not retry automatically. Quote the \
         provider request id if you contact Vercel support",
    )
}

/// `GET {base}/models?include_availability` (free, authenticated, `IdempotentRead`):
/// eligible for this team → available, ineligible or not listed → unavailable,
/// `unknown` or an unreadable list → unknown; 401 → `authentication_failed`. Other
/// answers are unknown; transport failures are errors.
pub(super) async fn check_access(model: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError> {
    let auth =
        AuthHeader::new(super::CREDENTIAL_HEADER.name, super::CREDENTIAL_HEADER.prefix, &ctx.credential)?;
    let url = format!("{}?include_availability", endpoint(ctx, "models"));
    let call = Call::new(RetryClass::IdempotentRead, ctx.timeouts.poll)
        .with_request_id_header(REQUEST_ID_HEADER)
        .with_provider(VERCEL);
    let result = ctx.http.execute(&call, |c| Ok(auth.apply(c.get(&url))), classify_read).await;
    let error = match result {
        Ok(resp) => {
            return Ok(match wire::availability(&resp.body, model) {
                Availability::Eligible => AccountAccess::Available,
                Availability::Ineligible => AccountAccess::Unavailable,
                Availability::Unknown => AccountAccess::Unknown,
            });
        }
        Err(HttpError::Error(e)) => match e.provider_status {
            Some(401) | None => e,
            Some(_) => return Ok(AccountAccess::Unknown),
        },
        Err(e) => e.into_iris(),
    };
    Err(scrub_credential(error, ctx))
}

fn classify_read(resp: &HttpResponse) -> Verdict {
    let wire = WireError::parse(&resp.body);
    let status = resp.status.as_u16();
    let err = |code: ErrorCode, message: String| error_from(resp, &wire, code, message);
    match status {
        401 => Verdict::Final(
            err(
                ErrorCode::AuthenticationFailed,
                format!("the Vercel AI Gateway rejected the API key (HTTP 401){}", provider_suffix(&wire)),
            )
            .with_hint("check that AI_GATEWAY_API_KEY holds a valid AI Gateway API key"),
        ),
        429 => Verdict::RetryableRejection {
            error: err(
                ErrorCode::RateLimited,
                "the Vercel AI Gateway is rate limiting requests (HTTP 429)".to_string(),
            ),
            retry_after: None,
        },
        408 | 500..=599 => Verdict::Transient {
            error: err(
                ErrorCode::ProviderError,
                format!("the Vercel AI Gateway returned a server error (HTTP {status})"),
            )
            .with_retryable(Some(true)),
            retry_after: None,
        },
        _ => {
            let mut e = resp.fallback_error(Some(VERCEL));
            annotate(&mut e, resp, &wire);
            Verdict::Final(e)
        }
    }
}
