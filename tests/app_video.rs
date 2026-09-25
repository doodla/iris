//! `video generate` and `jobs *` workflows with a fake video adapter and a
//! wiremock file host: records before submission, submission outcomes, waiting,
//! Ctrl-C, downloads (failure, retry, repeat, expiry), and resuming a job from a
//! fresh context (a new "process").

#[path = "app_support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use iris::app::jobs::{self, ListFilter, Target, WaitArgs};
use iris::app::video::{self, VideoArgs};
use iris::app::{GenerationArgs, GenerationOutcome, Interrupt};
use iris::catalog::{OptionSource, RawOption};
use iris::config::{CliOverrides, Resolved, SettingSource};
use iris::domain::{DownloadState, JobStatus, ModelSource, ProviderId};
use iris::error::{ErrorCode, IrisError};
use iris::jobs::{JobId, JobStore};
use iris::output::results::{JobResult, JobView};
use iris::providers::RemoteStatus;
use support::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FILE_PATH: &str = "/v1beta/files/abc123:download";

/// A request for the fake video model (`-m fake-video-1`).
fn vargs(prompt: &str) -> VideoArgs {
    VideoArgs {
        common: GenerationArgs {
            prompt: prompt.into(),
            model: Some("fake-video-1".into()),
            ..GenerationArgs::default()
        },
        ..VideoArgs::default()
    }
}

fn detached(prompt: &str) -> VideoArgs {
    VideoArgs { detach: true, ..vargs(prompt) }
}

fn completed(outcome: GenerationOutcome<JobResult>) -> JobResult {
    match outcome {
        GenerationOutcome::Completed(r) => r,
        GenerationOutcome::Planned(_) => panic!("expected a completed result"),
    }
}

fn record_json(state: &std::path::Path, job: &str) -> serde_json::Value {
    let bytes = std::fs::read(state.join("jobs").join(format!("{job}.json"))).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn store(sandbox: &Sandbox) -> JobStore {
    JobStore::new(sandbox.state())
}

fn status_of(sandbox: &Sandbox, job: &JobView) -> JobStatus {
    store(sandbox).load(&JobId::parse(&job.job_id).unwrap()).unwrap().status()
}

struct Fixture {
    sandbox: Sandbox,
    server: MockServer,
    gemini: Arc<FakeProvider>,
}

impl Fixture {
    async fn new() -> Fixture {
        Fixture {
            sandbox: Sandbox::new(),
            server: MockServer::start().await,
            gemini: Arc::new(FakeProvider::gemini()),
        }
    }

    fn uri(&self) -> String {
        format!("{}{FILE_PATH}", self.server.uri())
    }

    fn settings(&self) -> iris::config::Settings {
        let mut s = settings(&self.sandbox.env());
        set_base_url(&mut s, ProviderId::Gemini, &self.server.uri());
        s
    }

    fn ctx(&self) -> iris::app::AppContext {
        context(self.settings(), vec![self.gemini.clone()])
    }

    async fn mount_video(&self, expect: u64) {
        Mock::given(method("GET"))
            .and(path(FILE_PATH))
            .and(header("x-goog-api-key", GEMINI_KEY))
            .respond_with(
                ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(mp4(4)),
            )
            .expect(expect)
            .mount(&self.server)
            .await;
    }

    fn submits(&self) -> usize {
        self.gemini.videos().submit_calls.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn detach_records_the_job_before_and_after_submission() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let mut a = detached("waves at dusk");
    a.common.options = vec![RawOption {
        name: "negative_prompt".into(),
        value: "no boats".into(),
        source: OptionSource::Flag("--negative-prompt"),
    }];
    let res = completed(video::run(&ctx, a, &mut w).await.unwrap());
    let job = &res.job;
    assert_eq!(job.status, JobStatus::Running);
    assert_eq!(job.remote_operation_id.as_deref(), Some("models/fake-video-1/operations/op0"));
    assert_eq!(job.model, "fake-video-1");
    assert_eq!(job.model_source, Some(ModelSource::Flag));
    assert!(job.submitted_at.is_some());
    assert_eq!(
        res.next_steps,
        vec![format!("iris jobs status {}", job.job_id), format!("iris jobs wait {}", job.job_id)]
    );
    assert!(has_warning(&w, "preview_model"));
    assert!((job.cost_estimate.as_ref().unwrap().amount - 0.8).abs() < 1e-9, "8 s default x $0.10");
    assert_eq!(f.submits(), 1);

    let rec = record_json(&f.sandbox.state(), &job.job_id);
    assert_eq!(rec["status"], "running");
    assert_eq!(rec["model_source"], "flag");
    assert_eq!(rec["prompt"]["chars"], 13);
    assert!(rec["prompt"]["text"].is_null(), "prompt text is not stored by default");
    assert!(
        rec["request"]["negative_prompt"]["sha256"].is_string(),
        "free text is hashed: {}",
        rec["request"]
    );
    // The effective values the job runs with, including declared defaults.
    assert_eq!(rec["request"]["duration"], 8, "{}", rec["request"]);
    assert_eq!(rec["request"]["resolution"], "720p", "{}", rec["request"]);
    assert_eq!(job.request["duration"], 8);
    assert_eq!(rec["output_plan"]["dir"], f.sandbox.work().to_str().unwrap());
    let raw =
        std::fs::read_to_string(f.sandbox.state().join("jobs").join(format!("{}.json", job.job_id))).unwrap();
    assert!(!raw.contains("waves at dusk") && !raw.contains("no boats"));
}

#[tokio::test]
async fn wait_and_download_saves_a_validated_video_with_the_credential_only_for_the_provider_origin() {
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let v = f.gemini.videos();
    v.push_poll(Ok(RemoteStatus::Running { progress: Some(40.0) }));
    v.push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let mut a = vargs("a paper boat");
    a.common.output = Some(f.sandbox.path("boat.mp4"));
    let res = completed(video::run(&ctx, a, &mut w).await.unwrap());
    let job = &res.job;
    assert_eq!(job.status, JobStatus::Succeeded);
    assert_eq!(job.artifacts.len(), 1);
    let art = &job.artifacts[0];
    assert_eq!(art.path, f.sandbox.path("boat.mp4").to_str().unwrap());
    assert_eq!(art.media_type, "video/mp4");
    assert_eq!(art.duration_seconds, Some(4.0));
    assert_eq!(std::fs::read(&art.path).unwrap(), mp4(4));
    assert_eq!(job.outputs[0].download_state, DownloadState::Downloaded);
    assert!(res.next_steps.is_empty());
    assert!(job.remote_expires_at.is_some());
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 2);
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn wait_timeout_leaves_the_job_running_and_resumable() {
    let f = Fixture::new().await;
    let mut s = f.settings();
    s.wait_timeout = Resolved { value: Duration::from_millis(150), source: SettingSource::Flag };
    let ctx = context(s, vec![f.gemini.clone()]);
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("slow"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::WaitTimeout);
    assert_eq!(e.exit_code(), 4);
    assert_eq!(e.job_status, Some(JobStatus::Running));
    let id = e.job_id.clone().unwrap();
    assert!(e.hint.as_deref().unwrap().contains(&format!("iris jobs wait {id}")));
    assert_eq!(store(&f.sandbox).load(&JobId::parse(&id).unwrap()).unwrap().status(), JobStatus::Running);
    assert!(f.gemini.videos().poll_calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn transient_poll_failures_never_fail_the_job() {
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let v = f.gemini.videos();
    v.push_poll(Err(IrisError::new(ErrorCode::NetworkError, "connection reset")));
    v.push_poll(Err(IrisError::new(ErrorCode::ProviderError, "503").with_retryable(Some(true))));
    v.push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let res = completed(video::run(&ctx, vargs("x"), &mut w).await.unwrap());
    assert_eq!(res.job.status, JobStatus::Succeeded);
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 3);

    // A non-retryable poll failure stops waiting but leaves the job running.
    let f = Fixture::new().await;
    f.gemini.videos().push_poll(Err(IrisError::new(ErrorCode::AuthenticationFailed, "bad key")));
    let ctx = f.ctx();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::AuthenticationFailed);
    assert_eq!(e.job_status, Some(JobStatus::Running));
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    assert_eq!(store(&f.sandbox).load(&id).unwrap().status(), JobStatus::Running);
}

#[tokio::test]
async fn uncertain_submissions_are_recorded_and_exit_5_without_resubmitting() {
    let f = Fixture::new().await;
    f.gemini.videos().push_submit(Err(
        IrisError::new(ErrorCode::SubmissionUncertain, "reset after send").with_hint("check the console")
    ));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SubmissionUncertain);
    assert_eq!(e.exit_code(), 5);
    assert_eq!(e.job_status, Some(JobStatus::SubmissionUnknown));
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    assert_eq!(store(&f.sandbox).load(&id).unwrap().status(), JobStatus::SubmissionUnknown);
    assert_eq!(f.submits(), 1);

    // A later wait reports the uncertainty; it never resubmits.
    let e = jobs::wait(&ctx, id.as_str(), &WaitArgs { download: true, target: Target::default() }, &mut w)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::SubmissionUncertain);
    assert_eq!(f.submits(), 1);

    // A timeout flagged charge_possible is uncertain too.
    f.gemini.videos().push_submit(Err(
        IrisError::new(ErrorCode::RequestTimeout, "timed out").with_detail("charge_possible", true)
    ));
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::SubmissionUncertain);
    assert_eq!(e.job_status, Some(JobStatus::SubmissionUnknown));
}

#[tokio::test]
async fn definite_rejections_mark_the_job_failed() {
    let f = Fixture::new().await;
    f.gemini.videos().push_submit(Err(IrisError::new(ErrorCode::PermissionDenied, "billing required")));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(e.job_status, Some(JobStatus::Failed));
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    let rec = store(&f.sandbox).load(&id).unwrap();
    assert_eq!(rec.status(), JobStatus::Failed);
    assert_eq!(rec.error().unwrap().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn failed_downloads_keep_the_job_succeeded_and_a_later_download_needs_no_resubmission() {
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(5)
        .mount(&f.server)
        .await;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DownloadFailed);
    assert_eq!(e.retryable, Some(true));
    assert_eq!(e.job_status, Some(JobStatus::Succeeded));
    let id = e.job_id.clone().unwrap();
    let rec = store(&f.sandbox).load(&JobId::parse(&id).unwrap()).unwrap();
    assert_eq!(rec.status(), JobStatus::Succeeded);
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Failed);
    assert!(rec.outputs()[0].last_error.is_some());
    assert!(files_in(&f.sandbox.work()).is_empty(), "no partial file: {:?}", files_in(&f.sandbox.work()));

    // The host recovers; `jobs download` fetches without resubmitting.
    f.mount_video(1).await;
    let res = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap();
    assert_eq!(res.job.artifacts.len(), 1);
    assert_eq!(res.job.outputs[0].download_state, DownloadState::Downloaded);
    assert!(res.job.artifacts[0].path.ends_with(&format!("{id}.mp4")));
    assert_eq!(f.submits(), 1, "download never resubmits");

    // Repeating is safe and needs no network (the mock expects exactly one fetch).
    let mut w2 = Vec::new();
    let again = jobs::download(&ctx, &id, &Target::default(), &mut w2).await.unwrap();
    assert!(has_warning(&w2, "already_downloaded"));
    assert_eq!(again.job.artifacts, res.job.artifacts);

    // A different file in the way is a local conflict that names the job and its provider.
    let taken = f.sandbox.path("taken.mp4");
    std::fs::write(&taken, b"something else").unwrap();
    let e = jobs::download(&ctx, &id, &Target { output: Some(taken), overwrite: false }, &mut w2)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::OutputExists);
    assert_eq!(e.job_id.as_deref(), Some(id.as_str()));
    assert_eq!(e.provider, Some(ProviderId::Gemini));

    // Another target is a local copy (still no network).
    let copy = f.sandbox.path("copy.mp4");
    let res = jobs::download(&ctx, &id, &Target { output: Some(copy.clone()), overwrite: false }, &mut w2)
        .await
        .unwrap();
    assert_eq!(res.job.artifacts[0].path, copy.to_str().unwrap());
    assert_eq!(std::fs::read(&copy).unwrap(), mp4(4));
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn partial_files_left_by_a_killed_download_are_removed_by_the_next_one() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    // What a download killed with SIGKILL leaves next to its target, and an
    // unrelated temp file that must stay.
    let stale = f.sandbox.path(&format!(".{id}.mp4.iris-part-Kill9xyz"));
    std::fs::write(&stale, vec![0u8; 4096]).unwrap();
    let unrelated = f.sandbox.path(".other.mp4.iris-part-Kill9xyz");
    std::fs::write(&unrelated, b"x").unwrap();

    f.mount_video(1).await;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    jobs::wait(&ctx, &id, &WaitArgs { download: true, target: Target::default() }, &mut w).await.unwrap();
    assert!(!stale.exists(), "the stale partial file was removed");
    assert!(unrelated.exists());
    assert_eq!(
        files_in(&f.sandbox.work()),
        vec![".other.mp4.iris-part-Kill9xyz".to_string(), format!("{id}.mp4")]
    );
}

#[tokio::test]
async fn a_local_copy_whose_source_changes_meanwhile_is_fetched_instead() {
    let f = Fixture::new().await;
    f.mount_video(2).await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    jobs::wait(&ctx, &id, &WaitArgs { download: true, target: Target::default() }, &mut w).await.unwrap();
    let first = f.sandbox.path(&format!("{id}.mp4"));
    assert_eq!(std::fs::read(&first).unwrap(), mp4(4));

    // The recorded file is intact when the copy is decided, then changes before it
    // is copied (the progress line is printed in between).
    let source = first.clone();
    let progress = iris::app::Progress::new(move |line| {
        if line.starts_with("Copying output") {
            std::fs::write(&source, b"edited by someone else").unwrap();
        }
    });
    let ctx =
        iris::app::AppContext::new(f.settings(), deps(vec![f.gemini.clone()], Interrupt::manual()), progress);
    let copy = f.sandbox.path("copy.mp4");
    let res = jobs::download(&ctx, &id, &Target { output: Some(copy.clone()), overwrite: false }, &mut w)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), mp4(4), "fetched from the provider, not copied");
    assert_eq!(res.job.artifacts[0].path, copy.to_str().unwrap());
    assert_eq!(std::fs::read(&first).unwrap(), b"edited by someone else", "the changed file is left alone");
    assert_eq!(f.submits(), 1);
    // `mount_video(2)` verifies the second fetch when the server drops.
}

#[tokio::test]
async fn a_new_context_resumes_a_job_submitted_by_an_earlier_one() {
    let f = Fixture::new().await;
    let id = {
        let first = f.ctx();
        let mut w = Vec::new();
        completed(video::run(&first, detached("x"), &mut w).await.unwrap()).job.job_id
    };
    f.mount_video(1).await;
    // A fresh provider instance and context, as in a later process.
    let later = Arc::new(FakeProvider::gemini());
    later.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = context(f.settings(), vec![later.clone()]);
    let mut w = Vec::new();
    let res =
        jobs::wait(&ctx, &id, &WaitArgs { download: true, target: Target::default() }, &mut w).await.unwrap();
    assert_eq!(res.job.status, JobStatus::Succeeded);
    assert_eq!(res.job.artifacts.len(), 1);
    assert_eq!(f.submits(), 1);
    assert_eq!(later.videos().submit_calls.load(Ordering::SeqCst), 0);
    assert_eq!(later.videos().poll_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn outputs_are_artifact_expired_only_on_410_or_after_the_retention_period() {
    // The file host answers 410: gone for good.
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(ResponseTemplate::new(410))
        .mount(&f.server)
        .await;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ArtifactExpired);
    let rec = store(&f.sandbox).load(&JobId::parse(e.job_id.as_deref().unwrap()).unwrap()).unwrap();
    assert_eq!(rec.status(), JobStatus::Succeeded);
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Expired);

    // A 403 inside the retention period is a retryable download failure; the
    // output stays re-downloadable and a later download works.
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&f.server)
        .await;
    f.mount_video(1).await;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::DownloadFailed);
    assert_eq!(e.retryable, Some(true));
    assert_eq!(e.provider_status, Some(403));
    assert_eq!(e.job_status, Some(JobStatus::Succeeded));
    let id = e.job_id.clone().unwrap();
    assert!(e.hint.as_deref().unwrap().contains(&format!("iris jobs download {id}")), "{e:?}");
    let rec = store(&f.sandbox).load(&JobId::parse(&id).unwrap()).unwrap();
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Failed);
    let res = jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    assert_eq!(res.next_steps, vec![format!("iris jobs download {id}")]);
    let res = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap();
    assert_eq!(res.job.outputs[0].download_state, DownloadState::Downloaded);
    assert_eq!(f.submits(), 1);

    // Past the retention period the fetch is still attempted (the provider may keep
    // outputs longer); only then does a 404 mean the output is gone.
    let past_retention = || {
        Arc::new(FakeProvider {
            video: Some(FakeVideo { retention: Some(Duration::ZERO), ..FakeVideo::default() }),
            ..FakeProvider::gemini()
        })
    };
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&f.server)
        .await;
    let gemini = past_retention();
    gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = context(f.settings(), vec![gemini]);
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    let mut w = Vec::new();
    let status = jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    assert_eq!(status.job.status, JobStatus::Succeeded);
    let warning = w.iter().find(|w| w.code == "retention_limited").unwrap();
    assert!(warning.message.contains("may already be deleted"), "{}", warning.message);
    let e = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ArtifactExpired);
    assert_eq!(e.retryable, Some(false));
    assert_eq!(e.provider_status, Some(404));
    let rec = store(&f.sandbox).load(&JobId::parse(&id).unwrap()).unwrap();
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Expired);

    // Without a documented retention, a 404 never proves the output is gone.
    let f = Fixture::new().await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(ResponseTemplate::new(404))
        .mount(&f.server)
        .await;
    let gemini = Arc::new(FakeProvider {
        video: Some(FakeVideo { retention: None, ..FakeVideo::default() }),
        ..FakeProvider::gemini()
    });
    gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = context(f.settings(), vec![gemini]);
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!((e.code, e.retryable), (ErrorCode::DownloadFailed, Some(true)));
    assert!(e.hint.as_deref().unwrap().contains("documents no retention period"), "{e:?}");

    // ... and if the host still serves the file, it is saved.
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let gemini = past_retention();
    gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let ctx = context(f.settings(), vec![gemini]);
    let res = completed(video::run(&ctx, vargs("x"), &mut w).await.unwrap());
    assert_eq!(res.job.outputs[0].download_state, DownloadState::Downloaded);
}

#[tokio::test]
async fn first_ctrl_c_during_submission_is_deferred_until_the_operation_id_is_recorded() {
    let f = Fixture::new().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let gemini = Arc::new(FakeProvider {
        video: Some(FakeVideo { submit_gate: Some(gate.clone()), ..FakeVideo::default() }),
        ..FakeProvider::gemini()
    });
    let interrupt = Interrupt::manual();
    let ctx = context_with_interrupt(f.settings(), vec![gemini.clone()], interrupt.clone());
    let mut w = Vec::new();
    let entered = gemini.videos().submit_entered.clone();
    let run = video::run(&ctx, vargs("x"), &mut w);
    let driver = async {
        entered.notified().await;
        interrupt.trigger();
        tokio::time::sleep(Duration::from_millis(50)).await;
        gate.notify_one();
    };
    let (r, ()) = tokio::join!(run, driver);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Interrupted);
    assert_eq!(e.exit_code(), 130);
    assert_eq!(e.job_status, Some(JobStatus::Running));
    assert!(e.remote_operation_id.is_some());
    // The job was accepted: re-running the command would bill another one.
    assert_eq!(e.retryable, Some(false));
    assert_eq!(e.details["charge_possible"], true);
    assert!(e.hint.as_deref().unwrap().contains("iris jobs wait"), "{e:?}");
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    assert_eq!(store(&f.sandbox).load(&id).unwrap().status(), JobStatus::Running);
    assert_eq!(gemini.videos().poll_calls.load(Ordering::SeqCst), 0, "no waiting after an interrupt");
}

#[tokio::test]
async fn second_ctrl_c_during_submission_exits_at_once_leaving_the_record_submitting() {
    let f = Fixture::new().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let gemini = Arc::new(FakeProvider {
        video: Some(FakeVideo { submit_gate: Some(gate), ..FakeVideo::default() }),
        ..FakeProvider::gemini()
    });
    let interrupt = Interrupt::manual();
    let ctx = context_with_interrupt(f.settings(), vec![gemini.clone()], interrupt.clone());
    let mut w = Vec::new();
    let entered = gemini.videos().submit_entered.clone();
    let run = video::run(&ctx, vargs("x"), &mut w);
    let driver = async {
        entered.notified().await;
        interrupt.trigger();
        tokio::task::yield_now().await;
        interrupt.trigger();
    };
    let (r, ()) = tokio::join!(run, driver);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Interrupted);
    assert_eq!(e.exit_code(), 130);
    assert_eq!(e.job_status, Some(JobStatus::Submitting));
    // The request may have reached the provider: not retryable, charge possible.
    assert_eq!(e.retryable, Some(false));
    assert_eq!(e.details["charge_possible"], true);
    assert_eq!(e.provider, Some(ProviderId::Gemini));
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    let raw = record_json(&f.sandbox.state(), id.as_str());
    assert_eq!(raw["status"], "submitting");
    // The hint says how long the record stays `submitting` (the stale threshold).
    let hint = e.hint.as_deref().unwrap();
    let rec = store(&f.sandbox).load(&id).unwrap();
    let until = rec.created_at().checked_add(ctx.store.submit_budget() + iris::jobs::SUBMIT_GRACE).unwrap();
    assert!(hint.contains(&format!("submitting until about {until}")), "{hint}");
    assert!(hint.contains("submission_unknown after that"), "{hint}");
    assert!(hint.contains(&format!("iris jobs status {id}")), "{hint}");
}

#[tokio::test]
async fn an_interrupt_before_the_request_is_sent_stops_without_sending_it() {
    // The interrupt lands after the handlers are armed and the record is written,
    // but before the paid request goes out. This one is manual; a real signal in
    // the same window is covered by tests/app_video_signal.rs.
    let f = Fixture::new().await;
    let interrupt = Interrupt::manual();
    let on_submit = interrupt.clone();
    let progress = iris::app::Progress::new(move |line| {
        if line.starts_with("Submitting job") {
            on_submit.trigger();
        }
    });
    let ctx = iris::app::AppContext::new(f.settings(), deps(vec![f.gemini.clone()], interrupt), progress);
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Interrupted, "{e:?}");
    assert_eq!(e.exit_code(), 130);
    // Nothing was sent: running the command again is harmless, and there is no
    // job to point at.
    assert_eq!(e.retryable, Some(true));
    assert!(!e.details.contains_key("charge_possible"), "{e:?}");
    assert_eq!(e.provider, Some(ProviderId::Gemini));
    assert_eq!(e.provider_status, None);
    assert!(e.job_id.is_none() && e.job_status.is_none(), "{e:?}");
    assert!(e.message.contains("nothing was submitted"), "{}", e.message);
    assert_eq!(f.submits(), 0, "the paid request was not sent");
    let listing = JobStore::new(f.sandbox.state()).list().unwrap();
    assert!(listing.records.is_empty(), "no record: {:?}", listing.records);
}

#[tokio::test]
async fn ctrl_c_while_waiting_leaves_the_job_running() {
    let f = Fixture::new().await;
    let interrupt = Interrupt::manual();
    let ctx = context_with_interrupt(f.settings(), vec![f.gemini.clone()], interrupt.clone());
    let mut w = Vec::new();
    let polls = &f.gemini.videos().poll_calls;
    let run = video::run(&ctx, vargs("x"), &mut w);
    let driver = async {
        while polls.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        interrupt.trigger();
    };
    let (r, ()) = tokio::join!(run, driver);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Interrupted);
    assert_eq!(e.job_status, Some(JobStatus::Running));
    assert!(e.hint.as_deref().unwrap().contains("iris jobs wait"));
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    assert_eq!(store(&f.sandbox).load(&id).unwrap().status(), JobStatus::Running);
}

#[tokio::test]
async fn status_refreshes_running_jobs_once_and_reports_refresh_failures_as_warnings() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    let v = f.gemini.videos();

    let res = jobs::status(&ctx, &id, false, &mut w).await.unwrap();
    assert_eq!(res.job.status, JobStatus::Running);
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 0, "--no-refresh never polls");

    v.push_poll(Err(IrisError::new(ErrorCode::NetworkError, "offline")));
    let mut w = Vec::new();
    let res = jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    assert_eq!(res.job.status, JobStatus::Running);
    assert!(has_warning(&w, "status_refresh_failed"));

    v.push_poll(Ok(remote_success(&f.uri())));
    let mut w = Vec::new();
    let res = jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    assert_eq!(res.job.status, JobStatus::Succeeded);
    assert_eq!(res.next_steps, vec![format!("iris jobs download {id}")]);
    assert!(has_warning(&w, "retention_limited"));
    assert_eq!(status_of(&f.sandbox, &res.job), JobStatus::Succeeded, "the refresh is persisted");

    // Terminal jobs are not polled again.
    let before = v.poll_calls.load(Ordering::SeqCst);
    jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn download_of_unfinished_or_failed_jobs_reports_the_right_codes() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    let e = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::JobNotReady);
    assert_eq!(e.exit_code(), 4);
    assert_eq!(f.gemini.videos().poll_calls.load(Ordering::SeqCst), 1, "the running record was refreshed");

    f.gemini.videos().push_poll(Ok(RemoteStatus::Failed {
        error: IrisError::new(ErrorCode::ContentBlocked, "blocked by the provider"),
    }));
    let e = jobs::wait(&ctx, &id, &WaitArgs { download: true, target: Target::default() }, &mut w)
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::ContentBlocked);
    assert_eq!(e.job_status, Some(JobStatus::Failed));
    let e = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ContentBlocked);

    assert_eq!(err_code(jobs::status(&ctx, "job_notvalid", true, &mut w).await), ErrorCode::InvalidArgument);
    assert_eq!(
        err_code(jobs::status(&ctx, "job_00000000000000000000000000", true, &mut w).await),
        ErrorCode::JobNotFound
    );
}

#[tokio::test]
async fn download_refreshes_a_stale_running_record_once_before_deciding() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    let v = f.gemini.videos();

    // The refresh fails transiently: the last known status stands, with a warning,
    // and the error says the status was not checked.
    v.push_poll(Err(IrisError::new(ErrorCode::NetworkError, "offline")));
    let mut w = Vec::new();
    let e = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::JobNotReady);
    assert_eq!(e.details["status_checked"], false);
    assert!(e.message.contains("last known to be running"), "{}", e.message);
    assert!(has_warning(&w, "status_refresh_failed"), "{w:?}");
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 1);

    // Any other refresh failure is the download's error, for the job.
    v.push_poll(Err(IrisError::new(ErrorCode::PermissionDenied, "no access").with_provider_status(403)));
    let mut w = Vec::new();
    let e = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(e.exit_code(), 3);
    assert_eq!(e.job_id.as_deref(), Some(id.as_str()));
    assert_eq!(e.job_status, Some(JobStatus::Running));
    assert!(e.remote_operation_id.is_some());
    assert!(!has_warning(&w, "status_refresh_failed"), "{w:?}");
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 2);

    // The job finished since the last check: one refresh, then the download.
    f.mount_video(1).await;
    v.push_poll(Ok(remote_success(&f.uri())));
    let mut w = Vec::new();
    let res = jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap();
    assert_eq!(res.job.status, JobStatus::Succeeded);
    assert_eq!(res.job.outputs[0].download_state, DownloadState::Downloaded);
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 3);

    // A succeeded record is never polled again.
    jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap();
    assert_eq!(v.poll_calls.load(Ordering::SeqCst), 3);
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn wait_without_download_reports_next_steps_and_retention() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    let mut w = Vec::new();
    let res = jobs::wait(&ctx, &id, &WaitArgs { download: false, target: Target::default() }, &mut w)
        .await
        .unwrap();
    assert_eq!(res.job.status, JobStatus::Succeeded);
    assert!(res.job.artifacts.is_empty());
    assert_eq!(res.next_steps, vec![format!("iris jobs download {id}")]);
    assert!(has_warning(&w, "retention_limited"));
}

#[tokio::test]
async fn list_filters_and_delete_refuses_active_jobs_without_force() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let running = completed(video::run(&ctx, detached("a"), &mut w).await.unwrap()).job.job_id;
    // A provider's definite rejection (HTTP 400): the job is recorded as failed.
    f.gemini
        .videos()
        .push_submit(Err(IrisError::new(ErrorCode::InvalidArgument, "rejected").with_provider_status(400)));
    let failed = video::run(&ctx, detached("b"), &mut w).await.unwrap_err().job_id.clone().unwrap();

    let all = jobs::list(&ctx, &ListFilter::default(), &mut w).unwrap();
    assert_eq!(all.jobs.len(), 2);
    assert_eq!(all.jobs[0].job_id, failed, "newest first");
    let only =
        jobs::list(&ctx, &ListFilter { status: Some(JobStatus::Running), ..Default::default() }, &mut w)
            .unwrap();
    assert_eq!(only.jobs.iter().map(|j| j.job_id.as_str()).collect::<Vec<_>>(), [running.as_str()]);
    let none =
        jobs::list(&ctx, &ListFilter { provider: Some(ProviderId::OpenAi), ..Default::default() }, &mut w)
            .unwrap();
    assert!(none.jobs.is_empty());
    let one = jobs::list(&ctx, &ListFilter { limit: Some(1), ..Default::default() }, &mut w).unwrap();
    assert_eq!(one.jobs.len(), 1);

    let e = jobs::delete(&ctx, std::slice::from_ref(&running), false, false, &mut w).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(e.job_status, Some(JobStatus::Running));
    let e = jobs::delete(&ctx, &[], true, false, &mut w).unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument, "--all refuses while a job is active");
    assert_eq!(jobs::list(&ctx, &ListFilter::default(), &mut w).unwrap().jobs.len(), 2, "nothing deleted");

    let res = jobs::delete(&ctx, std::slice::from_ref(&failed), false, false, &mut w).unwrap();
    assert_eq!(res.deleted, vec![failed.clone()]);
    assert_eq!(res.remote_effect, "none");
    let res = jobs::delete(&ctx, &[], true, true, &mut w).unwrap();
    assert_eq!(res.deleted, vec![running]);
    assert!(jobs::list(&ctx, &ListFilter::default(), &mut w).unwrap().jobs.is_empty());
    assert_eq!(err_code(jobs::delete(&ctx, &[failed], false, false, &mut w)), ErrorCode::JobNotFound);
    assert_eq!(f.submits(), 2, "deletion never contacts the provider");
}

#[tokio::test]
async fn video_validation_and_dry_run_happen_before_any_record_or_request() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let img = f.sandbox.path("frame.png");
    std::fs::write(&img, png(8, 8)).unwrap();

    let mut a = vargs("x");
    a.references = vec![img.clone(), img.clone(), img.clone()];
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::InvalidArgument);
    let mut a = vargs("x");
    a.last_frame = Some(img.clone());
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::InvalidArgument, "model validator");
    let mut a = vargs("x");
    a.common.options = vec![RawOption {
        name: "duration".into(),
        value: "5".into(),
        source: OptionSource::Flag("--duration"),
    }];
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::InvalidArgument);
    let mut a = vargs("x");
    a.common.output = Some(f.sandbox.path("clip.png"));
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::InvalidArgument, "-o must be a video");
    let existing = f.sandbox.path("exists.mp4");
    std::fs::write(&existing, b"x").unwrap();
    let mut a = vargs("x");
    a.common.output = Some(existing);
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::OutputExists);
    let mut a = vargs("x");
    a.common.model = Some("fake-image-1".into());
    assert_eq!(err_code(video::run(&ctx, a, &mut w).await), ErrorCode::UnsupportedOperation);
    assert!(!f.sandbox.state().join("jobs").exists(), "no record was created");
    assert_eq!(f.submits(), 0);

    let mut a = vargs("x");
    a.common.dry_run = true;
    a.first_frame = Some(img.clone());
    a.common.options = vec![RawOption {
        name: "duration".into(),
        value: "4".into(),
        source: OptionSource::Flag("--duration"),
    }];
    let ctx_nokey = context(settings(&f.sandbox.env_without_keys()), vec![f.gemini.clone()]);
    let plan = match video::run(&ctx_nokey, a, &mut w).await.unwrap() {
        GenerationOutcome::Planned(p) => p,
        GenerationOutcome::Completed(_) => panic!("expected a plan"),
    };
    assert!(plan.async_job);
    assert!(!plan.credential_present);
    assert_eq!(plan.inputs.len(), 1);
    assert_eq!(plan.inputs[0].role, "first_frame");
    assert!(plan.outputs[0].ends_with("<job_id>.mp4"), "{:?}", plan.outputs);
    assert_eq!(plan.options["duration"], 4, "explicit");
    assert_eq!(plan.options["resolution"], "720p", "declared default: {:?}", plan.options);
    assert!((plan.cost_estimate.unwrap().amount - 0.4).abs() < 1e-9);
    assert!(!f.sandbox.state().join("jobs").exists());
    assert_eq!(f.submits(), 0);

    // Missing credentials: after validation, before any record.
    let e = video::run(&ctx_nokey, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    assert!(e.message.contains("GEMINI_API_KEY"));
    assert!(!f.sandbox.state().join("jobs").exists());
}

#[tokio::test]
async fn a_submit_refused_before_sending_leaves_no_job_record() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    // The adapter refuses the request locally (exit 2, no provider status): nothing
    // was sent, so no job exists and no record is kept.
    f.gemini.videos().push_submit(Err(IrisError::new(ErrorCode::InvalidArgument, "model id cannot be sent")));
    let e = video::run(&ctx, detached("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(e.exit_code(), 2);
    assert!(e.job_id.is_none(), "no job to point at: {e:?}");
    let listing = JobStore::new(f.sandbox.state()).list().unwrap();
    assert!(listing.records.is_empty(), "no record: {:?}", listing.records);

    // A definite provider rejection is a failed job, as before.
    f.gemini
        .videos()
        .push_submit(Err(IrisError::new(ErrorCode::InvalidArgument, "400").with_provider_status(400)));
    let e = video::run(&ctx, detached("x"), &mut w).await.unwrap_err();
    assert_eq!(e.job_status, Some(JobStatus::Failed));
    assert_eq!(JobStore::new(f.sandbox.state()).list().unwrap().records.len(), 1);
}

#[tokio::test]
async fn real_runs_check_the_credential_before_creating_output_directories() {
    let f = Fixture::new().await;
    let without_key = |out_dir: &std::path::Path| {
        let mut s = settings_with(
            &f.sandbox.env_without_keys(),
            &CliOverrides { out_dir: Some(out_dir.to_path_buf()), ..CliOverrides::default() },
        );
        set_base_url(&mut s, ProviderId::Gemini, &f.server.uri());
        context(s, vec![f.gemini.clone()])
    };
    let mut w = Vec::new();

    // video generate: missing_credentials, and -d was not created.
    let new_dir = f.sandbox.path("new/videos");
    let e = video::run(&without_key(&new_dir), vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    assert!(!f.sandbox.path("new").exists(), "no directory was created");
    assert!(!f.sandbox.state().join("jobs").exists(), "no record was created");

    // jobs download of a finished job that must be fetched from the provider: the
    // same, before anything is created.
    f.mount_video(1).await;
    let ctx = f.ctx();
    let id = completed(video::run(&ctx, detached("x"), &mut w).await.unwrap()).job.job_id;
    f.gemini.videos().push_poll(Ok(remote_success(&f.uri())));
    jobs::status(&ctx, &id, true, &mut w).await.unwrap();
    let other = f.sandbox.path("other/videos");
    let e = jobs::download(&without_key(&other), &id, &Target::default(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::MissingCredentials);
    assert_eq!(e.job_id.as_deref(), Some(id.as_str()));
    assert!(!f.sandbox.path("other").exists(), "no directory was created");

    // A local copy of a downloaded output needs no credential.
    jobs::download(&ctx, &id, &Target::default(), &mut w).await.unwrap();
    let res = jobs::download(&without_key(&other), &id, &Target::default(), &mut w).await.unwrap();
    assert_eq!(res.job.artifacts[0].path, other.join(format!("{id}.mp4")).to_str().unwrap());
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn the_api_key_is_never_sent_to_another_origin() {
    let f = Fixture::new().await;
    let other = MockServer::start().await;
    Mock::given(method("GET"))
        .and(header("x-goog-api-key", GEMINI_KEY))
        .respond_with(ResponseTemplate::new(403))
        .expect(0)
        .mount(&other)
        .await;
    Mock::given(method("GET"))
        .and(path(FILE_PATH))
        .respond_with(
            ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(mp4(4)),
        )
        .expect(1)
        .mount(&other)
        .await;
    f.gemini.videos().push_poll(Ok(remote_success(&format!("{}{FILE_PATH}", other.uri()))));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let res = completed(video::run(&ctx, vargs("x"), &mut w).await.unwrap());
    assert_eq!(res.job.artifacts.len(), 1);
}

#[tokio::test]
async fn detach_preflights_the_recorded_output_before_submitting() {
    let f = Fixture::new().await;
    let ctx = f.ctx();
    let mut w = Vec::new();
    let existing = f.sandbox.path("clip.mp4");
    std::fs::write(&existing, b"old").unwrap();
    let mut a = detached("waves");
    a.common.output = Some(existing.clone());
    let e = video::run(&ctx, a.clone(), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::OutputExists);
    assert_eq!(e.exit_code(), 2);

    // A file where -o's directory should be: nothing is sent either.
    let blocker = f.sandbox.path("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let mut b = detached("waves");
    b.common.output = Some(blocker.join("clip.mp4"));
    let code = err_code(video::run(&ctx, b, &mut w).await);
    assert!(matches!(code, ErrorCode::IoError | ErrorCode::InvalidArgument), "{code:?}");
    assert_eq!(f.submits(), 0);
    assert!(files_in(&f.sandbox.state().join("jobs")).is_empty(), "no job record");

    // With --overwrite the same -o is accepted and recorded for the later download.
    a.common.overwrite = true;
    let res = completed(video::run(&ctx, a, &mut w).await.unwrap());
    let rec = record_json(&f.sandbox.state(), &res.job.job_id);
    assert_eq!(rec["output_plan"]["path"], existing.to_str().unwrap());
    assert_eq!(rec["output_plan"]["overwrite"], true);
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn a_file_that_appears_while_waiting_never_blocks_saving_the_paid_video() {
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let v = f.gemini.videos();
    v.push_poll(Ok(RemoteStatus::Running { progress: None }));
    v.push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let target = f.sandbox.path("clip.mp4");
    let mut a = vargs("x");
    a.common.output = Some(target.clone());
    let entered = v.submit_entered.clone();
    let run = video::run(&ctx, a, &mut w);
    let driver = async {
        // After the preflight, before the download: another writer takes the name.
        entered.notified().await;
        std::fs::write(&target, b"another agent's file").unwrap();
    };
    let (r, ()) = tokio::join!(run, driver);
    let res = completed(r.unwrap());
    let renamed = f.sandbox.path("clip.1.mp4");
    assert_eq!(res.job.artifacts[0].path, renamed.to_str().unwrap());
    assert_eq!(std::fs::read(&renamed).unwrap(), mp4(4));
    assert_eq!(std::fs::read(&target).unwrap(), b"another agent's file", "never replaced");
    assert!(has_warning(&w, "output_renamed"), "{w:?}");
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn video_generate_downloads_with_its_own_out_dir_and_overwrite() {
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let v = f.gemini.videos();
    v.push_poll(Ok(RemoteStatus::Running { progress: None }));
    v.push_poll(Ok(remote_success(&f.uri())));
    let dir = f.sandbox.path("videos");
    let mut s = settings_with(
        &f.sandbox.env(),
        &CliOverrides { out_dir: Some(dir.clone()), ..CliOverrides::default() },
    );
    set_base_url(&mut s, ProviderId::Gemini, &f.server.uri());
    let ctx = context(s, vec![f.gemini.clone()]);
    let mut w = Vec::new();
    let mut a = vargs("x");
    a.common.overwrite = true;
    let entered = v.submit_entered.clone();
    let state = f.sandbox.state();
    let run = video::run(&ctx, a, &mut w);
    let driver = async {
        entered.notified().await;
        let listing = JobStore::new(&state).list().unwrap();
        let id = listing.records[0].job_id().to_string();
        std::fs::write(dir.join(format!("{id}.mp4")), b"stale").unwrap();
    };
    let (r, ()) = tokio::join!(run, driver);
    let res = completed(r.unwrap());
    let art = &res.job.artifacts[0];
    assert_eq!(art.path, dir.join(format!("{}.mp4", res.job.job_id)).to_str().unwrap(), "-d applies");
    assert_eq!(std::fs::read(&art.path).unwrap(), mp4(4), "--overwrite applies with -d");
    assert!(!has_warning(&w, "output_renamed"));
}

#[tokio::test]
async fn local_save_failures_after_success_are_download_failed_never_exit_2() {
    let f = Fixture::new().await;
    f.mount_video(1).await;
    let v = f.gemini.videos();
    v.push_poll(Ok(RemoteStatus::Running { progress: None }));
    v.push_poll(Ok(remote_success(&f.uri())));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let sub = f.sandbox.path("sub");
    let mut a = vargs("x");
    a.common.output = Some(sub.join("clip.mp4"));
    let entered = v.submit_entered.clone();
    let run = video::run(&ctx, a, &mut w);
    let driver = async {
        // The output directory is replaced by a file while the job runs.
        entered.notified().await;
        std::fs::remove_dir(&sub).unwrap();
        std::fs::write(&sub, b"in the way").unwrap();
    };
    let (r, ()) = tokio::join!(run, driver);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::DownloadFailed);
    assert_eq!(e.exit_code(), 1, "the paid job exists: never exit 2");
    assert_eq!(e.job_status, Some(JobStatus::Succeeded));
    assert_eq!(e.details["cause_code"], "invalid_argument");
    let hint = e.hint.as_deref().unwrap();
    let id = e.job_id.clone().unwrap();
    assert!(hint.contains(&format!("iris jobs download {id}")), "{hint}");
    assert!(hint.contains("do not re-run `iris video generate`"), "{hint}");

    // The recorded job is downloaded elsewhere without resubmitting.
    let other = f.sandbox.path("other.mp4");
    let res = jobs::download(&ctx, &id, &Target { output: Some(other.clone()), overwrite: false }, &mut w)
        .await
        .unwrap();
    assert_eq!(res.job.artifacts[0].path, other.to_str().unwrap());
    assert_eq!(f.submits(), 1);
}

#[tokio::test]
async fn provider_rejections_while_waiting_after_submission_are_not_exit_2() {
    let f = Fixture::new().await;
    f.gemini.videos().push_poll(Err(IrisError::invalid("malformed operation name")));
    let ctx = f.ctx();
    let mut w = Vec::new();
    let e = video::run(&ctx, vargs("x"), &mut w).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::ProviderError);
    assert_eq!(e.exit_code(), 1);
    assert_eq!(e.details["cause_code"], "invalid_argument");
    assert_eq!(e.job_status, Some(JobStatus::Running));
    assert!(e.remote_operation_id.is_some());
    let hint = e.hint.as_deref().unwrap();
    assert!(hint.contains("iris jobs wait") && hint.contains("do not re-run"), "{hint}");
    let id = JobId::parse(e.job_id.as_deref().unwrap()).unwrap();
    assert_eq!(store(&f.sandbox).load(&id).unwrap().status(), JobStatus::Running);
}

#[tokio::test]
async fn record_failures_after_the_paid_submit_never_hide_the_submission_outcome() {
    let f = Fixture::new().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let gemini = Arc::new(FakeProvider {
        video: Some(FakeVideo { submit_gate: Some(gate.clone()), ..FakeVideo::default() }),
        ..FakeProvider::gemini()
    });
    let ctx = context(f.settings(), vec![gemini.clone()]);
    let videos = gemini.videos();
    videos.push_submit(Ok(iris::providers::SubmittedOperation {
        remote_id: "models/fake-video-1/operations/accepted".into(),
        provider_request_id: None,
    }));
    videos.push_submit(Err(IrisError::new(ErrorCode::SubmissionUncertain, "reset after send")));
    videos.push_submit(Err(IrisError::new(ErrorCode::PermissionDenied, "billing required")));

    for expected in
        [ErrorCode::SubmissionUncertain, ErrorCode::SubmissionUncertain, ErrorCode::PermissionDenied]
    {
        let mut w = Vec::new();
        let entered = videos.submit_entered.clone();
        let state = f.sandbox.state();
        let run = video::run(&ctx, vargs("x"), &mut w);
        let driver = async {
            // Another process deletes every record (`jobs delete --all --force`)
            // while the paid request is in flight.
            entered.notified().await;
            let store = JobStore::new(&state);
            for rec in store.list().unwrap().records {
                store.delete(rec.job_id(), true).unwrap();
            }
            gate.notify_one();
        };
        let (r, ()) = tokio::join!(run, driver);
        let e = r.unwrap_err();
        assert_eq!(e.code, expected, "{e:?}");
        assert_eq!(e.details["record_error"]["code"], "job_not_found", "{e:?}");
        assert!(e.job_id.is_some());
    }
    assert_eq!(videos.submit_calls.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn an_accepted_job_whose_record_vanished_exits_5_with_the_remote_id() {
    let f = Fixture::new().await;
    let gate = Arc::new(tokio::sync::Notify::new());
    let gemini = Arc::new(FakeProvider {
        video: Some(FakeVideo { submit_gate: Some(gate.clone()), ..FakeVideo::default() }),
        ..FakeProvider::gemini()
    });
    let ctx = context(f.settings(), vec![gemini.clone()]);
    let mut w = Vec::new();
    let entered = gemini.videos().submit_entered.clone();
    let state = f.sandbox.state();
    let run = video::run(&ctx, vargs("x"), &mut w);
    let driver = async {
        entered.notified().await;
        let store = JobStore::new(&state);
        for rec in store.list().unwrap().records {
            store.delete(rec.job_id(), true).unwrap();
        }
        gate.notify_one();
    };
    let (r, ()) = tokio::join!(run, driver);
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::SubmissionUncertain);
    assert_eq!(e.exit_code(), 5);
    assert_eq!(e.remote_operation_id.as_deref(), Some("models/fake-video-1/operations/op0"));
    assert_eq!(e.details["provider_accepted"], true);
    assert!(e.hint.as_deref().unwrap().contains("do not resubmit"), "{:?}", e.hint);
    assert_eq!(gemini.videos().poll_calls.load(Ordering::SeqCst), 0);
}

/// Iris never picks a video model: without `-m` the config file's `video.model` is
/// used and recorded as `model_source: config`; with neither, `model_required`
/// comes before any record or request, in a dry run too.
#[tokio::test]
async fn the_video_model_comes_from_the_flag_or_the_config_file_and_is_otherwise_required() {
    let f = Fixture::new().await;
    let without_model = |dry_run: bool| {
        let mut a = detached("x");
        a.common.model = None;
        a.common.dry_run = dry_run;
        a
    };
    for dry_run in [false, true] {
        let e = video::run(&f.ctx(), without_model(dry_run), &mut Vec::new()).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::ModelRequired, "dry_run={dry_run}");
        assert_eq!(e.provider_status, None);
        assert_eq!(e.job_id, None);
        assert_eq!(e.details["operation"], "video.generate");
        assert_eq!(e.details["config_key"], "video.model");
        assert_eq!(e.details["candidates"][0]["model"], "fake-video-1");
        assert_eq!(e.details["candidates"].as_array().unwrap().len(), 1, "video models only");
    }
    assert!(!f.sandbox.state().join("jobs").exists(), "no record was created");
    assert_eq!(f.submits(), 0);

    let mut configured = f.settings();
    configured.video_model = Resolved { value: Some("fake-video-1".into()), source: SettingSource::File };
    let ctx = context(configured, vec![f.gemini.clone()]);
    let res = completed(video::run(&ctx, without_model(false), &mut Vec::new()).await.unwrap());
    assert_eq!((res.job.model.as_str(), res.job.model_source), ("fake-video-1", Some(ModelSource::Config)));
    assert_eq!(record_json(&f.sandbox.state(), &res.job.job_id)["model_source"], "config");
}
