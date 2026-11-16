//! HTTP calls to the Vercel AI Gateway and the classification of its answers (see
//! docs/contributing/architecture.md "Where invariants live" for retry classes and the
//! paid-submit policy).
//!
//! Paid image requests run under [`RetryClass::PaidSubmit`]: they are resent only
//! when the gateway provably did not process them, which is a connection that failed
//! before sending. A 429 is not resent: the rate-limit page says to retry it, but with
//! your own provider key (BYOK) the gateway retries a failed request with Vercel's
//! keys, so a 429 can follow an attempt that the provider may have billed. The gateway
//! documents no
//! idempotency key for its Images endpoint, and no 5xx answer as "not processed", so
//! any other open outcome (HTTP 408 or 5xx, a timeout or a lost connection after
//! sending) is `submission_uncertain`: reported, never retried.

use bytes::Bytes;
use reqwest::header::CONTENT_TYPE;

use super::wire::{Availability, AvailabilityList, parse_error};
use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::http::{AuthHeader, Call, HttpError, HttpResponse, RetryClass, Verdict};
use crate::providers::errors::{self, scrub_credential};
use crate::providers::{AccountAccess, ProviderContext};

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
    // A 429 is returned, not resent (see `classify_paid`), with the delay asked for.
    let call = Call::new(RetryClass::PaidSubmit, ctx.timeouts.generate)
        .with_request_id_header(REQUEST_ID_HEADER)
        .with_provider(VERCEL)
        .without_rejection_retries();
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
        Err(HttpError::Transport(t)) if t.after_send => errors::uncertain_transport(&t, &WORDING, ""),
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

/// How the paid-request errors name the gateway.
const WORDING: errors::PaidWording = errors::PaidWording {
    provider: VERCEL,
    who: "the Vercel AI Gateway",
    usage: "the AI Gateway usage page of your Vercel team",
    support: "Vercel support",
};

/// Classify a non-2xx answer to a paid image request:
///
/// | answer | code | retried |
/// |---|---|---|
/// | 429 | `rate_limited` (retryable by the caller) | no: with BYOK an earlier attempt may have been billed |
/// | 402 (budget or credits) | `quota_exceeded` | no |
/// | 400, 409, 422, 413 | `invalid_argument` | no |
/// | 401 | `authentication_failed` | no |
/// | 403 | `permission_denied` | no |
/// | 404 | `permission_denied` (model not found) | no |
/// | 408, 500–599 | `submission_uncertain` (`charge_possible`, not retryable) | no |
fn classify_paid(resp: &HttpResponse, model: &str) -> Verdict {
    let status = resp.status.as_u16();
    let wire = parse_error(&resp.body);
    let says = wire.suffix();
    let err = |code: ErrorCode, message: String| wire.error(resp, VERCEL, code, message);
    match status {
        // Not retried: with your own provider key (BYOK), the gateway retries a failed
        // request with Vercel's keys, so a 429 can follow an attempt that the provider
        // may have billed (docs/contributing/decisions.md, "Vercel AI Gateway").
        429 => Verdict::RetryableRejection {
            error: err(
                ErrorCode::RateLimited,
                format!("the Vercel AI Gateway is rate limiting requests (HTTP 429){says}"),
            )
            .with_retryable(Some(true))
                .with_hint(
                    "wait and run the command again; Iris does not resend a paid gateway request itself, because \
                     with your own provider key an earlier attempt may have been billed. The free tier has lower \
                     per-model limits, and buying AI Gateway Credits removes the gateway's own limits",
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
            .with_hint(
                "`iris models show <MODEL> --check-access` checks availability for free; if every model gets this \
                 error, check the base URL (IRIS_VERCEL_BASE_URL or providers.vercel.base_url), which ends in /v1",
            ),
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
            wire.annotate(&mut e, resp);
            Verdict::Final(e)
        }
    }
}

/// A 2xx answer Iris cannot use. The request completed, so it may have been billed.
pub(super) fn bad_response(resp: &HttpResponse, why: &str) -> IrisError {
    errors::bad_response(resp, why, &WORDING)
}

/// `GET {base}/models?include_availability` (free, authenticated, `IdempotentRead`),
/// read once for every model in `models`: eligible for this team → available,
/// ineligible or not listed → unavailable, `unknown` or an unreadable list → unknown.
/// A 401, 403, or 404 is an error for every model; a rate limit, a server error, or a
/// network failure that outlasted the retries is unknown, as for the other providers.
pub(super) async fn check_access(
    models: &[&str],
    ctx: &ProviderContext,
) -> Vec<Result<AccountAccess, IrisError>> {
    let all = |result: Result<AccountAccess, IrisError>| models.iter().map(|_| result.clone()).collect();
    let auth = match AuthHeader::new(
        super::CREDENTIAL_HEADER.name,
        super::CREDENTIAL_HEADER.prefix,
        &ctx.credential,
    ) {
        Ok(auth) => auth,
        Err(e) => return all(Err(e)),
    };
    let url = format!("{}?include_availability", endpoint(ctx, "models"));
    let call = Call::new(RetryClass::IdempotentRead, ctx.timeouts.poll)
        .with_request_id_header(REQUEST_ID_HEADER)
        .with_provider(VERCEL);
    let result = ctx.http.execute(&call, |c| Ok(auth.apply(c.get(&url))), classify_read).await;
    let error = match result {
        Ok(resp) => {
            let list = AvailabilityList::parse(&resp.body, models);
            return models
                .iter()
                .map(|model| {
                    Ok(match list.of(model) {
                        Availability::Eligible => AccountAccess::Available,
                        Availability::Ineligible => AccountAccess::Unavailable,
                        Availability::Unknown => AccountAccess::Unknown,
                    })
                })
                .collect();
        }
        // A refusal is an error for every model: a 401, a 403 (a blocked team or a plan
        // restriction), or a 404 (a wrong base URL) says nothing about one model.
        Err(HttpError::Error(e)) if matches!(e.provider_status, Some(401 | 403 | 404)) => e,
        // A rate limit, a server error, or a network failure that outlasted the retries
        // leaves the answer open, as for the other providers.
        Err(other) => {
            let err = other.into_iris();
            tracing::debug!(code = %err.code, "vercel access check inconclusive");
            return all(Ok(AccountAccess::Unknown));
        }
    };
    all(Err(scrub_credential(error, ctx)))
}

fn classify_read(resp: &HttpResponse) -> Verdict {
    let wire = parse_error(&resp.body);
    let status = resp.status.as_u16();
    let err = |code: ErrorCode, message: String| wire.error(resp, VERCEL, code, message);
    match status {
        401 => Verdict::Final(
            err(
                ErrorCode::AuthenticationFailed,
                format!("the Vercel AI Gateway rejected the API key (HTTP 401){}", wire.suffix()),
            )
            .with_hint("check that AI_GATEWAY_API_KEY holds a valid AI Gateway API key"),
        ),
        403 => Verdict::Final(
            err(
                ErrorCode::PermissionDenied,
                format!("the Vercel AI Gateway denied access to its model list (HTTP 403){}", wire.suffix()),
            )
            .with_hint(
                "the team may be blocked or its plan may not allow this; check its AI Gateway settings",
            ),
        ),
        404 => Verdict::Final(
            err(
                ErrorCode::ProviderError,
                format!("the Vercel AI Gateway has no model list at this URL (HTTP 404){}", wire.suffix()),
            )
            .with_retryable(Some(false))
            .with_hint(
                "check the base URL (IRIS_VERCEL_BASE_URL or providers.vercel.base_url); it ends in /v1",
            ),
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
            wire.annotate(&mut e, resp);
            Verdict::Final(e)
        }
    }
}
