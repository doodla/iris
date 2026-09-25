//! The CLI end to end, in-process, over fake providers and the fake catalog:
//! argument parsing, prompt sources, option flags, the JSON envelope, human
//! output, exit codes, and every command of the frozen tree.

#[path = "app_support.rs"]
mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use iris::providers::RemoteStatus;
use serde_json::Value;
use support::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Fixture {
    sandbox: Sandbox,
    openai: Arc<FakeProvider>,
    gemini: Arc<FakeProvider>,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture {
            sandbox: Sandbox::new(),
            openai: Arc::new(FakeProvider::openai()),
            gemini: Arc::new(FakeProvider::gemini()),
        }
    }

    fn setup(&self) -> CliSetup {
        CliSetup::new(self.sandbox.env(), vec![self.openai.clone(), self.gemini.clone()])
    }

    async fn run(&self, args: &[&str]) -> CliRun {
        run_cli(self.setup(), args).await
    }

    fn image_calls(&self) -> usize {
        self.openai.images().calls.load(Ordering::SeqCst) + self.gemini.images().calls.load(Ordering::SeqCst)
    }
}

fn paths_of(v: &Value) -> Vec<String> {
    v["result"]["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["path"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test]
async fn image_generate_json_prints_exactly_one_envelope_and_progress_on_stderr() {
    let f = Fixture::new();
    let out = f.sandbox.path("fox.png");
    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "a watercolor fox",
            "-o",
            out.to_str().unwrap(),
            "--json",
        ])
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    let v = run.json();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "image.generate");
    assert!(v["error"].is_null());
    assert_eq!(v["result"]["status"], "succeeded");
    assert_eq!(v["result"]["model"], "fake-image-1");
    assert_eq!(paths_of(&v), vec![out.to_str().unwrap().to_string()]);
    let unavailable =
        v["warnings"].as_array().unwrap().iter().find(|w| w["code"] == "cost_estimate_unavailable");
    assert_eq!(
        unavailable.unwrap()["message"],
        format!("no cost estimate: {FAKE_AUTO_QUALITY}"),
        "the estimator's own reason: {v}"
    );
    assert!(run.stderr.contains("Requesting 1 image from openai"), "{}", run.stderr);
    assert!(run.stderr.contains("; this is a paid request"), "{}", run.stderr);
    assert!(out.is_file());
}

#[tokio::test]
async fn human_output_lists_saved_paths_on_stdout_and_quiet_silences_progress() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(4, 4), png(4, 4)])));
    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "a fox",
            "--count",
            "2",
            "--quality",
            "low",
            "-d",
            "pics",
        ])
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{}", run.stdout);
    for line in &lines {
        let p = line.strip_prefix("Saved ").expect("Saved prefix");
        assert!(p.starts_with(f.sandbox.path("pics").to_str().unwrap()), "{p}");
        assert!(std::path::Path::new(p).is_file());
    }
    assert!(run.stderr.contains("Estimated cost: ~$0.0200"), "{}", run.stderr);

    let quiet = f.run(&["-q", "image", "generate", "-m", "fake-image-1", "a fox"]).await;
    assert_eq!(quiet.code, 0);
    assert!(!quiet.stderr.contains("Requesting"), "{}", quiet.stderr);
}

#[tokio::test]
async fn prompt_sources_are_exclusive_and_validated() {
    let f = Fixture::new();
    let run = f.run(&["image", "generate", "-m", "fake-image-1", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "usage_error");
    assert_eq!(v["error"]["message"], "a prompt is required (PROMPT, --prompt-file, or --prompt-stdin)");

    let file = f.sandbox.path("prompt.txt");
    std::fs::write(&file, "a fox from a file\n\n").unwrap();
    let run = f
        .run(&["image", "generate", "-m", "fake-image-1", "inline", "-f", file.to_str().unwrap(), "--json"])
        .await;
    assert_eq!(run.error_code(), "usage_error");

    let run =
        f.run(&["image", "generate", "-m", "fake-image-1", "-f", file.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(req.prompt, "a fox from a file", "trailing newlines trimmed");

    let bad = f.sandbox.path("bad.txt");
    std::fs::write(&bad, [0xff, 0xfe, 0x00]).unwrap();
    let run =
        f.run(&["image", "generate", "-m", "fake-image-1", "-f", bad.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");

    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "-f",
            f.sandbox.path("missing.txt").to_str().unwrap(),
            "--json",
        ])
        .await;
    assert_eq!(run.error_code(), "input_file_invalid");

    let mut setup = f.setup();
    setup.stdin = b"from stdin \n".to_vec();
    let run = run_cli(setup, &["image", "generate", "-m", "fake-image-1", "--prompt-stdin", "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(f.openai.images().last_request.lock().unwrap().clone().unwrap().prompt, "from stdin");

    let mut setup = f.setup();
    setup.stdin_is_tty = true;
    let run = run_cli(setup, &["image", "generate", "-m", "fake-image-1", "--prompt-stdin", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["image", "generate", "-m", "fake-image-1", "   ", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
}

#[tokio::test]
async fn option_flags_map_to_catalog_options_and_are_rejected_when_undeclared() {
    let f = Fixture::new();
    let run =
        f.run(&["image", "generate", "-m", "fake-image-1", "x", "--aspect-ratio", "16:9", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "unsupported_option");
    assert!(v["error"]["message"].as_str().unwrap().contains("--aspect-ratio"), "{v}");
    // Video-only flags do not exist on image commands (and vice versa).
    let run = f.run(&["image", "generate", "-m", "fake-image-1", "x", "--duration", "4", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");
    let run = f.run(&["video", "generate", "-m", "fake-video-1", "x", "--quality", "low", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["image", "generate", "-m", "fake-image-1", "x", "-O", "nope=1", "--json"]).await;
    assert_eq!(run.error_code(), "unsupported_option");
    let run = f.run(&["image", "generate", "-m", "fake-image-1", "x", "-O", "nope", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error", "= is required");
    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "x",
            "--quality",
            "low",
            "-O",
            "quality=high",
            "--json",
        ])
        .await;
    assert_eq!(run.error_code(), "usage_error", "typed flag and -O for one option");
    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "x",
            "-O",
            "background=opaque",
            "-O",
            "background=auto",
            "--json",
        ])
        .await;
    assert_eq!(run.error_code(), "invalid_argument", "duplicate -O");
    let run = f.run(&["image", "generate", "-m", "fake-image-1", "x", "--quality", "ultra", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    assert_eq!(f.image_calls(), 0, "nothing was sent");

    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "x",
            "--size",
            "64x32",
            "-O",
            "background=transparent",
            "--json",
        ])
        .await;
    assert_eq!(run.code, 0, "{run:?}");
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    let opts: Vec<(String, String)> = req.options.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();
    assert_eq!(
        opts,
        [("background".to_string(), "transparent".to_string()), ("size".into(), "64x32".into())]
    );
}

/// `-o -` does not stream media to standard output: Iris writes files and prints
/// their paths, and says so before anything is sent (real run and dry run alike).
#[tokio::test]
async fn output_to_standard_output_is_refused_with_a_hint() {
    let f = Fixture::new();
    for extra in [&["--dry-run"][..], &[]] {
        let args =
            [&["image", "generate", "-m", "fake-image-1", "x", "-o", "-", "--json"][..], extra].concat();
        let run = f.run(&args).await;
        assert_eq!(run.code, 2, "{run:?}");
        let v = run.json();
        assert_eq!(v["error"]["code"], "invalid_argument", "{v}");
        assert!(v["error"]["hint"].as_str().unwrap().contains("prints their paths"), "{v}");
    }
    let run =
        f.run(&["video", "generate", "-m", "fake-video-1", "x", "-o", "-", "--dry-run", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    assert_eq!(f.image_calls(), 0);
    assert!(!f.sandbox.work().join("-").exists());

    // `./-` is the usual way to name a file called `-`, and names one here too.
    let run =
        f.run(&["image", "generate", "-m", "fake-image-1", "x", "-o", "./-", "--dry-run", "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(run.json()["result"]["outputs"][0], f.sandbox.path("-.png").to_str().unwrap());
}

#[tokio::test]
async fn clap_usage_errors_become_json_envelopes() {
    let f = Fixture::new();
    let run =
        f.run(&["image", "generate", "-m", "fake-image-1", "x", "-o", "a.png", "-d", "dir", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["command"], "image.generate");
    assert_eq!(v["error"]["code"], "usage_error");
    assert_eq!(v["error"]["category"], "usage");

    let run = f.run(&["--json", "image", "generate", "-m", "fake-image-1", "x", "--bogus"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["nonsense", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert!(v["command"].is_null());

    let run = f.run(&["image", "edit", "-m", "fake-image-1", "x", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error", "edit requires --image");
    let message = run.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(message.contains("--image <PATH>"), "the message names the missing argument: {message}");

    let run = f
        .run(&["video", "generate", "-m", "fake-video-1", "x", "--detach", "--timeout", "5m", "--json"])
        .await;
    assert_eq!(run.error_code(), "usage_error");

    // Without --json, clap's own message goes to stderr with exit 2.
    let run = f.run(&["image", "generate", "-m", "fake-image-1", "--bogus"]).await;
    assert_eq!(run.code, 2);
    assert!(run.stdout.is_empty());
    assert!(run.stderr.contains("--bogus"), "{}", run.stderr);
}

#[tokio::test]
async fn help_and_version_have_json_forms() {
    let f = Fixture::new();
    let run = f.run(&["image", "generate", "--help", "--json"]).await;
    assert_eq!(run.code, 0);
    let v = run.json();
    assert_eq!(v["ok"], true);
    // A help result is not the named command's result: command is null.
    assert!(v["command"].is_null(), "{v}");
    let help = v["result"]["help"].as_str().unwrap();
    assert!(help.contains("Examples:") && help.contains("billed by the provider"), "{help}");

    // clap's help subcommand, with --json anywhere.
    for args in [&["help", "--json"][..], &["jobs", "help", "wait", "--json"], &["--json", "help", "jobs"]] {
        let run = f.run(args).await;
        assert_eq!(run.code, 0, "{args:?}: {run:?}");
        let v = run.json();
        assert!(v["command"].is_null());
        assert!(v["result"]["help"].as_str().unwrap().contains("Usage:"), "{args:?}");
    }
    let v = f.run(&["jobs", "help", "wait", "--json"]).await.json();
    assert!(v["result"]["help"].as_str().unwrap().contains("--no-download"));

    // `--json=VALUE` is a JSON request too; clap rejects the value, as one envelope.
    let run = f.run(&["version", "--json=true"]).await;
    assert_eq!(run.code, 2);
    let v: Value = serde_json::from_str(run.stdout.trim_end()).unwrap();
    assert_matches_schema(&v);
    assert_eq!(v["error"]["code"], "usage_error");
    assert!(run.stderr.is_empty(), "{}", run.stderr);

    let run = f.run(&["--help", "--json"]).await;
    let v = run.json();
    assert!(v["command"].is_null());
    assert!(v["result"]["help"].as_str().unwrap().contains("jobs"));

    let run = f.run(&["--version", "--json"]).await;
    let v = run.json();
    assert_eq!(v["command"], "version");
    assert_eq!(v["result"]["name"], "iris");
    assert_eq!(v["result"]["schema_version"], 1);
    assert!(!v["result"]["target"].as_str().unwrap().is_empty());

    let run = f.run(&["--version"]).await;
    assert_eq!(run.code, 0);
    assert!(run.stdout.starts_with(&format!("iris {} (", env!("CARGO_PKG_VERSION"))), "{}", run.stdout);
    let run = f.run(&["version"]).await;
    assert!(run.stdout.contains("JSON schema v1"), "{}", run.stdout);
}

#[tokio::test]
async fn video_detach_prints_the_job_id_and_follow_up_commands() {
    let f = Fixture::new();
    let run =
        f.run(&["video", "generate", "-m", "fake-video-1", "waves", "--duration", "4", "--detach"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let first = run.stdout.lines().next().unwrap();
    assert!(first.starts_with("Submitted job job_") && first.contains("running"), "{}", run.stdout);
    let id = first.split_whitespace().nth(2).unwrap().trim_end_matches(':');
    assert!(run.stdout.contains(&format!("Next: iris jobs status {id}")));
    assert!(run.stdout.contains(&format!("Next: iris jobs wait {id}")));
    assert!(run.stderr.contains("preview model"), "{}", run.stderr);

    let run = f.run(&["video", "generate", "-m", "fake-video-1", "waves", "--detach", "--json"]).await;
    let v = run.json();
    assert_eq!(v["command"], "video.generate");
    assert_eq!(v["result"]["job"]["status"], "running");
    assert_eq!(v["result"]["next_steps"].as_array().unwrap().len(), 2);
    assert_eq!(f.gemini.videos().submit_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn video_generate_waits_and_saves_through_the_cli() {
    let f = Fixture::new();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1beta/files/vid:download"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(mp4(4)),
        )
        .expect(1)
        .mount(&server)
        .await;
    f.gemini.videos().push_poll(Ok(RemoteStatus::Running { progress: None }));
    f.gemini.videos().push_poll(Ok(remote_success(&format!("{}/v1beta/files/vid:download", server.uri()))));
    let env = f.sandbox.env().with_var("IRIS_GEMINI_BASE_URL", &server.uri());
    let setup = CliSetup::new(env, vec![f.gemini.clone()]);
    let run = run_cli(
        setup,
        &["video", "generate", "-m", "fake-video-1", "boat", "-o", "boat.mp4", "--poll-interval", "2s"],
    )
    .await;
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(run.stdout, format!("Saved {}\n", f.sandbox.path("boat.mp4").display()));
    assert!(run.stderr.contains("succeeded"), "{}", run.stderr);
}

#[tokio::test]
async fn dry_run_plans_show_the_effective_options_including_defaults() {
    let f = Fixture::new();
    let run = f
        .run(&["video", "generate", "-m", "fake-video-1", "waves", "--duration", "4", "--dry-run", "--json"])
        .await;
    let v = run.json();
    assert_eq!(v["result"]["options"]["duration"], "4");
    assert_eq!(v["result"]["options"]["resolution"], "720p", "{v}");
    let run = f.run(&["video", "generate", "-m", "fake-video-1", "waves", "--dry-run"]).await;
    assert!(run.stdout.contains("duration=8") && run.stdout.contains("resolution=720p"), "{}", run.stdout);
}

#[tokio::test]
async fn wait_limits_exit_4_and_bad_durations_are_invalid_arguments() {
    let f = Fixture::new();
    let run = f
        .run(&[
            "video",
            "generate",
            "-m",
            "fake-video-1",
            "slow",
            "--timeout",
            "1",
            "--poll-interval",
            "2s",
            "--json",
        ])
        .await;
    assert_eq!(run.code, 4, "{run:?}");
    let v = run.json();
    assert_eq!(v["error"]["code"], "wait_timeout");
    assert_eq!(v["error"]["job_status"], "running");
    assert!(v["error"]["job_id"].as_str().unwrap().starts_with("job_"));

    let run =
        f.run(&["video", "generate", "-m", "fake-video-1", "x", "--poll-interval", "1s", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    let run = f.run(&["video", "generate", "-m", "fake-video-1", "x", "--timeout", "soon", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "invalid_argument");
    assert!(v["error"]["message"].as_str().unwrap().contains("--timeout"));
    assert_eq!(f.gemini.videos().submit_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn jobs_commands_list_show_and_delete_local_records() {
    let f = Fixture::new();
    let v = f.run(&["video", "generate", "-m", "fake-video-1", "x", "--detach", "--json"]).await.json();
    let id = v["result"]["job"]["job_id"].as_str().unwrap().to_string();

    let run = f.run(&["jobs", "list"]).await;
    assert_eq!(run.code, 0);
    assert!(run.stdout.starts_with("JOB ID"), "{}", run.stdout);
    assert!(run.stdout.contains(&id) && run.stdout.contains("running"));

    let run = f.run(&["jobs", "status", &id, "--no-refresh", "--json"]).await;
    let v = run.json();
    assert_eq!(v["command"], "jobs.status");
    assert_eq!(v["result"]["job"]["job_id"], id.as_str());
    assert_eq!(f.gemini.videos().poll_calls.load(Ordering::SeqCst), 0);

    let run = f.run(&["jobs", "status", &id]).await;
    assert!(run.stdout.starts_with(&format!("{id}\n")), "{}", run.stdout);
    assert!(run.stdout.contains("status:") && run.stdout.contains("remote op:"));
    assert_eq!(f.gemini.videos().poll_calls.load(Ordering::SeqCst), 1);

    let run = f.run(&["jobs", "delete", &id, "--json"]).await;
    assert_eq!(run.code, 2);
    assert_eq!(run.json()["error"]["job_status"], "running");
    let run = f.run(&["jobs", "delete", &id, "--force", "--json"]).await;
    let v = run.json();
    assert_eq!(v["result"]["deleted"][0], id.as_str());
    assert_eq!(v["result"]["remote_effect"], "none");

    let run = f.run(&["jobs", "list", "--status", "bogus", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    let run = f.run(&["jobs", "status", "not-a-job", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    let run = f.run(&["jobs", "delete", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");
    let run = f.run(&["jobs", "list", "--json"]).await;
    assert_eq!(run.json()["result"]["jobs"], serde_json::json!([]));
}

#[tokio::test]
async fn models_and_providers_describe_the_catalog_without_revealing_keys() {
    let f = Fixture::new();
    let v = f.run(&["models", "list", "--json"]).await.json();
    let ids: Vec<&str> =
        v["result"]["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["fake-image-1", "fake-gemini-image", "fake-video-1"]);
    // To choose by: what each model is for, and the estimate of its cheapest
    // single-output request (null without an estimator), in JSON and in the table.
    let models = v["result"]["models"].as_array().unwrap();
    assert_eq!(models[0]["summary"], FAKE_IMAGE_MODEL.summary);
    assert_eq!(models[0]["lowest_estimate"]["options"], serde_json::json!({"quality": "low"}));
    assert_eq!(models[0]["lowest_estimate"]["cost_estimate"]["amount"], 0.01);
    assert!(models[1]["lowest_estimate"].is_null(), "{}", models[1]);
    assert_eq!(models[2]["lowest_estimate"]["options"], serde_json::json!({"duration": "4"}));
    // The billing, and the estimate always with the options that give it.
    let table = f.run(&["models", "list"]).await.stdout;
    let lines: Vec<&str> = table.lines().collect();
    assert!(lines[0].starts_with("MODEL ") && lines[0].ends_with(" ALIASES"), "{table}");
    assert!(lines[1].starts_with("fake-image-1 "), "{table}");
    assert_eq!(lines[2], format!("  {}", FAKE_IMAGE_MODEL.summary));
    assert_eq!(lines[3], "  paid; cheapest single-output request: ~$0.0100 with quality=low");
    assert!(lines[4].starts_with("fake-gemini-image "), "{table}");
    assert_eq!(lines[6], "  paid; no estimate before the call");
    assert_eq!(lines.last().unwrap(), &"  paid; cheapest single-output request: ~$0.4000 with duration=4");
    let v = f.run(&["models", "list", "--operation", "video.generate", "--json"]).await.json();
    assert_eq!(v["result"]["models"].as_array().unwrap().len(), 1);
    let v = f.run(&["models", "list", "--provider", "gemini", "--json"]).await.json();
    assert_eq!(v["result"]["models"].as_array().unwrap().len(), 2);

    let v = f.run(&["models", "show", "fake-img", "--json"]).await.json();
    let m = &v["result"]["model"];
    assert_eq!(m["id"], "fake-image-1");
    assert_eq!(m["access"]["account_access"], "not_checked");
    assert_eq!(m["access"]["credential_env"], "OPENAI_API_KEY");
    assert_eq!(m["access"]["credential_present"], true);
    let quality = m["options"].as_array().unwrap().iter().find(|o| o["name"] == "quality").unwrap();
    assert_eq!(quality["flag"], "--quality");
    assert_eq!(quality["type"], "enum");
    assert_eq!(quality["default"], "auto");
    let background = m["options"].as_array().unwrap().iter().find(|o| o["name"] == "background").unwrap();
    assert!(background["flag"].is_null());
    assert_eq!(m["limits"]["max_prompt_chars"], 100);
    assert_eq!(m["capabilities_source"], "catalog");

    // Constraints and defaults are machine-readable: a free-text option's length in
    // max_chars, and each default typed like the option's values.
    let option = |m: &Value, name: &str| {
        m["options"].as_array().unwrap().iter().find(|o| o["name"] == name).unwrap().clone()
    };
    let count = option(m, "count");
    assert_eq!(count["type"], "integer");
    assert_eq!(count["default"], 1, "an integer default is a JSON number: {count}");
    assert!(count["max_chars"].is_null());
    assert!(quality["max_chars"].is_null());
    let v = f.run(&["models", "show", "fake-video-1", "--json"]).await.json();
    let negative = option(&v["result"]["model"], "negative_prompt");
    assert_eq!(negative["type"], "string");
    assert_eq!(negative["max_chars"], 100, "{negative}");
    assert!(negative["default"].is_null());
    assert_eq!(option(&v["result"]["model"], "duration")["default"], "8", "an enum default stays a string");

    let v = f.run(&["models", "show", "fake-video-1", "--check-access", "--json"]).await.json();
    assert_eq!(v["result"]["model"]["access"]["account_access"], "available");
    assert!(v["result"]["model"]["access"]["checked_at"].is_string());
    assert_eq!(f.gemini.access_calls.load(Ordering::SeqCst), 1);
    let run = f.run(&["models", "show", "fake-video-1", "--check-access"]).await;
    assert!(run.stdout.contains("account access: visible to this key (checked "), "{}", run.stdout);
    assert!(
        run.stdout.contains("model metadata only, billing and verification not checked"),
        "{}",
        run.stdout
    );

    let run = f.run(&["models", "show", "nope", "--json"]).await;
    assert_eq!(run.error_code(), "unknown_model");
    let run = f.run(&["models", "show", "fake-img"]).await;
    assert!(run.stdout.contains("--quality: low|high|auto (default auto)"), "{}", run.stdout);
    assert!(
        run.stdout.contains(&format!("\n  summary:     {}\n", FAKE_IMAGE_MODEL.summary)),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("\n  cheapest:    ~$0.0100 USD with --quality low (1 image(s) x $0.01 (fake))\n"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "\n  billing:     paid (requests are billed to the provider account at its published prices; no free \
             tier)\n"
        ),
        "{}",
        run.stdout
    );
    let v = f.run(&["models", "show", "fake-img", "--json"]).await.json();
    assert_eq!(v["result"]["model"]["billing"], "paid");
    let v = f.run(&["models", "list", "--json"]).await.json();
    assert!(v["result"]["models"].as_array().unwrap().iter().all(|m| m["billing"] == "paid"), "{v}");

    let run = f.run(&["providers", "list", "--json"]).await;
    let v = run.json();
    let providers = v["result"]["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0]["credential_env"], "OPENAI_API_KEY");
    assert_eq!(providers[0]["credential_present"], true);
    assert_eq!(
        providers[1]["operations"],
        serde_json::json!(["image.generate", "image.edit", "video.generate"])
    );
    assert!(!run.stdout.contains(OPENAI_KEY) && !run.stdout.contains(GEMINI_KEY));

    let setup = CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone(), f.gemini.clone()]);
    let v = run_cli(setup, &["providers", "list", "--json"]).await.json();
    assert_eq!(v["result"]["providers"][1]["credential_present"], false);
}

#[tokio::test]
async fn config_show_and_path_report_sources_and_base_url_overrides() {
    let f = Fixture::new();
    let env = f.sandbox.env().with_var("IRIS_GEMINI_BASE_URL", "http://127.0.0.1:9");
    let run = run_cli(CliSetup::new(env, vec![]), &["config", "show", "--json"]).await;
    assert_eq!(run.code, 0);
    let v = run.json();
    let settings = v["result"]["settings"].as_array().unwrap();
    let base = settings.iter().find(|s| s["key"] == "providers.gemini.base_url").unwrap();
    assert_eq!(base["source"], "env");
    assert_eq!(base["env_var"], "IRIS_GEMINI_BASE_URL");
    let state = settings.iter().find(|s| s["key"] == "state_dir").unwrap();
    assert_eq!(state["source"], "env");
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "non_default_base_url"));
    assert_eq!(v["result"]["credentials"][0]["present"], true);
    assert!(!run.stdout.contains(GEMINI_KEY));

    let v = f.run(&["config", "path", "--json"]).await.json();
    assert_eq!(v["result"]["state_dir"], f.sandbox.state().to_str().unwrap());
    assert_eq!(v["result"]["jobs_dir"], f.sandbox.state().join("jobs").to_str().unwrap());
}

#[tokio::test]
async fn invalid_configuration_is_config_invalid_but_doctor_still_reports() {
    let f = Fixture::new();
    let config = f.sandbox.path("iris.toml");
    std::fs::write(&config, "api_key = \"nope\"\n").unwrap();
    let run = f.run(&["--config", config.to_str().unwrap(), "jobs", "list", "--json"]).await;
    assert_eq!(run.code, 2);
    assert_eq!(run.error_code(), "config_invalid");

    let mut setup = f.setup();
    setup.google_api_key_present = true;
    let run = run_cli(setup, &["doctor", "--config", config.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.code, 0);
    let v = run.json();
    assert_eq!(v["result"]["healthy"], false);
    let checks = v["result"]["checks"].as_array().unwrap();
    let find = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("{id}: {v}"));
    assert_eq!(find("config")["status"], "error");
    assert_eq!(find("credentials.openai")["status"], "ok");
    assert_eq!(find("credentials.google_api_key")["status"], "warning");

    let v = f.run(&["doctor", "--check-access", "--json"]).await.json();
    let checks = v["result"]["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["id"] == "access.openai.fake-image-1" && c["status"] == "ok"), "{v}");
    assert!(f.openai.access_calls.load(Ordering::SeqCst) >= 1);
}

/// With no provider key at all, every generation command would fail with
/// `missing_credentials`, so doctor is unhealthy (a `credentials` error, still exit
/// 0), with the configuration valid or not. One key is enough: the other provider's
/// missing key stays a warning.
#[tokio::test]
async fn doctor_is_unhealthy_without_any_provider_key() {
    let f = Fixture::new();
    let invalid = f.sandbox.path("invalid.toml");
    std::fs::write(&invalid, "api_key = \"nope\"\n").unwrap();
    let doctor = |env, args: &[&str]| {
        let setup = CliSetup::new(env, vec![f.openai.clone(), f.gemini.clone()]);
        let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        async move { run_cli(setup, &args.iter().map(String::as_str).collect::<Vec<_>>()).await }
    };
    for args in [&["doctor", "--json"][..], &["doctor", "--config", invalid.to_str().unwrap(), "--json"]] {
        let run = doctor(f.sandbox.env_without_keys(), args).await;
        assert_eq!(run.code, 0);
        let v = run.json();
        assert_eq!(v["result"]["healthy"], false, "{v}");
        let checks = v["result"]["checks"].as_array().unwrap();
        let find = |id: &str| checks.iter().find(|c| c["id"] == id).unwrap_or_else(|| panic!("{id}: {v}"));
        assert_eq!(find("credentials.openai")["status"], "warning");
        assert_eq!(find("credentials.gemini")["status"], "warning");
        assert_eq!(find("credentials")["status"], "error");
        assert_eq!(
            find("credentials")["message"],
            "no provider API key is set (OPENAI_API_KEY, GEMINI_API_KEY): every generation command would fail \
             with missing_credentials"
        );
    }
    let human = doctor(f.sandbox.env_without_keys(), &["doctor"]).await;
    assert_eq!(human.code, 0);
    assert!(human.stdout.contains("[error]   credentials: no provider API key is set"), "{}", human.stdout);
    assert!(human.stdout.ends_with("Problems found (see [error] lines).\n"), "{}", human.stdout);

    let v =
        doctor(f.sandbox.env_without_keys().with_var("GEMINI_API_KEY", GEMINI_KEY), &["doctor", "--json"])
            .await
            .json();
    assert_eq!(v["result"]["healthy"], true, "{v}");
    let checks = v["result"]["checks"].as_array().unwrap();
    assert!(checks.iter().all(|c| c["id"] != "credentials"), "{v}");
    assert!(checks.iter().any(|c| c["id"] == "credentials.openai" && c["status"] == "warning"), "{v}");
}

#[tokio::test]
async fn doctor_check_ids_are_unique_and_access_means_visible_to_the_key() {
    let f = Fixture::new();
    // One check per catalog model of a provider whose key is set: the fake catalog has
    // one OpenAI model and two Gemini models (image and video).
    let run = f.run(&["doctor", "--check-access", "--json"]).await;
    assert_eq!(run.code, 0);
    let v = run.json();
    let checks = v["result"]["checks"].as_array().unwrap();
    let ids: Vec<&str> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
    let mut unique = ids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "duplicate check ids: {ids:?}");
    let access: Vec<&str> = ids.iter().copied().filter(|id| id.starts_with("access.")).collect();
    assert_eq!(
        access,
        ["access.openai.fake-image-1", "access.gemini.fake-gemini-image", "access.gemini.fake-video-1"]
    );
    for check in checks.iter().filter(|c| c["id"].as_str().unwrap().starts_with("access.")) {
        let message = check["message"].as_str().unwrap();
        assert!(message.contains("is visible to this key"), "{message}");
        assert!(message.contains("organization verification are not checked"), "{message}");
        assert!(!message.contains("can use"), "{message}");
    }

    // A provider whose key is not set gets one skipped check under its own id.
    let setup = CliSetup::new(
        f.sandbox.env_without_keys().with_var("OPENAI_API_KEY", OPENAI_KEY),
        vec![f.openai.clone(), f.gemini.clone()],
    );
    let v = run_cli(setup, &["doctor", "--check-access", "--json"]).await.json();
    let checks = v["result"]["checks"].as_array().unwrap();
    let gemini: Vec<&Value> = checks.iter().filter(|c| c["id"] == "access.gemini").collect();
    assert_eq!(gemini.len(), 1, "{v}");
    assert_eq!(gemini[0]["status"], "warning");

    // Problems found still exit 0 (the diagnostics ran); `healthy` carries the verdict.
    f.gemini.access.lock().unwrap().clone_from(&Err(iris::error::IrisError::new(
        iris::error::ErrorCode::AuthenticationFailed,
        "the key was rejected",
    )));
    let run = f.run(&["doctor", "--check-access", "--json"]).await;
    assert_eq!(run.code, 0);
    let v = run.json();
    assert_eq!(v["ok"], true);
    assert_eq!(v["result"]["healthy"], false, "{v}");
    let human = f.run(&["doctor", "--check-access"]).await;
    assert_eq!(human.code, 0);
    assert!(human.stdout.contains("[error]   access.gemini.fake-video-1: "), "{}", human.stdout);
    assert!(human.stdout.ends_with("Problems found (see [error] lines).\n"), "{}", human.stdout);

    // The help says so, and says what the access check does not cover.
    let help = f.run(&["doctor", "--help"]).await;
    let words = help.stdout.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(words.contains("doctor exits 0 whenever its checks ran, even when it finds problems"), "{words}");
    assert!(words.contains("result.healthy"), "{words}");
    assert!(words.contains("visible to your key"), "{words}");
}

#[tokio::test]
async fn schema_and_completions_are_printed_raw_without_json() {
    let f = Fixture::new();
    let run = f.run(&["schema"]).await;
    assert_eq!(run.code, 0);
    let doc: Value = serde_json::from_str(&run.stdout).unwrap();
    assert_eq!(doc, iris::output::schema());
    assert!(run.stdout.ends_with("}\n"));
    let v = f.run(&["schema", "--json"]).await.json();
    assert_eq!(v["result"]["schema"], iris::output::schema());

    for shell in ["bash", "zsh", "fish", "elvish"] {
        let run = f.run(&["completions", shell]).await;
        assert_eq!(run.code, 0, "{shell}");
        assert!(run.stdout.contains("iris"), "{shell}");
        let v = f.run(&["completions", shell, "--json"]).await.json();
        assert_eq!(v["result"]["shell"], shell);
        assert!(v["result"]["script"].as_str().unwrap().contains("jobs"));
    }
    let run = f.run(&["completions", "powershell", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");
}

#[tokio::test]
async fn dry_run_plans_through_the_cli_need_no_credentials() {
    let f = Fixture::new();
    let setup = CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone(), f.gemini.clone()]);
    let run = run_cli(
        setup,
        &[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "x",
            "--quality",
            "low",
            "-o",
            "p.webp",
            "--dry-run",
            "--json",
        ],
    )
    .await;
    assert_eq!(run.code, 0, "{run:?}");
    let v = run.json();
    assert_eq!(v["command"], "image.generate");
    let plan = &v["result"];
    assert_eq!(plan["dry_run"], true);
    assert_eq!(plan["credential_present"], false);
    assert_eq!(plan["options"]["format"], "webp");
    assert_eq!(plan["outputs"][0], f.sandbox.path("p.webp").to_str().unwrap());
    assert_eq!(f.image_calls(), 0);

    let run = f.run(&["image", "generate", "-m", "fake-image-1", "x", "--dry-run"]).await;
    assert!(run.stdout.starts_with("Dry run: nothing was sent"), "{}", run.stdout);

    let setup = CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone()]);
    let run = run_cli(setup, &["image", "generate", "-m", "fake-image-1", "x", "--json"]).await;
    assert_eq!(run.code, 3);
    assert_eq!(run.error_code(), "missing_credentials");
}

/// A model resolved with --capabilities-from borrows the template's capabilities,
/// never its prices: no estimate before the call, none from reported usage after it.
#[tokio::test]
async fn borrowed_capabilities_come_without_the_templates_prices() {
    let f = Fixture::new();
    let borrowed_warning = |v: &Value| {
        let w = v["warnings"].as_array().unwrap();
        assert!(w.iter().any(|w| w["code"] == "unverified_model_capabilities"), "{v}");
        let cost: Vec<&Value> = w.iter().filter(|w| w["code"] == "cost_estimate_unavailable").collect();
        assert_eq!(cost.len(), 1, "{v}");
        cost[0]["message"].as_str().unwrap().to_string()
    };

    // The template would estimate this request ($0.01 with an explicit quality).
    let template = f
        .run(&["image", "generate", "-m", "fake-image-1", "x", "--quality", "low", "--dry-run", "--json"])
        .await
        .json();
    assert!(template["result"]["cost_estimate"]["amount"].is_number(), "{template}");

    let args = [
        "image",
        "generate",
        "x",
        "-m",
        "fake-image-9",
        "--capabilities-from",
        "fake-image-1",
        "--quality",
        "low",
    ];
    let v = f.run(&[&args[..], &["--dry-run", "--json"]].concat()).await.json();
    assert!(v["result"]["cost_estimate"].is_null(), "{v}");
    let message = borrowed_warning(&v);
    assert!(message.contains("'fake-image-1'") && message.contains("prices are not assumed"), "{message}");
    // Its billing is the template's: the safe assumption that its requests cost money too.
    assert_eq!(v["result"]["billing"], FAKE_IMAGE_MODEL.billing.as_str(), "{v}");

    // After the call too, even when the provider reports usage the template would price.
    let mut output = image_output(vec![png(8, 8)]);
    output.usage = Some(iris::domain::Usage { output_tokens: Some(100), ..Default::default() });
    f.openai.images().push(Ok(output));
    let v = f.run(&[&args[..], &["--json"]].concat()).await.json();
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["result"]["cost_estimate"].is_null(), "{v}");
    assert!(v["result"]["usage"]["output_tokens"].is_number(), "usage is still reported: {v}");
    borrowed_warning(&v);

    // Video: the plan and the persisted job carry no estimate either.
    let args = ["video", "generate", "x", "-m", "fake-video-9", "--capabilities-from", "fake-video-1"];
    let v = f.run(&[&args[..], &["--dry-run", "--json"]].concat()).await.json();
    assert!(v["result"]["cost_estimate"].is_null(), "{v}");
    borrowed_warning(&v);
    let v = f.run(&[&args[..], &["--detach", "--json"]].concat()).await.json();
    assert!(v["result"]["job"]["cost_estimate"].is_null(), "{v}");
    assert_eq!(v["result"]["job"]["model"], "fake-video-9");
    borrowed_warning(&v);

    // The template itself still has its estimate.
    let v = f.run(&["video", "generate", "x", "-m", "fake-video-1", "--dry-run", "--json"]).await.json();
    assert!(v["result"]["cost_estimate"]["amount"].is_number(), "{v}");
}

fn encode_image(img: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, format).unwrap();
    buf.into_inner()
}

/// An RGBA PNG of pseudo-random pixels (does not compress; about 4 bytes per pixel).
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(width, height, |x, y| {
        let v = (x.wrapping_mul(7919) ^ y.wrapping_mul(104_729)).wrapping_mul(2_654_435_761);
        image::Rgba([v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8])
    });
    encode_image(image::DynamicImage::ImageRgba8(img), image::ImageFormat::Png)
}

/// Every local rule a real run applies before sending, including the input rules
/// adapters used to be the first to enforce (mask format, alpha channel, and size;
/// the inline request cap), fails a `--dry-run` the same way, and fails a real run
/// without a key as the input problem it is (exit 2), not as missing_credentials.
#[tokio::test]
async fn input_rules_fail_dry_runs_and_come_before_the_credential_check() {
    let f = Fixture::new();
    let write = |name: &str, bytes: Vec<u8>| std::fs::write(f.sandbox.path(name), bytes).unwrap();
    write("a.png", png(8, 8));
    write("mask.jpg", jpeg(8, 8));
    let rgb = image::DynamicImage::ImageRgb8(image::RgbImage::new(8, 8));
    write("opaque.png", encode_image(rgb, image::ImageFormat::Png));
    let small = image::DynamicImage::ImageRgba8(image::RgbaImage::new(4, 4));
    write("small.png", encode_image(small, image::ImageFormat::Png));
    write("noise.png", noise_png(160, 160));
    assert!(std::fs::metadata(f.sandbox.path("noise.png")).unwrap().len() > 75_000);

    let cases: [(&[&str], &str, &str); 5] = [
        (
            &["image", "edit", "-m", "fake-image-1", "-i", "a.png", "--mask", "mask.jpg", "x"],
            "input_file_invalid",
            "accepts image/png",
        ),
        (
            &["image", "edit", "-m", "fake-image-1", "-i", "a.png", "--mask", "opaque.png", "x"],
            "input_file_invalid",
            "no alpha channel",
        ),
        (
            &["image", "edit", "-m", "fake-image-1", "-i", "a.png", "--mask", "small.png", "x"],
            "input_file_invalid",
            "4x4",
        ),
        (
            &["image", "edit", "-m", "fake-gemini-image", "-i", "noise.png", "x"],
            "invalid_argument",
            "at most 100000 bytes",
        ),
        (
            &["video", "generate", "-m", "fake-video-1", "x", "--image", "noise.png"],
            "invalid_argument",
            "at most 100000 bytes",
        ),
    ];
    for (args, code, needle) in cases {
        for (keys, dry_run) in [(true, true), (false, true), (false, false)] {
            let env = if keys { f.sandbox.env() } else { f.sandbox.env_without_keys() };
            let mut argv = args.to_vec();
            argv.push("--json");
            if dry_run {
                argv.push("--dry-run");
            }
            let run = run_cli(CliSetup::new(env, vec![f.openai.clone(), f.gemini.clone()]), &argv).await;
            assert_eq!(run.code, 2, "{argv:?}: {}", run.stdout);
            let v = run.json();
            assert_eq!(v["error"]["code"], code, "{argv:?}: {v}");
            assert!(v["error"]["message"].as_str().unwrap().contains(needle), "{argv:?}: {v}");
        }
    }
    assert_eq!(f.image_calls(), 0, "nothing was sent");
    assert_eq!(f.gemini.videos().submit_calls.load(Ordering::SeqCst), 0);
    assert!(!f.sandbox.state().join("jobs").exists() || files_in(&f.sandbox.state().join("jobs")).is_empty());

    // Within the rules, the same inputs pass.
    let run = f
        .run(&[
            "image",
            "edit",
            "-m",
            "fake-image-1",
            "-i",
            "a.png",
            "--mask",
            "a.png",
            "x",
            "--dry-run",
            "--json",
        ])
        .await;
    assert_eq!(run.code, 0, "{}", run.stdout);
}

#[tokio::test]
async fn a_format_contradicting_the_output_extension_names_the_flag_given() {
    let f = Fixture::new();
    let setup = || CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone(), f.gemini.clone()]);
    for (args, given) in
        [(&["-O", "format=jpeg"][..], "-O format=jpeg"), (&["--format", "jpeg"], "--format jpeg")]
    {
        for dry_run in [true, false] {
            let mut argv = vec!["image", "generate", "-m", "fake-image-1", "x", "-o", "a.png", "--json"];
            argv.extend_from_slice(args);
            if dry_run {
                argv.push("--dry-run");
            }
            let run = run_cli(setup(), &argv).await;
            assert_eq!(run.code, 2, "{argv:?}: {}", run.stdout);
            let v = run.json();
            assert_eq!(v["error"]["code"], "invalid_argument", "{v}");
            let message = v["error"]["message"].as_str().unwrap();
            assert_eq!(message, format!("-o/--output extension '.png' contradicts {given}"), "{argv:?}");
            assert_eq!(v["error"]["details"]["option"], "format");
        }
    }
    let run = run_cli(
        setup(),
        &[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "x",
            "-o",
            "a.jpg",
            "-O",
            "format=jpeg",
            "--dry-run",
            "--json",
        ],
    )
    .await;
    assert_eq!(run.code, 0, "{}", run.stdout);
    assert_eq!(f.image_calls(), 0);
}

#[tokio::test]
async fn dry_run_output_paths_are_normalized_and_show_what_the_real_run_names() {
    let f = Fixture::new();
    let v = f
        .run(&["image", "generate", "-m", "fake-image-1", "x", "-o", "../up/./x.png", "--dry-run", "--json"])
        .await
        .json();
    let parent = f.sandbox.work().parent().unwrap().to_path_buf();
    assert_eq!(v["result"]["outputs"][0], parent.join("up").join("x.png").to_str().unwrap(), "{v}");

    // Default image names carry a fresh id per plan (indicative); video plans show a placeholder.
    let v = f
        .run(&["image", "generate", "-m", "fake-image-1", "x", "-d", "a/../out", "--dry-run", "--json"])
        .await
        .json();
    let planned = v["result"]["outputs"][0].as_str().unwrap().to_string();
    assert!(planned.starts_with(f.sandbox.path("out").join("iris-").to_str().unwrap()), "{planned}");
    let v = f.run(&["video", "generate", "-m", "fake-video-1", "x", "--dry-run", "--json"]).await.json();
    assert_eq!(v["result"]["outputs"][0], f.sandbox.path("<job_id>.mp4").to_str().unwrap(), "{v}");
    // The placeholder path of a video plan is normalized like every other planned path.
    let v = f
        .run(&["video", "generate", "-m", "fake-video-1", "x", "-d", "a/../out", "--dry-run", "--json"])
        .await
        .json();
    let expected = f.sandbox.path("out").join("<job_id>.mp4");
    assert_eq!(v["result"]["outputs"][0], expected.to_str().unwrap(), "{v}");
}

#[tokio::test]
async fn unusable_output_locations_are_invalid_arguments_and_nothing_is_sent() {
    let f = Fixture::new();
    let afile = f.sandbox.path("afile");
    std::fs::write(&afile, b"not a directory").unwrap();
    for args in [
        &["image", "generate", "-m", "fake-image-1", "x", "-d", "afile/sub", "--json"][..],
        &["image", "generate", "-m", "fake-image-1", "x", "-o", "afile/x.png", "--json"],
        &["image", "generate", "-m", "fake-image-1", "x", "-o", "afile/x.png", "--dry-run", "--json"],
        &["video", "generate", "-m", "fake-video-1", "x", "-d", "afile/sub", "--json"],
        &["video", "generate", "-m", "fake-video-1", "x", "-o", "afile/x.mp4", "--detach", "--json"],
    ] {
        let run = f.run(args).await;
        assert_eq!(run.code, 2, "{args:?}: {}", run.stdout);
        let v = run.json();
        assert_eq!(v["error"]["code"], "invalid_argument", "{args:?}: {v}");
        assert_eq!(v["error"]["details"]["path"], afile.to_str().unwrap(), "{args:?}: {v}");
        assert!(v["error"]["provider_status"].is_null());
        assert!(v["error"]["hint"].as_str().unwrap().contains("-d/--out-dir"), "{v}");
    }

    // A directory that cannot be created (Linux refuses new entries in /proc).
    if cfg!(target_os = "linux") {
        let v = f
            .run(&["image", "generate", "-m", "fake-image-1", "x", "-d", "/proc/iris-nope/sub", "--json"])
            .await
            .json();
        assert_eq!(v["error"]["code"], "invalid_argument", "{v}");
        assert_eq!(v["error"]["details"]["path"], "/proc/iris-nope/sub");
    }

    // No write permission (skipped when the tests run with privileges that bypass it).
    use std::os::unix::fs::PermissionsExt;
    let locked = f.sandbox.path("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::create_dir(locked.join("probe")).is_err() {
        let v = f
            .run(&["image", "generate", "-m", "fake-image-1", "x", "-d", "locked/sub", "--json"])
            .await
            .json();
        assert_eq!(v["error"]["code"], "invalid_argument", "{v}");
        assert_eq!(v["error"]["details"]["path"], locked.join("sub").to_str().unwrap());
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(f.image_calls(), 0, "nothing was sent");
    assert_eq!(f.gemini.videos().submit_calls.load(Ordering::SeqCst), 0);
    assert!(!f.sandbox.state().join("jobs").exists() || files_in(&f.sandbox.state().join("jobs")).is_empty());
}

#[tokio::test]
async fn human_text_says_where_model_text_is_and_completion_only_for_reported_outcomes() {
    let f = Fixture::new();
    // The output layer rewords the warning by its public code (WarningCode).
    // Model text: JSON keeps pointing at the result's `text`; human mode prints it.
    let text_output = || {
        let mut output = image_output(vec![png(8, 8)]);
        output.text = Some("a short caption".into());
        output.warnings.push(iris::domain::Warning::new(
            iris::domain::WarningCode::ProviderTextOutput,
            "the model also returned text; it is reported in the result's `text` field",
        ));
        output
    };
    f.gemini.images().push(Ok(text_output()));
    let run = f.run(&["image", "generate", "-m", "fake-gemini-image", "x"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    assert!(
        run.stderr.contains("warning[provider_text_output]: the model also returned text"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("`text` field"), "{}", run.stderr);
    assert!(run.stderr.contains("Model text: a short caption"), "{}", run.stderr);
    f.gemini.images().push(Ok(text_output()));
    let v = f.run(&["image", "generate", "-m", "fake-gemini-image", "x", "--json"]).await.json();
    assert_eq!(v["result"]["text"], "a short caption");
    assert!(v["warnings"][0]["message"].as_str().unwrap().contains("`text` field"), "{v}");

    // A submission whose outcome is unknown has no completion time to show.
    f.gemini.videos().push_submit(Err(iris::error::IrisError::new(
        iris::error::ErrorCode::SubmissionUncertain,
        "no answer",
    )));
    let v = f.run(&["video", "generate", "-m", "fake-video-1", "boat", "--json"]).await.json();
    let id = v["error"]["job_id"].as_str().unwrap().to_string();
    let run = f.run(&["jobs", "status", &id, "--no-refresh"]).await;
    assert!(run.stdout.contains(" submission_unknown\n"), "{}", run.stdout);
    assert!(!run.stdout.contains("completed:"), "{}", run.stdout);

    // A running job has none either; a succeeded one does.
    let v = f.run(&["video", "generate", "-m", "fake-video-1", "boat", "--detach", "--json"]).await.json();
    let id = v["result"]["job"]["job_id"].as_str().unwrap().to_string();
    f.gemini.videos().push_poll(Ok(iris::providers::RemoteStatus::Running { progress: None }));
    let run = f.run(&["jobs", "status", &id]).await;
    assert!(run.stdout.contains(" running\n") && !run.stdout.contains("completed:"), "{}", run.stdout);
    f.gemini.videos().push_poll(Ok(remote_success("http://127.0.0.1:9/v1beta/files/x:download")));
    let run = f.run(&["jobs", "status", &id]).await;
    assert!(run.stdout.contains(" succeeded\n") && run.stdout.contains("completed:"), "{}", run.stdout);
    // The retention estimate is a lower bound, never an expiry the provider promised.
    let v = f.run(&["jobs", "status", &id, "--no-refresh", "--json"]).await.json();
    let kept = v["result"]["job"]["remote_expires_at"].as_str().unwrap().to_string();
    assert!(run.stdout.contains(&format!("kept until: at least {kept}\n")), "{}", run.stdout);
    assert!(!run.stdout.contains("expires:"), "{}", run.stdout);
}

#[tokio::test]
async fn human_errors_still_list_images_saved_before_the_failure() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(4, 4), b"not an image".to_vec()])));
    let run = f
        .run(&["image", "generate", "-m", "fake-image-1", "two foxes", "--count", "2", "-o", "fox.png"])
        .await;
    assert_ne!(run.code, 0, "{run:?}");
    let first = f.sandbox.path("fox-1.png");
    assert!(first.is_file());
    assert_eq!(run.stdout, format!("Saved {}\n", first.display()), "{run:?}");
    assert!(run.stderr.contains("error["), "{}", run.stderr);

    // The same failure in JSON mode lists them in details.saved.
    f.openai.images().push(Ok(image_output(vec![png(4, 4), b"not an image".to_vec()])));
    let run = f
        .run(&[
            "image",
            "generate",
            "-m",
            "fake-image-1",
            "two foxes",
            "--count",
            "2",
            "-d",
            "more",
            "--json",
        ])
        .await;
    let v = run.json();
    assert_eq!(v["error"]["details"]["saved"].as_array().unwrap().len(), 1, "{v}");
}
