//! Job store: atomic writes, locking, listing, deletion (see docs/concepts/video-jobs.md).

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use iris::domain::{JobStatus, ModelSource, Operation, ProviderId};
use iris::error::{ErrorCode, IrisError};
use iris::http::Timeouts;
use iris::jobs::{
    JobId, JobLabel, JobRecord, JobStore, NewJob, OutputPlan, PromptRecord, RefusalKind, paid_submit_budget,
};
use iris::providers::SubmittedOperation;
use jiff::Timestamp;
use serde_json::{Map, Value, json};

fn new_job() -> NewJob {
    NewJob {
        label: None,
        provider: ProviderId::Gemini,
        model: "veo-test".into(),
        model_source: ModelSource::Flag,
        operation: Operation::VideoGenerate,
        request: Map::new(),
        prompt: PromptRecord::new("a lighthouse at dusk", false),
        output_plan: OutputPlan::default(),
        cost_estimate: None,
    }
}

fn now() -> Timestamp {
    iris::jobs::now()
}

fn ago(secs: i64) -> Timestamp {
    Timestamp::from_second(now().as_second() - secs).unwrap()
}

/// A fresh store in a temp dir with one `running` job.
fn store_with_running_job() -> (tempfile::TempDir, JobStore, JobId) {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let mut rec = JobRecord::new(new_job(), now()).unwrap();
    rec.mark_submitted(
        &SubmittedOperation { remote_id: "operations/1".into(), provider_request_id: None },
        now(),
    )
    .unwrap();
    store.create(&rec).unwrap();
    let id = rec.job_id().clone();
    (dir, store, id)
}

fn counter(rec: &JobRecord) -> u64 {
    rec.extra().get("test_counter").and_then(Value::as_u64).unwrap_or(0)
}

fn increment(store: &JobStore, id: &JobId) {
    store
        .update(id, |rec| {
            let next = counter(rec) + 1;
            rec.set_extra("test_counter", json!(next))
        })
        .unwrap();
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn create_and_load_round_trip_with_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let store = JobStore::new(&state);
    assert!(!state.exists(), "constructing a store touches nothing");
    let rec = JobRecord::new(new_job(), now()).unwrap();
    store.create(&rec).unwrap();

    let path = store.record_path(rec.job_id());
    assert_eq!(path, state.join("jobs").join(format!("{}.json", rec.job_id())));
    let loaded = store.load(rec.job_id()).unwrap();
    // As created, plus the store's submit budget (see the stale-submitting rule).
    let mut expected = serde_json::to_value(&rec).unwrap();
    expected["submit_budget_seconds"] = store.submit_budget().as_secs().into();
    assert_eq!(serde_json::to_value(&loaded).unwrap(), expected);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&state.join("jobs")), 0o700);
        assert_eq!(mode(&state), 0o700);
    }
}

#[test]
fn create_never_overwrites() {
    let (_dir, store, id) = store_with_running_job();
    let before = fs::read(store.record_path(&id)).unwrap();
    let mut dup = JobRecord::with_id(id.clone(), new_job(), now()).unwrap();
    dup.set_extra("dup", json!(true)).unwrap();
    let err = store.create(&dup).unwrap_err();
    assert_eq!(err.code, ErrorCode::InternalError);
    assert_eq!(fs::read(store.record_path(&id)).unwrap(), before);
}

#[test]
fn missing_jobs_are_not_found_without_creating_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let id = JobId::generate();
    assert_eq!(store.load(&id).unwrap_err().code, ErrorCode::JobNotFound);
    assert_eq!(store.update(&id, |_| Ok(())).unwrap_err().code, ErrorCode::JobNotFound);
    assert_eq!(store.download_lock(&id).unwrap_err().code, ErrorCode::JobNotFound);
    assert_eq!(store.delete(&id, true).unwrap_err().code, ErrorCode::JobNotFound);
    assert!(store.list().unwrap().records.is_empty(), "missing jobs dir lists as empty");

    fs::create_dir_all(store.dir()).unwrap();
    assert_eq!(store.update(&id, |_| Ok(())).unwrap_err().code, ErrorCode::JobNotFound);
    assert!(names_in(store.dir()).is_empty(), "no stray lock files: {:?}", names_in(store.dir()));
}

#[test]
fn update_persists_and_failed_update_writes_nothing() {
    let (_dir, store, id) = store_with_running_job();
    let (rec, value) = store
        .update(&id, |rec| {
            rec.set_extra("note", json!("hello"))?;
            Ok(42)
        })
        .unwrap();
    assert_eq!(value, 42);
    assert_eq!(rec.extra()["note"], "hello");
    assert_eq!(store.load(&id).unwrap().extra()["note"], "hello");

    let before = fs::read(store.record_path(&id)).unwrap();
    let err = store
        .update(&id, |rec| -> Result<(), IrisError> {
            rec.set_extra("note", json!("changed"))?;
            Err(IrisError::internal("closure failed"))
        })
        .unwrap_err();
    assert_eq!(err.message, "closure failed");
    assert_eq!(fs::read(store.record_path(&id)).unwrap(), before);
}

#[test]
fn concurrent_updates_from_threads_never_lose_a_write() {
    let (_dir, store, id) = store_with_running_job();
    let store = Arc::new(store);
    const THREADS: u64 = 8;
    const PER_THREAD: u64 = 25;
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let store = Arc::clone(&store);
            let id = id.clone();
            std::thread::spawn(move || {
                for _ in 0..PER_THREAD {
                    increment(&store, &id);
                }
            })
        })
        .collect();
    // Lock-free readers only ever see complete records.
    for _ in 0..200 {
        let listing = store.list().unwrap();
        assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
        assert_eq!(listing.records.len(), 1);
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(counter(&store.load(&id).unwrap()), THREADS * PER_THREAD);
    // Only the record and its lock file remain (temp files were renamed away).
    assert_eq!(names_in(store.dir()), vec![format!("{id}.json"), format!("{id}.lock")]);
}

const HELPER_DIR: &str = "IRIS_T07_HELPER_STATE_DIR";
const HELPER_JOB: &str = "IRIS_T07_HELPER_JOB_ID";
const HELPER_COUNT: &str = "IRIS_T07_HELPER_COUNT";

/// Not a test on its own: the body of the child processes spawned by
/// `concurrent_updates_from_processes_never_lose_a_write`.
#[test]
#[ignore = "helper process for concurrent_updates_from_processes_never_lose_a_write"]
fn helper_process_increments() {
    let (Ok(dir), Ok(job), Ok(count)) =
        (std::env::var(HELPER_DIR), std::env::var(HELPER_JOB), std::env::var(HELPER_COUNT))
    else {
        return;
    };
    let store = JobStore::new(dir);
    let id = JobId::parse(&job).unwrap();
    for _ in 0..count.parse::<u64>().unwrap() {
        increment(&store, &id);
    }
}

#[test]
fn concurrent_updates_from_processes_never_lose_a_write() {
    let (dir, store, id) = store_with_running_job();
    const PROCESSES: u64 = 3;
    const PER_PROCESS: u64 = 40;
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..PROCESSES)
        .map(|_| {
            Command::new(&exe)
                .args(["helper_process_increments", "--exact", "--ignored", "--test-threads=1", "--quiet"])
                .env(HELPER_DIR, dir.path())
                .env(HELPER_JOB, id.as_str())
                .env(HELPER_COUNT, PER_PROCESS.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    // This process competes too.
    for _ in 0..PER_PROCESS {
        increment(&store, &id);
    }
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "helper process failed:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // The helper really ran (it is #[ignore]d unless selected explicitly).
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"));
    }
    assert_eq!(counter(&store.load(&id).unwrap()), (PROCESSES + 1) * PER_PROCESS);
}

#[test]
fn corrupt_records_are_skipped_by_list_with_a_warning() {
    let (_dir, store, good) = store_with_running_job();
    let bad = JobId::generate();
    fs::write(store.record_path(&bad), b"{\"schema_version\": 1, \"job_id\": \"trunc").unwrap();

    let listing = store.list().unwrap();
    assert_eq!(listing.records.len(), 1);
    assert_eq!(listing.records[0].job_id(), &good);
    assert_eq!(listing.warnings.len(), 1);
    assert_eq!(listing.warnings[0].code, "job_record_unreadable");
    assert!(listing.warnings[0].message.contains(bad.as_str()));

    let err = store.load(&bad).unwrap_err();
    assert_eq!(err.code, ErrorCode::StateInvalid);
    assert_eq!(err.job_id.as_deref(), Some(bad.as_str()));
}

#[test]
fn records_from_a_newer_iris_are_rejected_not_rewritten() {
    let (_dir, store, id) = store_with_running_job();
    let path = store.record_path(&id);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["schema_version"] = json!(2);
    value["status"] = json!("some_future_status");
    let newer = serde_json::to_vec(&value).unwrap();
    fs::write(&path, &newer).unwrap();

    let err = store.load(&id).unwrap_err();
    assert_eq!(err.code, ErrorCode::StateInvalid);
    assert!(err.message.contains("newer iris"), "{}", err.message);
    assert_eq!(store.update(&id, |_| Ok(())).unwrap_err().code, ErrorCode::StateInvalid);
    assert_eq!(fs::read(&path).unwrap(), newer, "never rewritten by an older binary");

    let listing = store.list().unwrap();
    assert!(listing.records.is_empty());
    assert_eq!(listing.warnings[0].code, "job_record_unreadable");
}

#[test]
fn unknown_fields_are_preserved_by_locked_updates() {
    let (_dir, store, id) = store_with_running_job();
    let path = store.record_path(&id);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["added_in_1_1"] = json!({"nested": [1, 2, 3]});
    value["prompt"]["added_in_1_1"] = json!("p");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

    store
        .update(&id, |rec| {
            rec.apply_poll(iris::providers::RemoteStatus::Running { progress: None }, None, now())
        })
        .unwrap();
    let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["added_in_1_1"], json!({"nested": [1, 2, 3]}));
    assert_eq!(after["prompt"]["added_in_1_1"], "p");
    assert!(after["last_checked_at"].is_string());
}

#[test]
fn nested_fields_and_error_codes_from_a_newer_iris_survive_a_rewrite() {
    use iris::domain::{CostEstimate, DownloadState, Usage};
    use iris::error::ErrorCategory;
    use iris::providers::{RemoteArtifact, RemoteStatus};

    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let mut job = new_job();
    job.cost_estimate =
        Some(CostEstimate::usd(0.4, "4 s x $0.1/s", "https://example.invalid/p", "2026-09-24"));
    let mut rec = JobRecord::new(job, now()).unwrap();
    rec.mark_submitted(
        &SubmittedOperation { remote_id: "operations/1".into(), provider_request_id: None },
        now(),
    )
    .unwrap();
    let outputs = vec![RemoteArtifact { uri: "https://example.invalid/v".into(), media_type: None }];
    let usage = Some(Usage { input_tokens: Some(3), ..Usage::default() });
    rec.apply_poll(RemoteStatus::Succeeded { outputs, usage, warnings: vec![] }, None, now()).unwrap();
    rec.mark_output_failed(0, &IrisError::new(ErrorCode::DownloadFailed, "reset"), now()).unwrap();
    store.create(&rec).unwrap();
    let id = rec.job_id().clone();

    // What a newer Iris could have written: unknown fields at every nested level
    // and error codes this version does not know.
    let path = store.record_path(&id);
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["usage"]["future_usage"] = json!({"video_seconds": 4});
    value["cost_estimate"]["future_cost"] = json!({"tier": "standard", "credits": [1, 2]});
    let last_error = &mut value["outputs"][0]["last_error"];
    last_error["code"] = json!("quota_exhausted");
    last_error["category"] = json!("quota");
    last_error["future_error_field"] = json!({"deep": {"deeper": true}});
    last_error["details"] = json!({"future_detail": [1, {"x": "y"}]});
    value["error"] = json!({
        "code": "some_future_code", "category": "some_future_category", "message": "from the future",
        "retryable": null, "retry_after_seconds": null, "hint": null, "provider": "gemini",
        "provider_status": 404, "provider_code": "NOT_FOUND", "provider_request_id": null,
        "job_id": id.as_str(), "remote_operation_id": null, "job_status": null, "details": null,
        "future_error_field": "kept"
    });
    fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    // A rewrite that does not touch those parts.
    store.update(&id, |rec| rec.set_extra("x_touched", json!(true))).unwrap();
    let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for part in ["usage", "cost_estimate", "error"] {
        assert_eq!(after[part], value[part], "{part} changed on rewrite");
        let text = |v: &Value| serde_json::to_string_pretty(&v[part]).unwrap();
        assert_eq!(text(&after), text(&value), "{part} is written back byte for byte");
    }
    assert_eq!(after["outputs"][0]["last_error"], value["outputs"][0]["last_error"]);
    assert_eq!(after["x_touched"], true);

    // This version reads what it understands and shows unknown codes as
    // internal_error, keeping the code as written in details.recorded_code.
    let rec = store.load(&id).unwrap();
    assert_eq!(rec.outputs()[0].download_state, DownloadState::Failed);
    assert_eq!(rec.outputs()[0].last_error.as_ref().unwrap().code, ErrorCode::InternalError);
    assert_eq!(rec.usage().unwrap().input_tokens, Some(3));
    assert!((rec.cost_estimate().unwrap().amount - 0.4).abs() < 1e-9);
    let view = rec.to_view();
    let shown = view.outputs[0].last_error.as_ref().unwrap();
    assert_eq!((shown.code, shown.category), (ErrorCode::InternalError, ErrorCategory::Internal));
    assert_eq!(shown.details.as_ref().unwrap()["recorded_code"], "quota_exhausted");
    assert_eq!(shown.details.as_ref().unwrap()["future_detail"], json!([1, {"x": "y"}]));
    let job_error = view.error.unwrap();
    assert_eq!(job_error.code, ErrorCode::InternalError);
    assert_eq!(job_error.details.unwrap()["recorded_code"], "some_future_code");
    let view_text = serde_json::to_string(&rec.to_view()).unwrap();
    assert!(!view_text.contains("future_error_field"), "unknown fields stay in the record: {view_text}");

    // Replacing an error writes the new one as this version knows it.
    store
        .update(&id, |rec| {
            rec.mark_output_failed(0, &IrisError::new(ErrorCode::DownloadFailed, "again"), now())
        })
        .unwrap();
    let after: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(after["outputs"][0]["last_error"]["code"], "download_failed");
    assert!(after["outputs"][0]["last_error"].get("future_error_field").is_none());
    assert_eq!(after["usage"], value["usage"]);
}

#[test]
fn a_record_whose_id_disagrees_with_its_file_name_is_invalid() {
    let (_dir, store, id) = store_with_running_job();
    let other = JobId::generate();
    fs::copy(store.record_path(&id), store.record_path(&other)).unwrap();
    let err = store.load(&other).unwrap_err();
    assert_eq!(err.code, ErrorCode::StateInvalid);
}

#[test]
fn list_is_newest_first_and_ignores_other_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let old = JobRecord::new(new_job(), ago(300)).unwrap();
    let mid = JobRecord::new(new_job(), ago(200)).unwrap();
    let new = JobRecord::new(new_job(), ago(100)).unwrap();
    for rec in [&mid, &old, &new] {
        store.create(rec).unwrap();
    }
    // Leftovers of an interrupted write, lock files, and unrelated files are ignored.
    fs::write(store.dir().join(format!(".{}.json.abcdef12.tmp", new.job_id())), b"{\"partial").unwrap();
    fs::write(store.dir().join(format!("{}.lock", old.job_id())), b"").unwrap();
    fs::write(store.dir().join("notes.json"), b"{}").unwrap();
    fs::write(store.dir().join("job_UPPERCASE0000000000000000.json"), b"{}").unwrap();

    let listing = store.list().unwrap();
    assert!(listing.warnings.is_empty(), "{:?}", listing.warnings);
    let ids: Vec<&JobId> = listing.records.iter().map(|r| r.job_id()).collect();
    assert_eq!(ids, vec![new.job_id(), mid.job_id(), old.job_id()]);
    // The interrupted temp write did not disturb the record.
    assert_eq!(store.load(new.job_id()).unwrap().created_at(), new.created_at());
}

#[test]
fn stale_submitting_records_are_reported_and_rewritten_as_submission_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let stale = JobRecord::new(new_job(), ago(3600)).unwrap();
    let fresh = JobRecord::new(new_job(), ago(5)).unwrap();
    store.create(&stale).unwrap();
    store.create(&fresh).unwrap();
    let raw_status = |id: &JobId| -> String {
        let v: Value = serde_json::from_slice(&fs::read(store.record_path(id)).unwrap()).unwrap();
        v["status"].as_str().unwrap().to_string()
    };

    // Reported (load and list) without rewriting.
    assert_eq!(store.load(stale.job_id()).unwrap().status(), JobStatus::SubmissionUnknown);
    let listing = store.list().unwrap();
    let status_of = |id: &JobId| listing.records.iter().find(|r| r.job_id() == id).unwrap().status();
    assert_eq!(status_of(stale.job_id()), JobStatus::SubmissionUnknown);
    assert_eq!(status_of(fresh.job_id()), JobStatus::Submitting);
    assert_eq!(raw_status(stale.job_id()), "submitting");

    // Rewritten on the next locked update.
    let (rec, ()) = store.update(stale.job_id(), |_| Ok(())).unwrap();
    assert_eq!(rec.status(), JobStatus::SubmissionUnknown);
    assert_eq!(rec.error().unwrap().code, ErrorCode::SubmissionUncertain);
    assert_eq!(raw_status(stale.job_id()), "submission_unknown");
    assert_eq!(store.load(fresh.job_id()).unwrap().status(), JobStatus::Submitting);

    // A longer configured submit budget moves the threshold.
    let patient = JobStore::new(dir.path()).with_submit_budget(Duration::from_secs(7200));
    let another = JobRecord::new(new_job(), ago(3600)).unwrap();
    patient.create(&another).unwrap();
    assert_eq!(patient.load(another.job_id()).unwrap().status(), JobStatus::Submitting);
    // The creating process records its budget, so a process with the default
    // (shorter) budget does not declare that submitter dead early either...
    assert_eq!(raw(&store, another.job_id())["submit_budget_seconds"], 7200);
    assert_eq!(store.load(another.job_id()).unwrap().status(), JobStatus::Submitting);
    assert!(
        store
            .list()
            .unwrap()
            .records
            .iter()
            .any(|r| r.job_id() == another.job_id() && r.status() == JobStatus::Submitting)
    );
    // ...but once that budget has passed too, it is stale for everyone.
    let long_dead = JobRecord::new(new_job(), ago(7200 + 61)).unwrap();
    patient.create(&long_dead).unwrap();
    assert_eq!(store.load(long_dead.job_id()).unwrap().status(), JobStatus::SubmissionUnknown);
}

fn raw(store: &JobStore, id: &JobId) -> Value {
    serde_json::from_slice(&fs::read(store.record_path(id)).unwrap()).unwrap()
}

#[test]
fn default_stale_threshold_covers_a_worst_case_paid_submit() {
    // 3 attempts × (15s connect + 60s submit + 600s largest upload allowance)
    // + 2 × 60s Retry-After = 2145s.
    let budget = paid_submit_budget(&Timeouts::default());
    assert_eq!(budget, Duration::from_secs(2145));
    let slow = Timeouts { submit: Duration::from_secs(120), ..Timeouts::default() };
    assert_eq!(paid_submit_budget(&slow), Duration::from_secs(3 * 735 + 120));

    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    assert_eq!(store.submit_budget(), budget);
    // Still inside a slow, retried submission (well past the bare 60s + 60s): not stale.
    let slow_submit = JobRecord::new(new_job(), ago(2100)).unwrap();
    store.create(&slow_submit).unwrap();
    assert_eq!(store.load(slow_submit.job_id()).unwrap().status(), JobStatus::Submitting);
    // Past budget + grace (2205s): stale.
    let dead = JobRecord::new(new_job(), ago(2206)).unwrap();
    store.create(&dead).unwrap();
    assert_eq!(store.load(dead.job_id()).unwrap().status(), JobStatus::SubmissionUnknown);
}

#[test]
fn delete_removes_local_files_only_and_protects_active_jobs() {
    let (_dir, store, running) = store_with_running_job();
    // Lock files exist after an update and a download lock.
    increment(&store, &running);
    drop(store.download_lock(&running).unwrap());
    assert_eq!(names_in(store.dir()).len(), 3);

    let err = store.delete(&running, false).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.job_status, Some(JobStatus::Running));
    assert!(err.hint.as_deref().unwrap().contains("--force"));
    assert!(store.load(&running).is_ok());

    store.delete(&running, true).unwrap();
    assert!(names_in(store.dir()).is_empty(), "{:?}", names_in(store.dir()));
    assert_eq!(store.load(&running).unwrap_err().code, ErrorCode::JobNotFound);

    // Terminal jobs need no force.
    let mut rejected = JobRecord::new(new_job(), now()).unwrap();
    rejected.mark_rejected(&IrisError::new(ErrorCode::ProviderError, "400"), now()).unwrap();
    store.create(&rejected).unwrap();
    store.delete(rejected.job_id(), false).unwrap();
    assert!(!store.record_path(rejected.job_id()).exists());

    // Unreadable records can be removed with --force only.
    let corrupt = JobId::generate();
    fs::write(store.record_path(&corrupt), b"not json").unwrap();
    assert_eq!(store.delete(&corrupt, false).unwrap_err().code, ErrorCode::StateInvalid);
    store.delete(&corrupt, true).unwrap();
    assert!(!store.record_path(&corrupt).exists());
}

#[test]
fn delete_uses_the_status_on_disk_so_a_slow_submitter_keeps_its_record() {
    // A record still `submitting` on disk is reported as submission_unknown once
    // past the stale threshold, but its submitter may be alive (slow retries, or a
    // store configured with a shorter budget). Deleting it without --force would
    // make that process's later mark_submitted fail and lose the operation id.
    let dir = tempfile::tempdir().unwrap();
    let impatient = JobStore::new(dir.path()).with_submit_budget(Duration::from_secs(1));
    let rec = JobRecord::new(new_job(), ago(3600)).unwrap();
    impatient.create(&rec).unwrap();
    let id = rec.job_id().clone();
    assert_eq!(impatient.load(&id).unwrap().status(), JobStatus::SubmissionUnknown);

    let err = impatient.delete(&id, false).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(err.job_status, Some(JobStatus::Submitting));
    assert!(err.message.contains("submission_unknown"), "{}", err.message);
    assert!(err.hint.as_deref().unwrap().contains("--force"));

    // The live submitter can still record its operation id.
    let op = SubmittedOperation { remote_id: "operations/late".into(), provider_request_id: None };
    let (after, ()) = impatient.update(&id, |r| r.mark_submitted(&op, now())).unwrap();
    assert_eq!(after.status(), JobStatus::Running);
    assert_eq!(after.remote_operation_id(), Some("operations/late"));

    // --force still removes a submitting record.
    let other = JobRecord::new(new_job(), ago(3600)).unwrap();
    impatient.create(&other).unwrap();
    impatient.delete(other.job_id(), true).unwrap();
    assert_eq!(impatient.load(other.job_id()).unwrap_err().code, ErrorCode::JobNotFound);
}

#[test]
fn deletion_can_be_checked_first_with_the_same_rule() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path()).with_submit_budget(Duration::from_secs(1));
    let mut running = JobRecord::new(new_job(), now()).unwrap();
    running
        .mark_submitted(
            &SubmittedOperation { remote_id: "operations/1".into(), provider_request_id: None },
            now(),
        )
        .unwrap();
    store.create(&running).unwrap();
    let fresh = JobRecord::new(new_job(), now()).unwrap();
    store.create(&fresh).unwrap();
    let abandoned = JobRecord::new(new_job(), ago(3600)).unwrap();
    store.create(&abandoned).unwrap();
    let mut rejected = JobRecord::new(new_job(), now()).unwrap();
    rejected.mark_rejected(&IrisError::new(ErrorCode::ProviderError, "400"), now()).unwrap();
    store.create(&rejected).unwrap();
    let corrupt = JobId::generate();
    fs::write(store.record_path(&corrupt), b"not json").unwrap();
    let missing = JobId::generate();

    let kind = |id: &JobId, force: bool| store.check_delete(id, force).err().map(|r| r.kind);
    assert_eq!(kind(running.job_id(), false), Some(RefusalKind::Active));
    assert_eq!(kind(fresh.job_id(), false), Some(RefusalKind::Active));
    assert_eq!(kind(abandoned.job_id(), false), Some(RefusalKind::Abandoned));
    assert_eq!(kind(rejected.job_id(), false), None);
    assert_eq!(kind(&corrupt, false), Some(RefusalKind::Unreadable));
    assert_eq!(kind(&missing, false), Some(RefusalKind::NotFound));
    for id in [running.job_id(), fresh.job_id(), abandoned.job_id(), &corrupt] {
        assert_eq!(kind(id, true), None, "--force deletes {id}");
    }
    assert_eq!(kind(&missing, true), Some(RefusalKind::NotFound));

    // The abandoned submission points at --force, not at waiting.
    let refusal = store.check_delete(abandoned.job_id(), false).unwrap_err();
    assert_eq!(refusal.error.job_status, Some(JobStatus::Submitting));
    let hint = refusal.error.hint.as_deref().unwrap();
    assert!(hint.contains("--force") && !hint.contains("jobs wait"), "{hint}");
    assert!(refusal.summary.contains("abandoned"), "{}", refusal.summary);
    // Checking deletes nothing; `delete` refuses the same way.
    assert_eq!(names_in(store.dir()).len(), 5);
    assert_eq!(store.delete(abandoned.job_id(), false).unwrap_err().code, ErrorCode::InvalidArgument);
    assert_eq!(store.delete(&corrupt, false).unwrap_err().code, ErrorCode::StateInvalid);

    // The listing names unreadable record files (regular files only).
    fs::create_dir(store.record_path(&missing)).unwrap();
    let listing = store.list().unwrap();
    assert_eq!(listing.warnings.len(), 2, "{:?}", listing.warnings);
    assert_eq!(listing.unreadable.len(), 1);
    assert_eq!(listing.unreadable[0].id, corrupt);
    assert!(listing.warnings.contains(&listing.unreadable[0].warning));
}

/// A job that succeeded at `at` with `uris` as outputs, retained for 2 days.
fn create_succeeded(store: &JobStore, at: Timestamp, uris: &[&str]) -> JobId {
    use iris::providers::{RemoteArtifact, RemoteStatus};
    let mut rec = JobRecord::new(new_job(), at).unwrap();
    rec.mark_submitted(
        &SubmittedOperation { remote_id: "operations/9".into(), provider_request_id: None },
        at,
    )
    .unwrap();
    let outputs = uris.iter().map(|u| RemoteArtifact { uri: u.to_string(), media_type: None }).collect();
    rec.apply_poll(
        RemoteStatus::Succeeded { outputs, usage: None, warnings: vec![] },
        Some(Duration::from_secs(2 * 24 * 3600)),
        at,
    )
    .unwrap();
    store.create(&rec).unwrap();
    rec.job_id().clone()
}

#[test]
fn succeeded_jobs_with_outputs_still_to_download_need_force_while_the_provider_keeps_them() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let good = "https://generativelanguage.googleapis.com/v1beta/files/a:download?alt=media";
    let id = create_succeeded(&store, ago(60), &[good, good, "not a url"]);

    let refusal = store.check_delete(&id, false).unwrap_err();
    assert_eq!(refusal.kind, RefusalKind::NotDownloaded);
    let e = refusal.error;
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert_eq!(e.job_status, Some(JobStatus::Succeeded));
    // The unusable output (index 2) can never be downloaded, so it protects nothing.
    assert_eq!(e.details["outputs_not_downloaded"], json!([0, 1]));
    let until = store.load(&id).unwrap().remote_expires_at().unwrap().to_string();
    assert_eq!(e.details["remote_expires_at"], json!(until));
    assert!(e.message.contains("output(s) 0, 1") && e.message.contains(&until), "{}", e.message);
    assert!(e.hint.as_deref().unwrap().contains(&format!("iris jobs download {id}")));
    assert!(store.delete(&id, false).is_err());
    assert!(store.load(&id).is_ok());

    // A failed download still leaves the output worth protecting; once every
    // usable output is downloaded (or expired), the record is no longer needed.
    let artifact = |index| iris::domain::Artifact {
        index,
        path: format!("/tmp/out-{index}.mp4"),
        media_type: "video/mp4".into(),
        bytes: 1,
        sha256: "00".into(),
        width: None,
        height: None,
        duration_seconds: None,
    };
    store.update(&id, |r| r.mark_output_downloaded(0, &artifact(0), now())).unwrap();
    store
        .update(&id, |r| r.mark_output_failed(1, &IrisError::new(ErrorCode::DownloadFailed, "x"), now()))
        .unwrap();
    assert_eq!(
        store.check_delete(&id, false).unwrap_err().error.details["outputs_not_downloaded"],
        json!([1])
    );
    store
        .update(&id, |r| r.mark_output_expired(1, &IrisError::new(ErrorCode::ArtifactExpired, "gone"), now()))
        .unwrap();
    assert!(store.check_delete(&id, false).is_ok());

    // Past the retention period the outputs may be gone: no protection.
    let old = create_succeeded(&store, ago(3 * 24 * 3600), &[good]);
    assert!(store.check_delete(&old, false).is_ok());
    // --force deletes a protected record.
    let other = create_succeeded(&store, ago(60), &[good]);
    store.delete(&other, true).unwrap();
    assert_eq!(store.load(&other).unwrap_err().code, ErrorCode::JobNotFound);
}

#[test]
fn download_lock_is_exclusive() {
    let (_dir, store, id) = store_with_running_job();
    let held = store.download_lock(&id).unwrap();
    assert_eq!(held.job_id(), &id);
    assert!(store.try_download_lock(&id).unwrap().is_none(), "second holder must be refused");
    // The record lock is independent of the download lock.
    increment(&store, &id);
    drop(held);
    let again = store.try_download_lock(&id).unwrap();
    assert!(again.is_some());
}

#[test]
fn download_lock_blocks_until_released() {
    let (_dir, store, id) = store_with_running_job();
    let store = Arc::new(store);
    let held = store.download_lock(&id).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = {
        let store = Arc::clone(&store);
        let id = id.clone();
        std::thread::spawn(move || {
            let _lock = store.download_lock(&id).unwrap();
            tx.send(()).unwrap();
        })
    };
    assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "must wait while the lock is held");
    drop(held);
    rx.recv_timeout(Duration::from_secs(10)).expect("acquired after release");
    waiter.join().unwrap();
}

#[tokio::test]
async fn async_download_lock_waits_without_blocking_the_runtime() {
    let (_dir, store, id) = store_with_running_job();
    let held = store.download_lock(&id).unwrap();

    // On this single-threaded runtime a blocking flock would never let the timer
    // fire; the async form yields between attempts, so the timeout does.
    let waited = tokio::time::timeout(
        Duration::from_millis(150),
        store.download_lock_async(&id, Duration::from_millis(10)),
    )
    .await;
    assert!(waited.is_err(), "must still be waiting while another holder has the lock");

    drop(held);
    let lock = tokio::time::timeout(
        Duration::from_secs(10),
        store.download_lock_async(&id, Duration::from_millis(10)),
    )
    .await
    .expect("acquired after release")
    .unwrap();
    assert_eq!(lock.job_id(), &id);
    assert!(store.try_download_lock(&id).unwrap().is_none(), "the async lock is a real lock");

    let missing = JobId::generate();
    assert_eq!(
        store.download_lock_async(&missing, Duration::from_millis(10)).await.unwrap_err().code,
        ErrorCode::JobNotFound
    );
}

// ----- labels ------------------------------------------------------------------------------------

/// A new job carrying `label`.
fn labeled(label: &str) -> NewJob {
    NewJob { label: Some(JobLabel::parse(label, "--label").unwrap()), ..new_job() }
}

/// A record with `label` in `status`, reached through the record's own transitions.
fn labeled_in(label: &str, status: JobStatus) -> JobRecord {
    use iris::providers::{RemoteArtifact, RemoteStatus};
    let mut rec = JobRecord::new(labeled(label), now()).unwrap();
    let submitted = SubmittedOperation { remote_id: "operations/7".into(), provider_request_id: None };
    let rejected = IrisError::new(ErrorCode::ProviderError, "rejected");
    match status {
        JobStatus::Submitting => {}
        JobStatus::Failed => rec.mark_rejected(&rejected, now()).unwrap(),
        JobStatus::SubmissionUnknown => rec.mark_submission_unknown(&rejected, now()).unwrap(),
        JobStatus::Running | JobStatus::Succeeded | JobStatus::Expired => {
            rec.mark_submitted(&submitted, now()).unwrap();
            let poll = match status {
                JobStatus::Succeeded => Some(RemoteStatus::Succeeded {
                    outputs: vec![RemoteArtifact {
                        uri: "https://example.invalid/v".into(),
                        media_type: None,
                    }],
                    usage: None,
                    warnings: vec![],
                }),
                JobStatus::Expired => Some(RemoteStatus::Gone { error: rejected.clone() }),
                _ => None,
            };
            if let Some(poll) = poll {
                rec.apply_poll(poll, None, now()).unwrap();
            }
        }
    }
    assert_eq!(rec.status(), status);
    rec
}

/// No two records share a label: whatever the status of the record that has it, a
/// new record with the label is `label_in_use`, naming that job, and is not
/// written; another label, or none, is created as usual. The hint says what to do
/// for that job's status, and what submitting again costs.
#[test]
fn a_label_is_refused_while_any_record_has_it_with_a_hint_for_its_status() {
    let hint = |status: JobStatus, id: &JobId| match status {
        JobStatus::Submitting | JobStatus::Running => format!(
            "the job is still {status}: follow it with `iris jobs status {id}` or `iris jobs wait {id}`; deleting \
             its local record does not cancel the remote job, which keeps running and is billed; to submit \
             another paid job, use another label"
        ),
        JobStatus::Succeeded => format!(
            "the job succeeded: save its outputs with `iris jobs download {id}` (or `iris jobs wait {id}`); to \
             submit another paid job under this label, delete its local record first (`iris jobs delete {id}`), \
             or use another label"
        ),
        JobStatus::SubmissionUnknown => format!(
            "the provider may have accepted and billed this job's submission, and Iris cannot follow it; check \
             usage and billing in the provider's console before submitting again, whether after deleting its \
             local record (`iris jobs delete {id}`) or under another label"
        ),
        JobStatus::Failed | JobStatus::Expired => format!(
            "the job {status}; a new submission is billed: to submit one under this label, delete its local \
             record first (`iris jobs delete {id}`), or use another label"
        ),
    };
    for status in [
        JobStatus::Submitting,
        JobStatus::Running,
        JobStatus::Succeeded,
        JobStatus::Failed,
        JobStatus::SubmissionUnknown,
        JobStatus::Expired,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::new(dir.path());
        let first = labeled_in("paper-boat-1", status);
        store.create(&first).unwrap();

        let second = JobRecord::new(labeled("paper-boat-1"), now()).unwrap();
        for e in [store.check_label("paper-boat-1").unwrap_err(), store.create(&second).unwrap_err()] {
            assert_eq!(e.code, ErrorCode::LabelInUse, "{status}");
            assert_eq!(e.exit_code(), 2);
            assert_eq!(e.job_id.as_deref(), Some(first.job_id().as_str()), "{status}");
            assert_eq!(e.job_status, Some(status));
            assert_eq!(e.details["label"], "paper-boat-1");
            assert_eq!(e.details["model"], "veo-test");
            assert_eq!(e.details["created_at"], first.created_at().to_string());
            assert_eq!(e.hint.as_deref(), Some(hint(status, first.job_id()).as_str()), "{status}");
        }
        assert!(!store.record_path(second.job_id()).exists(), "{status}: nothing written");

        // Labels are compared exactly.
        for other in ["Paper-Boat-1", "paper-boat-2"] {
            store.create(&JobRecord::new(labeled(other), now()).unwrap()).unwrap();
        }
        store.create(&JobRecord::new(new_job(), now()).unwrap()).unwrap();
        assert_eq!(store.list().unwrap().records.len(), 4);
    }
}

/// A record the store cannot read could have the label: while one exists, a
/// labeled record is refused (`state_invalid`, naming the file), in the check and
/// in `create`; a record without a label is created as usual.
#[test]
fn a_label_is_refused_while_any_record_cannot_be_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    store.create(&JobRecord::new(labeled("boat-old"), now()).unwrap()).unwrap();
    // A newer Iris's record with the label, and a corrupt one.
    let newer = JobRecord::new(labeled("boat-new"), now()).unwrap();
    store.create(&newer).unwrap();
    let mut value = raw(&store, newer.job_id());
    value["schema_version"] = 2.into();
    fs::write(store.record_path(newer.job_id()), serde_json::to_vec(&value).unwrap()).unwrap();
    let corrupt = store.dir().join(format!("{}.json", JobId::generate()));
    fs::write(&corrupt, b"{ not json").unwrap();
    let mut unreadable = vec![store.record_path(newer.job_id()), corrupt.clone()];
    unreadable.sort();

    let second = JobRecord::new(labeled("boat-new"), now()).unwrap();
    for e in [store.check_label("boat-new").unwrap_err(), store.create(&second).unwrap_err()] {
        assert_eq!(e.code, ErrorCode::StateInvalid);
        assert_eq!(e.retryable, Some(false));
        assert_eq!(e.details["label"], "boat-new");
        let mut named: Vec<String> =
            e.details["unreadable"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().into()).collect();
        named.sort();
        assert_eq!(named, unreadable.iter().map(|p| p.display().to_string()).collect::<Vec<_>>());
        let hint = e.hint.clone().unwrap();
        assert!(hint.contains("`iris jobs list`") && hint.contains("without --label"), "{hint}");
    }
    assert!(!store.record_path(second.job_id()).exists());
    // A readable record with the label is still reported as such.
    assert_eq!(store.check_label("boat-old").unwrap_err().code, ErrorCode::LabelInUse);
    // Unlabeled records are not affected; once the files are gone, labels are free.
    store.create(&JobRecord::new(new_job(), now()).unwrap()).unwrap();
    for path in &unreadable {
        fs::remove_file(path).unwrap();
    }
    store.create(&second).unwrap();
}

/// Deleting the record that has a label frees it.
#[test]
fn deleting_the_record_frees_its_label() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let first = labeled_in("paper-boat-1", JobStatus::Failed);
    store.create(&first).unwrap();
    store.delete(first.job_id(), false).unwrap();
    store.check_label("paper-boat-1").unwrap();
    store.create(&JobRecord::new(labeled("paper-boat-1"), now()).unwrap()).unwrap();
}

/// The label is stored in the record and shown in its view; a record without one
/// (written by an older Iris, with no `label` field at all) reads as `null`, keeps
/// no label after a locked rewrite, and never matches a label.
#[test]
fn a_label_is_recorded_and_older_records_have_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::new(dir.path());
    let rec = JobRecord::new(labeled("paper-boat-1"), now()).unwrap();
    store.create(&rec).unwrap();
    assert_eq!(raw(&store, rec.job_id())["label"], "paper-boat-1");
    let loaded = store.load(rec.job_id()).unwrap();
    assert_eq!(loaded.label(), Some("paper-boat-1"));
    assert_eq!(loaded.to_view().label.as_deref(), Some("paper-boat-1"));

    let old = JobRecord::new(new_job(), now()).unwrap();
    store.create(&old).unwrap();
    let mut value = raw(&store, old.job_id());
    value.as_object_mut().unwrap().remove("label");
    fs::write(store.record_path(old.job_id()), serde_json::to_vec(&value).unwrap()).unwrap();
    let loaded = store.load(old.job_id()).unwrap();
    assert_eq!(loaded.label(), None);
    assert!(loaded.to_view().label.is_none());
    increment(&store, old.job_id());
    assert_eq!(raw(&store, old.job_id())["label"], Value::Null);
    store.check_label("paper-boat-2").unwrap();
}

/// The label check and the record are one step under the store lock: of many
/// threads, each with its own store and its own file handles (the lock excludes
/// them as it excludes processes), creating a record with one label at the same
/// moment, exactly one succeeds and every other gets `label_in_use` naming it.
#[test]
fn concurrent_creates_with_one_label_leave_one_record() {
    const THREADS: usize = 8;
    for round in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let store = JobStore::new(dir.path());
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let rec = JobRecord::new(labeled("same-label"), now()).unwrap();
                    barrier.wait();
                    store.create(&rec).map(|()| rec.job_id().clone())
                })
            })
            .collect();
        let results: Vec<Result<JobId, IrisError>> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let created: Vec<&JobId> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
        assert_eq!(created.len(), 1, "round {round}: {results:?}");
        for e in results.iter().filter_map(|r| r.as_ref().err()) {
            assert_eq!(e.code, ErrorCode::LabelInUse, "round {round}: {e:?}");
            assert_eq!(e.job_id.as_deref(), Some(created[0].as_str()));
        }
        let store = JobStore::new(dir.path());
        let listing = store.list().unwrap();
        assert_eq!(listing.records.len(), 1, "round {round}");
        assert_eq!(listing.records[0].label(), Some("same-label"));
    }
}
