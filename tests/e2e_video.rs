//! End-to-end Veo job scenarios: every step is a separate `iris` process against a
//! 127.0.0.1 mock of the Gemini API (predictLongRunning, operations, Files API) and a
//! second mock origin standing in for the signed-URL file host the download
//! redirects to.
//!
//! Covers scenarios 5 (happy path across processes), 6 (submission
//! uncertainty), 7 (wait limit and Ctrl-C), 8 (download failure, recovery, expiry),
//! 9 (concurrent waits), and the video half of 10 (secret hygiene with `-vv`), plus
//! the one-command wait-and-save flow, provider-side outcomes seen by later
//! processes (poll retries, remote failure, safety block, operation gone), and a
//! rate-limited submission that is retried into a single job.
//!
//! Every test ends with [`VeoMock::assert_no_credential_leaks`]: the key reached the
//! API origin only as `x-goog-api-key`, never the file host, and never a URL.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use support::*;

const PROMPT: &str = "a paper boat drifting down a rainy street";

/// `video generate --detach --json` with the cheapest settings; returns the job id.
fn submit_detached(sb: &Sandbox, veo: &VeoMock, extra: &[&str]) -> String {
    let v = sb
        .iris()
        .gemini(&veo.api)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--resolution", "720p"])
        .args(["--aspect-ratio", "16:9", "--detach", "--json"])
        .args(extra)
        .run()
        .ok();
    let job = &v["result"]["job"];
    assert_eq!(job["status"], "running", "{v}");
    job["job_id"].as_str().unwrap().to_string()
}

fn job_of(v: &Value) -> &Value {
    &v["result"]["job"]
}

fn assert_job_id(id: &str) {
    let suffix = id.strip_prefix("job_").expect(id);
    assert_eq!(suffix.len(), 26, "{id}");
    assert!(suffix.chars().all(|c| c.is_ascii_digit() || c.is_ascii_lowercase()), "{id}");
}

/// The downloaded video artifact matches the file host's bytes.
fn assert_video(art: &Value, path: &Path) {
    let video = veo_video();
    assert_eq!(art["path"], path.to_str().unwrap(), "{art}");
    assert_eq!(std::fs::read(path).unwrap(), video);
    assert_eq!(art["media_type"], "video/mp4");
    assert_eq!(art["bytes"], video.len() as u64);
    assert_eq!(art["sha256"], sha256_hex(&video));
    assert_eq!(art["duration_seconds"], 4.0, "the MP4 structure was parsed: {art}");
}

// ----- scenario 5: happy path across processes ------------------------------------------------

#[test]
fn a_veo_job_is_followed_across_processes_and_downloaded_through_a_redirect() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();

    // 1. Submit and return.
    let v = sb
        .iris()
        .gemini(&veo.api)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--resolution", "720p"])
        .args(["--aspect-ratio", "16:9", "--detach", "--json"])
        .run()
        .ok();
    assert_eq!(v["command"], "video.generate");
    let job = job_of(&v);
    let id = job["job_id"].as_str().unwrap().to_string();
    assert_job_id(&id);
    assert_eq!(job["status"], "running");
    assert_eq!(job["provider"], "gemini");
    assert_eq!(job["model"], VEO_LITE);
    assert_eq!(job["remote_operation_id"], veo.op_name.as_str());
    assert!(job["submitted_at"].is_string());
    assert_eq!(job["cost_estimate"]["estimated"], true);
    assert!((job["cost_estimate"]["amount"].as_f64().unwrap() - 0.20).abs() < 1e-9, "4 s × $0.05: {job}");
    assert_eq!(
        v["result"]["next_steps"],
        json!([format!("iris jobs status {id}"), format!("iris jobs wait {id}")])
    );

    let submits = veo.api.hits("POST", &veo_submit_path(VEO_LITE));
    assert_eq!(submits.len(), 1);
    assert_eq!(header(&submits[0], "x-goog-api-key").as_deref(), Some(GEMINI_KEY));
    assert!(submits[0].url.query().is_none());
    assert_eq!(
        body_json(&submits[0]),
        json!({
            "instances": [ { "prompt": PROMPT } ],
            "parameters": { "aspectRatio": "16:9", "resolution": "720p", "durationSeconds": 4 }
        }),
        "duration, resolution, and aspect ratio always sent; nothing else"
    );
    assert_eq!(veo.polls(), 0, "--detach does not poll");

    // The record persists what a later process needs, but not the prompt.
    let rec = sb.record(&id);
    assert_eq!(rec["status"], "running");
    assert_eq!(rec["remote_operation_id"], veo.op_name.as_str());
    assert!(rec["prompt"]["text"].is_null());
    assert_eq!(rec["prompt"]["sha256"], sha256_hex(PROMPT.as_bytes()));
    assert!(!std::fs::read_to_string(sb.record_path(&id)).unwrap().contains(PROMPT));

    // 2. A new process refreshes the status once.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
    assert_eq!(v["command"], "jobs.status");
    assert_eq!(job_of(&v)["status"], "running");
    assert!(job_of(&v)["last_checked_at"].is_string());
    assert_eq!(veo.polls(), 1);

    // 3. The provider finishes; 4. a new process waits and downloads.
    veo.succeed();
    let out = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run();
    let v = out.ok();
    let job = job_of(&v);
    assert_eq!(job["status"], "succeeded");
    assert!(job["completed_at"].is_string() && job["remote_expires_at"].is_string(), "{job}");
    let target = sb.path(&format!("{id}.mp4"));
    assert_eq!(job["artifacts"].as_array().unwrap().len(), 1);
    assert_video(&job["artifacts"][0], &target);
    assert_eq!(job["outputs"][0]["download_state"], "downloaded");
    assert!(out.stderr.contains("Downloading output 0"), "{}", out.stderr);

    // The credential went to the API origin only; the redirect hop to the file host
    // (another origin) carried none.
    let api_downloads = veo.api.hits("GET", &VeoMock::download_path());
    assert_eq!(api_downloads.len(), 1);
    assert_eq!(header(&api_downloads[0], "x-goog-api-key").as_deref(), Some(GEMINI_KEY));
    let fetched = veo.files.requests();
    assert_eq!(fetched.len(), 1);
    for req in &fetched {
        assert!(header(req, "x-goog-api-key").is_none(), "the key crossed origins: {:?}", req.headers);
        assert!(header(req, "authorization").is_none());
        assert!(req.url.query().unwrap().contains(SIGNATURE), "the signed URL was followed as given");
    }

    // 5. A repeat download is a no-op without network.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert_eq!(v["command"], "jobs.download");
    assert!(warning_codes(&v).contains(&"already_downloaded".to_string()), "{v}");
    assert_video(&job_of(&v)["artifacts"][0], &target);
    assert_eq!((veo.api_downloads(), veo.file_fetches()), (1, 1), "no new file-host request");

    // 6. A download to another directory copies the local file, without network.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "-d", "other", "--json"]).run().ok();
    assert_video(&job_of(&v)["artifacts"][0], &sb.path(&format!("other/{id}.mp4")));
    assert_eq!((veo.api_downloads(), veo.file_fetches()), (1, 1), "copied locally");
    assert!(target.is_file(), "the first copy is untouched");

    assert_eq!(veo.submits(), 1, "no step ever resubmitted");
    assert_eq!(veo.polls(), 2, "status refreshed once; wait polled once; downloads never poll");
    veo.assert_no_credential_leaks();
}

#[test]
fn jobs_download_checks_a_running_record_once_and_downloads_a_job_that_finished_meanwhile() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);

    // Still running: one status check, then job_not_ready (exit 4).
    let out = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run();
    let v = out.err(4, "job_not_ready");
    assert_eq!(v["error"]["job_status"], "running");
    assert_eq!(veo.polls(), 1);

    // The provider finished since: the stale local record is refreshed first.
    veo.succeed();
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert_eq!(job_of(&v)["status"], "succeeded");
    assert_video(&job_of(&v)["artifacts"][0], &sb.path(&format!("{id}.mp4")));
    assert_eq!(veo.polls(), 2);
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();
}

#[test]
fn video_generate_waits_and_saves_in_one_command() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    // The provider is already done at the first poll, so no poll interval passes.
    veo.succeed();
    let out = sb
        .iris()
        .gemini(&veo.api)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "-o", "boat.mp4"])
        .run();
    assert_eq!(out.human(), format!("Saved {}\n", sb.path("boat.mp4").display()));
    assert_eq!(std::fs::read(sb.path("boat.mp4")).unwrap(), veo_video());
    assert!(out.stderr.contains("paid request"), "{}", out.stderr);

    let v = sb
        .iris()
        .gemini(&veo.api)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "-d", "clips", "--json"])
        .run()
        .ok();
    let job = job_of(&v);
    assert_eq!(job["status"], "succeeded");
    let id = job["job_id"].as_str().unwrap();
    assert_video(&job["artifacts"][0], &sb.path(&format!("clips/{id}.mp4")));
    assert_eq!(sb.record(id)["outputs"][0]["download_state"], "downloaded");
    assert_eq!(veo.submits(), 2, "one submission per command");
    veo.assert_no_credential_leaks();
}

#[test]
fn provider_side_outcomes_of_a_running_job_are_reported_by_later_processes() {
    // Transient poll failures are retried (IdempotentRead) and never change the job.
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    let busy = google_error(503, "UNAVAILABLE", "The service is currently unavailable.", json!([]))
        .insert_header("retry-after-ms", "5");
    veo.operation.set(busy.clone());
    let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
    assert_eq!(job_of(&v)["status"], "running", "an unreachable provider never fails the job");
    assert!(warning_codes(&v).contains(&"status_refresh_failed".to_string()), "{v}");
    assert_eq!(veo.polls(), 5, "reads are retried (5 attempts)");
    assert_eq!(sb.record(&id)["status"], "running");

    // The job failed remotely.
    let failed = json_response(
        200,
        json!({ "name": veo.op_name, "done": true, "error": { "code": 13, "message": "Video generation failed." } }),
    );
    veo.operation.set(failed);
    let v =
        sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "remote_job_failed");
    assert_eq!(v["error"]["job_status"], "failed");
    assert_eq!(v["error"]["provider_code"], "INTERNAL");
    assert_eq!(sb.record(&id)["status"], "failed");
    let v = sb
        .iris()
        .gemini(&veo.api)
        .args(["jobs", "download", &id, "--json"])
        .run()
        .err(1, "remote_job_failed");
    assert_eq!(v["error"]["job_id"], id.as_str());
    veo.assert_no_credential_leaks();

    // The provider's safety filters blocked the video.
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.operation.set(json_response(
        200,
        json!({
            "name": veo.op_name,
            "done": true,
            "response": { "generateVideoResponse": {
                "raiMediaFilteredCount": 1,
                "raiMediaFilteredReasons": ["The video was blocked by a safety filter."]
            } }
        }),
    ));
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "content_blocked");
    assert_eq!(v["error"]["job_status"], "failed");
    assert!(v["error"]["hint"].as_str().unwrap().contains("not charged"), "{v}");
    veo.assert_no_credential_leaks();

    veo.assert_no_credential_leaks();
}

#[test]
fn a_poll_404_expires_a_job_only_when_google_says_not_found_after_the_retention_period() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);

    // A 404 that is not Google's (a web server or proxy page, e.g. a wrong base
    // URL), and Google's own NOT_FOUND seconds after submission: the job stays
    // running. `jobs status` reports a refresh failure; `jobs wait` stops with the
    // error, keeping the provider's status and the operation id.
    let html =
        wiremock::ResponseTemplate::new(404).set_body_raw("<html><h1>Not Found</h1></html>", "text/html");
    let not_found = google_error(404, "NOT_FOUND", "Operation not found.", json!([]));
    for answer in [html.clone(), not_found.clone()] {
        veo.operation.set(answer);
        let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
        assert_eq!(job_of(&v)["status"], "running", "{v}");
        assert!(warning_codes(&v).contains(&"status_refresh_failed".to_string()), "{v}");

        let v = sb
            .iris()
            .gemini(&veo.api)
            .args(["jobs", "wait", &id, "--json"])
            .run()
            .err(3, "permission_denied");
        let error = &v["error"];
        assert_eq!(error["job_status"], "running");
        assert_eq!(error["provider"], "gemini");
        assert_eq!(error["provider_status"], 404);
        assert_eq!(error["remote_operation_id"], veo.op_name.as_str());
        let hint = error["hint"].as_str().unwrap();
        assert!(hint.contains("GEMINI_API_KEY") && hint.contains("base URL"), "{hint}");
        let rec = sb.record(&id);
        assert_eq!(rec["status"], "running", "an early 404 never ends the job");
        assert!(rec["error"].is_null() && rec["completed_at"].is_null(), "{rec}");
    }

    // Three days later (past the 2-day retention), a non-Google 404 still proves nothing.
    backdate_record(&sb, &id, 3);
    veo.operation.set(html);
    sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(3, "permission_denied");
    assert_eq!(sb.record(&id)["status"], "running");

    // Google's NOT_FOUND past the retention period: the job is expired, and the
    // record keeps the provider's evidence.
    veo.operation.set(not_found);
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "artifact_expired");
    assert_eq!(v["error"]["job_status"], "expired");
    assert_eq!(v["error"]["provider"], "gemini");
    assert_eq!(v["error"]["provider_status"], 404);
    let rec = sb.record(&id);
    assert_eq!(rec["status"], "expired");
    assert_eq!(rec["error"]["provider"], "gemini");
    assert_eq!(rec["error"]["provider_status"], 404);
    assert_eq!(rec["error"]["provider_code"], "NOT_FOUND");
    assert!(rec["error"]["message"].as_str().unwrap().contains("retention period"), "{rec}");
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();
}

/// A pass-through proxy base URL: it forwards the API calls but not the Files API
/// URIs in the answer, which stay on the real API origin (here `veo.api`).
fn pass_through_proxy(veo: &VeoMock) -> MockApi {
    let proxy = MockApi::start();
    proxy.on("POST", &veo_submit_path(VEO_LITE), json_response(200, json!({ "name": veo.op_name })));
    proxy.on(
        "GET",
        &format!("/v1beta/{}", veo.op_name),
        json_response(
            200,
            json!({
                "name": veo.op_name,
                "done": true,
                "response": { "generateVideoResponse": {
                    "generatedSamples": [ { "video": { "uri": veo.output_uri() } } ]
                } }
            }),
        ),
    );
    proxy
}

#[test]
fn an_output_uri_off_the_configured_origin_keeps_the_job_succeeded_until_the_base_url_is_fixed() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let proxy = pass_through_proxy(&veo);
    let v = sb
        .iris()
        .gemini(&proxy)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--detach", "--json"])
        .run()
        .ok();
    let id = job_of(&v)["job_id"].as_str().unwrap().to_string();

    // The provider finished; Iris refuses to fetch a URI outside the base URL.
    let out = sb.iris().gemini(&proxy).args(["jobs", "wait", &id, "--json"]).run();
    let v = out.err(1, "download_failed");
    let error = &v["error"];
    assert_eq!(error["job_status"], "succeeded", "a refused download is not a failed generation: {v}");
    assert_eq!(error["retryable"], false);
    assert_eq!(error["provider"], "gemini");
    assert_eq!(error["details"]["uri"], veo.output_uri().as_str());
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains("proxy") && hint.contains(&format!("iris jobs download {id}")), "{hint}");
    assert!(out.stderr.contains(&format!("Job {id} succeeded")), "{}", out.stderr);

    let rec = sb.record(&id);
    assert_eq!(rec["status"], "succeeded");
    assert!(rec["error"].is_null());
    assert_eq!(rec["outputs"][0]["remote_uri"], veo.output_uri().as_str(), "the raw URI is kept");
    assert_eq!(rec["outputs"][0]["download_state"], "failed");
    assert_eq!(rec["outputs"][0]["last_error"]["code"], "download_failed");
    assert_eq!(veo.api.total(), 0, "nothing (and no key) went to the other origin");

    // Later processes still offer the download; with the same base URL it is refused again.
    let v = sb.iris().gemini(&proxy).args(["jobs", "status", &id, "--json"]).run().ok();
    assert_eq!(job_of(&v)["status"], "succeeded");
    assert_eq!(job_of(&v)["outputs"][0]["download_state"], "failed");
    assert_eq!(v["result"]["next_steps"], json!([format!("iris jobs download {id}")]));
    assert!(!v.to_string().contains("remote_uri"), "{v}");
    sb.iris().gemini(&proxy).args(["jobs", "download", &id, "--json"]).run().err(1, "download_failed");
    assert_eq!(veo.api.total(), 0);

    // With the base URL pointing at the real API origin, the same job downloads.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert_video(&job_of(&v)["artifacts"][0], &sb.path(&format!("{id}.mp4")));
    assert_eq!(sb.record(&id)["outputs"][0]["download_state"], "downloaded");
    assert_eq!((veo.api_downloads(), veo.file_fetches()), (1, 1));
    assert_eq!(proxy.count("POST", &veo_submit_path(VEO_LITE)), 1, "nothing was resubmitted");
    assert_eq!((veo.submits(), veo.polls()), (0, 0), "downloading never submits or polls");
    proxy.assert_credentials_only_in(Some(("x-goog-api-key", GEMINI_KEY)));
    veo.assert_no_credential_leaks();
}

#[test]
fn a_rate_limited_veo_submission_is_retried_into_one_job() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let limited = google_error(
        429,
        "RESOURCE_EXHAUSTED",
        "Resource has been exhausted (e.g. check quota).",
        json!([{ "@type": "type.googleapis.com/google.rpc.RetryInfo", "retryDelay": "0.05s" }]),
    );
    veo.submit.set_sequence(vec![limited, json_response(200, json!({ "name": veo.op_name }))]);
    let id = submit_detached(&sb, &veo, &[]);
    assert_eq!(veo.submits(), 2, "the 429 was a definite rejection, so it was resent once");
    assert_eq!(files_in(&sb.jobs_dir()).iter().filter(|n| n.ends_with(".json")).count(), 1, "one job record");
    assert_eq!(sb.record(&id)["status"], "running");
    veo.assert_no_credential_leaks();
}

// ----- scenario 6: submission uncertainty ------------------------------------------------------

#[test]
fn an_uncertain_veo_submission_is_recorded_and_never_resubmitted() {
    let answers = [
        google_error(500, "INTERNAL", "An internal error has occurred.", json!([])),
        json_response(200, json!({ "unexpected": "shape" })),
    ];
    for answer in answers {
        let sb = Sandbox::new();
        let veo = VeoMock::start();
        veo.submit.set(answer);

        let out = sb
            .iris()
            .gemini(&veo.api)
            .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--json"])
            .run();
        let v = out.err(5, "submission_uncertain");
        let error = &v["error"];
        assert_eq!(error["category"], "uncertain");
        assert_eq!(error["retryable"], false);
        assert_eq!(error["job_status"], "submission_unknown");
        assert_eq!(error["details"]["charge_possible"], true);
        assert!(error["hint"].as_str().unwrap().contains("resubmit"), "{error}");
        let id = error["job_id"].as_str().unwrap().to_string();
        assert_job_id(&id);
        assert_eq!(veo.submits(), 1);
        assert_eq!(sb.record(&id)["status"], "submission_unknown");

        // Later processes show it and never resubmit it.
        let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
        assert_eq!(job_of(&v)["status"], "submission_unknown");
        assert_eq!(job_of(&v)["error"]["code"], "submission_uncertain");
        let v = sb.iris().gemini(&veo.api).args(["jobs", "list", "--json"]).run().ok();
        assert_eq!(v["result"]["jobs"][0]["job_id"], id.as_str());
        assert_eq!(v["result"]["jobs"][0]["status"], "submission_unknown");
        for cmd in [["jobs", "wait"], ["jobs", "download"]] {
            let v = sb
                .iris()
                .gemini(&veo.api)
                .args(cmd)
                .args([id.as_str(), "--json"])
                .run()
                .err(5, "submission_uncertain");
            assert_eq!(v["error"]["job_id"], id.as_str());
        }
        assert_eq!(veo.submits(), 1, "no process ever resubmits");
        assert_eq!(veo.api.total(), 1, "nothing else was requested (no operation id to poll)");
        assert!(files_in(&sb.work()).is_empty());
        veo.assert_no_credential_leaks();
    }
}

// ----- scenario 7: wait limit and Ctrl-C ---------------------------------------------------------

#[test]
fn a_wait_limit_exits_4_and_leaves_the_job_running() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);

    let out = sb
        .iris()
        .gemini(&veo.api)
        .args(["jobs", "wait", &id, "--timeout", "3s", "--poll-interval", "2s", "--json"])
        .run();
    let v = out.err(4, "wait_timeout");
    assert_eq!(v["error"]["category"], "pending");
    assert_eq!(v["error"]["job_id"], id.as_str());
    assert_eq!(v["error"]["job_status"], "running");
    assert!(v["error"]["hint"].as_str().unwrap().contains(&format!("iris jobs wait {id}")), "{v}");
    assert!(
        out.elapsed >= Duration::from_secs(3) && out.elapsed < Duration::from_secs(20),
        "{:?}",
        out.elapsed
    );
    assert!(veo.polls() >= 1);

    assert_eq!(sb.record(&id)["status"], "running", "a local wait limit never fails the job");
    let v = sb.iris().args(["jobs", "status", &id, "--no-refresh", "--json"]).run().ok();
    assert_eq!(job_of(&v)["status"], "running");
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();
}

/// Send `signal` (e.g. `TERM`) to a running `iris` through the `kill` utility.
fn send_signal(child: &Running, signal: &str) {
    let status = std::process::Command::new("kill")
        .args([&format!("-{signal}"), &child.pid().to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -{signal} failed");
}

#[test]
fn ctrl_c_sigterm_or_sighup_during_jobs_wait_exits_130_and_leaves_the_job_running() {
    for signal in ["INT", "TERM", "HUP"] {
        let sb = Sandbox::new();
        let veo = VeoMock::start();
        let id = submit_detached(&sb, &veo, &[]);

        let child = sb
            .iris()
            .gemini(&veo.api)
            .args(["jobs", "wait", &id, "--timeout", "60s", "--poll-interval", "2s", "--json"])
            .spawn();
        // The first poll has been answered (and the handler armed) once this is printed.
        child.wait_for_stderr(&format!("Job {id} is running"), Duration::from_secs(30));
        send_signal(&child, signal);
        let out = child.finish();
        let v = out.err(130, "interrupted");
        assert_eq!(v["command"], "jobs.wait");
        assert_eq!(v["error"]["job_id"], id.as_str());
        assert_eq!(v["error"]["job_status"], "running");
        assert!(v["error"]["hint"].as_str().unwrap().contains(&format!("iris jobs wait {id}")), "{v}");
        assert!(out.elapsed < Duration::from_secs(30), "SIG{signal}: {:?}", out.elapsed);

        assert_eq!(sb.record(&id)["status"], "running", "SIG{signal} never fails the job");
        assert_eq!(veo.submits(), 1);
        veo.assert_no_credential_leaks();
    }
}

#[test]
fn sigterm_or_sighup_during_a_paid_submit_is_deferred_until_the_operation_id_is_recorded() {
    for signal in ["TERM", "HUP"] {
        let sb = Sandbox::new();
        let veo = VeoMock::start();
        // The provider takes a while to accept the submission.
        veo.submit.set(json_response(200, json!({ "name": veo.op_name })).set_delay(Duration::from_secs(2)));
        let child = sb
            .iris()
            .gemini(&veo.api)
            .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--detach", "--json"])
            .spawn();
        // The handlers are installed before this line is printed.
        child.wait_for_stderr("Submitting job", Duration::from_secs(30));
        veo.api.wait_for("POST", &veo_submit_path(VEO_LITE), 1, Duration::from_secs(30));
        send_signal(&child, signal);
        child.wait_for_stderr("Interrupt received", Duration::from_secs(30));
        let out = child.finish();
        // Exactly one envelope (checked by `err`), exit 130, and the job was recorded.
        let v = out.err(130, "interrupted");
        let error = &v["error"];
        assert_eq!(error["job_status"], "running", "SIG{signal}: {v}");
        assert_eq!(error["remote_operation_id"], veo.op_name.as_str());
        assert_eq!(error["retryable"], false, "running the command again would bill another job");
        assert_eq!(error["details"]["charge_possible"], true);
        let id = error["job_id"].as_str().unwrap();
        let rec = sb.record(id);
        assert_eq!(rec["status"], "running");
        assert_eq!(rec["remote_operation_id"], veo.op_name.as_str());
        assert_eq!(veo.submits(), 1);

        // A later process follows the job as usual.
        veo.succeed();
        let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", id, "--json"]).run().ok();
        assert_eq!(job_of(&v)["status"], "succeeded");
        assert_eq!(veo.submits(), 1);
        veo.assert_no_credential_leaks();
    }
}

// ----- scenario 8: download failure, recovery, expiry --------------------------------------------

#[test]
fn a_failed_download_keeps_the_job_succeeded_and_a_later_download_recovers() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.succeed();
    // The file host fails (quick Retry-After so the bounded retries finish fast).
    veo.file.set(
        json_response(500, json!({ "error": "backend unavailable" })).insert_header("retry-after-ms", "5"),
    );

    let out = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run();
    let v = out.err(1, "download_failed");
    let error = &v["error"];
    assert_eq!(error["category"], "artifact");
    assert_eq!(error["retryable"], true);
    assert_eq!(error["job_id"], id.as_str());
    assert_eq!(error["job_status"], "succeeded", "generation success and download failure are separate");
    assert!(error["hint"].as_str().unwrap().contains(&format!("iris jobs download {id}")), "{error}");
    let url = error["details"]["url"].as_str().expect("the failing URL is reported");
    assert!(url.contains("X-Goog-Signature=REDACTED") && !url.contains(SIGNATURE), "{url}");
    assert_eq!(veo.file_fetches(), 5, "the Download retry class makes 5 attempts");

    let rec = sb.record(&id);
    assert_eq!(rec["status"], "succeeded");
    assert_eq!(rec["outputs"][0]["download_state"], "failed");
    assert!(files_in(&sb.work()).is_empty(), "no partial file is left behind");
    assert_eq!(veo.submits(), 1, "a download failure never triggers another generation");

    // The file host recovers: a plain download finishes the job's outputs.
    veo.file.set(wiremock::ResponseTemplate::new(200).set_body_raw(veo_video(), "video/mp4"));
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert_video(&job_of(&v)["artifacts"][0], &sb.path(&format!("{id}.mp4")));
    assert_eq!(sb.record(&id)["outputs"][0]["download_state"], "downloaded");
    assert_eq!(veo.submits(), 1);
    assert_eq!(files_in(&sb.work()), [format!("{id}.mp4")]);
    // The five retried file-host fetches and the recovery carried no credential.
    veo.assert_no_credential_leaks();
}

#[test]
fn an_error_document_served_as_media_is_invalid_media_and_nothing_is_saved() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.succeed();
    veo.file.set(json_response(200, json!({ "error": { "code": 403, "message": "signature expired" } })));

    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "invalid_media");
    assert_eq!(v["error"]["job_status"], "succeeded");
    assert_eq!(sb.record(&id)["status"], "succeeded");
    assert!(files_in(&sb.work()).is_empty());
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();
}

/// Shift every `*_at` timestamp of a job record back by `days`, as if the job had
/// been submitted (and finished) that long ago (the retention clock counts from
/// `submitted_at`).
fn backdate_record(sb: &Sandbox, id: &str, days: i64) {
    let path = sb.record_path(id);
    let mut rec = sb.record(id);
    for (key, value) in rec.as_object_mut().unwrap() {
        let Some(text) = value.as_str() else { continue };
        if !key.ends_with("_at") {
            continue;
        }
        let ts: jiff::Timestamp = text.parse().unwrap();
        let shifted = ts.checked_sub(jiff::SignedDuration::from_hours(24 * days)).unwrap();
        *value = json!(shifted.strftime("%Y-%m-%dT%H:%M:%SZ").to_string());
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&rec).unwrap()).unwrap();
}

#[test]
fn a_file_host_403_is_retryable_before_the_retention_period_ends_and_expired_after() {
    // A 403 seconds after the job succeeded: a retryable download failure, the
    // output stays re-downloadable, and the next download works.
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.succeed();
    veo.file.set(json_response(403, json!({ "error": "denied" })));
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "download_failed");
    let error = &v["error"];
    assert_eq!(error["retryable"], true);
    assert_eq!(error["job_status"], "succeeded");
    assert_eq!(error["provider_status"], 403);
    assert!(error["hint"].as_str().unwrap().contains(&format!("iris jobs download {id}")), "{error}");
    assert_eq!(veo.file_fetches(), 1, "403 is never retried automatically");
    let rec = sb.record(&id);
    assert_eq!(rec["status"], "succeeded");
    assert_eq!(rec["outputs"][0]["download_state"], "failed");
    let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
    assert_eq!(v["result"]["next_steps"], json!([format!("iris jobs download {id}")]));
    assert!(warning_codes(&v).contains(&"retention_limited".to_string()), "{v}");
    veo.file.set(wiremock::ResponseTemplate::new(200).set_body_raw(veo_video(), "video/mp4"));
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert_video(&job_of(&v)["artifacts"][0], &sb.path(&format!("{id}.mp4")));
    assert!(files_in(&sb.work()).iter().all(|n| !n.contains("iris-part")), "no partial files");
    veo.assert_no_credential_leaks();

    // The job was submitted three days ago: past the 2-day retention, Iris still
    // asks (never a local short-circuit), and a 403/404 then means gone.
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.succeed();
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--no-download", "--json"]).run().ok();
    assert!(warning_codes(&v).contains(&"retention_limited".to_string()), "{v}");
    backdate_record(&sb, &id, 3);
    veo.file.set(json_response(404, json!({ "error": "not found" })));
    let v =
        sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().err(1, "artifact_expired");
    assert_eq!(v["error"]["retryable"], false);
    assert_eq!(v["error"]["job_status"], "succeeded");
    let rec = sb.record(&id);
    assert_eq!(rec["status"], "succeeded", "the job itself stays succeeded");
    assert_eq!(rec["outputs"][0]["download_state"], "expired");
    assert_eq!((veo.api_downloads(), veo.file_fetches()), (1, 1), "the download was attempted once");
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();

    // A 410 is gone at once, whatever the retention estimate says.
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    veo.succeed();
    veo.file.set(json_response(410, json!({ "error": "gone" })));
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().err(1, "artifact_expired");
    assert_eq!(v["error"]["retryable"], false);
    assert_eq!(sb.record(&id)["outputs"][0]["download_state"], "expired");
    veo.assert_no_credential_leaks();
}

// ----- scenario 9: concurrent waits --------------------------------------------------------------

#[test]
fn two_concurrent_waits_download_the_output_once() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let id = submit_detached(&sb, &veo, &[]);
    // The file host holds the first download until the other waiter has reached the
    // download lock, so the two downloads overlap however the processes are
    // scheduled (the limit only bounds a failing run).
    let gate = veo.hold_file(
        wiremock::ResponseTemplate::new(200).set_body_raw(veo_video(), "video/mp4"),
        Duration::from_secs(30),
    );

    let wait = || {
        sb.iris()
            .gemini(&veo.api)
            .args(["jobs", "wait", &id, "--timeout", "60s", "--poll-interval", "2s", "--json"])
            .spawn()
    };
    let (a, b) = (wait(), wait());
    // Each process has polled once and is waiting; then the provider finishes.
    let running = format!("Job {id} is running");
    a.wait_for_stderr(&running, Duration::from_secs(30));
    b.wait_for_stderr(&running, Duration::from_secs(30));
    veo.succeed();
    // One process downloads (held by the file host); the other reaches the lock and
    // waits for it. Only then does the file host answer.
    let waited = format!("Waiting for another iris process that is downloading job {id}");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !(a.stderr_has(&waited) || b.stderr_has(&waited)) {
        assert!(std::time::Instant::now() < deadline, "neither process waited for the other's download lock");
        std::thread::sleep(Duration::from_millis(20));
    }
    gate.open();
    let (a, b) = (a.finish(), b.finish());
    let (va, vb) = (a.ok(), b.ok());

    let target = sb.path(&format!("{id}.mp4"));
    for v in [&va, &vb] {
        assert_eq!(job_of(v)["status"], "succeeded");
        assert_video(&job_of(v)["artifacts"][0], &target);
    }
    assert_eq!(veo.file_fetches(), 1, "one download; the other process found it done");
    let repeats =
        [&va, &vb].iter().filter(|v| warning_codes(v).contains(&"already_downloaded".to_string())).count();
    assert_eq!(repeats, 1, "exactly one waiter reports already_downloaded");
    let waited = format!("Waiting for another iris process that is downloading job {id}");
    assert!(
        a.stderr.contains(&waited) || b.stderr.contains(&waited),
        "the downloads overlapped and one process waited for the other's lock:\n{}\n{}",
        a.stderr,
        b.stderr
    );

    assert_eq!(files_in(&sb.work()), [format!("{id}.mp4")], "one final file, no temp leftovers");
    for name in files_in(&sb.jobs_dir()) {
        assert!(
            [format!("{id}.json"), format!("{id}.lock"), format!("{id}.download.lock")].contains(&name),
            "leftover in the jobs directory: {name}"
        );
    }
    let rec = sb.record(&id);
    assert_eq!(rec["status"], "succeeded");
    assert_eq!(rec["outputs"][0]["download_state"], "downloaded");
    assert_eq!(rec["outputs"][0]["sha256"], sha256_hex(&veo_video()));
    assert_eq!(veo.submits(), 1);
    veo.assert_no_credential_leaks();
}

// ----- scenario 10 (video): secret hygiene with -vv ---------------------------------------------

#[test]
fn verbose_video_runs_never_reveal_the_key_or_signed_urls() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let mut outputs = Vec::new();

    let out = sb
        .iris()
        .gemini(&veo.api)
        .env("OPENAI_API_KEY", OPENAI_KEY)
        .args(["-vv", "video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--detach", "--json"])
        .run();
    let id = job_of(&out.ok())["job_id"].as_str().unwrap().to_string();
    outputs.push(out);
    outputs.push(sb.iris().gemini(&veo.api).args(["-vv", "jobs", "status", &id, "--json"]).run());
    veo.succeed();
    let out = sb.iris().gemini(&veo.api).args(["-vv", "jobs", "wait", &id, "--json"]).run();
    out.ok();
    // The redirect hop to the signed URL is logged, redacted.
    assert!(out.stderr.contains("X-Goog-Signature=REDACTED"), "{}", out.stderr);
    outputs.push(out);
    outputs.push(sb.iris().gemini(&veo.api).args(["-vv", "jobs", "list"]).run());
    outputs.push(sb.iris().gemini(&veo.api).args(["-vv", "jobs", "status", &id]).run());

    for out in &outputs {
        assert_eq!(out.code, 0, "{out:?}");
        out.assert_hygiene();
        for text in [&out.stdout, &out.stderr] {
            assert!(!text.contains(SIGNATURE), "signed URL value printed: {text}");
            assert!(!text.contains(PROMPT), "prompt printed: {text}");
            assert_printed_urls_redacted(text);
        }
    }
    for dir in [sb.state(), sb.work(), sb.home()] {
        assert_no_file_contains(&dir, &[GEMINI_KEY, OPENAI_KEY, SIGNATURE]);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&sb.jobs_dir()), 0o700, "jobs directory is private");
        assert_eq!(mode(&sb.record_path(&id)), 0o600, "job records are private");
    }
    veo.assert_no_credential_leaks();
}
