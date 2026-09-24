//! Veo video generation: `predictLongRunning` submission and operation polling
//! (provider-native asynchronous jobs; see the model catalog's Veo section).
//!
//! The submission is paid and not idempotent. Only a connection failure before
//! sending and a 429 rate limit are retried; every answer that leaves the outcome
//! open (408/5xx, a timeout or reset after sending, an unusable 2xx body) is
//! `submission_uncertain`, and Iris never resubmits it.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::client::{self, API_V1BETA};
use super::wire::{
    GenerateVideoResponse, Operation, PredictLongRunningRequest, RpcStatus, VeoImage, VeoInstance,
    VeoParameters, VeoReference,
};
use crate::catalog::OptionValue;
use crate::catalog::veo::{DEFAULT_ASPECT_RATIO, DEFAULT_DURATION, DEFAULT_RESOLUTION, MAX_REQUEST_BYTES};
use crate::domain::{ProviderId, Warning};
use crate::error::{ErrorCode, IrisError};
use crate::http::{HttpError, HttpResponse, RetryClass, Verdict, same_origin};
use crate::providers::{
    InputImage, ProviderContext, RemoteArtifact, RemoteStatus, SubmittedOperation, VideoRequest,
};
use crate::redact;

/// Media type of Veo outputs (MP4, 24 fps).
const VIDEO_MP4: &str = "video/mp4";

/// Warning code: the provider filtered some outputs of an otherwise successful job.
pub const WARNING_CONTENT_FILTERED: &str = "content_filtered";

const UNCERTAIN_HINT: &str = "the provider may have accepted this paid request; check usage/billing in Google AI \
                              Studio before resubmitting; Iris will not resubmit automatically";

/// Longest operation name accepted (they are short in practice).
const MAX_OPERATION_NAME: usize = 512;

/// The local checks of [`submit`]: a model id that is safe in a URL path and a
/// request that encodes within the inline size limit. Nothing is sent.
pub fn validate(req: &VideoRequest) -> Result<(), IrisError> {
    client::validate_model_id(&req.model)?;
    encode_request(req).map(|_| ())
}

/// Submit a Veo job. Returns the operation name as `remote_id`.
pub async fn submit(req: &VideoRequest, ctx: &ProviderContext) -> Result<SubmittedOperation, IrisError> {
    client::validate_model_id(&req.model)?;
    let body = encode_request(req)?;
    let auth = client::auth(ctx)?;
    let url = client::endpoint(ctx, API_V1BETA, &format!("models/{}:predictLongRunning", req.model));
    let call = client::call(RetryClass::PaidSubmit, ctx.timeouts.submit);

    let resp = ctx
        .http
        .execute(
            &call,
            |c| {
                Ok(auth
                    .apply(c.post(&url))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body.clone()))
            },
            classify_submit,
        )
        .await
        .map_err(|err| match err {
            HttpError::Transport(t) if t.after_send => uncertain(format!(
                "the video request was sent but no complete answer arrived ({}: {}); the provider may have \
                 accepted it",
                t.kind.as_str(),
                t.message
            ))
            .with_detail("transport", t.kind.as_str())
            .with_detail("attempts", t.attempts),
            other => other.into_iris(),
        })?;

    let request_id = client::request_id(&resp);
    let op: Operation = resp.json().map_err(|_| {
        uncertain(
            "the Gemini API accepted the video request but its answer could not be parsed, so the operation \
             id is unknown"
                .to_string(),
        )
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(request_id.clone())
    })?;
    let Some(name) = op.name.filter(|n| is_operation_name(n)) else {
        return Err(uncertain(
            "the Gemini API accepted the video request but returned no usable operation name".to_string(),
        )
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(request_id));
    };
    Ok(SubmittedOperation { remote_id: name, provider_request_id: request_id })
}

/// Build the `predictLongRunning` body. Duration, resolution, and aspect ratio are
/// always sent with their effective values (explicit or the Iris default) so the
/// cost is bounded; the other options only when set. Unknown options are an
/// internal error (never dropped).
pub fn encode_request(req: &VideoRequest) -> Result<bytes::Bytes, IrisError> {
    let mut duration = DEFAULT_DURATION;
    let mut resolution = DEFAULT_RESOLUTION;
    let mut aspect_ratio = DEFAULT_ASPECT_RATIO;
    let mut negative_prompt = None;
    let mut person_generation = None;
    for (name, value) in req.options.iter() {
        match name.as_str() {
            "count" => {
                if value.as_int() != Some(1) {
                    return Err(IrisError::internal(format!(
                        "the Veo adapter can only request one video (count={value})"
                    )));
                }
            }
            "duration" => duration = str_option(name, value)?,
            "resolution" => resolution = str_option(name, value)?,
            "aspect_ratio" => aspect_ratio = str_option(name, value)?,
            "negative_prompt" => negative_prompt = Some(str_option(name, value)?),
            "person_generation" => person_generation = Some(str_option(name, value)?),
            other => {
                return Err(IrisError::internal(format!("the Veo adapter does not map option '{other}'")));
            }
        }
    }
    let duration_seconds: u8 = duration.parse().map_err(|_| {
        IrisError::internal(format!("duration '{duration}' is not a whole number of seconds"))
    })?;

    // The inline images alone are a lower bound of the body size: reject early
    // without encoding them when that bound already reaches the cap.
    let images_b64: usize = [&req.first_frame, &req.last_frame]
        .into_iter()
        .flatten()
        .chain(&req.references)
        .map(|i| client::base64_len(i.bytes.len()))
        .sum();
    if images_b64 >= MAX_REQUEST_BYTES {
        return Err(request_too_large(images_b64, "at least "));
    }
    let references = (!req.references.is_empty()).then(|| {
        req.references.iter().map(|r| VeoReference { image: veo_image(r), reference_type: "ASSET" }).collect()
    });
    let body = PredictLongRunningRequest {
        instances: [VeoInstance {
            prompt: &req.prompt,
            image: req.first_frame.as_ref().map(veo_image),
            last_frame: req.last_frame.as_ref().map(veo_image),
            reference_images: references,
        }],
        parameters: VeoParameters {
            aspect_ratio,
            resolution,
            duration_seconds,
            negative_prompt,
            person_generation,
        },
    };
    let bytes = serde_json::to_vec(&body)
        .map_err(|e| IrisError::internal(format!("could not encode the Veo request: {e}")))?;
    if bytes.len() >= MAX_REQUEST_BYTES {
        return Err(request_too_large(bytes.len(), ""));
    }
    Ok(bytes::Bytes::from(bytes))
}

/// `invalid_argument` for a body at or over the 100 MB inline cap (nothing is sent).
fn request_too_large(size: usize, qualifier: &str) -> IrisError {
    IrisError::invalid(format!(
        "the encoded video request is {qualifier}{size} bytes; inline requests to the Gemini API must stay \
         below {MAX_REQUEST_BYTES} bytes"
    ))
    .with_hint("use smaller input images")
    .with_detail("request_bytes", size as u64)
    .with_detail("limit_bytes", MAX_REQUEST_BYTES as u64)
}

fn veo_image(image: &InputImage) -> VeoImage<'_> {
    VeoImage { bytes_base64_encoded: STANDARD.encode(&image.bytes), mime_type: &image.media_type }
}

fn str_option<'a>(name: &str, value: &'a OptionValue) -> Result<&'a str, IrisError> {
    value
        .as_str()
        .ok_or_else(|| IrisError::internal(format!("option '{name}' must be a string, got {value}")))
}

/// Submission classifier: 408 and every 5xx leave the outcome open (the job may
/// exist and be billed), so they are final `submission_uncertain`, never
/// `Transient` and never `provider_error`. 429 stays a retryable rejection;
/// 4xx rejections map like image errors.
fn classify_submit(resp: &HttpResponse) -> Verdict {
    let status = resp.status.as_u16();
    if status == 408 || resp.status.is_server_error() {
        let (mapped, _) = client::map_error(resp);
        let mut err = uncertain(format!(
            "the Gemini API answered the video request with HTTP {status}, which does not prove the job was \
             not created"
        ))
        .with_provider_status(status)
        .with_provider_request_id(mapped.provider_request_id.clone());
        if let Some(code) = &mapped.provider_code {
            err = err.with_provider_code(code.clone());
        }
        if let Some(msg) = mapped.details.get("provider_message") {
            err = err.with_detail("provider_message", msg.clone());
        }
        return Verdict::Final(err);
    }
    client::classify(resp)
}

/// `submission_uncertain` with [`UNCERTAIN_HINT`] (exit 5; the app records the job as
/// `submission_unknown` and never resubmits).
fn uncertain(message: String) -> IrisError {
    IrisError::new(ErrorCode::SubmissionUncertain, message)
        .with_provider(ProviderId::Gemini)
        .with_detail("charge_possible", true)
        .with_hint(UNCERTAIN_HINT)
}

/// Operation names are placed in the poll URL path, so they must be exactly
/// `models/<segment>/operations/<segment>` (`^models/[^/]+/operations/[^/]+$`).
/// A segment may hold any RFC 3986 unreserved character (`A-Z a-z 0-9 - . _ ~`) but
/// must not be a dot segment (`.` or `..`). Those characters mean nothing in a URL
/// and are never percent-decoded. Everything that could change the requested URL
/// is refused: `/`, `\`, `?`, `#`, `%`, spaces, control characters, and `:`, which
/// selects a custom method in Google APIs (`…/operations/x:cancel`). A real id with
/// another character would turn an accepted, paid submit into
/// `submission_uncertain`, so the set is kept as wide as that safety allows.
pub fn is_operation_name(name: &str) -> bool {
    if name.len() > MAX_OPERATION_NAME {
        return false;
    }
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()),
        (Some("models"), Some(model), Some("operations"), Some(id), None)
            if is_operation_segment(model, 128) && is_operation_segment(id, 256)
    )
}

/// 1..=`max` unreserved characters, and not `.` or `..`.
fn is_operation_segment(segment: &str, max: usize) -> bool {
    !segment.is_empty()
        && segment.len() <= max
        && segment != "."
        && segment != ".."
        && segment.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Poll an operation once (`IdempotentRead`: 429/5xx/timeouts retried with backoff).
pub async fn poll(remote_id: &str, ctx: &ProviderContext) -> Result<RemoteStatus, IrisError> {
    if !is_operation_name(remote_id) {
        return Err(IrisError::invalid(format!(
            "'{}' is not a Veo operation name (models/<model>/operations/<id>)",
            redact::truncate(&redact::scrub(remote_id), 120)
        )));
    }
    let auth = client::auth(ctx)?;
    let url = client::endpoint(ctx, API_V1BETA, remote_id);
    let call = client::call(RetryClass::IdempotentRead, ctx.timeouts.poll);
    let resp = match ctx.http.execute(&call, |c| Ok(auth.apply(c.get(&url))), client::classify).await {
        Ok(resp) => resp,
        Err(HttpError::Error(e)) if e.provider_status == Some(404) => {
            let google_not_found =
                e.provider_code.as_deref().is_some_and(|c| c.split(':').next() == Some("NOT_FOUND"));
            let error = operation_not_found(e, remote_id);
            // Only Google's own NOT_FOUND answer says the operation is unknown; any
            // other 404 (an HTML page, a proxy, a wrong base URL) is just an error.
            return if google_not_found { Ok(RemoteStatus::Gone { error }) } else { Err(error) };
        }
        Err(e) => return Err(e.into_iris().with_remote_operation(remote_id)),
    };
    let request_id = client::request_id(&resp);
    let op: Operation = resp.json().map_err(|_| {
        IrisError::new(
            ErrorCode::ProviderBadResponse,
            "the Gemini API answered the status request with a body Iris could not parse",
        )
        .with_provider(ProviderId::Gemini)
        .with_provider_status(resp.status.as_u16())
        .with_provider_request_id(request_id.clone())
        .with_remote_operation(remote_id)
    })?;
    Ok(interpret(op, remote_id))
}

/// A 404 on a status request. Operations stay pollable for the provider's
/// retention period, so inside it a 404 means the request did not reach the
/// project that owns the job: the error (still `permission_denied`, not retryable
/// as is) keeps the provider's status, code, and request id, and says what to check.
fn operation_not_found(e: IrisError, remote_id: &str) -> IrisError {
    let mut e = e.with_remote_operation(remote_id).with_hint(
        "the job itself is unaffected; check that GEMINI_API_KEY belongs to the Google Cloud project that \
         submitted this job, and that the Gemini base URL (IRIS_GEMINI_BASE_URL / providers.gemini.base_url) is \
         the API the job was submitted through, then check the job again",
    );
    e.message = "the Gemini API answered the status request with not found (HTTP 404)".to_string();
    e
}

/// Map a finished or running operation to [`RemoteStatus`].
///
/// A done operation with output URIs is `Succeeded` with every URI recorded as
/// given, whatever its host: whether Iris is willing to fetch a URI is decided at
/// download time ([`check_output_uri`]), against the base URL configured then, so a
/// refused URI never turns a finished (and billed) job into a failed one. Only an
/// operation error, or a done operation without any output, is `Failed`.
fn interpret(op: Operation, remote_id: &str) -> RemoteStatus {
    if op.done != Some(true) {
        return RemoteStatus::Running { progress: progress_percent(op.metadata.as_ref()) };
    }
    if let Some(status) = op.error {
        return RemoteStatus::Failed { error: operation_error(&status, remote_id) };
    }
    let video = op.response.and_then(|r| r.generate_video_response).unwrap_or_default();
    match outputs(&video) {
        Ok(outputs) if !outputs.is_empty() => {
            let mut warnings = Vec::new();
            if let Some(n) = video.rai_media_filtered_count.filter(|n| *n > 0) {
                warnings.push(Warning::new(
                    WARNING_CONTENT_FILTERED,
                    format!("the provider filtered {n} output(s) of this job for safety"),
                ));
            }
            RemoteStatus::Succeeded { outputs, usage: None, warnings }
        }
        Ok(_) => RemoteStatus::Failed { error: no_output_error(&video, remote_id) },
        Err(error) => RemoteStatus::Failed { error: error.with_remote_operation(remote_id) },
    }
}

/// A numeric percentage (0–100) from the operation metadata, if the provider sends
/// one. The metadata contents for Veo are undocumented, so this is best effort.
fn progress_percent(metadata: Option<&serde_json::Value>) -> Option<f32> {
    let meta = metadata?.as_object()?;
    ["progressPercent", "progressPercentage", "progress"]
        .iter()
        .find_map(|k| meta.get(*k).and_then(serde_json::Value::as_f64))
        .filter(|p| (0.0..=100.0).contains(p))
        .map(|p| p as f32)
}

/// `done` + `error`: the remote job failed (`remote_job_failed`).
fn operation_error(status: &RpcStatus, remote_id: &str) -> IrisError {
    let code_name = status.code.map(client::rpc_code_name);
    let described = code_name.clone().unwrap_or_else(|| "no code".to_string());
    let mut err = IrisError::new(ErrorCode::RemoteJobFailed, format!("the Veo job failed ({described})"))
        .with_provider(ProviderId::Gemini)
        .with_remote_operation(remote_id)
        .with_hint("submitting a new job is billed again; check the prompt and inputs first");
    if let Some(name) = code_name {
        err = err.with_provider_code(name);
    }
    if let Some(m) = status.message.as_deref().map(client::safe_text).filter(|m| !m.trim().is_empty()) {
        err = err.with_detail("provider_message", m);
    }
    err
}

/// Download references of every generated sample, kept exactly as the provider
/// sent them (the raw URI is stored only in the private job record). Only a
/// structural check applies here ([`structural_uri_problem`]); a URI that fails it
/// is not a usable answer at all (`provider_bad_response`).
fn outputs(video: &GenerateVideoResponse) -> Result<Vec<RemoteArtifact>, IrisError> {
    let mut out = Vec::new();
    for sample in video.generated_samples.iter().flatten() {
        let Some(v) = &sample.video else { continue };
        let Some(uri) = v.uri.as_deref() else { continue };
        if let Some(why) = structural_uri_problem(uri) {
            return Err(IrisError::new(
                ErrorCode::ProviderBadResponse,
                format!("the Veo job finished, but the provider's output URI is unusable: {why}"),
            )
            .with_provider(ProviderId::Gemini)
            .with_detail("uri", redact::redact_url(uri)));
        }
        // Veo outputs are MP4; the downloader verifies the bytes.
        out.push(RemoteArtifact { uri: uri.to_string(), media_type: Some(VIDEO_MP4.to_string()) });
    }
    Ok(out)
}

/// Why `uri` cannot be a download reference at all: not a URL, not http(s), or
/// carrying user information or a fragment. `None` if it is structurally usable.
fn structural_uri_problem(uri: &str) -> Option<&'static str> {
    let Ok(url) = url::Url::parse(uri) else {
        return Some("it is not a valid URL");
    };
    if !matches!(url.scheme(), "http" | "https") {
        return Some("it is not an http(s) URL");
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Some("it carries user information or a fragment");
    }
    None
}

/// `done` without a usable video: filtered → `content_blocked`; otherwise the
/// answer is malformed → `provider_bad_response`.
fn no_output_error(video: &GenerateVideoResponse, remote_id: &str) -> IrisError {
    let filtered = video.rai_media_filtered_count.unwrap_or(0);
    let err = if filtered > 0 {
        let reasons: Vec<String> =
            video.rai_media_filtered_reasons.iter().flatten().map(|r| client::safe_text(r)).collect();
        IrisError::new(ErrorCode::ContentBlocked, "the provider's safety filters blocked the generated video")
            .with_detail("reasons", reasons)
            .with_detail("filtered_count", filtered)
            .with_hint("blocked videos are not charged; change the prompt or inputs and submit a new job")
    } else if video
        .generated_samples
        .iter()
        .flatten()
        .any(|s| s.video.as_ref().is_some_and(|v| v.encoded_video.is_some()))
    {
        IrisError::new(
            ErrorCode::ProviderBadResponse,
            "the Veo job finished with inline video bytes instead of a download URI, which Iris does not support",
        )
    } else {
        IrisError::new(ErrorCode::ProviderBadResponse, "the Veo job finished without a video or an error")
    };
    err.with_provider(ProviderId::Gemini).with_remote_operation(remote_id)
}

/// Download-time trust check of a recorded output URI against the base URL
/// configured now (see [`validate_output_uri`]). A refused URI is `download_failed`
/// (not retryable as is) with the redacted URI in `details.uri`; the job itself
/// stays `succeeded`, and a later download re-checks against the configuration of
/// that time. The credential-origin rule of the downloader applies independently.
pub fn check_output_uri(uri: &str, base_url: &url::Url) -> Result<(), IrisError> {
    validate_output_uri(uri, base_url).map(|_| ()).map_err(|why| {
        IrisError::new(ErrorCode::DownloadFailed, format!("Iris will not download this Veo output: {why}"))
            .with_retryable(Some(false))
            .with_provider(ProviderId::Gemini)
            .with_detail("uri", redact::redact_url(uri))
            .with_detail("base_url", redact::redact_url(base_url.as_str()))
            .with_hint(
                "the job succeeded and the provider keeps its output for about 2 days; Iris downloads Veo \
                 outputs only from Files API download URLs under the configured Gemini base URL. A proxy \
                 base URL must rewrite these URIs to its own origin and path prefix; otherwise point \
                 IRIS_GEMINI_BASE_URL / providers.gemini.base_url back at \
                 https://generativelanguage.googleapis.com and download again",
            )
    })
}

/// Accept an output URI only if it is on the configured base origin and its path is
/// `<base path>/v1beta/files/<id>:download` with a File id of 1–40 lowercase
/// letters, digits, or dashes, not starting or ending with a dash. Any query
/// (the provider adds `alt=media`) is kept; userinfo and fragments are refused.
pub fn validate_output_uri(uri: &str, base_url: &url::Url) -> Result<url::Url, String> {
    let url = url::Url::parse(uri).map_err(|_| "the output URI is not a valid URL".to_string())?;
    if !same_origin(&url, base_url) {
        return Err("the output URI is not on the configured Gemini API origin".to_string());
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err("the output URI carries user information or a fragment".to_string());
    }
    let prefix = format!("{}/{API_V1BETA}/files/", base_url.path().trim_end_matches('/'));
    let id = url
        .path()
        .strip_prefix(&prefix)
        .and_then(|rest| rest.strip_suffix(":download"))
        .ok_or_else(|| "the output URI is not a Files API download path".to_string())?;
    if !is_file_id(id) {
        return Err("the output URI names an invalid file id".to_string());
    }
    Ok(url)
}

/// `^[a-z0-9]([a-z0-9-]{0,38}[a-z0-9])?$`.
fn is_file_id(id: &str) -> bool {
    let b = id.as_bytes();
    let edge = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= 40
        && edge(b[0])
        && edge(b[b.len() - 1])
        && b.iter().all(|&c| edge(c) || c == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_names_are_strictly_shaped() {
        for ok in [
            "models/veo-3.1-lite-generate-preview/operations/abc123xyz",
            "models/veo-3.1-generate-preview/operations/_Op.id~2-x",
            "models/veo/operations/-leading-dash",
            "models/veo/operations/.x",
            "models/veo/operations/a..b",
            "models/veo/operations/...",
        ] {
            assert!(is_operation_name(ok), "{ok}");
        }
        for bad in [
            "",
            "models/veo/operations",
            "models/veo/operations/",
            "models/veo/operations/abc/extra",
            "models/../operations/abc",
            "models/./operations/abc",
            "models/veo/operations/..",
            "models/veo/operations/.",
            "models/veo/operations/a?b",
            "models/veo/operations/a#b",
            "models/veo/operations/a%2Fb",
            "models/veo/operations/a\\b",
            "models/veo/operations/abc:cancel",
            "models/veo/operations/a b",
            "models/veo/operations/a\tb",
            "models/veo/operations/a;b",
            "models/veo/operations/a@b",
            "models/veo/operations/é",
            "operations/abc",
            "models/veo/other/abc",
            "/models/veo/operations/abc",
            "https://evil.example/models/veo/operations/abc",
        ] {
            assert!(!is_operation_name(bad), "{bad}");
        }
        assert!(is_operation_name(&format!("models/veo/operations/{}", "a".repeat(256))));
        assert!(!is_operation_name(&format!("models/veo/operations/{}", "a".repeat(257))));
    }

    #[test]
    fn file_ids_follow_the_files_api_grammar() {
        for ok in ["a", "abc123", "abc-123", "a1-b2-c3", &"a".repeat(40)] {
            assert!(is_file_id(ok), "{ok}");
        }
        for bad in ["", "-abc", "abc-", "ABC", "a_b", "a.b", &"a".repeat(41)] {
            assert!(!is_file_id(bad), "{bad}");
        }
    }
}
