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
    let run = f.run(&["image", "generate", "a watercolor fox", "-o", out.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let v = run.json();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "image.generate");
    assert!(v["error"].is_null());
    assert_eq!(v["result"]["status"], "succeeded");
    assert_eq!(v["result"]["model"], "fake-image-1");
    assert_eq!(paths_of(&v), vec![out.to_str().unwrap().to_string()]);
    assert!(v["warnings"].as_array().unwrap().iter().any(|w| w["code"] == "cost_estimate_unavailable"));
    assert!(run.stderr.contains("Requesting 1 image from openai"), "{}", run.stderr);
    assert!(out.is_file());
}

#[tokio::test]
async fn human_output_lists_saved_paths_on_stdout_and_quiet_silences_progress() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(4, 4), png(4, 4)])));
    let run = f.run(&["image", "generate", "a fox", "--count", "2", "--quality", "low", "-d", "pics"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let lines: Vec<&str> = run.stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{}", run.stdout);
    for line in &lines {
        let p = line.strip_prefix("Saved ").expect("Saved prefix");
        assert!(p.starts_with(f.sandbox.path("pics").to_str().unwrap()), "{p}");
        assert!(std::path::Path::new(p).is_file());
    }
    assert!(run.stderr.contains("Estimated cost: ~$0.0200"), "{}", run.stderr);

    let quiet = f.run(&["-q", "image", "generate", "a fox"]).await;
    assert_eq!(quiet.code, 0);
    assert!(!quiet.stderr.contains("Requesting"), "{}", quiet.stderr);
}

#[tokio::test]
async fn prompt_sources_are_exclusive_and_validated() {
    let f = Fixture::new();
    let run = f.run(&["image", "generate", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "usage_error");
    assert_eq!(v["error"]["message"], "a prompt is required (PROMPT, --prompt-file, or --prompt-stdin)");

    let file = f.sandbox.path("prompt.txt");
    std::fs::write(&file, "a fox from a file\n\n").unwrap();
    let run = f.run(&["image", "generate", "inline", "-f", file.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["image", "generate", "-f", file.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    assert_eq!(req.prompt, "a fox from a file", "trailing newlines trimmed");

    let bad = f.sandbox.path("bad.txt");
    std::fs::write(&bad, [0xff, 0xfe, 0x00]).unwrap();
    let run = f.run(&["image", "generate", "-f", bad.to_str().unwrap(), "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");

    let run =
        f.run(&["image", "generate", "-f", f.sandbox.path("missing.txt").to_str().unwrap(), "--json"]).await;
    assert_eq!(run.error_code(), "input_file_invalid");

    let mut setup = f.setup();
    setup.stdin = b"from stdin \n".to_vec();
    let run = run_cli(setup, &["image", "generate", "--prompt-stdin", "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(f.openai.images().last_request.lock().unwrap().clone().unwrap().prompt, "from stdin");

    let mut setup = f.setup();
    setup.stdin_is_tty = true;
    let run = run_cli(setup, &["image", "generate", "--prompt-stdin", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["image", "generate", "   ", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
}

#[tokio::test]
async fn option_flags_map_to_catalog_options_and_are_rejected_when_undeclared() {
    let f = Fixture::new();
    let run = f.run(&["image", "generate", "x", "--duration", "4", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "unsupported_option");
    assert!(v["error"]["message"].as_str().unwrap().contains("--duration"), "{v}");

    let run = f.run(&["image", "generate", "x", "-O", "nope=1", "--json"]).await;
    assert_eq!(run.error_code(), "unsupported_option");
    let run = f.run(&["image", "generate", "x", "-O", "nope", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error", "= is required");
    let run = f.run(&["image", "generate", "x", "--quality", "low", "-O", "quality=high", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error", "typed flag and -O for one option");
    let run = f
        .run(&["image", "generate", "x", "-O", "background=opaque", "-O", "background=auto", "--json"])
        .await;
    assert_eq!(run.error_code(), "invalid_argument", "duplicate -O");
    let run = f.run(&["image", "generate", "x", "--quality", "ultra", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    let run = f.run(&["image", "generate", "x", "--provider", "acme", "--json"]).await;
    assert_eq!(run.error_code(), "unknown_provider");
    assert_eq!(f.image_calls(), 0, "nothing was sent");

    let run =
        f.run(&["image", "generate", "x", "--size", "64x32", "-O", "background=transparent", "--json"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let req = f.openai.images().last_request.lock().unwrap().clone().unwrap();
    let opts: Vec<(String, String)> = req.options.iter().map(|(k, v)| (k.clone(), v.to_string())).collect();
    assert_eq!(
        opts,
        [("background".to_string(), "transparent".to_string()), ("size".into(), "64x32".into())]
    );
}

#[tokio::test]
async fn clap_usage_errors_become_json_envelopes() {
    let f = Fixture::new();
    let run = f.run(&["image", "generate", "x", "-o", "a.png", "-d", "dir", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["command"], "image.generate");
    assert_eq!(v["error"]["code"], "usage_error");
    assert_eq!(v["error"]["category"], "usage");

    let run = f.run(&["--json", "image", "generate", "x", "--bogus"]).await;
    assert_eq!(run.error_code(), "usage_error");

    let run = f.run(&["nonsense", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert!(v["command"].is_null());

    let run = f.run(&["image", "edit", "x", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error", "edit requires --image");
    let message = run.json()["error"]["message"].as_str().unwrap().to_string();
    assert!(message.contains("--image <PATH>"), "the message names the missing argument: {message}");

    let run = f.run(&["video", "generate", "x", "--detach", "--timeout", "5m", "--json"]).await;
    assert_eq!(run.error_code(), "usage_error");

    // Without --json, clap's own message goes to stderr with exit 2.
    let run = f.run(&["image", "generate", "--bogus"]).await;
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
    let run = f.run(&["video", "generate", "waves", "--duration", "4", "--detach"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    let first = run.stdout.lines().next().unwrap();
    assert!(first.starts_with("Submitted job job_") && first.contains("running"), "{}", run.stdout);
    let id = first.split_whitespace().nth(2).unwrap().trim_end_matches(':');
    assert!(run.stdout.contains(&format!("Next: iris jobs status {id}")));
    assert!(run.stdout.contains(&format!("Next: iris jobs wait {id}")));
    assert!(run.stderr.contains("preview model"), "{}", run.stderr);

    let run = f.run(&["video", "generate", "waves", "--detach", "--json"]).await;
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
    let run = run_cli(setup, &["video", "generate", "boat", "-o", "boat.mp4", "--poll-interval", "2s"]).await;
    assert_eq!(run.code, 0, "{run:?}");
    assert_eq!(run.stdout, format!("Saved {}\n", f.sandbox.path("boat.mp4").display()));
    assert!(run.stderr.contains("succeeded"), "{}", run.stderr);
}

#[tokio::test]
async fn dry_run_plans_show_the_effective_options_including_defaults() {
    let f = Fixture::new();
    let run = f.run(&["video", "generate", "waves", "--duration", "4", "--dry-run", "--json"]).await;
    let v = run.json();
    assert_eq!(v["result"]["options"]["duration"], "4");
    assert_eq!(v["result"]["options"]["resolution"], "720p", "{v}");
    let run = f.run(&["video", "generate", "waves", "--dry-run"]).await;
    assert!(run.stdout.contains("duration=8") && run.stdout.contains("resolution=720p"), "{}", run.stdout);
}

#[tokio::test]
async fn wait_limits_exit_4_and_bad_durations_are_invalid_arguments() {
    let f = Fixture::new();
    let run =
        f.run(&["video", "generate", "slow", "--timeout", "1", "--poll-interval", "2s", "--json"]).await;
    assert_eq!(run.code, 4, "{run:?}");
    let v = run.json();
    assert_eq!(v["error"]["code"], "wait_timeout");
    assert_eq!(v["error"]["job_status"], "running");
    assert!(v["error"]["job_id"].as_str().unwrap().starts_with("job_"));

    let run = f.run(&["video", "generate", "x", "--poll-interval", "1s", "--json"]).await;
    assert_eq!(run.error_code(), "invalid_argument");
    let run = f.run(&["video", "generate", "x", "--timeout", "soon", "--json"]).await;
    assert_eq!(run.code, 2);
    let v = run.json();
    assert_eq!(v["error"]["code"], "invalid_argument");
    assert!(v["error"]["message"].as_str().unwrap().contains("--timeout"));
    assert_eq!(f.gemini.videos().submit_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn jobs_commands_list_show_and_delete_local_records() {
    let f = Fixture::new();
    let v = f.run(&["video", "generate", "x", "--detach", "--json"]).await.json();
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

    let setup = CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone(), f.gemini.clone()]);
    let run = run_cli(setup, &["doctor"]).await;
    assert_eq!(run.code, 0);
    assert!(run.stdout.contains("[warning] credentials.openai: OPENAI_API_KEY is not set"), "{}", run.stdout);
    assert!(run.stdout.contains("[ok]      state_dir"), "{}", run.stdout);
    assert!(run.stdout.ends_with("Healthy.\n"));

    let v = f.run(&["doctor", "--check-access", "--json"]).await.json();
    let checks = v["result"]["checks"].as_array().unwrap();
    assert!(checks.iter().any(|c| c["id"] == "access.openai.fake-image-1" && c["status"] == "ok"), "{v}");
    assert!(f.openai.access_calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn doctor_check_ids_are_unique_and_access_means_visible_to_the_key() {
    let f = Fixture::new();
    // Gemini has two default models in the fake catalog (image and video): one check each.
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
        &["image", "generate", "x", "--quality", "low", "-o", "p.webp", "--dry-run", "--json"],
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

    let run = f.run(&["image", "generate", "x", "--dry-run"]).await;
    assert!(run.stdout.starts_with("Dry run: nothing was sent"), "{}", run.stdout);

    let setup = CliSetup::new(f.sandbox.env_without_keys(), vec![f.openai.clone()]);
    let run = run_cli(setup, &["image", "generate", "x", "--json"]).await;
    assert_eq!(run.code, 3);
    assert_eq!(run.error_code(), "missing_credentials");
}

#[tokio::test]
async fn human_errors_still_list_images_saved_before_the_failure() {
    let f = Fixture::new();
    f.openai.images().push(Ok(image_output(vec![png(4, 4), b"not an image".to_vec()])));
    let run = f.run(&["image", "generate", "two foxes", "--count", "2", "-o", "fox.png"]).await;
    assert_ne!(run.code, 0, "{run:?}");
    let first = f.sandbox.path("fox-1.png");
    assert!(first.is_file());
    assert_eq!(run.stdout, format!("Saved {}\n", first.display()), "{run:?}");
    assert!(run.stderr.contains("error["), "{}", run.stderr);

    // The same failure in JSON mode lists them in details.saved.
    f.openai.images().push(Ok(image_output(vec![png(4, 4), b"not an image".to_vec()])));
    let run = f.run(&["image", "generate", "two foxes", "--count", "2", "-d", "more", "--json"]).await;
    let v = run.json();
    assert_eq!(v["error"]["details"]["saved"].as_array().unwrap().len(), 1, "{v}");
}
