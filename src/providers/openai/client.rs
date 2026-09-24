//! HTTP calls to the OpenAI API and the classification of its answers
//! (see docs/architecture.md "Where invariants live" for retry classes and
//! paid-submit policy).
//!
//! Paid image requests run under [`RetryClass::PaidSubmit`]: they are resent only
//! when OpenAI provably did not process them (a connection that failed before
//! sending, a 429 rate limit, or a 503 `server_is_overloaded`), and never when the
//! response says `x-should-retry: false`. 500/502/504, timeouts, and resets after
//! sending are reported, never retried, because the Images API has no idempotency key.

use bytes::Bytes;
use reqwest::header::CONTENT_TYPE;

use super::wire::WireError;
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::http::{AuthHeader, Call, HttpError, HttpResponse, RetryClass, TransportError, Verdict};
use crate::providers::{AccountAccess, ProviderContext};
use crate::redact;

/// Response header carrying OpenAI's request id.
pub(super) const REQUEST_ID_HEADER: &str = "x-request-id";
/// Request header carrying Iris's own per-request id (ASCII, ≤ 512 chars).
pub(super) const CLIENT_REQUEST_ID_HEADER: &str = "x-client-request-id";

/// Longest provider text kept in messages and details (see docs/json-contract.md).
const PROVIDER_TEXT_MAX: usize = 500;

/// Billing and quota exhaustion codes (HTTP 429, `error.code`, or the broad
/// `error.type` `insufficient_quota`) from OpenAI's error-code guide. Retrying these
/// never helps.
const QUOTA_CODES: &[&str] = &[
    "insufficient_quota",
    "credit_balance_exhausted",
    "organization_spend_limit_exceeded",
    "project_spend_limit_exceeded",
    "organization_usage_limit_exceeded",
];

const OPENAI: ProviderId = ProviderId::OpenAi;

/// `{base}/{path}`, keeping the base URL's path (e.g. `/v1`).
pub(super) fn endpoint(ctx: &ProviderContext, path: &str) -> String {
    format!("{}/{path}", ctx.base_url.as_str().trim_end_matches('/'))
}

/// POST a JSON body to a paid Images endpoint under `PaidSubmit`.
///
/// Every attempt carries a fresh `X-Client-Request-Id` (a ULID). Errors are mapped
/// by [`classify_paid`]; a transport failure after sending becomes `request_timeout`
/// with `details.charge_possible = true` and the client request id of that attempt.
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
        .with_provider(OPENAI);
    let mut client_request_id = String::new();
    let result = ctx
        .http
        .execute(
            &call,
            |c| {
                client_request_id = ulid::Ulid::generate().to_string();
                Ok(auth
                    .apply(c.post(&url))
                    .header(CONTENT_TYPE, "application/json")
                    .header(CLIENT_REQUEST_ID_HEADER, client_request_id.as_str())
                    .body(body.clone()))
            },
            |resp| classify_paid(resp, model),
        )
        .await;
    let error = match result {
        Ok(response) => return Ok(response),
        Err(HttpError::Transport(t)) if t.after_send => uncertain_transport(&t, &client_request_id),
        Err(e) => {
            let mut e = e.into_iris();
            if e.retryable == Some(false) {
                // A Retry-After on a quota or validation error is not an invitation to retry.
                e.retry_after = None;
            }
            if e.provider_status.is_some_and(|s| s == 408 || s >= 500) {
                // The outcome of a server error is unknown; the id lets OpenAI support look it up.
                e = e.with_detail("client_request_id", client_request_id);
            }
            e
        }
    };
    Err(scrub_credential(error, ctx))
}

/// Remove the context's own credential from every string of an error.
///
/// Provider text is already passed through [`redact::scrub`], which covers the
/// credentials in the environment (the only source the CLI uses; see docs/configuration.md). This pass
/// also covers a key handed to the adapter another way, e.g. by a library caller.
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

/// A paid request may have reached OpenAI but no complete answer arrived
/// (timeout, reset, truncated body). Never retried (see docs/jobs.md).
fn uncertain_transport(t: &TransportError, client_request_id: &str) -> IrisError {
    let mut err = t.to_iris();
    err.code = ErrorCode::RequestTimeout;
    err.retryable = ErrorCode::RequestTimeout.default_retryable();
    let what = match t.kind {
        crate::http::TransportKind::Timeout => "no complete response arrived within the time limit",
        _ => "the connection failed after the request was sent",
    };
    err.message = format!("OpenAI did not return a complete response ({what}; {}): {}", t.url, t.message);
    err.hint = Some(format!(
        "Iris did not retry automatically; the provider may have billed this request. Check your OpenAI \
         usage before running the command again (X-Client-Request-Id {client_request_id})"
    ));
    err.with_detail("charge_possible", true).with_detail("client_request_id", client_request_id.to_string())
}

/// Classify a non-2xx answer to a paid image request:
///
/// | answer | code | retried |
/// |---|---|---|
/// | 429 quota/billing code | `quota_exceeded` | no |
/// | other 429 (rate limit) | `rate_limited` | yes, honoring `Retry-After` |
/// | 503 `server_is_overloaded` | `provider_error` (retryable) | yes |
/// | 400 `code = moderation_blocked`, or with `moderation_details` | `content_blocked` | no |
/// | other 400 `image_generation_user_error` | `invalid_argument` (fix the request) | no |
/// | other 400, 409, 422, 413 | `invalid_argument` | no |
/// | 401 | `authentication_failed` | no |
/// | 402 (undocumented; "Payment Required") | `quota_exceeded` | no |
/// | 403 | `permission_denied` (verification / region / project hint) | no |
/// | 404 | `permission_denied` (model not available to this key) | no |
/// | 408, 500–599 | `provider_error` (retryable by the caller) | no |
///
/// OpenAI's image guide uses `error.type = image_generation_user_error` for every
/// user-correctable failure and names `error.code` the stable discriminator; only
/// `moderation_blocked` is a content-policy block.
fn classify_paid(resp: &HttpResponse, model: &str) -> Verdict {
    let status = resp.status.as_u16();
    let wire = WireError::parse(&resp.body);
    let provider_says = provider_suffix(&wire);
    let err = |code: ErrorCode, message: String| error_from(resp, &wire, code, message);
    match status {
        429 if is_quota(&wire) => Verdict::Final(quota_error(resp, &wire)),
        429 => Verdict::RetryableRejection {
            error: err(
                ErrorCode::RateLimited,
                format!("OpenAI is rate limiting requests (HTTP 429){provider_says}"),
            )
            .with_hint(
                "wait and run the command again; unsuccessful requests also count toward the \
                     per-minute limits",
            ),
            retry_after: None,
        },
        503 if wire.is("server_is_overloaded") => Verdict::RetryableRejection {
            error: err(
                ErrorCode::ProviderError,
                format!("OpenAI reports the model is temporarily overloaded (HTTP 503){provider_says}"),
            )
            .with_retryable(Some(true))
            .with_hint("the request was not processed; run the command again later"),
            retry_after: None,
        },
        400 if is_moderation_block(&wire) => Verdict::Final(content_blocked(resp, &wire)),
        400 if wire.is("image_generation_user_error") => Verdict::Final(
            err(
                ErrorCode::InvalidArgument,
                format!("OpenAI could not carry out this image request (HTTP 400){provider_says}"),
            )
            .with_hint(
                "OpenAI reports a problem the request must fix (provider_code names it); change the \
                 prompt, input images, mask, or options accordingly. Iris does not retry this error",
            ),
        ),
        400 | 409 | 422 => Verdict::Final(
            err(
                ErrorCode::InvalidArgument,
                format!("OpenAI rejected the request as invalid (HTTP {status}){provider_says}"),
            )
            .with_hint(
                "check the options against `iris models show <MODEL>` (size, quality, format, compression, \
                 background) and the input images",
            ),
        ),
        413 => Verdict::Final(
            err(
                ErrorCode::InvalidArgument,
                format!("OpenAI rejected the request as too large (HTTP 413){provider_says}"),
            )
            .with_hint("use fewer or smaller input images"),
        ),
        401 => Verdict::Final(
            err(
                ErrorCode::AuthenticationFailed,
                format!("OpenAI rejected the API key (HTTP 401){provider_says}"),
            )
            .with_hint(
                "check that OPENAI_API_KEY holds a valid API key for the intended organization and \
                     project (an IP allowlist can also cause this)",
            ),
        ),
        402 => Verdict::Final(quota_error(resp, &wire)),
        403 => Verdict::Final(
            err(ErrorCode::PermissionDenied, format!("OpenAI denied access (HTTP 403){provider_says}"))
                .with_hint(ACCESS_HINT),
        ),
        404 => Verdict::Final(
            err(
                ErrorCode::PermissionDenied,
                format!(
                    "model '{model}' is not available to this API key or project (HTTP 404){provider_says}"
                ),
            )
            .with_hint(
                "check the project's model permissions and that the organization is verified for GPT \
                 Image models; `iris models show <MODEL> --check-access` checks availability for free",
            ),
        ),
        408 | 500..=599 => Verdict::Final(
            err(
                ErrorCode::ProviderError,
                format!("OpenAI returned a server error (HTTP {status}){provider_says}"),
            )
            .with_retryable(Some(true))
            .with_hint(
                "Iris did not retry automatically because OpenAI may already have processed (and \
                     billed) this request; check your OpenAI usage, then run the command again if needed",
            ),
        ),
        _ => {
            let mut e = resp.fallback_error(Some(OPENAI));
            annotate(&mut e, resp, &wire);
            Verdict::Final(e)
        }
    }
}

const ACCESS_HINT: &str = "GPT Image models may require API Organization Verification \
    (https://platform.openai.com/settings/organization/general); a 403 can also mean an unsupported \
    country or region, or a project without permission for this model";

/// Quota or billing exhaustion: `error.code`/`error.type` in [`QUOTA_CODES`].
fn is_quota(wire: &WireError) -> bool {
    QUOTA_CODES.iter().any(|c| wire.is(c))
}

/// A content-policy block: `code = moderation_blocked` (the documented
/// discriminator), or a `moderation_details` object, which OpenAI attaches only to
/// moderation blocks.
fn is_moderation_block(wire: &WireError) -> bool {
    wire.is("moderation_blocked") || wire.moderation_details
}

fn quota_error(resp: &HttpResponse, wire: &WireError) -> IrisError {
    let hint = match wire.code.as_deref() {
        Some("credit_balance_exhausted") => {
            "the organization has no prepaid credits left; add credits at \
             https://platform.openai.com/settings/organization/billing"
        }
        Some("organization_spend_limit_exceeded") => {
            "the organization reached its spend limit; raise it at \
             https://platform.openai.com/settings/organization/limits"
        }
        Some("project_spend_limit_exceeded") => {
            "the project reached its spend limit; raise it in the project settings on platform.openai.com"
        }
        Some("organization_usage_limit_exceeded") => {
            "the organization reached its OpenAI-assigned usage limit; request a higher limit at \
             https://platform.openai.com/settings/organization/limits"
        }
        _ => {
            "check credits, billing, and spend limits at https://platform.openai.com/settings/organization/billing"
        }
    };
    error_from(
        resp,
        wire,
        ErrorCode::QuotaExceeded,
        format!(
            "OpenAI refused the request because of billing or quota limits (HTTP {}){}",
            resp.status.as_u16(),
            provider_suffix(wire)
        ),
    )
    .with_hint(format!("{hint}; retrying will not help until the limit changes"))
}

fn content_blocked(resp: &HttpResponse, wire: &WireError) -> IrisError {
    let mut context = Vec::new();
    if let Some(stage) = &wire.moderation_stage {
        context.push(format!("moderation stage: {}", clean(stage, 40)));
    }
    if !wire.categories.is_empty() {
        let cats: Vec<String> = wire.categories.iter().take(16).map(|c| clean(c, 40)).collect();
        context.push(format!("categories: {}", cats.join(", ")));
    }
    let context = if context.is_empty() { String::new() } else { format!(" ({})", context.join("; ")) };
    let mut e = error_from(
        resp,
        wire,
        ErrorCode::ContentBlocked,
        format!("OpenAI blocked this request under its content policy{context}"),
    )
    .with_hint("change the prompt or input images; Iris does not retry blocked requests");
    if let Some(stage) = &wire.moderation_stage {
        e = e.with_detail("moderation_stage", clean(stage, 40));
    }
    if !wire.categories.is_empty() {
        let cats: Vec<serde_json::Value> =
            wire.categories.iter().take(16).map(|c| serde_json::Value::String(clean(c, 40))).collect();
        e = e.with_detail("categories", serde_json::Value::Array(cats));
    }
    e
}

/// An error for a non-2xx answer: provider, status, request id, provider code, and
/// the scrubbed provider message.
fn error_from(resp: &HttpResponse, wire: &WireError, code: ErrorCode, message: String) -> IrisError {
    let mut e = IrisError::new(code, message)
        .with_provider(OPENAI)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(resp.request_id.clone());
    annotate(&mut e, resp, wire);
    e
}

/// Attach `provider_code` (`error.code`, else `error.type`) and
/// `details.provider_message` (the scrubbed `error.message`, or a scrubbed body
/// excerpt when the body is not the documented JSON error).
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

/// `": <scrubbed provider message>"` for human messages, or empty.
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
    IrisError::new(ErrorCode::ProviderBadResponse, format!("OpenAI returned an unusable response: {why}"))
        .with_provider(OPENAI)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(resp.request_id.clone())
        .with_detail("charge_possible", true)
        .with_hint(
            "the request completed, so OpenAI may have billed it; Iris did not retry automatically. \
             Quote the provider request id if you contact OpenAI support",
        )
}

/// `GET {base}/models/{model}`: a free metadata read (`IdempotentRead`).
/// 200 → available, 404 → unavailable, 401 → `authentication_failed`, any other
/// answer → unknown. Transport failures are returned as errors.
pub(super) async fn check_access(model: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError> {
    let auth =
        AuthHeader::new(super::CREDENTIAL_HEADER.name, super::CREDENTIAL_HEADER.prefix, &ctx.credential)?;
    let mut url = ctx.base_url.clone();
    url.path_segments_mut()
        .map_err(|()| IrisError::internal("the OpenAI base URL cannot carry a path"))?
        .pop_if_empty()
        .push("models")
        .push(model);
    let call = Call::new(RetryClass::IdempotentRead, ctx.timeouts.poll)
        .with_request_id_header(REQUEST_ID_HEADER)
        .with_provider(OPENAI);
    let result = ctx
        .http
        .execute(&call, |c| Ok(auth.apply(c.get(url.clone()))), |resp| classify_read(resp, model))
        .await;
    let error = match result {
        Ok(_) => return Ok(AccountAccess::Available),
        Err(HttpError::Error(e)) => match e.provider_status {
            Some(404) => return Ok(AccountAccess::Unavailable),
            Some(401) | None => e,
            Some(_) => return Ok(AccountAccess::Unknown),
        },
        Err(e) => e.into_iris(),
    };
    Err(scrub_credential(error, ctx))
}

fn classify_read(resp: &HttpResponse, model: &str) -> Verdict {
    let wire = WireError::parse(&resp.body);
    let status = resp.status.as_u16();
    let err = |code: ErrorCode, message: String| error_from(resp, &wire, code, message);
    match status {
        401 => Verdict::Final(
            err(
                ErrorCode::AuthenticationFailed,
                format!("OpenAI rejected the API key (HTTP 401){}", provider_suffix(&wire)),
            )
            .with_hint("check that OPENAI_API_KEY holds a valid API key"),
        ),
        404 => Verdict::Final(err(
            ErrorCode::PermissionDenied,
            format!("model '{model}' is not available to this API key or project (HTTP 404)"),
        )),
        429 if is_quota(&wire) => Verdict::Final(quota_error(resp, &wire)),
        429 => Verdict::RetryableRejection {
            error: err(ErrorCode::RateLimited, "OpenAI is rate limiting requests (HTTP 429)".to_string()),
            retry_after: None,
        },
        408 | 500..=599 => Verdict::Transient {
            error: err(ErrorCode::ProviderError, format!("OpenAI returned a server error (HTTP {status})"))
                .with_retryable(Some(true)),
            retry_after: None,
        },
        _ => {
            let mut e = resp.fallback_error(Some(OPENAI));
            annotate(&mut e, resp, &wire);
            Verdict::Final(e)
        }
    }
}
