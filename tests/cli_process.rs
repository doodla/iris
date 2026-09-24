//! Process-level tests of the built `iris` binary (assert_cmd): real argv, real
//! environment handling, exit codes, the JSON envelope, and recovery of jobs
//! created by an earlier process (records written through the jobs API; files
//! served by a local wiremock server).
//!
//! Every command runs with a temporary HOME and IRIS_STATE_DIR, fake keys set
//! through `Command::env` only when needed, real keys removed, proxies disabled,
//! and provider base URLs pointing at an unused 127.0.0.1 port, so a request that
//! should not happen fails loudly instead of reaching a paid API.

#[path = "app_support.rs"]
mod support;

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use assert_cmd::Command;
use iris::catalog::ModelSpec;
use iris::domain::{JobStatus, Operation, ProviderId};
use iris::error::{ErrorCode, IrisError};
use iris::jobs::{JobId, JobRecord, JobStore, NewJob, OutputPlan, PromptRecord};
use iris::providers::{RemoteStatus, SubmittedOperation};
use serde_json::Value;
use support::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BIN: &str = env!("CARGO_BIN_EXE_iris");

/// Variables that must not leak from the developer's environment into a test.
const SCRUBBED: &[&str] = &[
    "OPENAI_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "IRIS_CONFIG",
    "IRIS_OUTPUT_DIR",
    "IRIS_STATE_DIR",
    "IRIS_IMAGE_PROVIDER",
    "IRIS_WAIT_TIMEOUT",
    "IRIS_POLL_INTERVAL",
    "IRIS_STORE_PROMPTS",
    "IRIS_OPENAI_BASE_URL",
    "IRIS_GEMINI_BASE_URL",
    "IRIS_LOG",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
];

/// A port nothing listens on (bound, then released).
fn unused_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn configure(cmd: &mut std::process::Command, sandbox: &Sandbox) {
    for var in SCRUBBED {
        cmd.env_remove(var);
    }
    let port = unused_port();
    cmd.current_dir(sandbox.work())
        .env("HOME", sandbox.home())
        .env("IRIS_STATE_DIR", sandbox.state())
        .env("NO_PROXY", "127.0.0.1,localhost")
        .env("no_proxy", "127.0.0.1,localhost")
        .env("IRIS_OPENAI_BASE_URL", format!("http://127.0.0.1:{port}/v1"))
        .env("IRIS_GEMINI_BASE_URL", format!("http://127.0.0.1:{port}"));
}

/// `iris` in the sandbox with no credentials and unreachable providers.
fn iris(sandbox: &Sandbox) -> Command {
    let mut std_cmd = std::process::Command::new(BIN);
    configure(&mut std_cmd, sandbox);
    let mut cmd = Command::from_std(std_cmd);
    cmd.timeout(Duration::from_secs(60));
    cmd
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    /// The single JSON envelope on stdout, validated against the committed schema.
    fn json(&self) -> Value {
        let lines: Vec<&str> = self.stdout.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "expected one JSON line on stdout:\n{}\nstderr:\n{}",
            self.stdout,
            self.stderr
        );
        let v: Value = serde_json::from_str(lines[0]).unwrap();
        assert_matches_schema(&v);
        v
    }

    fn error_code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap().to_string()
    }
}

fn run(cmd: &mut Command) -> Out {
    let output = cmd.output().unwrap();
    Out {
        code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

// ----- job records written as an earlier process would ---------------------------------

fn new_job(sandbox: &Sandbox) -> NewJob {
    NewJob {
        provider: ProviderId::Gemini,
        model: "veo-test-model".into(),
        operation: Operation::VideoGenerate,
        request: serde_json::Map::new(),
        prompt: PromptRecord::new("a lighthouse", false),
        output_plan: OutputPlan::new(Some(&sandbox.work().join("videos")), None, false).unwrap(),
        cost_estimate: None,
    }
}

fn store(sandbox: &Sandbox) -> JobStore {
    JobStore::new(sandbox.state())
}

fn create_running(sandbox: &Sandbox) -> JobId {
    create_running_at(sandbox, iris::jobs::now())
}

/// A job created and submitted at `at`.
fn create_running_at(sandbox: &Sandbox, at: jiff::Timestamp) -> JobId {
    let mut rec = JobRecord::new(new_job(sandbox), at).unwrap();
    rec.mark_submitted(
        &SubmittedOperation {
            remote_id: "models/veo-test-model/operations/op1".into(),
            provider_request_id: None,
        },
        at,
    )
    .unwrap();
    store(sandbox).create(&rec).unwrap();
    rec.job_id().clone()
}

/// A job submitted and finished at `at`.
fn create_succeeded(sandbox: &Sandbox, uri: &str, retention: Duration, at: jiff::Timestamp) -> JobId {
    let id = create_running_at(sandbox, at);
    store(sandbox).update(&id, |r| r.apply_poll(remote_success(uri), Some(retention), at)).unwrap();
    id
}

fn create_failed(sandbox: &Sandbox) -> JobId {
    let id = create_running(sandbox);
    let error = IrisError::new(ErrorCode::RemoteJobFailed, "the provider reported an internal failure");
    store(sandbox)
        .update(&id, |r| r.apply_poll(RemoteStatus::Failed { error }, None, iris::jobs::now()))
        .unwrap();
    id
}

async fn video_server(expected_fetches: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/abc:download"))
        .and(header("x-goog-api-key", GEMINI_KEY))
        .respond_with(
            ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(mp4(4)),
        )
        .expect(expected_fetches)
        .mount(&server)
        .await;
    server
}

// ----- basics ------------------------------------------------------------------------------

#[test]
fn version_includes_the_target_triple() {
    let sandbox = Sandbox::new();
    let out = run(iris(&sandbox).arg("--version"));
    assert_eq!(out.code, 0);
    let v = run(iris(&sandbox).args(["version", "--json"])).json();
    let target = v["result"]["target"].as_str().unwrap().to_string();
    assert!(target.contains('-') && target != "unknown", "{target}");
    assert_eq!(out.stdout, format!("iris {} ({target})\n", env!("CARGO_PKG_VERSION")));
    assert_eq!(v["result"]["version"], env!("CARGO_PKG_VERSION"));
    let human = run(iris(&sandbox).arg("version"));
    assert_eq!(human.stdout, format!("iris {} ({target}; JSON schema v1)\n", env!("CARGO_PKG_VERSION")));
}

/// `version.git_commit` is the commit named at build time through
/// `IRIS_GIT_COMMIT`, lowercased, when it is 7 to 40 hex digits, and null
/// otherwise (unset, empty or invalid, as in a plain local build). `GITHUB_SHA`
/// never counts: it names the commit of whichever repository's workflow is
/// running, so a build with only `GITHUB_SHA` set (a GitHub Actions job that
/// does not set `IRIS_GIT_COMMIT`) must report null. This test is compiled in
/// the same cargo run as the binary, so it sees the same variables; the rule is
/// restated here rather than read back from build.rs.
#[test]
fn version_reports_the_git_commit_named_at_build_time() {
    let named = option_env!("IRIS_GIT_COMMIT");
    let expected = named
        .filter(|value| (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|value| Value::from(value.to_ascii_lowercase()))
        .unwrap_or(Value::Null);
    let sandbox = Sandbox::new();
    let out = run(iris(&sandbox).args(["version", "--json"]));
    assert_eq!(out.code, 0);
    assert_eq!(
        out.json()["result"]["git_commit"],
        expected,
        "built with IRIS_GIT_COMMIT={named:?}, GITHUB_SHA={:?}",
        option_env!("GITHUB_SHA")
    );
}

#[test]
fn every_command_has_help_with_examples_and_the_top_level_notes_billing() {
    let sandbox = Sandbox::new();
    let top = run(iris(&sandbox).arg("--help"));
    assert_eq!(top.code, 0);
    for needle in [
        "Examples:",
        "billed by the provider",
        "OPENAI_API_KEY",
        "GEMINI_API_KEY",
        "Exit codes:",
        "image ",
        "video ",
        "jobs ",
        "models ",
        "providers ",
        "config ",
        "doctor ",
        "schema ",
        "completions ",
        "version ",
    ] {
        assert!(top.stdout.contains(needle), "top-level help lacks {needle:?}:\n{}", top.stdout);
    }
    // Exit 2 also covers a provider's outright rejection, so the help must not claim that
    // nothing was sent; `error.provider_status` tells the two apart.
    let words = top.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        words.contains(
            "2 invalid request: fix it before retrying (error.provider_status null means nothing was sent; \
             otherwise the provider rejected it)"
        ),
        "{}",
        top.stdout
    );
    let leaves: &[&[&str]] = &[
        &["image", "generate"],
        &["image", "edit"],
        &["video", "generate"],
        &["jobs", "list"],
        &["jobs", "status"],
        &["jobs", "wait"],
        &["jobs", "download"],
        &["jobs", "delete"],
        &["models", "list"],
        &["models", "show"],
        &["providers", "list"],
        &["config", "show"],
        &["config", "path"],
        &["doctor"],
        &["schema"],
        &["completions"],
        &["version"],
    ];
    for leaf in leaves {
        let out = run(iris(&sandbox).args(*leaf).arg("--help"));
        assert_eq!(out.code, 0, "{leaf:?}");
        assert!(out.stdout.contains("Examples:\n  iris "), "{leaf:?} help has no examples:\n{}", out.stdout);
        // Every command has a long_about: --help says more than -h.
        let short = run(iris(&sandbox).args(*leaf).arg("-h"));
        assert_eq!(short.code, 0, "{leaf:?}");
        assert_ne!(short.stdout, out.stdout, "{leaf:?}: -h and --help are identical (no long_about)");
    }
    for group in ["image", "video", "jobs", "models", "providers", "config"] {
        let out = run(iris(&sandbox).args([group, "--help"]));
        assert_eq!(out.code, 0, "{group}");
        assert!(out.stdout.contains("Commands:"), "{group}");
        assert!(out.stdout.contains("Examples:\n  iris "), "{group} help has no examples:\n{}", out.stdout);
        assert_ne!(run(iris(&sandbox).args([group, "-h"])).stdout, out.stdout, "{group}");
    }
    let gen_help = run(iris(&sandbox).args(["image", "generate", "--help"])).stdout;
    for flag in [
        "--prompt-file",
        "--prompt-stdin",
        "--capabilities-from",
        "--option <KEY=VALUE>",
        "--dry-run",
        "--overwrite",
        "--out-dir",
    ] {
        assert!(gen_help.contains(flag), "{flag}");
    }
    let json = run(iris(&sandbox).args(["jobs", "wait", "--help", "--json"])).json();
    assert!(json["result"]["help"].as_str().unwrap().contains("Examples:"));
}

#[test]
fn clap_errors_are_json_envelopes_with_exit_2_and_nothing_on_stderr() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["image", "generate", "x", "--bogus", "--json"],
        vec!["image", "generate", "x", "-o", "a.png", "-d", "out", "--json"],
        vec!["video", "generate", "x", "--no-audio", "--json"],
        vec!["image", "generate", "x", "--seed", "7", "--json"],
        vec!["video", "generate", "x", "--detach", "--timeout", "5m", "--json"],
        vec!["jobs", "delete", "--json"],
        vec!["jobs", "wait", "job_00000000000000000000000000", "--no-download", "-o", "x.mp4", "--json"],
        vec!["completions", "tcsh", "--json"],
        vec!["--json"],
    ] {
        let out = run(iris(&sandbox).args(&args));
        assert_eq!(out.code, 2, "{args:?}: {}", out.stdout);
        let v = out.json();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "usage_error", "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}: {}", out.stderr);
    }
    let v = run(iris(&sandbox).args(["image", "edit", "x", "--json"])).json();
    assert_eq!(v["command"], "image.edit");
    // Missing-argument messages name the argument (clap spreads them over lines).
    for (args, missing) in [
        (&["jobs", "status", "--json"][..], "<JOB_ID>"),
        (&["jobs", "download", "--json"], "<JOB_ID>"),
        (&["models", "show", "--json"], "<MODEL>"),
        (&["image", "edit", "x", "--json"], "--image <PATH>"),
    ] {
        let v = run(iris(&sandbox).args(args)).json();
        let message = v["error"]["message"].as_str().unwrap();
        assert_eq!(
            message,
            format!("the following required arguments were not provided: {missing}"),
            "{args:?}"
        );
    }
    let v = run(iris(&sandbox).args(["bogus", "--json"])).json();
    assert!(v["command"].is_null());
}

#[test]
fn prompt_sources_are_validated_before_anything_else() {
    let sandbox = Sandbox::new();
    let out = run(iris(&sandbox).args(["image", "generate", "--json"]));
    assert_eq!(out.code, 2);
    let v = out.json();
    assert_eq!(v["error"]["code"], "usage_error");
    assert_eq!(v["error"]["message"], "a prompt is required (PROMPT, --prompt-file, or --prompt-stdin)");

    let file = sandbox.path("p.txt");
    std::fs::write(&file, "from a file\n").unwrap();
    let out =
        run(iris(&sandbox).args(["video", "generate", "x", "--prompt-stdin", "--json"]).write_stdin("y"));
    assert_eq!(out.error_code(), "usage_error");
    let out =
        run(iris(&sandbox).args(["image", "edit", "-i", "a.png", "-f", "p.txt", "--prompt-stdin", "--json"]));
    assert_eq!(out.error_code(), "usage_error");

    let out =
        run(iris(&sandbox).args(["image", "generate", "--prompt-stdin", "--json"]).write_stdin(" \n\n"));
    assert_eq!(out.error_code(), "invalid_argument");
    let out = run(iris(&sandbox)
        .args(["image", "generate", "--prompt-stdin", "--json"])
        .write_stdin(vec![0xffu8, 0xfe]));
    assert_eq!(out.error_code(), "invalid_argument");
    let out = run(iris(&sandbox).args(["image", "generate", "-f", "missing.txt", "--json"]));
    assert_eq!(out.error_code(), "input_file_invalid");

    use std::os::unix::ffi::OsStrExt;
    let out = run(iris(&sandbox)
        .args(["image", "generate", "--json"])
        .arg(std::ffi::OsStr::from_bytes(b"\xff\xfe")));
    assert_eq!(out.error_code(), "invalid_argument");

    // A valid prompt gets past prompt validation (what follows depends on the catalog).
    let out =
        run(iris(&sandbox).args(["image", "generate", "-f", "p.txt", "--model", "no-such-model", "--json"]));
    assert_eq!(out.error_code(), "unknown_model");
    let out = run(iris(&sandbox)
        .args(["video", "generate", "--prompt-stdin", "-m", "nope", "--json"])
        .write_stdin("x"));
    assert_eq!(out.error_code(), "unknown_model");
}

#[test]
fn schema_output_is_the_committed_file_byte_for_byte() {
    let sandbox = Sandbox::new();
    let out = run(iris(&sandbox).arg("schema"));
    assert_eq!(out.code, 0);
    let committed =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/schema/iris-output.v1.schema.json"))
            .unwrap();
    assert!(out.stdout == committed, "`iris schema` differs from schema/iris-output.v1.schema.json");
    let v = run(iris(&sandbox).args(["schema", "--json"])).json();
    assert_eq!(&v["result"]["schema"], committed_schema());
}

#[test]
fn completions_are_generated_for_each_supported_shell() {
    let sandbox = Sandbox::new();
    for (shell, needle) in
        [("bash", "_iris()"), ("zsh", "#compdef iris"), ("fish", "complete -c iris"), ("elvish", "iris")]
    {
        let out = run(iris(&sandbox).args(["completions", shell]));
        assert_eq!(out.code, 0, "{shell}");
        assert!(out.stdout.contains(needle), "{shell}: {}", &out.stdout[..out.stdout.len().min(200)]);
        let v = run(iris(&sandbox).args(["completions", shell, "--json"])).json();
        assert_eq!(v["result"]["shell"], shell);
        for removed in ["seed", "no-audio"] {
            assert!(!out.stdout.contains(removed), "{shell} completes the removed --{removed}");
        }
    }
    // Each command completes only its own typed flags.
    let fish = run(iris(&sandbox).args(["completions", "fish"])).stdout;
    let lines = |command: &str| -> Vec<String> {
        fish.lines().filter(|l| l.contains(command)).map(str::to_string).collect()
    };
    let image = lines("__fish_iris_using_subcommand image; and __fish_seen_subcommand_from generate");
    let video = lines("__fish_iris_using_subcommand video; and __fish_seen_subcommand_from generate");
    assert!(
        image.iter().any(|l| l.contains("-l quality")) && !image.iter().any(|l| l.contains("-l duration"))
    );
    assert!(
        video.iter().any(|l| l.contains("-l duration")) && !video.iter().any(|l| l.contains("-l quality"))
    );
}

#[test]
fn providers_config_and_doctor_report_presence_but_never_key_values() {
    let sandbox = Sandbox::new();
    let out = run(iris(&sandbox).args(["providers", "list", "--json"]).env("OPENAI_API_KEY", OPENAI_KEY));
    assert_eq!(out.code, 0);
    let v = out.json();
    let providers = v["result"]["providers"].as_array().unwrap();
    let find = |id: &str| providers.iter().find(|p| p["id"] == id).unwrap().clone();
    assert_eq!(find("openai")["credential_present"], true);
    assert_eq!(find("gemini")["credential_present"], false);
    assert_eq!(find("gemini")["credential_env"], "GEMINI_API_KEY");
    assert!(!out.stdout.contains(OPENAI_KEY) && !out.stderr.contains(OPENAI_KEY));

    let out = run(iris(&sandbox).args(["config", "show", "--json"]).env("GEMINI_API_KEY", GEMINI_KEY));
    let v = out.json();
    let warnings = v["warnings"].as_array().unwrap();
    assert_eq!(warnings.iter().filter(|w| w["code"] == "non_default_base_url").count(), 2, "{v}");
    let creds = v["result"]["credentials"].as_array().unwrap();
    assert!(creds.iter().any(|c| c["env"] == "GEMINI_API_KEY" && c["present"] == true));
    assert!(!out.stdout.contains(GEMINI_KEY));
    let human = run(iris(&sandbox).args(["config", "show"]));
    assert!(human.stderr.contains("warning[non_default_base_url]"), "{}", human.stderr);

    let v = run(iris(&sandbox).args(["config", "path", "--json"])).json();
    assert_eq!(v["result"]["state_dir"], sandbox.state().to_str().unwrap());
    assert_eq!(
        v["result"]["config_file"],
        sandbox.home().join(".config/iris/config.toml").to_str().unwrap(),
        "Linux default without XDG_CONFIG_HOME"
    );

    let out =
        run(iris(&sandbox).args(["doctor", "--json"]).env("GOOGLE_API_KEY", "google-key-should-be-ignored"));
    assert_eq!(out.code, 0);
    let v = out.json();
    let checks = v["result"]["checks"].as_array().unwrap();
    let status =
        |id: &str| checks.iter().find(|c| c["id"] == id).map(|c| c["status"].as_str().unwrap().to_string());
    assert_eq!(status("credentials.google_api_key").as_deref(), Some("warning"));
    assert_eq!(status("credentials.openai").as_deref(), Some("warning"));
    assert_eq!(status("base_url.gemini").as_deref(), Some("warning"));
    assert_eq!(status("state_dir").as_deref(), Some("ok"));
    assert!(!out.stdout.contains("google-key-should-be-ignored"));

    // An explicitly requested config file that does not exist is config_invalid.
    let out =
        run(iris(&sandbox).args(["jobs", "list", "--json"]).env("IRIS_CONFIG", sandbox.path("nope.toml")));
    assert_eq!(out.code, 2);
    assert_eq!(out.error_code(), "config_invalid");
}

#[test]
fn models_list_is_consistent_with_the_catalog() {
    let sandbox = Sandbox::new();
    let v = run(iris(&sandbox).args(["models", "list", "--json"])).json();
    let ids: Vec<&str> =
        v["result"]["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    let expected: Vec<&str> = iris::catalog::all().map(|m| m.id).collect();
    assert_eq!(ids, expected);
    let out = run(iris(&sandbox).args(["models", "show", "definitely-not-a-model", "--json"]));
    assert_eq!(out.code, 2);
    assert_eq!(out.error_code(), "unknown_model");
    let out = run(iris(&sandbox).args(["models", "list", "--provider", "acme", "--json"]));
    assert_eq!(out.error_code(), "unknown_provider");
    let out = run(iris(&sandbox).args(["models", "list", "--operation", "audio.generate", "--json"]));
    assert_eq!(out.error_code(), "invalid_argument");
    if let Some(spec) = iris::catalog::all().next() {
        let v = run(iris(&sandbox).args(["models", "show", spec.id, "--json"])).json();
        assert_eq!(v["result"]["model"]["id"], spec.id);
        assert_eq!(v["result"]["model"]["access"]["account_access"], "not_checked");
    }
    // Input rules are machine-readable: mask requirements and the inline request cap.
    for spec in iris::catalog::all() {
        let v = run(iris(&sandbox).args(["models", "show", spec.id, "--json"])).json();
        let inputs = &v["result"]["model"]["inputs"];
        assert_eq!(inputs["mask"], spec.inputs.mask.is_some(), "{}", spec.id);
        match spec.inputs.mask {
            Some(mask) => {
                let m = &inputs["mask_requirements"];
                assert_eq!(m["media_types"], serde_json::json!(mask.media_types), "{}", spec.id);
                assert_eq!(m["max_bytes"], mask.max_bytes);
                assert_eq!(m["alpha_channel_required"], mask.requires_alpha);
                assert_eq!(m["same_size_as_first_image"], mask.same_size_as_first_image);
            }
            None => assert!(inputs["mask_requirements"].is_null(), "{}", spec.id),
        }
        assert_eq!(
            inputs["max_request_bytes"],
            serde_json::json!(spec.inputs.max_request.map(|l| l.max_bytes))
        );
    }
}

// ----- jobs from an earlier process --------------------------------------------------------

#[test]
fn jobs_list_status_delete_on_records_from_the_jobs_api() {
    let sandbox = Sandbox::new();
    let running = create_running(&sandbox);
    std::thread::sleep(Duration::from_millis(1100));
    let failed = create_failed(&sandbox);
    std::fs::write(sandbox.state().join("jobs").join("job_0000000000000000000000000z.json"), b"{ not json")
        .unwrap();

    let out = run(iris(&sandbox).args(["jobs", "list", "--json"]));
    assert_eq!(out.code, 0);
    let v = out.json();
    let ids: Vec<&str> =
        v["result"]["jobs"].as_array().unwrap().iter().map(|j| j["job_id"].as_str().unwrap()).collect();
    assert_eq!(ids, [failed.as_str(), running.as_str()], "newest first");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "job_record_unreadable"));
    let v = run(iris(&sandbox).args(["jobs", "list", "--status", "failed", "--json"])).json();
    assert_eq!(v["result"]["jobs"].as_array().unwrap().len(), 1);

    let v = run(iris(&sandbox).args(["jobs", "status", running.as_str(), "--no-refresh", "--json"])).json();
    assert_eq!(v["result"]["job"]["status"], "running");
    assert!(v["result"]["job"]["last_checked_at"].is_null(), "no poll happened");
    assert_eq!(v["result"]["next_steps"][1], format!("iris jobs wait {running}"));
    assert!(!serde_json::to_string(&v).unwrap().contains("lighthouse"), "prompt text is never shown");

    // A refresh that cannot reach the provider keeps the last known status.
    let out = run(iris(&sandbox)
        .args(["jobs", "status", running.as_str(), "--json"])
        .env("GEMINI_API_KEY", GEMINI_KEY));
    assert_eq!(out.code, 0);
    let v = out.json();
    assert_eq!(v["result"]["job"]["status"], "running");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "status_refresh_failed"), "{v}");
    assert_eq!(store(&sandbox).load(&running).unwrap().status(), JobStatus::Running);

    let out = run(iris(&sandbox).args(["jobs", "download", running.as_str(), "--json"]));
    assert_eq!(out.code, 4);
    assert_eq!(out.error_code(), "job_not_ready");
    let out = run(iris(&sandbox).args(["jobs", "wait", failed.as_str(), "--json"]));
    assert_eq!(out.code, 1);
    let v = out.json();
    assert_eq!(v["error"]["code"], "remote_job_failed");
    assert_eq!(v["error"]["job_status"], "failed");
    let out = run(iris(&sandbox).args(["jobs", "wait", running.as_str(), "--json"]));
    assert_eq!(out.code, 3, "polling needs the credential: {}", out.stdout);
    assert_eq!(out.error_code(), "missing_credentials");

    let out = run(iris(&sandbox).args(["jobs", "delete", running.as_str(), "--json"]));
    assert_eq!(out.code, 2);
    let v = out.json();
    assert_eq!(v["error"]["job_status"], "running");
    let out = run(iris(&sandbox).args(["jobs", "delete", running.as_str(), failed.as_str(), "--force"]));
    assert_eq!(out.code, 0);
    assert!(out.stdout.contains(&format!("Deleted {running}")) && out.stdout.contains("Local records only"));
    let out = run(iris(&sandbox).args(["jobs", "status", failed.as_str(), "--json"]));
    assert_eq!(out.error_code(), "job_not_found");
    let out = run(iris(&sandbox).args(["jobs", "status", "../../etc/passwd", "--json"]));
    assert_eq!(out.error_code(), "invalid_argument");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_process_downloads_a_finished_job_and_repeats_safely() {
    let sandbox = Sandbox::new();
    let server = video_server(1).await;
    let uri = format!("{}/v1beta/files/abc:download", server.uri());
    let id = create_succeeded(&sandbox, &uri, Duration::from_secs(48 * 3600), iris::jobs::now());
    let base = server.uri();
    let with_server = |cmd: &mut Command| {
        cmd.env("IRIS_GEMINI_BASE_URL", &base).env("GEMINI_API_KEY", GEMINI_KEY);
    };

    let out = tokio::task::block_in_place(|| {
        let mut cmd = iris(&sandbox);
        with_server(&mut cmd);
        run(cmd.args(["jobs", "wait", id.as_str(), "--json"]))
    });
    assert_eq!(out.code, 0, "{}\n{}", out.stdout, out.stderr);
    let v = out.json();
    let art = &v["result"]["job"]["artifacts"][0];
    let expected = sandbox.work().join("videos").join(format!("{id}.mp4"));
    assert_eq!(art["path"], expected.to_str().unwrap(), "the recorded output plan applies");
    assert_eq!(art["media_type"], "video/mp4");
    assert_eq!(art["duration_seconds"], 4.0);
    assert_eq!(std::fs::read(&expected).unwrap(), mp4(4));
    assert!(out.stderr.contains("Downloading output 0"), "progress on stderr: {}", out.stderr);
    assert!(!out.stdout.contains(GEMINI_KEY) && !out.stderr.contains(GEMINI_KEY));

    // Repeats need no network (the mock expects exactly one fetch).
    let out = tokio::task::block_in_place(|| {
        let mut cmd = iris(&sandbox);
        with_server(&mut cmd);
        run(cmd.args(["jobs", "download", id.as_str(), "--json"]))
    });
    let v = out.json();
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "already_downloaded"), "{v}");
    let out = tokio::task::block_in_place(|| {
        let mut cmd = iris(&sandbox);
        with_server(&mut cmd);
        run(cmd.args(["-q", "jobs", "download", id.as_str(), "-o", "copy.mp4"]))
    });
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert_eq!(out.stdout, format!("Saved {}\n", sandbox.path("copy.mp4").display()));
    assert!(!out.stderr.contains("Copying") && !out.stderr.contains("Downloading"), "-q: {}", out.stderr);
    assert_eq!(std::fs::read(sandbox.path("copy.mp4")).unwrap(), mp4(4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_404_means_artifact_expired_only_after_the_retention_period() {
    let sandbox = Sandbox::new();
    let server = MockServer::start().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).expect(2).mount(&server).await;
    let uri = format!("{}/v1beta/files/abc:download", server.uri());
    let recent = create_succeeded(&sandbox, &uri, Duration::from_secs(48 * 3600), iris::jobs::now());
    let past = jiff::Timestamp::from_second(iris::jobs::now().as_second() - 3 * 86400).unwrap();
    let retention_passed = create_succeeded(&sandbox, &uri, Duration::from_secs(48 * 3600), past);

    for (id, code, state) in [
        (&recent, "download_failed", iris::domain::DownloadState::Failed),
        (&retention_passed, "artifact_expired", iris::domain::DownloadState::Expired),
    ] {
        let out = tokio::task::block_in_place(|| {
            run(iris(&sandbox)
                .args(["jobs", "download", id.as_str(), "--json"])
                .env("IRIS_GEMINI_BASE_URL", server.uri())
                .env("GEMINI_API_KEY", GEMINI_KEY))
        });
        assert_eq!(out.code, 1, "{}", out.stdout);
        let v = out.json();
        assert_eq!(v["error"]["code"], code);
        assert_eq!(v["error"]["retryable"], code == "download_failed");
        assert_eq!(v["error"]["job_status"], "succeeded");
        let rec = store(&sandbox).load(id).unwrap();
        assert_eq!(rec.status(), JobStatus::Succeeded, "the job itself stays succeeded");
        assert_eq!(rec.outputs()[0].download_state, state);
    }
}

#[test]
fn ctrl_c_stops_waiting_with_exit_130_and_leaves_the_job_untouched() {
    let sandbox = Sandbox::new();
    // A job still being submitted by another process: waiting needs no network.
    let rec = JobRecord::new(new_job(&sandbox), iris::jobs::now()).unwrap();
    store(&sandbox).create(&rec).unwrap();
    let id = rec.job_id().clone();

    let mut cmd = std::process::Command::new(BIN);
    configure(&mut cmd, &sandbox);
    cmd.args(["jobs", "wait", id.as_str(), "--timeout", "60s", "--poll-interval", "2s", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let started = Instant::now();
    loop {
        let line = lines.next().expect("iris exited early").unwrap();
        if line.contains("has no operation id yet") {
            // It says when the record will be reported as submission_unknown.
            assert!(line.contains("becomes submission_unknown at about 20"), "{line}");
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "no progress line");
    }
    let status = std::process::Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(status.success());
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_matches_schema(&v);
    assert_eq!(v["error"]["code"], "interrupted");
    assert_eq!(v["error"]["job_id"], id.as_str());
    assert!(v["error"]["hint"].as_str().unwrap().contains("iris jobs wait"));
    assert_eq!(store(&sandbox).load(&id).unwrap().status(), JobStatus::Submitting, "the record is untouched");
}

// ----- generation against the built-in catalog (no network) --------------------------------

/// The typed option flags of the command that runs `op`.
fn typed_flags(op: Operation) -> &'static [(&'static str, &'static str)] {
    match op {
        Operation::VideoGenerate => iris::cli::args::VIDEO_FLAGS,
        _ => iris::cli::args::IMAGE_FLAGS,
    }
}

fn builtin_default(provider: ProviderId, op: Operation) -> Option<&'static ModelSpec> {
    let spec = iris::catalog::default_model(provider, op);
    if spec.is_none() {
        eprintln!("skipped: the built-in catalog has no default {provider} model for {op} in this build");
    }
    spec
}

/// A typed flag of `op`'s command that `spec` does not declare, if there is one.
fn undeclared_flag(spec: &ModelSpec, op: Operation) -> Option<&'static str> {
    typed_flags(op)
        .iter()
        .find(|(_, name)| !spec.options_for(op).any(|o| o.name == *name))
        .map(|(flag, _)| *flag)
}

/// No command offers a typed flag that no built-in model accepts for its operation:
/// such a flag could only ever fail (options for future models use `-O name=value`).
#[test]
fn every_typed_flag_is_declared_by_some_model_for_its_command() {
    for op in Operation::ALL {
        for (flag, name) in typed_flags(*op) {
            let declared = iris::catalog::all()
                .filter(|m| m.supports(*op))
                .any(|m| m.options_for(*op).any(|o| o.name == *name && o.flag == Some(*flag)));
            assert!(declared, "{flag} ({name}) is offered for {op} but no model declares it");
        }
    }
    let sandbox = Sandbox::new();
    for (words, absent) in [
        (
            &["image", "generate", "--help"][..],
            &["--seed", "--audio", "--no-audio", "--duration", "--negative-prompt"][..],
        ),
        (
            &["image", "edit", "--help"],
            &["--seed", "--audio", "--no-audio", "--duration", "--negative-prompt"],
        ),
        (
            &["video", "generate", "--help"],
            &["--seed", "--audio", "--no-audio", "--size", "--quality", "--format"],
        ),
    ] {
        let help = run(iris(&sandbox).args(words)).stdout;
        for (flag, _) in
            typed_flags(if words[0] == "video" { Operation::VideoGenerate } else { Operation::ImageEdit })
        {
            assert!(help.contains(flag), "{words:?} lacks {flag}");
        }
        for flag in absent {
            assert!(!help.contains(&format!("{flag} ")), "{words:?} offers {flag}:\n{help}");
        }
    }
}

fn assert_nothing_sent(out: &Out, code: &str, exit: i32) {
    assert_eq!(out.code, exit, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(out.error_code(), code);
}

#[test]
fn image_generation_is_validated_locally_before_any_request() {
    let Some(spec) = builtin_default(ProviderId::OpenAi, Operation::ImageGenerate) else { return };
    let sandbox = Sandbox::new();
    let op = Operation::ImageGenerate;

    let out = run(iris(&sandbox).args(["image", "generate", "a fox", "--dry-run", "--json"]));
    assert_eq!(out.code, 0, "{}", out.stdout);
    let v = out.json();
    assert_eq!(v["result"]["dry_run"], true);
    assert_eq!(v["result"]["model"], spec.id);
    assert_eq!(v["result"]["credential_present"], false);
    assert!(v["result"]["outputs"][0].as_str().unwrap().starts_with(sandbox.work().to_str().unwrap()));
    assert!(files_in(&sandbox.work()).is_empty());

    let out =
        run(iris(&sandbox).args(["image", "generate", "x", "-O", "definitely_not_an_option=1", "--json"]));
    assert_nothing_sent(&out, "unsupported_option", 2);
    if let Some(flag) = undeclared_flag(spec, op) {
        let out = run(iris(&sandbox).args(["image", "generate", "x", flag, "1", "--json"]));
        assert_nothing_sent(&out, "unsupported_option", 2);
        assert!(out.json()["error"]["message"].as_str().unwrap().contains(flag));
    }
    // A video-only flag is not an image flag at all.
    let out = run(iris(&sandbox).args(["image", "generate", "x", "--duration", "4", "--json"]));
    assert_nothing_sent(&out, "usage_error", 2);

    std::fs::write(sandbox.path("taken.png"), b"existing").unwrap();
    let out = run(iris(&sandbox)
        .args(["image", "generate", "x", "-o", "taken.png", "--json"])
        .env("OPENAI_API_KEY", OPENAI_KEY));
    assert_nothing_sent(&out, "output_exists", 2);
    assert_eq!(std::fs::read(sandbox.path("taken.png")).unwrap(), b"existing");

    let out = run(iris(&sandbox).args(["image", "generate", "x", "--json"]));
    assert_nothing_sent(&out, "missing_credentials", 3);
    assert!(out.json()["error"]["message"].as_str().unwrap().contains("OPENAI_API_KEY"));

    let out = run(iris(&sandbox)
        .args(["image", "edit", "-i", "missing.png", "x", "--json"])
        .env("OPENAI_API_KEY", OPENAI_KEY));
    assert_nothing_sent(&out, "input_file_invalid", 2);
}

#[test]
fn video_generation_is_validated_locally_before_any_record_or_request() {
    let Some(spec) = builtin_default(ProviderId::Gemini, Operation::VideoGenerate) else { return };
    let sandbox = Sandbox::new();
    let op = Operation::VideoGenerate;

    let out = run(iris(&sandbox).args(["video", "generate", "waves", "--dry-run", "--json"]));
    assert_eq!(out.code, 0, "{}", out.stdout);
    let v = out.json();
    assert_eq!(v["result"]["async_job"], true);
    assert_eq!(v["result"]["model"], spec.id);
    assert!(
        v["warnings"].as_array().unwrap().iter().all(|w| w["code"] != "cost_estimate_unavailable")
            || spec.estimate.is_none()
    );

    let out =
        run(iris(&sandbox).args(["video", "generate", "x", "-O", "definitely_not_an_option=1", "--json"]));
    assert_nothing_sent(&out, "unsupported_option", 2);
    if let Some(flag) = undeclared_flag(spec, op) {
        let out = run(iris(&sandbox).args(["video", "generate", "x", flag, "1", "--json"]));
        assert_nothing_sent(&out, "unsupported_option", 2);
    }
    let out = run(iris(&sandbox).args(["video", "generate", "x", "--format", "png", "--json"]));
    assert_nothing_sent(&out, "usage_error", 2);
    let out = run(iris(&sandbox).args(["video", "generate", "x", "--json"]));
    assert_nothing_sent(&out, "missing_credentials", 3);
    assert!(!Path::new(&sandbox.state().join("jobs")).exists(), "no job record before a submission");
}

/// OpenAI's mask rules (PNG, alpha channel, same size as the first image) are declared
/// in the catalog and checked before a dry run returns and before the key is needed.
#[test]
fn mask_problems_fail_a_dry_run_and_come_before_missing_credentials() {
    if builtin_default(ProviderId::OpenAi, Operation::ImageEdit).is_none_or(|m| m.inputs.mask.is_none()) {
        return;
    }
    let sandbox = Sandbox::new();
    std::fs::write(sandbox.path("in.png"), png(16, 16)).unwrap();
    std::fs::write(sandbox.path("mask.jpg"), jpeg(16, 16)).unwrap();
    std::fs::write(sandbox.path("small.png"), png(8, 8)).unwrap();
    for (mask, needle) in [("mask.jpg", "accepts image/png"), ("small.png", "8x8")] {
        for extra in [&["--dry-run"][..], &[]] {
            let out = run(iris(&sandbox)
                .args(["image", "edit", "-i", "in.png", "--mask", mask, "x", "--json"])
                .args(extra));
            assert_nothing_sent(&out, "input_file_invalid", 2);
            let v = out.json();
            assert!(v["error"]["message"].as_str().unwrap().contains(needle), "{v}");
        }
    }
    let out = run(iris(&sandbox).args([
        "image",
        "edit",
        "-i",
        "in.png",
        "--mask",
        "in.png",
        "x",
        "--dry-run",
        "--json",
    ]));
    assert_eq!(out.code, 0, "{}", out.stdout);
}

/// Unknown models borrow a known model's capabilities, not its prices.
#[test]
fn borrowed_capabilities_get_no_cost_estimate() {
    let sandbox = Sandbox::new();
    for args in [
        &[
            "image",
            "generate",
            "x",
            "-m",
            "gpt-image-3",
            "--capabilities-from",
            "gpt-image-2",
            "--quality",
            "low",
            "--size",
            "1024x1024",
        ][..],
        &["video", "generate", "x", "-m", "veo-4-new", "--capabilities-from", "veo"],
        &[
            "video",
            "generate",
            "x",
            "-m",
            "veo-9-ultra",
            "--capabilities-from",
            "veo-lite",
            "--duration",
            "4",
        ],
    ] {
        let template = args[args.iter().position(|a| *a == "--capabilities-from").unwrap() + 1];
        if iris::catalog::find(template).is_none() {
            eprintln!("skipped: {template} is not in this build's catalog");
            continue;
        }
        let out = run(iris(&sandbox).args(args).args(["--dry-run", "--json"]));
        assert_eq!(out.code, 0, "{}", out.stdout);
        let v = out.json();
        assert!(v["result"]["cost_estimate"].is_null(), "{args:?}: {v}");
        let warning = v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["code"] == "cost_estimate_unavailable")
            .unwrap_or_else(|| panic!("{args:?}: {v}"))
            .clone();
        let message = warning["message"].as_str().unwrap();
        assert!(message.contains("prices are not assumed"), "{message}");
    }
}

#[test]
fn unsupported_operations_fail_locally_with_exit_2() {
    let sandbox = Sandbox::new();
    // OpenAI has no video model, whatever the catalog holds.
    let out = run(iris(&sandbox)
        .args(["video", "generate", "waves", "--provider", "openai", "--json"])
        .env("OPENAI_API_KEY", OPENAI_KEY));
    assert_nothing_sent(&out, "unsupported_operation", 2);
    assert!(!sandbox.state().join("jobs").exists());
    if let Some(video) = builtin_default(ProviderId::Gemini, Operation::VideoGenerate) {
        let out = run(iris(&sandbox).args(["image", "generate", "x", "-m", video.id, "--json"]));
        assert_nothing_sent(&out, "unsupported_operation", 2);
    }
}

#[test]
fn verbose_logs_never_contain_the_prompt_or_the_key() {
    let Some(_) = builtin_default(ProviderId::OpenAi, Operation::ImageGenerate) else { return };
    let sandbox = Sandbox::new();
    // The provider port is closed: the request fails before anything is sent.
    let out = run(iris(&sandbox)
        .args(["-vv", "image", "generate", "UNIQUE-PROMPT-7f3a", "--json"])
        .env("OPENAI_API_KEY", OPENAI_KEY));
    assert_eq!(out.code, 1, "{}\n{}", out.stdout, out.stderr);
    assert_eq!(out.error_code(), "network_error");
    for text in [&out.stdout, &out.stderr] {
        assert!(!text.contains("UNIQUE-PROMPT-7f3a"), "prompt leaked: {text}");
        assert!(!text.contains(OPENAI_KEY), "key leaked: {text}");
    }
    assert!(files_in(&sandbox.work()).is_empty());
}
