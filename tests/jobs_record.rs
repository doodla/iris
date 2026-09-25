//! Job record schema and status transitions (see docs/jobs.md).

use std::time::Duration;

use iris::catalog::{
    InputCounts, InputSpec, Lifecycle, Limits, ModelSpec, OptionKind, OptionSpec, OptionValue, OutputSpec,
    ResolvedOptions,
};
use iris::domain::{Artifact, DownloadState, JobStatus, Operation, ProviderId, Usage, Warning};
use iris::error::{ErrorCode, IrisError};
use iris::jobs::{JobId, JobRecord, NewJob, OutputPlan, PollApplied, PromptRecord, request_metadata};
use iris::providers::{RemoteArtifact, RemoteStatus, SubmittedOperation};
use jiff::Timestamp;
use serde_json::{Map, Value, json};

const T0: i64 = 1_790_000_000; // 2026-09-21T14:13:20Z
const REMOTE_URI: &str =
    "https://generativelanguage.googleapis.com/v1beta/files/abc:download?alt=media&sig=s3cr3t";

fn ts(offset_secs: i64) -> Timestamp {
    Timestamp::from_second(T0 + offset_secs).unwrap()
}

fn new_job() -> NewJob {
    let mut request = Map::new();
    request.insert("duration_seconds".into(), json!(4));
    NewJob {
        provider: ProviderId::Gemini,
        model: "veo-test".into(),
        operation: Operation::VideoGenerate,
        request,
        prompt: PromptRecord::new("a cat surfing", false),
        output_plan: OutputPlan::default(),
        cost_estimate: None,
    }
}

fn submitted() -> SubmittedOperation {
    SubmittedOperation {
        remote_id: "models/veo-test/operations/op123".into(),
        provider_request_id: Some("req-1".into()),
    }
}

fn running_record() -> JobRecord {
    let mut rec = JobRecord::new(new_job(), ts(0)).unwrap();
    rec.mark_submitted(&submitted(), ts(1)).unwrap();
    rec
}

fn succeeded_record(outputs: usize) -> JobRecord {
    let mut rec = running_record();
    let outputs = (0..outputs)
        .map(|i| RemoteArtifact { uri: format!("{REMOTE_URI}&n={i}"), media_type: Some("video/mp4".into()) })
        .collect();
    let applied = rec
        .apply_poll(RemoteStatus::Succeeded { outputs, usage: None, warnings: vec![] }, None, ts(30))
        .unwrap();
    assert!(matches!(applied, PollApplied::Succeeded { .. }));
    rec
}

fn artifact(index: u32) -> Artifact {
    Artifact {
        index,
        path: format!("/tmp/out/job-{index}.mp4"),
        media_type: "video/mp4".into(),
        bytes: 1234,
        sha256: "ab".repeat(32),
        width: None,
        height: None,
        duration_seconds: Some(4.0),
    }
}

#[test]
fn new_record_matches_the_v1_schema_shape() {
    let rec = JobRecord::new(new_job(), ts(0)).unwrap();
    assert_eq!(rec.status(), JobStatus::Submitting);
    assert!(JobId::is_valid(rec.job_id().as_str()));
    let value = serde_json::to_value(&rec).unwrap();
    let mut keys: Vec<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    let mut expected = vec![
        "schema_version",
        "job_id",
        "provider",
        "model",
        "operation",
        "status",
        "created_at",
        "updated_at",
        "submitted_at",
        "completed_at",
        "last_checked_at",
        "remote_operation_id",
        "provider_request_id",
        "remote_expires_at",
        "request",
        "prompt",
        "output_plan",
        "outputs",
        "error",
        "usage",
        "cost_estimate",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["status"], "submitting");
    assert_eq!(value["provider"], "gemini");
    assert_eq!(value["operation"], "video.generate");
    assert_eq!(value["created_at"], ts(0).to_string());
    assert!(value["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(value["submitted_at"], Value::Null);
    assert_eq!(value["output_plan"], json!({"dir": null, "path": null, "overwrite": false}));
    assert_eq!(value["prompt"]["text"], Value::Null);
    assert_eq!(value["prompt"]["chars"], 13);
}

#[test]
fn synchronous_operations_never_create_job_records() {
    for op in [Operation::ImageGenerate, Operation::ImageEdit] {
        let mut job = new_job();
        job.operation = op;
        let err = JobRecord::new(job, ts(0)).unwrap_err();
        assert_eq!(err.code, ErrorCode::InternalError);
    }
}

#[test]
fn prompt_text_is_stored_only_when_enabled() {
    let hidden = PromptRecord::new("héllo wörld", false);
    assert_eq!(hidden.text, None);
    assert_eq!(hidden.chars, 11);
    assert_eq!(hidden.sha256.len(), 64);
    let stored = PromptRecord::new("héllo wörld", true);
    assert_eq!(stored.text.as_deref(), Some("héllo wörld"));
    assert_eq!(stored.sha256, hidden.sha256);
    // Known vector: sha256("abc").
    assert_eq!(
        PromptRecord::new("abc", false).sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn submission_transitions() {
    let mut rec = JobRecord::new(new_job(), ts(0)).unwrap();
    rec.mark_submitted(&submitted(), ts(5)).unwrap();
    assert_eq!(rec.status(), JobStatus::Running);
    assert_eq!(rec.remote_operation_id(), Some("models/veo-test/operations/op123"));
    assert_eq!(rec.provider_request_id(), Some("req-1"));
    assert_eq!(rec.submitted_at(), Some(ts(5)));
    assert_eq!(rec.updated_at(), ts(5));

    // A running job cannot be "submitted" or "rejected" again.
    assert_eq!(rec.mark_submitted(&submitted(), ts(6)).unwrap_err().code, ErrorCode::InternalError);
    let rejection = IrisError::new(ErrorCode::ProviderError, "boom");
    assert_eq!(rec.mark_rejected(&rejection, ts(6)).unwrap_err().code, ErrorCode::InternalError);
    assert_eq!(rec.status(), JobStatus::Running);

    // Definite rejection.
    let mut rejected = JobRecord::new(new_job(), ts(0)).unwrap();
    let err = IrisError::new(ErrorCode::InvalidArgument, "bad duration").with_provider_status(400);
    rejected.mark_rejected(&err, ts(2)).unwrap();
    assert_eq!(rejected.status(), JobStatus::Failed);
    let body = rejected.error().unwrap();
    assert_eq!(body.code, ErrorCode::InvalidArgument);
    assert_eq!(body.job_id.as_deref(), Some(rejected.job_id().as_str()));
    assert_eq!(rejected.completed_at(), Some(ts(2)));

    // Ambiguous outcome.
    let mut unknown = JobRecord::new(new_job(), ts(0)).unwrap();
    let err = IrisError::new(ErrorCode::SubmissionUncertain, "timed out after sending");
    unknown.mark_submission_unknown(&err, ts(3)).unwrap();
    assert_eq!(unknown.status(), JobStatus::SubmissionUnknown);
    assert!(unknown.status().is_terminal());
    assert_eq!(unknown.error().unwrap().code, ErrorCode::SubmissionUncertain);
    assert_eq!(unknown.completed_at(), Some(ts(3)));
    // It cannot be polled.
    let poll = unknown.apply_poll(RemoteStatus::Running { progress: None }, None, ts(4));
    assert_eq!(poll.unwrap_err().code, ErrorCode::InternalError);
}

#[test]
fn late_operation_id_resolves_a_stale_submission() {
    // The stale rule may relabel a record while its own process is still waiting
    // for the (slow, retried) submit response; a definite id still wins.
    let mut rec = JobRecord::new(new_job(), ts(0)).unwrap();
    assert!(rec.resolve_stale_submitting(ts(1000), Duration::from_secs(60)));
    assert_eq!(rec.status(), JobStatus::SubmissionUnknown);
    assert!(rec.completed_at().is_some());
    rec.mark_submitted(&submitted(), ts(1001)).unwrap();
    assert_eq!(rec.status(), JobStatus::Running);
    assert!(rec.error().is_none());
    assert_eq!(rec.completed_at(), None, "a running job has not completed");
}

#[test]
fn stale_submitting_becomes_submission_unknown_after_timeout_plus_grace() {
    let timeout = Duration::from_secs(60);
    let mut rec = JobRecord::new(new_job(), ts(0)).unwrap();
    assert!(!rec.is_stale_submitting(ts(120), timeout), "exactly timeout + 60s is not yet stale");
    assert!(!rec.resolve_stale_submitting(ts(120), timeout));
    assert_eq!(rec.status(), JobStatus::Submitting);

    assert!(rec.is_stale_submitting(ts(121), timeout));
    assert!(rec.resolve_stale_submitting(ts(121), timeout));
    assert_eq!(rec.status(), JobStatus::SubmissionUnknown);
    let err = rec.error().unwrap();
    assert_eq!(err.code, ErrorCode::SubmissionUncertain);
    assert!(err.hint.as_deref().unwrap().contains("before resubmitting"));
    assert_eq!(rec.updated_at(), ts(121));
    // Terminal from the moment it became stale; stable across repeated reports.
    assert_eq!(rec.completed_at(), Some(ts(120)));
    let mut later = JobRecord::with_id(rec.job_id().clone(), new_job(), ts(0)).unwrap();
    later.resolve_stale_submitting(ts(5000), timeout);
    assert_eq!(later.completed_at(), Some(ts(120)));

    // Idempotent, and never applies to other statuses.
    assert!(!rec.resolve_stale_submitting(ts(10_000), timeout));
    let mut running = running_record();
    assert!(!running.resolve_stale_submitting(ts(10_000), timeout));
    assert_eq!(running.status(), JobStatus::Running);
}

#[test]
fn poll_running_updates_last_checked_only() {
    let mut rec = running_record();
    let applied = rec.apply_poll(RemoteStatus::Running { progress: Some(0.5) }, None, ts(20)).unwrap();
    assert_eq!(applied, PollApplied::Running { progress: Some(0.5) });
    assert_eq!(rec.status(), JobStatus::Running);
    assert_eq!(rec.last_checked_at(), Some(ts(20)));
    assert_eq!(rec.completed_at(), None);
}

#[test]
fn poll_success_records_outputs_usage_and_retention() {
    let mut rec = running_record();
    let usage = Usage { input_tokens: Some(10), ..Usage::default() };
    let applied = rec
        .apply_poll(
            RemoteStatus::Succeeded {
                outputs: vec![RemoteArtifact {
                    uri: REMOTE_URI.into(),
                    media_type: Some("video/mp4".into()),
                }],
                usage: Some(usage.clone()),
                warnings: vec![Warning::new("retention_limited", "download within 2 days")],
            },
            Some(Duration::from_secs(2 * 24 * 3600)),
            ts(100),
        )
        .unwrap();
    assert_eq!(
        applied,
        PollApplied::Succeeded {
            warnings: vec![Warning::new("retention_limited", "download within 2 days")]
        }
    );
    assert_eq!(rec.status(), JobStatus::Succeeded);
    assert_eq!(rec.completed_at(), Some(ts(100)), "when Iris observed completion");
    // Retention counts from submission (ts(1)), not from when Iris happened to poll.
    assert_eq!(rec.remote_expires_at(), Some(ts(1 + 2 * 24 * 3600)));
    assert!(!rec.remote_expired(ts(2 * 24 * 3600)));
    assert!(rec.remote_expired(ts(1 + 2 * 24 * 3600)));
    assert_eq!(rec.usage(), Some(&usage));
    assert_eq!(rec.outputs().len(), 1);
    let out = &rec.outputs()[0];
    assert_eq!(out.index, 0);
    assert_eq!(out.remote_uri, REMOTE_URI);
    assert_eq!(out.download_state, DownloadState::Pending);

    // Without documented retention there is no expiry estimate.
    let mut other = running_record();
    other
        .apply_poll(RemoteStatus::Succeeded { outputs: vec![], usage: None, warnings: vec![] }, None, ts(5))
        .unwrap();
    assert_eq!(other.remote_expires_at(), None);
}

#[test]
fn outputs_with_unusable_uris_fail_alone_and_only_all_of_them_fail_the_job() {
    let video = |uri: &str| RemoteArtifact { uri: uri.into(), media_type: Some("video/mp4".into()) };
    let unusable = [
        "not a url",
        "ftp://generativelanguage.googleapis.com/v1beta/files/abc:download",
        "file:///etc/passwd",
        "https://generativelanguage.googleapis.com/v1beta/files/abc:download#frag",
        "https://user:pw@generativelanguage.googleapis.com/v1beta/files/abc:download",
    ];
    for bad in unusable {
        // A good sample, then an unusable one: the job succeeds with both recorded;
        // only the unusable one is failed, and a warning names it.
        let mut rec = running_record();
        let applied = rec
            .apply_poll(
                RemoteStatus::Succeeded {
                    outputs: vec![video(REMOTE_URI), video(bad)],
                    usage: None,
                    warnings: vec![],
                },
                Some(Duration::from_secs(2 * 24 * 3600)),
                ts(100),
            )
            .unwrap();
        let PollApplied::Succeeded { warnings } = applied else { panic!("{bad}: {applied:?}") };
        assert_eq!(rec.status(), JobStatus::Succeeded, "{bad}");
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "output_item_unusable");
        assert!(warnings[0].message.contains("output 1"), "{}", warnings[0].message);
        let (good, broken) = (&rec.outputs()[0], &rec.outputs()[1]);
        assert_eq!(good.download_state, DownloadState::Pending);
        assert!(good.awaits_download() && good.unusable_reason().is_none());
        assert_eq!(good.unusable_warning(rec.job_id()), None);
        assert_eq!(broken.download_state, DownloadState::Failed);
        assert!(!broken.awaits_download() && broken.unusable_reason().is_some(), "{bad}");
        // The same warning a later download gives for the output (so a command
        // that polls and then downloads can give it once).
        assert_eq!(broken.unusable_warning(rec.job_id()).as_ref(), Some(&warnings[0]));
        let error = broken.last_error.as_ref().unwrap();
        assert_eq!(error.code, ErrorCode::ProviderBadResponse);
        assert_eq!(error.retryable, Some(false));
        let shown = error.details.as_ref().unwrap()["uri"].as_str().unwrap();
        assert!(!shown.contains("pw@"), "{shown}");
        assert!(rec.remote_expires_at().is_some());

        // Nothing usable at all: the job failed, with the provider's bad answer.
        let mut rec = running_record();
        let applied = rec
            .apply_poll(
                RemoteStatus::Succeeded {
                    outputs: vec![video(bad), video(bad)],
                    usage: None,
                    warnings: vec![],
                },
                None,
                ts(100),
            )
            .unwrap();
        assert_eq!(applied, PollApplied::Failed, "{bad}");
        assert_eq!(rec.status(), JobStatus::Failed);
        assert_eq!(rec.completed_at(), Some(ts(100)));
        let error = rec.error().unwrap();
        assert_eq!(error.code, ErrorCode::ProviderBadResponse);
        assert!(error.message.contains("none of its 2 output URI(s)"), "{}", error.message);
        assert_eq!(error.remote_operation_id.as_deref(), Some("models/veo-test/operations/op123"));
        assert!(rec.outputs().is_empty());
    }
}

#[test]
fn errors_of_ended_jobs_are_shown_as_not_retryable_and_kept_as_written() {
    // A submission rejected before anything was accepted (retryable as a
    // submission), then shown by a later `jobs status`/`wait`/`download`.
    for (code, retryable) in [(ErrorCode::RateLimited, Some(true)), (ErrorCode::ProviderBadResponse, None)] {
        let mut rec = JobRecord::new(new_job(), ts(0)).unwrap();
        let error = IrisError::new(code, "rejected").with_retryable(retryable).with_hint("nothing was sent");
        rec.mark_rejected(&error, ts(1)).unwrap();
        assert_eq!(rec.error().unwrap().retryable, retryable, "the record keeps what was written");
        let view = rec.error_view().unwrap();
        assert_eq!(view.code, code);
        assert_eq!(view.retryable, Some(false), "{code:?}: waiting or downloading again cannot help");
        assert_eq!(view.details.as_ref().unwrap()["submission_retryable"], json!(retryable));
        let hint = view.hint.as_deref().unwrap();
        assert!(hint.starts_with("nothing was sent; ") && hint.contains("new, billed request"), "{hint}");
        assert_eq!(rec.to_view().error.unwrap().retryable, Some(false));
        // Written back unchanged.
        let written = serde_json::to_value(&rec).unwrap();
        assert_eq!(written["error"]["retryable"], json!(retryable));
        assert!(written["error"]["details"].get("submission_retryable").is_none());
    }

    // An error that already says not retryable is shown as it is.
    let mut rec = running_record();
    let blocked = IrisError::new(ErrorCode::ContentBlocked, "blocked").with_hint("change the prompt");
    rec.apply_poll(RemoteStatus::Failed { error: blocked }, None, ts(5)).unwrap();
    let view = rec.error_view().unwrap();
    assert_eq!(view.retryable, Some(false));
    assert!(view.details.is_none_or(|d| !d.contains_key("submission_retryable")));
    assert_eq!(view.hint.as_deref(), Some("change the prompt"));
}

#[test]
fn poll_failure_and_gone() {
    let mut failed = running_record();
    let err = IrisError::new(ErrorCode::ContentBlocked, "blocked by safety filters");
    assert_eq!(
        failed.apply_poll(RemoteStatus::Failed { error: err }, None, ts(50)).unwrap(),
        PollApplied::Failed
    );
    assert_eq!(failed.status(), JobStatus::Failed);
    assert_eq!(failed.completed_at(), Some(ts(50)));
    let body = failed.error().unwrap();
    assert_eq!(body.code, ErrorCode::ContentBlocked);
    assert_eq!(body.remote_operation_id.as_deref(), Some("models/veo-test/operations/op123"));

    // Without a known retention, the provider's "not found" is final.
    let mut gone = running_record();
    assert_eq!(gone.apply_poll(not_found(), None, ts(50)).unwrap(), PollApplied::Expired);
    assert_eq!(gone.status(), JobStatus::Expired);
    assert_eq!(gone.error().unwrap().code, ErrorCode::ArtifactExpired);
    assert_eq!(gone.completed_at(), Some(ts(50)));
}

/// The adapter's "operation not found" answer (a Google NOT_FOUND 404).
fn not_found() -> RemoteStatus {
    RemoteStatus::Gone {
        error: IrisError::new(ErrorCode::PermissionDenied, "not found (HTTP 404)")
            .with_retryable(Some(false))
            .with_provider(ProviderId::Gemini)
            .with_provider_status(404)
            .with_provider_code("NOT_FOUND")
            .with_provider_request_id(Some("req-404".into()))
            .with_detail("provider_message", "Operation not found.")
            .with_hint("check the key's project and the base URL"),
    }
}

#[test]
fn not_found_means_expired_only_after_the_retention_period_since_submission() {
    let retention = Some(Duration::from_secs(2 * 24 * 3600));
    let submitted = ts(1);
    let until = ts(1 + 2 * 24 * 3600);

    // Well inside the retention period: nothing changes, the error comes back.
    let mut rec = running_record();
    assert_eq!(rec.submitted_at(), Some(submitted));
    let before = serde_json::to_value(&rec).unwrap();
    let err = rec.apply_poll(not_found(), retention, ts(60)).unwrap_err();
    assert_eq!(serde_json::to_value(&rec).unwrap(), before, "the record is untouched");
    assert_eq!(rec.status(), JobStatus::Running);
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert_eq!(err.retryable, Some(false));
    assert_eq!((err.provider, err.provider_status), (Some(ProviderId::Gemini), Some(404)));
    assert_eq!(err.job_id.as_deref(), Some(rec.job_id().as_str()));
    assert_eq!(err.job_status, Some(JobStatus::Running));
    assert_eq!(err.remote_operation_id.as_deref(), Some("models/veo-test/operations/op123"));
    assert!(err.message.contains(&until.to_string()), "{}", err.message);
    assert_eq!(err.hint.as_deref(), Some("check the key's project and the base URL"));
    // One second before the end of the period, still not expired.
    assert!(rec.apply_poll(not_found(), retention, ts(2 * 24 * 3600)).is_err());

    // Once the period has passed: expired, keeping the provider's evidence.
    let applied = rec.apply_poll(not_found(), retention, until).unwrap();
    assert_eq!(applied, PollApplied::Expired);
    assert_eq!(rec.status(), JobStatus::Expired);
    assert_eq!(rec.completed_at(), Some(until));
    let body = rec.error().unwrap();
    assert_eq!(body.code, ErrorCode::ArtifactExpired);
    assert_eq!(body.retryable, Some(false));
    assert_eq!(body.provider, Some(ProviderId::Gemini));
    assert_eq!(body.provider_status, Some(404));
    assert_eq!(body.provider_code.as_deref(), Some("NOT_FOUND"));
    assert_eq!(body.provider_request_id.as_deref(), Some("req-404"));
    assert_eq!(body.details.as_ref().unwrap()["provider_message"], "Operation not found.");
    assert!(body.message.contains("retention period"), "{}", body.message);
}

#[test]
fn polls_on_terminal_jobs_change_nothing() {
    let mut rec = succeeded_record(1);
    let before = serde_json::to_value(&rec).unwrap();
    let applied = rec
        .apply_poll(
            RemoteStatus::Failed { error: IrisError::new(ErrorCode::RemoteJobFailed, "late") },
            None,
            ts(99),
        )
        .unwrap();
    assert_eq!(applied, PollApplied::AlreadyTerminal);
    assert_eq!(serde_json::to_value(&rec).unwrap(), before);
    assert_eq!(rec.status(), JobStatus::Succeeded);
}

#[test]
fn download_states_never_change_job_status() {
    let mut rec = succeeded_record(2);
    let err = IrisError::new(ErrorCode::DownloadFailed, "connection reset");
    rec.mark_output_failed(0, &err, ts(40)).unwrap();
    assert_eq!(rec.status(), JobStatus::Succeeded);
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Failed);
    assert_eq!(rec.outputs()[0].last_error.as_ref().unwrap().code, ErrorCode::DownloadFailed);

    assert!(rec.outputs()[0].recorded_file().is_none(), "nothing saved yet");
    rec.mark_output_downloaded(0, &artifact(0), ts(41)).unwrap();
    let out = &rec.outputs()[0];
    assert_eq!(out.download_state, DownloadState::Downloaded);
    let recorded = out.recorded_file().unwrap();
    assert_eq!(recorded.path, std::path::Path::new("/tmp/out/job-0.mp4"));
    assert_eq!((recorded.bytes, recorded.sha256), (1234, "ab".repeat(32).as_str()));
    assert_eq!(recorded.media_type, Some("video/mp4"));
    assert!(out.last_error.is_none());
    assert_eq!(out.bytes, Some(1234));
    assert_eq!(out.downloaded_at, Some(ts(41)));
    assert_eq!(out.artifact(), Some(artifact(0)));

    let expired = IrisError::new(ErrorCode::ArtifactExpired, "410 Gone");
    rec.mark_output_expired(1, &expired, ts(42)).unwrap();
    assert_eq!(rec.outputs()[1].download_state, DownloadState::Expired);
    assert_eq!(rec.status(), JobStatus::Succeeded);

    assert_eq!(
        rec.mark_output_downloaded(7, &artifact(7), ts(43)).unwrap_err().code,
        ErrorCode::InternalError
    );
}

#[test]
fn downloaded_outputs_survive_later_failed_attempts() {
    // E.g. `jobs download -o existing.mp4` without --overwrite after a successful
    // download: the copy fails, but the intact recorded file stays the artifact.
    let mut rec = succeeded_record(1);
    rec.mark_output_downloaded(0, &artifact(0), ts(40)).unwrap();

    let conflict = IrisError::new(ErrorCode::OutputExists, "output file /tmp/existing.mp4 already exists");
    rec.mark_output_failed(0, &conflict, ts(41)).unwrap();
    let out = &rec.outputs()[0];
    assert_eq!(out.download_state, DownloadState::Downloaded);
    assert_eq!(out.local_path.as_deref(), Some(std::path::Path::new("/tmp/out/job-0.mp4")));
    assert_eq!(out.last_error.as_ref().unwrap().code, ErrorCode::OutputExists);
    let view = rec.to_view();
    assert_eq!(view.artifacts, vec![artifact(0)]);
    assert_eq!(view.outputs[0].artifact, Some(artifact(0)));
    assert_eq!(view.outputs[0].last_error.as_ref().unwrap().code, ErrorCode::OutputExists);

    let expired = IrisError::new(ErrorCode::ArtifactExpired, "410 Gone");
    rec.mark_output_expired(0, &expired, ts(42)).unwrap();
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Downloaded);
    assert_eq!(rec.to_view().artifacts, vec![artifact(0)]);

    // A later successful save replaces the record and clears the error.
    rec.mark_output_downloaded(0, &artifact(0), ts(43)).unwrap();
    assert!(rec.outputs()[0].last_error.is_none());
    assert_eq!(rec.status(), JobStatus::Succeeded);
}

#[test]
fn local_failures_cannot_fail_a_running_job() {
    // Download bookkeeping and rejection are refused on a running job; status stays running.
    let mut rec = running_record();
    let err = IrisError::new(ErrorCode::DownloadFailed, "x");
    assert_eq!(rec.mark_output_failed(0, &err, ts(9)).unwrap_err().code, ErrorCode::InternalError);
    assert_eq!(
        rec.mark_output_downloaded(0, &artifact(0), ts(9)).unwrap_err().code,
        ErrorCode::InternalError
    );
    assert_eq!(rec.mark_rejected(&err, ts(9)).unwrap_err().code, ErrorCode::InternalError);
    assert_eq!(rec.mark_submission_unknown(&err, ts(9)).unwrap_err().code, ErrorCode::InternalError);
    assert_eq!(rec.status(), JobStatus::Running);
    assert!(rec.error().is_none());
}

#[test]
fn view_hides_remote_uris_and_lists_downloaded_artifacts() {
    let mut rec = succeeded_record(2);
    rec.mark_output_downloaded(1, &artifact(1), ts(60)).unwrap();
    let view = rec.to_view();
    assert_eq!(view.job_id, rec.job_id().as_str());
    assert_eq!(view.status, JobStatus::Succeeded);
    assert_eq!(view.remote_operation_id.as_deref(), Some("models/veo-test/operations/op123"));
    assert_eq!(view.outputs.len(), 2);
    assert!(view.outputs[0].artifact.is_none());
    assert_eq!(view.outputs[1].artifact, Some(artifact(1)));
    assert_eq!(view.artifacts, vec![artifact(1)]);
    assert_eq!(view.request.get("duration_seconds"), Some(&json!(4)));
    assert_eq!(view.created_at, ts(0).to_string());
    assert_eq!(view.completed_at, Some(ts(30).to_string()));

    let text = serde_json::to_string(&view).unwrap();
    assert!(!text.contains("s3cr3t"), "remote URI leaked: {text}");
    assert!(!text.contains("files/abc"), "remote URI leaked: {text}");
    assert!(!text.contains("remote_uri"));
}

#[test]
fn debug_output_never_shows_signed_uris_or_prompts() {
    let prompt = "a secret product launch teaser";
    let mut job = new_job();
    job.prompt = PromptRecord::new(prompt, true);
    job.request.insert("negative_prompt".into(), json!("no competitor logos"));
    let debug_new = format!("{job:?}");
    assert!(!debug_new.contains("secret product"), "{debug_new}");
    assert!(!debug_new.contains("competitor"), "{debug_new}");
    assert!(debug_new.contains("negative_prompt"), "keys are still listed: {debug_new}");

    let mut rec = JobRecord::new(job, ts(0)).unwrap();
    rec.mark_submitted(&submitted(), ts(1)).unwrap();
    let outputs = vec![RemoteArtifact { uri: REMOTE_URI.into(), media_type: Some("video/mp4".into()) }];
    rec.apply_poll(RemoteStatus::Succeeded { outputs, usage: None, warnings: vec![] }, None, ts(2)).unwrap();
    assert_eq!(rec.prompt().text.as_deref(), Some(prompt), "stored in the record itself");

    for text in [format!("{rec:?}"), format!("{rec:#?}"), format!("{:?}", rec.outputs()[0])] {
        assert!(!text.contains("s3cr3t"), "signed URI leaked: {text}");
        assert!(!text.contains("secret product"), "prompt leaked: {text}");
        assert!(!text.contains("competitor"), "free-text option leaked: {text}");
    }
    let text = format!("{rec:?}");
    assert!(text.contains("sig=REDACTED"), "{text}");
    assert!(text.contains(&format!("Some(<{} chars>)", prompt.chars().count())), "{text}");
    assert!(format!("{:?}", rec.prompt()).contains(&rec.prompt().sha256));
}

#[test]
fn record_round_trips_through_json() {
    let mut rec = succeeded_record(1);
    rec.mark_output_downloaded(0, &artifact(0), ts(70)).unwrap();
    let text = serde_json::to_string_pretty(&rec).unwrap();
    let back: JobRecord = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&rec).unwrap());
    assert_eq!(back.outputs()[0].artifact(), Some(artifact(0)));
}

#[test]
fn unknown_fields_survive_a_round_trip() {
    let rec = succeeded_record(1);
    let mut value = serde_json::to_value(&rec).unwrap();
    value["from_the_future"] = json!({"x": 1});
    value["outputs"][0]["future_output_field"] = json!("keep me");
    value["prompt"]["future_prompt_field"] = json!(true);
    value["output_plan"]["future_plan_field"] = json!([1, 2]);
    let back: JobRecord = serde_json::from_value(value).unwrap();
    let again = serde_json::to_value(&back).unwrap();
    assert_eq!(again["from_the_future"], json!({"x": 1}));
    assert_eq!(again["outputs"][0]["future_output_field"], "keep me");
    assert_eq!(again["prompt"]["future_prompt_field"], true);
    assert_eq!(again["output_plan"]["future_plan_field"], json!([1, 2]));
}

#[test]
fn views_show_the_category_of_the_code_they_show() {
    use iris::error::ErrorCategory;
    let mut rec = succeeded_record(1);
    rec.mark_output_failed(0, &IrisError::new(ErrorCode::DownloadFailed, "reset"), ts(40)).unwrap();
    let mut value = serde_json::to_value(&rec).unwrap();
    // A known code next to a category this version does not know.
    value["outputs"][0]["last_error"]["category"] = json!("some_future_category");
    let back: JobRecord = serde_json::from_value(value.clone()).unwrap();
    let shown = back.to_view().outputs[0].last_error.clone().unwrap();
    assert_eq!((shown.code, shown.category), (ErrorCode::DownloadFailed, ErrorCategory::Artifact));
    assert!(shown.details.is_none(), "a known code needs no recorded_code: {shown:?}");
    // The record itself keeps what was written.
    assert_eq!(
        serde_json::to_value(&back).unwrap()["outputs"][0]["last_error"],
        value["outputs"][0]["last_error"]
    );
}

#[test]
fn extension_fields_cannot_shadow_record_fields() {
    let mut rec = running_record();
    assert_eq!(rec.set_extra("status", json!("failed")).unwrap_err().code, ErrorCode::InternalError);
    assert_eq!(rec.set_extra("outputs", json!([])).unwrap_err().code, ErrorCode::InternalError);
    rec.set_extra("x_custom", json!(1)).unwrap();
    let value = serde_json::to_value(&rec).unwrap();
    assert_eq!(value["status"], "running");
    assert_eq!(value["x_custom"], 1);
}

#[test]
fn records_with_invalid_job_ids_do_not_parse() {
    let mut value = serde_json::to_value(JobRecord::new(new_job(), ts(0)).unwrap()).unwrap();
    value["job_id"] = json!("job_../../etc");
    assert!(serde_json::from_value::<JobRecord>(value).is_err());
}

static TEST_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        name: "duration_seconds",
        kind: OptionKind::Integer { min: 4, max: 8 },
        default: Some("8"),
        flag: Some("--duration"),
        operations: &[Operation::VideoGenerate],
        description: "clip length",
    },
    OptionSpec {
        name: "negative_prompt",
        kind: OptionKind::Text { max_chars: 200 },
        default: None,
        flag: Some("--negative-prompt"),
        operations: &[Operation::VideoGenerate],
        description: "what to avoid",
    },
    OptionSpec {
        name: "generate_audio",
        kind: OptionKind::Boolean,
        default: None,
        flag: Some("--audio"),
        operations: &[Operation::VideoGenerate],
        description: "audio",
    },
];

static TEST_SPEC: ModelSpec = ModelSpec {
    id: "veo-test",
    provider: ProviderId::Gemini,
    display_name: "Veo test",
    aliases: &[],
    lifecycle: Lifecycle::Preview,
    operations: &[Operation::VideoGenerate],
    default_for: &[],
    inputs: InputSpec::NONE,
    options: TEST_OPTIONS,
    outputs: OutputSpec { media_types: &["video/mp4"], max_count: 1 },
    limits: Limits { max_prompt_chars: None },
    pricing: &[],
    access_notes: &[],
    docs_url: "https://example.invalid/docs",
    validate: None,
    estimate: None,
    estimate_usage: None,
};

#[test]
fn request_metadata_keeps_options_and_counts_but_not_free_text() {
    let mut opts = ResolvedOptions::new();
    opts.insert("duration_seconds", OptionValue::Int(4));
    opts.insert("negative_prompt", OptionValue::Str("no dogs please".into()));
    opts.insert("generate_audio", OptionValue::Bool(false));
    let counts = InputCounts { images: 0, mask: false, first_frame: true, last_frame: false, references: 2 };

    let meta = request_metadata(&TEST_SPEC, &opts, &counts, false);
    assert_eq!(meta["duration_seconds"], json!(4));
    assert_eq!(meta["generate_audio"], json!(false));
    assert_eq!(meta["input_counts"], json!({"first_frame": 1, "last_frame": 0, "reference": 2}));
    let hidden = &meta["negative_prompt"];
    assert_eq!(hidden["chars"], json!(14));
    assert_eq!(hidden["sha256"].as_str().unwrap().len(), 64);
    assert!(!serde_json::to_string(&meta).unwrap().contains("dogs"));

    let stored = request_metadata(&TEST_SPEC, &opts, &counts, true);
    assert_eq!(stored["negative_prompt"], json!("no dogs please"));
}
