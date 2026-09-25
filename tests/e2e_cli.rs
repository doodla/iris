//! End-to-end configuration and command-line scenarios: the built `iris` binary as
//! a real process, with 127.0.0.1 mock providers attached so that any request that
//! should not happen is recorded.
//!
//! Covers scenarios 11 (settings precedence flag > env > file > default, and
//! credentials refused in the config file), 12 (usage errors in JSON mode), and 13
//! (`--dry-run` never contacts a provider and needs no keys).

mod support;

use std::path::Path;

use serde_json::{Value, json};
use support::*;

/// `(value, source)` of one row of `config show --json`.
fn setting(v: &Value, key: &str) -> (Value, String) {
    let row = v["result"]["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["key"] == key)
        .unwrap_or_else(|| panic!("config show has no {key}: {v}"));
    (row["value"].clone(), row["source"].as_str().unwrap().to_string())
}

/// Directory of the single saved artifact.
fn saved_dir(v: &Value) -> std::path::PathBuf {
    let path = Path::new(v["result"]["artifacts"][0]["path"].as_str().unwrap());
    assert!(path.is_file(), "{}", path.display());
    path.parent().unwrap().to_path_buf()
}

/// A mock that answers every provider route with a success.
fn answering_api() -> MockApi {
    let api = MockApi::start();
    let image = png(8, 8);
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&image], "req_e2e_cli"));
    api.on("POST", OPENAI_EDITS, openai_images(&[&image], "req_e2e_cli"));
    let part = serde_json::json!([inline_part("image/png", &image)]);
    api.on("POST", &gemini_generate_path(GEMINI_IMAGE_MODEL), gemini_parts(part));
    api.on(
        "POST",
        &veo_submit_path(VEO_LITE),
        json_response(200, serde_json::json!({ "name": format!("models/{VEO_LITE}/operations/never") })),
    );
    api
}

// ----- scenario 11: precedence ---------------------------------------------------------------------

#[test]
fn settings_follow_flag_over_env_over_file_over_default_end_to_end() {
    let sb = Sandbox::new();
    let (file_api, env_api) = (answering_api(), answering_api());
    let config = sb.config(
        "iris.toml",
        &format!(
            "output_dir = \"{}\"\n\n[providers.openai]\nbase_url = \"{}/v1\"\n",
            sb.path("from-file").display(),
            file_api.uri()
        ),
    );
    let generate = [
        "image",
        "generate",
        "-m",
        OPENAI_IMAGE_MODEL,
        "a fox",
        "--size",
        "1024x1024",
        "--quality",
        "low",
        "--json",
    ];

    // Default layer: no config file, no environment. Only inspected (config show and
    // a dry run), because the default base URL is the real provider.
    let mut default = sb.iris();
    default.env_remove("IRIS_OPENAI_BASE_URL");
    let v = default.clone().args(["config", "show", "--json"]).run().ok();
    assert_eq!(setting(&v, "output_dir"), (sb.work().to_str().unwrap().into(), "default".into()));
    assert_eq!(
        setting(&v, "providers.openai.base_url"),
        ("https://api.openai.com/v1".into(), "default".into())
    );
    assert_eq!(v["result"]["config_file_exists"], false);
    let v = default
        .clone()
        .args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "--dry-run", "--json"])
        .run()
        .ok();
    assert_eq!(Path::new(v["result"]["outputs"][0].as_str().unwrap()).parent().unwrap(), sb.work());

    // File layer (IRIS_CONFIG names the file).
    let mut file = default.clone();
    file.env("IRIS_CONFIG", &config).env("OPENAI_API_KEY", OPENAI_KEY);
    let v = file.clone().args(["config", "show", "--json"]).run().ok();
    assert_eq!(v["result"]["config_file"], config.to_str().unwrap());
    assert_eq!(setting(&v, "output_dir"), (sb.path("from-file").to_str().unwrap().into(), "file".into()));
    assert_eq!(setting(&v, "providers.openai.base_url").1, "file");
    let v = file.clone().args(generate).run().ok();
    assert_eq!(saved_dir(&v), sb.path("from-file"));
    assert_eq!((file_api.total(), env_api.total()), (1, 0), "the file's base URL was used");

    // Environment layer beats the file.
    let mut env = file.clone();
    env.env("IRIS_OUTPUT_DIR", sb.path("from-env"))
        .env("IRIS_OPENAI_BASE_URL", format!("{}/v1", env_api.uri()));
    let v = env.clone().args(["config", "show", "--json"]).run().ok();
    assert_eq!(setting(&v, "output_dir"), (sb.path("from-env").to_str().unwrap().into(), "env".into()));
    assert_eq!(
        setting(&v, "providers.openai.base_url"),
        (format!("{}/v1", env_api.uri()).into(), "env".into())
    );
    let v = env.clone().args(generate).run().ok();
    assert_eq!(saved_dir(&v), sb.path("from-env"));
    assert_eq!((file_api.total(), env_api.total()), (1, 1), "the environment's base URL was used");

    // Flag layer beats the environment: -d for the output directory, and --config
    // for the config file (over IRIS_CONFIG).
    let v = env.clone().args(generate).args(["-d", "from-flag"]).run().ok();
    assert_eq!(saved_dir(&v), sb.path("from-flag"));
    assert_eq!(env_api.total(), 2);
    let other = sb.config("other.toml", &format!("output_dir = \"{}\"\n", sb.path("from-other").display()));
    let v = file.clone().arg("--config").arg(&other).args(["config", "show", "--json"]).run().ok();
    assert_eq!(v["result"]["config_file"], other.to_str().unwrap());
    assert_eq!(setting(&v, "output_dir").0, sb.path("from-other").to_str().unwrap());
    assert_eq!(setting(&v, "providers.openai.base_url").1, "default", "the IRIS_CONFIG file was not read");
}

#[test]
fn a_credential_in_the_config_file_is_config_invalid_and_nothing_is_sent() {
    let secret = "sk-e2e-config-secret-0f9d";
    for toml in [
        format!("[providers.openai]\napi_key = \"{secret}\"\n"),
        format!("[image]\nmodel = \"gpt-image-2\"\n\n[image.auth]\nToken = \"{secret}\"\n"),
        format!("openai_key = \"{secret}\"\n"),
    ] {
        let sb = Sandbox::new();
        let api = answering_api();
        let config = sb.config("iris.toml", &toml);
        let out = sb
            .iris()
            .openai(&api)
            .env("IRIS_CONFIG", &config)
            .args([
                "image",
                "generate",
                "-m",
                OPENAI_IMAGE_MODEL,
                "a fox",
                "--size",
                "1024x1024",
                "--quality",
                "low",
                "--json",
            ])
            .run();
        let v = out.err(2, "config_invalid");
        let message = v["error"]["message"].as_str().unwrap();
        assert!(message.contains("OPENAI_API_KEY"), "{toml}: {message}");
        assert!(!out.stdout.contains(secret) && !out.stderr.contains(secret), "the value is never echoed");
        assert_eq!(api.total(), 0, "{toml}: nothing was sent");
    }
}

// ----- scenario 12: usage errors in JSON mode --------------------------------------------------------

#[test]
fn usage_errors_in_json_mode_are_a_single_envelope_with_exit_2() {
    let sb = Sandbox::new();
    let api = answering_api();
    sb.write("p.txt", "a prompt from a file\n");
    let cases: &[(&[&str], Option<&str>, Option<&str>)] = &[
        (
            &["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "--bogus", "--json"],
            None,
            Some("image.generate"),
        ),
        (
            &["--json", "image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "-f", "p.txt"],
            None,
            Some("image.generate"),
        ),
        (
            &["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "--prompt-stdin", "--json"],
            Some("from stdin"),
            Some("image.generate"),
        ),
        (
            &["video", "generate", "-m", VEO_LITE, "-f", "p.txt", "--prompt-stdin", "--json"],
            Some("x"),
            Some("video.generate"),
        ),
        (
            &["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "-o", "a.png", "-d", "out", "--json"],
            None,
            Some("image.generate"),
        ),
        (&["image", "edit", "-m", OPENAI_IMAGE_MODEL, "a fox", "--json"], None, Some("image.edit")),
        (&["image", "generate", "-m", OPENAI_IMAGE_MODEL, "--json"], None, Some("image.generate")),
        (&["frobnicate", "--json"], None, None),
        (&["--json"], None, None),
    ];
    for (args, stdin, command) in cases {
        let mut iris = sb.iris();
        iris.openai(&api).gemini(&api).args(*args);
        if let Some(input) = stdin {
            iris.stdin(*input);
        }
        let out = iris.run();
        let v = out.err(2, "usage_error");
        assert_eq!(v["error"]["category"], "usage", "{args:?}");
        assert_eq!(v["error"]["retryable"], false, "{args:?}");
        assert_eq!(v["command"].as_str(), *command, "{args:?}: {v}");
        assert!(!v["error"]["message"].as_str().unwrap().is_empty());
        assert!(out.stderr.is_empty(), "{args:?}: JSON mode keeps usage errors off stderr: {}", out.stderr);
    }
    assert_eq!(api.total(), 0, "usage errors never reach a provider");
    assert_eq!(files_in(&sb.work()), ["p.txt"]);
    assert!(!sb.jobs_dir().exists());
}

// ----- scenario 13: dry runs -----------------------------------------------------------------------

#[test]
fn dry_runs_never_contact_a_provider_and_need_no_keys() {
    let sb = Sandbox::new();
    let api = answering_api();
    sb.write("a.png", png(64, 64));
    sb.write("mask.png", mask_png(64, 64));

    for with_keys in [false, true] {
        let mut base = sb.iris();
        base.env("IRIS_OPENAI_BASE_URL", format!("{}/v1", api.uri())).env("IRIS_GEMINI_BASE_URL", api.uri());
        if with_keys {
            base.keys();
        }
        let plan = |args: &[&str]| {
            let v = base.clone().args(args).args(["--dry-run", "--json"]).run().ok();
            let p = v["result"].clone();
            assert_eq!(p["dry_run"], true, "{v}");
            assert_eq!(p["credential_present"], with_keys, "{v}");
            let spec = iris::catalog::find(p["model"].as_str().unwrap()).unwrap();
            assert_eq!(p["billing"], spec.billing.as_str(), "{v}");
            for out in p["outputs"].as_array().unwrap() {
                assert!(out.as_str().unwrap().starts_with(sb.work().to_str().unwrap()), "{v}");
            }
            p
        };

        let p = plan(&[
            "image",
            "generate",
            "-m",
            OPENAI_IMAGE_MODEL,
            "a fox",
            "--size",
            "1024x1024",
            "--quality",
            "low",
        ]);
        assert_eq!((p["provider"].as_str(), p["model"].as_str()), (Some("openai"), Some(OPENAI_IMAGE_MODEL)));
        assert_eq!(p["async_job"], false);
        assert_eq!(p["options"]["quality"], "low");
        assert_eq!(p["cost_estimate"]["estimated"], true);
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.00588).abs() < 1e-9, "{p}");

        let p = plan(&["image", "generate", "-m", GEMINI_IMAGE_MODEL, "a fox", "--resolution", "512"]);
        assert_eq!(p["model"], GEMINI_IMAGE_MODEL);
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.045).abs() < 1e-9, "{p}");

        let p = plan(&[
            "image",
            "edit",
            "-m",
            OPENAI_IMAGE_MODEL,
            "add a hat",
            "-i",
            "a.png",
            "--mask",
            "mask.png",
        ]);
        let inputs = p["inputs"].as_array().unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!((inputs[0]["role"].as_str(), inputs[1]["role"].as_str()), (Some("image"), Some("mask")));
        assert_eq!(inputs[0]["path"], sb.path("a.png").to_str().unwrap());
        assert_eq!(inputs[0]["media_type"], "image/png");
        assert_eq!(inputs[1]["bytes"], mask_png(64, 64).len() as u64);

        let p = plan(&["video", "generate", "waves", "-m", VEO_LITE, "--duration", "4"]);
        assert_eq!(p["async_job"], true);
        assert_eq!(p["options"]["duration"], 4, "an integer option is an integer in JSON");
        assert_eq!(p["options"]["resolution"], "720p", "effective defaults are shown");
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.20).abs() < 1e-9, "{p}");

        let out =
            base.clone().args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "--dry-run"]).run();
        assert!(out.human().starts_with("Dry run: nothing was sent"), "{}", out.stdout);
        assert!(
            out.human().contains(
                "\n  billing:    paid (requests are billed to the provider account at its published prices; no \
                 free tier)\n"
            ),
            "{}",
            out.stdout
        );
        assert!(
            out.stderr
                .contains("warning[cost_estimate_unavailable]: no cost estimate: quality and size are auto"),
            "{}",
            out.stderr
        );

        // While the model chooses the quality or the size there is no estimate, and the
        // warning names only the options to pass for one.
        let unavailable = |args: &[&str]| {
            let mut iris = base.clone();
            iris.args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, "a fox", "--dry-run", "--json"])
                .args(args);
            let v = iris.run().ok();
            assert!(v["result"]["cost_estimate"].is_null(), "{v}");
            let warnings = v["warnings"].as_array().unwrap();
            let w = warnings.iter().find(|w| w["code"] == "cost_estimate_unavailable").expect("the warning");
            w["message"].as_str().unwrap().to_string()
        };
        let both = unavailable(&[]);
        assert!(
            both.starts_with("no cost estimate: quality and size are auto, so the model chooses them"),
            "{both}"
        );
        assert!(
            both.contains("pass --quality (low, ") && both.contains(" and --size WIDTHxHEIGHT"),
            "{both}"
        );
        let quality = unavailable(&["--size", "1024x1024"]);
        assert!(quality.starts_with("no cost estimate: quality is auto"), "{quality}");
        assert!(quality.contains("pass --quality (") && !quality.contains("--size"), "{quality}");
        let size = unavailable(&["--quality", "low"]);
        assert!(size.starts_with("no cost estimate: size is auto"), "{size}");
        assert!(size.contains("pass --size WIDTHxHEIGHT") && !size.contains("--quality"), "{size}");
    }
    assert_eq!(api.total(), 0, "a dry run never contacts a provider");
    assert_eq!(files_in(&sb.work()), ["a.png", "mask.png"], "nothing was written");
    assert!(!sb.jobs_dir().exists(), "no job record");
}

/// A video plan that waits says how long the real run waits (it exits 4,
/// `wait_timeout`, when the limit passes) and how often it checks, each with where
/// the value came from and the names `config show` gives the setting; a plan that
/// does not wait (`--detach`, an image command) has none.
#[test]
fn a_video_plan_says_how_long_the_real_run_waits_and_why() {
    let sb = Sandbox::new();
    let config = sb.write("team.toml", "[video]\nwait_timeout = \"20m\"\n");
    let config = config.to_str().unwrap();
    let plan = |env: &[(&str, &str)], args: &[&str]| {
        let mut iris = sb.iris();
        for (key, value) in env {
            iris.env(key, value);
        }
        iris.args(["video", "generate", "-m", VEO_LITE, "waves", "--dry-run"]).args(args);
        let v = iris.clone().arg("--json").run().ok();
        (v["result"]["wait"].clone(), iris.run().human().to_string())
    };
    let wait = |seconds: f64, source: &str, poll: f64, poll_source: &str| {
        serde_json::json!({
            "timeout": { "seconds": seconds, "source": source, "flag": "--timeout",
                         "env_var": "IRIS_WAIT_TIMEOUT", "key": "video.wait_timeout" },
            "poll_interval": { "seconds": poll, "source": poll_source, "flag": "--poll-interval",
                               "env_var": "IRIS_POLL_INTERVAL", "key": "video.poll_interval" },
        })
    };
    let line = |human: &str| human.lines().find(|l| l.starts_with("  wait:")).unwrap_or_default().to_string();

    let (json, human) = plan(&[], &[]);
    assert_eq!(json, wait(600.0, "default", 10.0, "default"));
    assert_eq!(line(&human), "  wait:       up to 10m, polling every 10s", "{human}");

    let (json, human) = plan(&[("IRIS_CONFIG", config)], &[]);
    assert_eq!(json, wait(1200.0, "file", 10.0, "default"));
    assert_eq!(line(&human), "  wait:       up to 20m (config video.wait_timeout), polling every 10s");

    let (json, human) = plan(&[("IRIS_WAIT_TIMEOUT", "90s")], &["--poll-interval", "5s"]);
    assert_eq!(json, wait(90.0, "env", 5.0, "flag"));
    assert_eq!(
        line(&human),
        "  wait:       up to 1m 30s (env IRIS_WAIT_TIMEOUT), polling every 5s (--poll-interval)"
    );

    let (json, human) = plan(&[("IRIS_CONFIG", config)], &["--timeout", "1ms"]);
    assert_eq!(json, wait(0.001, "flag", 10.0, "default"));
    assert_eq!(line(&human), "  wait:       up to 1ms (--timeout), polling every 10s");

    // The names are the ones `config show` reports for the settings the plan used.
    let shown = sb.iris().env("IRIS_CONFIG", config).args(["config", "show", "--json"]).run().ok();
    for (key, env_var) in
        [("video.wait_timeout", "IRIS_WAIT_TIMEOUT"), ("video.poll_interval", "IRIS_POLL_INTERVAL")]
    {
        let rows = shown["result"]["settings"].as_array().unwrap();
        let row = rows.iter().find(|r| r["key"] == key).unwrap();
        assert_eq!(row["env_var"], env_var, "{row}");
    }
    assert_eq!(setting(&shown, "video.wait_timeout"), ("20m".into(), "file".into()));

    let (json, human) = plan(&[("IRIS_CONFIG", config)], &["--detach"]);
    assert!(json.is_null(), "a detached run does not wait: {json}");
    assert_eq!(line(&human), "", "{human}");
    let v = sb
        .iris()
        .args(["image", "generate", "-m", GEMINI_IMAGE_MODEL, "x", "--dry-run", "--json"])
        .run()
        .ok();
    assert!(v["result"]["wait"].is_null(), "an image command does not wait: {v}");
}

// ----- local validation parity: a dry run rejects what the real run would -------------------------

/// Unknown ids given with `--capabilities-from` must satisfy the id syntax of the
/// template's provider, the rule its adapter applies before sending. A dry run and
/// a real run reject the same ids before anything is sent (and before any job record
/// exists); an accepted id reaches the provider exactly as given.
#[test]
fn borrowed_model_ids_are_checked_as_the_adapter_checks_them() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let image = png(8, 8);
    let gemini_ok = ["gemini-9.9-flash-image", "Gem_1.x-image"];
    for id in gemini_ok {
        api.on(
            "POST",
            &gemini_generate_path(id),
            gemini_parts(serde_json::json!([inline_part("image/png", &image)])),
        );
    }
    let veo_ok = "veo-4.0_new-preview";
    api.on(
        "POST",
        &veo_submit_path(veo_ok),
        json_response(200, serde_json::json!({ "name": format!("models/{veo_ok}/operations/e2eids") })),
    );
    let openai_ok = "ft:gpt-image-2:org:custom@v2";
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&image], "req_e2e_ids"));

    let run = |args: &[&str], dry: bool| {
        let mut iris = sb.iris();
        iris.openai(&api).gemini(&api).args(args).args(["-d", "out", "--json"]);
        if dry {
            iris.arg("--dry-run");
        }
        iris.run()
    };
    let image_args = |id: &'static str, template: &'static str| {
        ["image", "generate", "a fox", "-m", id, "--capabilities-from", template]
    };

    for id in gemini_ok {
        run(&image_args(id, "nano-banana-2"), true).ok();
        run(&image_args(id, "nano-banana-2"), false).ok();
        assert_eq!(api.count("POST", &gemini_generate_path(id)), 1, "{id} reached the provider as given");
    }
    let video = ["video", "generate", "waves", "-m", veo_ok, "--capabilities-from", "veo-lite", "--detach"];
    run(&video, true).ok();
    run(&video, false).ok();
    assert_eq!(api.count("POST", &veo_submit_path(veo_ok)), 1);
    let v = run(&image_args(openai_ok, "gpt-image-2"), false).ok();
    assert_eq!(v["result"]["model"], openai_ok);
    assert_eq!(body_json(&api.hits("POST", OPENAI_GENERATIONS)[0])["model"], openai_ok);
    let sent = api.total();

    let long = "a".repeat(129);
    for id in ["gemini:9", "bad/../id", "-lead", ".hidden", "a%2Fb", long.as_str()] {
        let model = format!("--model={id}");
        for dry in [true, false] {
            let args = ["image", "generate", "a fox", &model, "--capabilities-from", "nano-banana-2"];
            let v = run(&args, dry).err(2, "invalid_argument");
            assert!(v["error"]["provider_status"].is_null(), "{id}: {v}");
            let args = ["video", "generate", "waves", &model, "--capabilities-from", "veo", "--detach"];
            run(&args, dry).err(2, "invalid_argument");
        }
    }
    assert_eq!(api.total(), sent, "rejected ids never reach a provider");
    let records = std::fs::read_dir(sb.jobs_dir())
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().ends_with(".json"));
    assert_eq!(records.count(), 1, "only the accepted Veo id left a job record");
}

/// The inline request cap is checked locally with the catalog's upper bound of the
/// encoded body. That bound must never be below the body the real adapter sends,
/// or a dry run could accept a request the adapter then refuses: compare it with
/// the bodies the mock receives, for requests with every kind of framing (JSON
/// escapes in the prompt and options, several inline images, every option set).
#[test]
fn the_inline_request_bound_covers_the_bodies_the_adapters_send() {
    use iris::catalog::{self, OptionValue, ResolvedOptions};

    let sb = Sandbox::new();
    let api = MockApi::start();
    let (a, b, c) = (png(64, 48), jpeg(30, 20), png(16, 16));
    sb.write("a.png", &a);
    sb.write("b.jpg", &b);
    sb.write("c.png", &c);
    let prompt = "a \"quoted\" fox\\with a\nnewline, a tab\t, a control \u{1} char, and unicode é🦊";
    let sizes = [a.len() as u64, b.len() as u64, c.len() as u64];
    let options = |pairs: &[(&str, &str)]| {
        let mut opts = ResolvedOptions::new();
        for (k, v) in pairs {
            opts.insert(*k, OptionValue::Str(v.to_string()));
        }
        opts
    };
    let body_len = |route: &str| api.hits("POST", route).last().unwrap().body.len() as u64;
    let check = |bound: u64, sent: u64| {
        assert!(sent <= bound, "the bound {bound} is below the {sent}-byte body the adapter sent");
        assert!(bound < sent + 4096, "the bound {bound} is far above the {sent}-byte body");
    };

    // Gemini generateContent: three inline images, every image option.
    let flash = catalog::find("gemini-3.1-flash-image").unwrap();
    let limit = flash.inputs.max_request.expect("Gemini image models declare the inline cap");
    let route = gemini_generate_path(flash.id);
    api.on("POST", &route, gemini_parts(serde_json::json!([inline_part("image/png", &png(8, 8))])));
    let pairs = [("aspect_ratio", "21:9"), ("resolution", "4K"), ("thinking_level", "high")];
    sb.iris()
        .gemini(&api)
        .args(["image", "edit", "-m", flash.id, "-i", "a.png", "-i", "b.jpg", "-i", "c.png", prompt])
        .args([
            "--aspect-ratio",
            "21:9",
            "--resolution",
            "4K",
            "-O",
            "thinking_level=high",
            "-d",
            "out",
            "--json",
        ])
        .run()
        .ok();
    check(limit.upper_bound(prompt, &options(&pairs), sizes), body_len(&route));
    sb.iris().gemini(&api).args(["image", "generate", "-m", flash.id, "x", "-d", "out", "--json"]).run().ok();
    check(limit.upper_bound("x", &ResolvedOptions::new(), []), body_len(&route));

    // Veo predictLongRunning: reference images plus every parameter they allow,
    // frames plus the negative prompt (which reference images exclude), and the
    // parameters the adapter always sends when none is given.
    let veo = catalog::find("veo").unwrap();
    let limit = veo.inputs.max_request.expect("Veo models declare the inline cap");
    let route = veo_submit_path(veo.id);
    api.on(
        "POST",
        &route,
        json_response(200, serde_json::json!({ "name": format!("models/{}/operations/bound", veo.id) })),
    );
    let negative = "no \"text\"\nor logos \u{2}";
    let pairs = [
        ("aspect_ratio", "9:16"),
        ("duration", "8"),
        ("person_generation", "allow_adult"),
        ("resolution", "1080p"),
    ];
    sb.iris()
        .gemini(&api)
        .args([
            "video", "generate", "-m", "veo", prompt, "--ref", "a.png", "--ref", "b.jpg", "--ref", "c.png",
        ])
        .args(["--aspect-ratio", "9:16", "--duration", "8", "--resolution", "1080p"])
        .args(["-O", "person_generation=allow_adult", "--detach", "--json"])
        .run()
        .ok();
    check(limit.upper_bound(prompt, &options(&pairs), sizes), body_len(&route));
    let pairs = [
        ("aspect_ratio", "9:16"),
        ("duration", "8"),
        ("negative_prompt", negative),
        ("person_generation", "allow_adult"),
        ("resolution", "1080p"),
    ];
    sb.iris()
        .gemini(&api)
        .args(["video", "generate", "-m", "veo", prompt, "--image", "a.png", "--last-frame", "b.jpg"])
        .args([
            "--aspect-ratio",
            "9:16",
            "--duration",
            "8",
            "--resolution",
            "1080p",
            "--negative-prompt",
            negative,
        ])
        .args(["-O", "person_generation=allow_adult", "--detach", "--json"])
        .run()
        .ok();
    check(limit.upper_bound(prompt, &options(&pairs), [sizes[0], sizes[1]]), body_len(&route));
    sb.iris()
        .gemini(&api)
        .args([
            "video",
            "generate",
            "-m",
            "veo",
            "x",
            "--image",
            "a.png",
            "--last-frame",
            "b.jpg",
            "--detach",
            "--json",
        ])
        .run()
        .ok();
    check(limit.upper_bound("x", &ResolvedOptions::new(), [sizes[0], sizes[1]]), body_len(&route));
}

// ----- the model is always named: -m, or the config file ----------------------------------------

/// Without `-m` and without a model in the config file, every generation command
/// fails with `model_required` before anything is sent, a dry run too: exit 2, no
/// provider status, no job record, and details that say how to choose.
#[test]
fn a_command_without_a_model_is_model_required_and_sends_nothing() {
    let sb = Sandbox::new();
    let api = answering_api();
    sb.write("a.png", png(16, 16));
    let config = sb.config("iris.toml", "[video]\nwait_timeout = \"5m\"\n");
    for (args, op, table) in [
        (&["image", "generate", "a fox"][..], "image.generate", "image"),
        (&["image", "edit", "-i", "a.png", "add a hat"], "image.edit", "image"),
        (&["video", "generate", "waves", "--detach"], "video.generate", "video"),
    ] {
        let candidates: Vec<&str> =
            iris::catalog::all().filter(|m| m.supports(op.parse().unwrap())).map(|m| m.id).collect();
        for dry_run in [false, true] {
            let mut iris = sb.iris();
            iris.openai(&api).gemini(&api).env("IRIS_CONFIG", &config).args(args).arg("--json");
            if dry_run {
                iris.arg("--dry-run");
            }
            let v = iris.run().err(2, "model_required");
            let e = &v["error"];
            assert_eq!(e["category"], "usage", "{v}");
            assert_eq!(e["retryable"], false, "{v}");
            assert!(e["provider_status"].is_null(), "{v}");
            assert!(e["job_id"].is_null(), "{v}");
            assert_eq!(
                e["message"],
                format!(
                    "{op} needs a model: pass -m/--model, or set model in the [{table}] table of the config file"
                )
            );
            assert_eq!(e["details"]["operation"], op);
            assert_eq!(e["details"]["config_key"], format!("{table}.model"));
            assert_eq!(e["details"]["config_file"], config.to_str().unwrap());
            let listed: Vec<&str> = e["details"]["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["model"].as_str().unwrap())
                .collect();
            assert_eq!(listed, candidates, "every catalog model for {op}, in catalog order");
            let first = &e["details"]["candidates"][0];
            let spec = iris::catalog::find(candidates[0]).unwrap();
            assert_eq!(first["provider"], spec.provider.as_str());
            assert_eq!(first["display_name"], spec.display_name);
            assert_eq!(first["summary"], spec.summary);
            assert_eq!(first["aliases"], serde_json::json!(spec.aliases));
            // What each model costs at least, to choose by, with the options that give it:
            // the `lowest_estimate` `models list` reports.
            for (candidate, id) in e["details"]["candidates"].as_array().unwrap().iter().zip(&candidates) {
                let (options, estimate) = iris::catalog::find(id).unwrap().lowest_estimate().unwrap();
                let lowest = serde_json::json!({ "options": options, "cost_estimate": estimate });
                assert_eq!(candidate["lowest_estimate"], lowest, "{id}");
            }
            // The config file was chosen explicitly (IRIS_CONFIG), so the command the hint
            // suggests names it.
            let path = config.to_str().unwrap();
            assert_eq!(
                e["hint"],
                format!(
                    "run `iris --config {path} models list --operation {op}` and pass -m <MODEL>, or set model \
                     under [{table}] in {path}"
                )
            );
        }
    }
    assert_eq!(api.total(), 0, "nothing was sent");
    assert!(!sb.jobs_dir().exists(), "no job record");
    assert_eq!(files_in(&sb.work()), ["a.png"], "nothing was written");
}

/// The config file's `[image] model` and `[video] model` name the model when `-m`
/// is absent: the result says `model_source: config` (and the human plan and progress
/// line name the key); `-m` wins over it and says `flag`.
#[test]
fn the_config_file_names_the_model_when_the_flag_is_absent() {
    let sb = Sandbox::new();
    let api = answering_api();
    let config =
        sb.config("iris.toml", "[image]\nmodel = \"nano-banana-2\"\n\n[video]\nmodel = \"veo-lite\"\n");
    let mut configured = sb.iris();
    configured.openai(&api).gemini(&api).env("IRIS_CONFIG", &config);

    let v = configured.clone().args(["config", "show", "--json"]).run().ok();
    assert_eq!(setting(&v, "image.model"), (GEMINI_IMAGE_MODEL.into(), "file".into()), "stored canonical");
    assert_eq!(setting(&v, "video.model"), (VEO_LITE.into(), "file".into()));

    let plan = configured.clone().args(["image", "generate", "x", "--dry-run", "--json"]).run().ok();
    assert_eq!(
        (plan["result"]["provider"].as_str(), plan["result"]["model"].as_str()),
        (Some("gemini"), Some(GEMINI_IMAGE_MODEL))
    );
    assert_eq!(plan["result"]["model_source"], "config");
    let human = configured.clone().args(["image", "generate", "x", "--dry-run"]).run();
    assert!(
        human.human().contains(&format!("model:      {GEMINI_IMAGE_MODEL} (config image.model)")),
        "{}",
        human.stdout
    );

    let v = configured.clone().args(["image", "generate", "x", "-o", "g.jpg", "--json"]).run().ok();
    assert_eq!(
        (v["result"]["model"].as_str(), v["result"]["model_source"].as_str()),
        (Some(GEMINI_IMAGE_MODEL), Some("config"))
    );
    assert_eq!(api.count("POST", &gemini_generate_path(GEMINI_IMAGE_MODEL)), 1);
    let out = configured.clone().args(["image", "generate", "x", "-o", "h.jpg"]).run();
    assert_eq!(out.code, 0, "{}", out.stderr);
    assert!(
        out.stderr.contains(&format!("Requesting 1 image from gemini ({GEMINI_IMAGE_MODEL}, config image.model); this is a paid request")),
        "{}",
        out.stderr
    );

    let v = configured
        .clone()
        .args(["image", "generate", "x", "-m", OPENAI_IMAGE_MODEL, "--dry-run", "--json"])
        .run()
        .ok();
    assert_eq!(
        (v["result"]["model"].as_str(), v["result"]["model_source"].as_str()),
        (Some(OPENAI_IMAGE_MODEL), Some("flag"))
    );
    let human =
        configured.clone().args(["image", "generate", "x", "-m", OPENAI_IMAGE_MODEL, "--dry-run"]).run();
    assert!(human.human().contains(&format!("model:      {OPENAI_IMAGE_MODEL}\n")), "{}", human.stdout);

    sb.write("a.png", png(16, 16));
    let v = configured.clone().args(["image", "edit", "-i", "a.png", "x", "--dry-run", "--json"]).run().ok();
    assert_eq!(
        (
            v["result"]["operation"].as_str(),
            v["result"]["model"].as_str(),
            v["result"]["model_source"].as_str()
        ),
        (Some("image.edit"), Some(GEMINI_IMAGE_MODEL), Some("config"))
    );

    let plan = configured.clone().args(["video", "generate", "waves", "--dry-run", "--json"]).run().ok();
    assert_eq!(
        (plan["result"]["model"].as_str(), plan["result"]["model_source"].as_str()),
        (Some(VEO_LITE), Some("config"))
    );
    let human = configured.clone().args(["video", "generate", "waves", "--dry-run"]).run();
    assert!(
        human.human().contains(&format!("model:      {VEO_LITE} (config video.model)\n")),
        "{}",
        human.stdout
    );
    let v = configured
        .clone()
        .args(["video", "generate", "waves", "-m", "veo", "--dry-run", "--json"])
        .run()
        .ok();
    assert_eq!(
        (v["result"]["model"].as_str(), v["result"]["model_source"].as_str()),
        (Some("veo-3.1-generate-preview"), Some("flag"))
    );

    let out = configured.clone().args(["video", "generate", "waves", "--detach", "--json"]).run();
    let v = out.ok();
    let job = &v["result"]["job"];
    assert_eq!((job["model"].as_str(), job["model_source"].as_str()), (Some(VEO_LITE), Some("config")));
    let id = job["job_id"].as_str().unwrap();
    assert_eq!(sb.record(id)["model_source"], "config");
    assert!(
        out.stderr.contains(&format!(
            "Submitting job {id} to gemini ({VEO_LITE}, config video.model); this is a paid request"
        )),
        "{}",
        out.stderr
    );
    let status = configured.clone().args(["jobs", "status", id, "--no-refresh"]).run();
    assert!(
        status.human().contains(&format!("model:      {VEO_LITE} (config video.model)\n")),
        "{}",
        status.stdout
    );
}

/// A configured model the catalog does not know is `config_invalid` naming the key,
/// even with `-m`. The command its hint names runs regardless, and so does
/// `models show` for a listed model, since neither reads the config file (`models
/// show --check-access` does); a name Iris deliberately gives no model gets the hint
/// `-m` gives it.
#[test]
fn an_unknown_configured_model_is_config_invalid_with_a_hint_that_runs() {
    let sb = Sandbox::new();
    let config = sb.config("iris.toml", "[image]\nmodel = \"nope\"\n");
    let mut configured = sb.iris();
    configured.env("IRIS_CONFIG", &config);
    let v = configured
        .clone()
        .args(["image", "generate", "x", "-m", OPENAI_IMAGE_MODEL, "--dry-run", "--json"])
        .run()
        .err(2, "config_invalid");
    assert_eq!(v["error"]["details"]["key"], "image.model");
    let hint = v["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("`iris models list --operation image.generate`"), "{hint}");
    let v = configured.clone().args(["models", "list", "--operation", "image.generate", "--json"]).run().ok();
    let listed: Vec<&str> =
        v["result"]["models"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    let expected: Vec<&str> = iris::catalog::all()
        .filter(|m| m.supports(iris::domain::Operation::ImageGenerate))
        .map(|m| m.id)
        .collect();
    assert_eq!(listed, expected);
    let v = configured.clone().args(["models", "show", listed[0], "--json"]).run().ok();
    assert_eq!(v["result"]["model"]["id"], listed[0]);
    let v = configured.clone().args(["models", "show", listed[0], "--check-access", "--json"]).run();
    assert_eq!(v.err(2, "config_invalid")["error"]["details"]["key"], "image.model");

    let flag = sb.iris().args(["image", "generate", "x", "-m", "nano-banana", "--dry-run", "--json"]).run();
    let flag = flag.err(2, "unknown_model");
    let config = sb.config("declined.toml", "[image]\nmodel = \"nano-banana\"\n");
    let file =
        sb.iris().env("IRIS_CONFIG", &config).args(["image", "generate", "x", "--dry-run", "--json"]).run();
    let file = file.err(2, "config_invalid");
    assert_eq!(file["error"]["hint"], flag["error"]["hint"]);
    let nano_banana = iris::catalog::declined("nano-banana").unwrap();
    assert_eq!(
        file["error"]["hint"],
        format!(
            "{}; use nano-banana-2 (gemini-3.1-flash-image) or nano-banana-pro (gemini-3-pro-image)",
            nano_banana.reason
        )
    );
    // A declined name none of whose replacements fits the key: the key's own hint follows
    // the reason.
    let config = sb.config("declined-video.toml", "[video]\nmodel = \"dall-e-3\"\n");
    let v = sb.iris().env("IRIS_CONFIG", &config).args(["config", "show", "--json"]).run();
    let v = v.err(2, "config_invalid");
    assert_eq!(
        v["error"]["hint"],
        format!(
            "{}; set video.model to a model listed by `iris models list --operation video.generate`",
            iris::catalog::declined("dall-e-3").unwrap().reason
        )
    );
}

/// The ids of an error's `details.candidates`.
fn candidate_ids(v: &Value) -> Vec<String> {
    let candidates = v["error"]["details"]["candidates"].as_array().unwrap_or_else(|| panic!("{v}"));
    candidates.iter().map(|c| c["model"].as_str().unwrap().to_string()).collect()
}

/// The ids of the catalog models for `op`, or every model.
fn catalog_ids(op: Option<&str>) -> Vec<String> {
    iris::catalog::all()
        .filter(|m| op.is_none_or(|op| m.supports(op.parse().unwrap())))
        .map(|m| m.id.to_string())
        .collect()
}

/// An unknown model is refused before anything is sent, with what to use instead:
/// `details.candidates` lists the models of the command's operation (every model
/// for `models show`), as `model_required` does. A name Iris declines gets a hint
/// saying why and naming the replacements the command can use (or, when none can,
/// where its models are listed), without `--capabilities-from`; any other name gets
/// a hint naming the command that lists the models and, for `-m`,
/// `--capabilities-from`.
#[test]
fn an_unknown_model_names_the_models_to_use_instead() {
    let sb = Sandbox::new();
    let api = answering_api();
    let reason = |name: &str| iris::catalog::declined(name).unwrap().reason;
    let dalle = "use gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2";
    let veo = "use veo (veo-3.1-generate-preview), veo-fast (veo-3.1-fast-generate-preview), or veo-lite \
               (veo-3.1-lite-generate-preview)";
    for (args, op, dalle_instead, veo_instead) in [
        (
            &["image", "generate", "a fox"][..],
            "image.generate",
            dalle.to_string(),
            "run `iris models list --operation image.generate` and pass -m <MODEL>".to_string(),
        ),
        (
            &["video", "generate", "waves"],
            "video.generate",
            "run `iris models list --operation video.generate` and pass -m <MODEL>".to_string(),
            veo.to_string(),
        ),
    ] {
        for (model, hint) in [
            ("dall-e-3", format!("{}; {dalle_instead}", reason("dall-e-3"))),
            ("veo-3", format!("{}; {veo_instead}", reason("veo-3"))),
            (
                "sora-2",
                format!(
                    "run `iris models list --operation {op}` and pass -m <MODEL>; to use a model Iris does not \
                     know yet, add --capabilities-from <KNOWN_MODEL> to declare which known model's \
                     capabilities it has"
                ),
            ),
        ] {
            let v = sb
                .iris()
                .openai(&api)
                .gemini(&api)
                .args(args)
                .args(["-m", model, "--dry-run", "--json"])
                .run()
                .err(2, "unknown_model");
            assert_eq!(v["error"]["message"], format!("unknown model '{model}'"));
            assert!(v["error"]["provider_status"].is_null(), "{v}");
            assert_eq!(candidate_ids(&v), catalog_ids(Some(op)), "{model} for {op}");
            assert_eq!(v["error"]["hint"], hint, "{model} for {op}");
            assert_eq!(v["error"]["details"]["suggestions"], json!([]), "{model} for {op}");
        }
    }
    // A declined template gets the same reason, with the replacements for the operation.
    let v = sb
        .iris()
        .args(["image", "generate", "x", "-m", "my-model", "--capabilities-from", "gpt-image-1", "--json"])
        .run()
        .err(2, "unknown_model");
    assert_eq!(v["error"]["message"], "--capabilities-from 'gpt-image-1' is not a known model");
    assert_eq!(v["error"]["hint"], format!("{}; {dalle}", reason("gpt-image-1")));
    assert_eq!(candidate_ids(&v), catalog_ids(Some("image.generate")));
    // `models show` has no operation: every model is a candidate, every replacement named.
    for (model, hint) in [
        ("imagen-4", format!("{}; use nano-banana-2 (gemini-3.1-flash-image)", reason("imagen-4"))),
        ("veo-3", format!("{}; {veo}", reason("veo-3"))),
        ("sora-2", "run `iris models list` to see the models Iris knows".to_string()),
    ] {
        let v = sb.iris().args(["models", "show", model, "--json"]).run().err(2, "unknown_model");
        assert_eq!(v["error"]["hint"], hint);
        assert_eq!(candidate_ids(&v), catalog_ids(None));
    }
    let human = sb.iris().args(["video", "generate", "waves", "-m", "veo-3", "--dry-run"]).run();
    assert_eq!(human.code, 2);
    assert!(
        human.stderr.contains(
            "hint: Google shut down Veo 2.0 and Veo 3.0 on the Gemini API, the last of them on \
                               2026-06-30"
        ),
        "{}",
        human.stderr
    );
    assert_eq!(api.total(), 0, "nothing was sent");
}

/// A near miss of a catalog model (another case, a display name, the start of an
/// id, its words with a slip or a word left out) is refused like any unknown model,
/// and asks "did you mean …?" with the models of the operation it nearly names
/// (`details.suggestions`) instead of offering `--capabilities-from`, which would
/// send a guessed id. A tier or version word it has is never dropped.
#[test]
fn a_near_miss_model_asks_did_you_mean() {
    let sb = Sandbox::new();
    let api = answering_api();
    for (args, model, suggestions) in [
        (
            &["image", "generate", "a fox"][..],
            "gpt-image-2.5",
            &["gpt-image-2.5-sunburst", "gpt-image-2.5-flare"][..],
        ),
        (&["image", "generate", "a fox"], "Nano-Banana-2", &["gemini-3.1-flash-image"]),
        (&["image", "generate", "a fox"], "GPT Image 2.5 Flare", &["gpt-image-2.5-flare"]),
        (&["video", "generate", "waves"], "veo-3.1-lite", &["veo-3.1-lite-generate-preview"]),
        (
            &["video", "generate", "waves"],
            "veo-3.1",
            &["veo-3.1-fast-generate-preview", "veo-3.1-generate-preview", "veo-3.1-lite-generate-preview"],
        ),
        // A tier or version word is never dropped: Fast is not Standard, 2.5 is not 2.
        (&["video", "generate", "waves"], "veo3-fast", &["veo-3.1-fast-generate-preview"]),
        (&["image", "generate", "a fox"], "gpt-image-2.5-flair", &["gpt-image-2.5-flare"]),
        (
            &["image", "generate", "a fox"],
            "gpt-image-2.5-mini",
            &["gpt-image-2.5-sunburst", "gpt-image-2.5-flare"],
        ),
        (&["image", "generate", "a fox"], "flare", &["gpt-image-2.5-flare"]),
        (&["image", "generate", "a fox"], "sunburst", &["gpt-image-2.5-sunburst"]),
        (&["image", "generate", "a fox"], "nano-banana-lite", &["gemini-3.1-flash-lite-image"]),
    ] {
        let v = sb
            .iris()
            .openai(&api)
            .gemini(&api)
            .args(args)
            .args(["-m", model, "--dry-run", "--json"])
            .run()
            .err(2, "unknown_model");
        assert_eq!(v["error"]["details"]["suggestions"], json!(suggestions), "{model}");
        let hint = v["error"]["hint"].as_str().unwrap();
        let op = format!("{}.generate", args[0]);
        assert_eq!(
            hint,
            format!(
                "did you mean {}? otherwise run `iris models list --operation {op}` and pass -m <MODEL>",
                match suggestions {
                    [one] => one.to_string(),
                    [a, b] => format!("{a} or {b}"),
                    [a, b, c] => format!("{a}, {b}, or {c}"),
                    _ => unreachable!(),
                }
            ),
            "{model}"
        );
        assert!(!hint.contains("--capabilities-from"), "{hint}");
        assert_eq!(candidate_ids(&v), catalog_ids(Some(&op)), "{model}");
    }
    // A near miss of models of another operation suggests nothing for this one; the
    // hint says what those models are for, and offers no --capabilities-from.
    for (model, hint) in [
        ("veo-3.1-lite", "veo-3.1-lite-generate-preview is a video.generate model"),
        (
            "veo-3.1",
            "veo-3.1-fast-generate-preview, veo-3.1-generate-preview, and veo-3.1-lite-generate-preview are \
             video.generate models",
        ),
    ] {
        let v = sb
            .iris()
            .args(["image", "generate", "x", "-m", model, "--dry-run", "--json"])
            .run()
            .err(2, "unknown_model");
        assert_eq!(v["error"]["details"]["suggestions"], json!([]));
        assert_eq!(
            v["error"]["hint"],
            format!("{hint}; run `iris models list --operation image.generate` and pass -m <MODEL>")
        );
    }
    let v = sb
        .iris()
        .args(["video", "generate", "x", "-m", "nano-banana-lite", "--dry-run", "--json"])
        .run()
        .err(2, "unknown_model");
    assert!(
        v["error"]["hint"].as_str().unwrap().starts_with(
            "gemini-3.1-flash-lite-image is an image.generate and image.edit model; run `iris models list \
             --operation video.generate`"
        ),
        "{v}"
    );
    // `models show` suggests among every model; a template near miss is suggested too.
    let v = sb.iris().args(["models", "show", "veo-lite-3.1", "--json"]).run().err(2, "unknown_model");
    assert_eq!(v["error"]["details"]["suggestions"], json!(["veo-3.1-lite-generate-preview"]));
    let v = sb
        .iris()
        .args([
            "image",
            "generate",
            "x",
            "-m",
            "my-model",
            "--capabilities-from",
            "nano-banana-2-LITE",
            "--json",
        ])
        .run()
        .err(2, "unknown_model");
    assert_eq!(v["error"]["details"]["suggestions"], json!(["gemini-3.1-flash-lite-image"]));
    assert_eq!(
        v["error"]["hint"],
        "did you mean gemini-3.1-flash-lite-image? otherwise run `iris models list --operation image.generate` \
         and pass one of its models to --capabilities-from"
    );
    let human = sb.iris().args(["image", "generate", "x", "-m", "gpt-image-2.5", "--dry-run"]).run();
    assert_eq!(human.code, 2);
    assert!(
        human.stderr.contains("hint: did you mean gpt-image-2.5-sunburst or gpt-image-2.5-flare?"),
        "{}",
        human.stderr
    );
    assert_eq!(api.total(), 0, "nothing was sent");
}

/// An unknown `-m` given with `--capabilities-from` is sent as typed, with the
/// `unverified_model_capabilities` warning; when it nearly names catalog models of
/// the operation, the warning asks "did you mean -m …?" (a new model can look like
/// a near miss, so the run is not refused).
#[test]
fn a_near_miss_sent_with_capabilities_from_is_named_in_the_warning() {
    let sb = Sandbox::new();
    let unverified = |model: &str| {
        let v = sb
            .iris()
            .args(["video", "generate", "waves", "-m", model, "--capabilities-from", "veo-lite"])
            .args(["--dry-run", "--json"])
            .run()
            .ok();
        assert_eq!(v["result"]["model"], model);
        let warnings = v["warnings"].as_array().unwrap();
        let w = warnings.iter().find(|w| w["code"] == "unverified_model_capabilities").unwrap();
        w["message"].as_str().unwrap().to_string()
    };
    let message = unverified("veo-3.1-lite");
    assert!(
        message.ends_with(
            "; did you mean -m veo-3.1-lite-generate-preview? --capabilities-from sends 'veo-3.1-lite' as typed"
        ),
        "{message}"
    );
    let message = unverified("veo-4-ultra");
    assert!(!message.contains("did you mean"), "{message}");
}

/// A model option without a flag of its own, typed as a flag, is a usage error
/// whose hint and `details.suggestions` give the `-O` form (in human output too),
/// naming the models that take it when the `-m` model does not.
#[test]
fn a_model_option_typed_as_a_flag_points_at_the_o_form() {
    let sb = Sandbox::new();
    for (args, name, rest) in [
        (
            &["image", "generate", "x", "-m", "nano-banana-2", "--background", "transparent"][..],
            "background",
            "; gemini-3.1-flash-image does not take it; the models that do: gpt-image-2.5-sunburst, \
             gpt-image-2.5-flare, gpt-image-2",
        ),
        (
            &["image", "edit", "x", "-i", "a.png", "-m", "nano-banana-2", "--thinking-level", "high"],
            "thinking_level",
            "",
        ),
        (&["video", "generate", "x", "-m", "veo", "--person-generation=allow_all"], "person_generation", ""),
    ] {
        let v = sb.iris().args(args).arg("--json").run().err(2, "usage_error");
        let form = format!("-O {name}=VALUE");
        assert_eq!(
            v["error"]["hint"],
            format!(
                "did you mean {form}? {name} is a model option without a flag of its own{rest}; run the command \
                 with --help for usage"
            ),
            "{args:?}"
        );
        assert_eq!(v["error"]["details"]["suggestions"], json!([form]));
    }
    let human =
        sb.iris().args(["image", "generate", "x", "-m", "gpt-image-2", "--background", "opaque"]).run();
    assert_eq!(human.code, 2);
    assert!(human.stderr.contains("hint: did you mean -O background=VALUE?"), "{}", human.stderr);
}

/// An option or input the model does not take is refused with the catalog models
/// that do take it (`details.supported_by`, named by the hint with `-m`), and an
/// enum value the option does not allow with the values it does (`details.allowed`);
/// nothing is sent.
#[test]
fn a_refused_option_names_the_models_and_values_that_work() {
    let sb = Sandbox::new();
    let api = answering_api();
    sb.write("a.png", png(16, 16));
    let ids = |pick: fn(&iris::catalog::ModelSpec) -> bool| -> Vec<&str> {
        iris::catalog::all().filter(|m| pick(m)).map(|m| m.id).collect()
    };
    let gemini_images = ids(|m| {
        m.provider == iris::domain::ProviderId::Gemini && m.supports("image.generate".parse().unwrap())
    });
    let openai = ids(|m| m.provider == iris::domain::ProviderId::OpenAi);
    let references = ids(|m| m.inputs.max_reference_images > 0);
    for (args, option, supported_by, hint) in [
        (
            &["image", "generate", "x", "-m", OPENAI_IMAGE_MODEL, "--aspect-ratio", "16:9"][..],
            "aspect_ratio",
            gemini_images.clone(),
            format!(
                "--aspect-ratio is supported by: {}; pass -m <MODEL>; options supported by this model",
                gemini_images.join(", ")
            ),
        ),
        (
            &["image", "generate", "x", "-m", "nano-banana-2", "-O", "compression=50"],
            "compression",
            openai.clone(),
            format!(
                "-O compression is supported by: {}; pass -m <MODEL>; options supported by this model",
                openai.join(", ")
            ),
        ),
        (
            &["image", "generate", "x", "-m", "nano-banana-2", "-O", "nope=1"],
            "nope",
            Vec::new(),
            "no model Iris knows supports -O nope for image.generate; options supported by this model"
                .to_string(),
        ),
        (
            &["image", "edit", "-i", "a.png", "x", "-m", "nano-banana-2", "--mask", "a.png"],
            "mask",
            openai.clone(),
            format!("--mask is supported by: {}; pass -m <MODEL>", openai.join(", ")),
        ),
        (
            &["video", "generate", "x", "-m", "veo-lite", "--ref", "a.png"],
            "reference",
            references.clone(),
            format!("--ref (reference images) is supported by: {}; pass -m <MODEL>", references.join(", ")),
        ),
    ] {
        let v = sb
            .iris()
            .openai(&api)
            .gemini(&api)
            .args(args)
            .args(["--dry-run", "--json"])
            .run()
            .err(2, "unsupported_option");
        assert_eq!(v["error"]["details"]["option"], option, "{v}");
        assert_eq!(v["error"]["details"]["supported_by"], serde_json::json!(supported_by), "{v}");
        assert!(v["error"]["hint"].as_str().unwrap().starts_with(&hint), "{v}");
    }
    let v = sb
        .iris()
        .args([
            "image",
            "generate",
            "x",
            "-m",
            OPENAI_IMAGE_MODEL,
            "--quality",
            "ultra",
            "--dry-run",
            "--json",
        ])
        .run()
        .err(2, "invalid_argument");
    let iris::catalog::OptionKind::Enum(qualities) =
        iris::catalog::find(OPENAI_IMAGE_MODEL).unwrap().option("quality").unwrap().kind
    else {
        panic!("quality is an enum");
    };
    assert_eq!(v["error"]["details"]["option"], "quality");
    assert_eq!(v["error"]["details"]["allowed"], serde_json::json!(qualities));
    // An integer option with listed values lists them as integers.
    let v = sb
        .iris()
        .args(["video", "generate", "x", "-m", VEO_LITE, "--duration", "5", "--dry-run", "--json"])
        .run()
        .err(2, "invalid_argument");
    assert_eq!(v["error"]["details"]["option"], "duration");
    assert_eq!(v["error"]["details"]["allowed"], serde_json::json!([4, 6, 8]));
    assert_eq!(api.total(), 0, "nothing was sent");
}

/// Veo's duration is an integer option that takes 4, 6, or 8: `models show` types
/// its values and default as integers, and plans and job records carry it as one.
#[test]
fn veo_duration_is_an_integer_with_listed_values() {
    let sb = Sandbox::new();
    let v = sb.iris().args(["models", "show", VEO_LITE, "--json"]).run().ok();
    let options = v["result"]["model"]["options"].as_array().unwrap();
    let duration = options.iter().find(|o| o["name"] == "duration").unwrap();
    assert_eq!(duration["type"], "integer");
    assert_eq!(duration["values"], serde_json::json!([4, 6, 8]));
    assert_eq!((duration["min"].clone(), duration["max"].clone()), (Value::Null, Value::Null));
    assert_eq!(duration["default"], 8);
    let human = sb.iris().args(["models", "show", VEO_LITE]).run();
    assert!(human.human().contains("--duration: 4|6|8 (default 8)"), "{}", human.stdout);
    let v = sb.iris().args(["models", "list", "--json"]).run().ok();
    let lite =
        v["result"]["models"].as_array().unwrap().iter().find(|m| m["id"] == VEO_LITE).unwrap().clone();
    assert_eq!(lite["lowest_estimate"]["options"]["duration"], 4);

    let api = answering_api();
    let v = sb
        .iris()
        .gemini(&api)
        .args(["video", "generate", "waves", "-m", VEO_LITE, "--detach", "--json"])
        .run()
        .ok();
    let id = v["result"]["job"]["job_id"].as_str().unwrap();
    assert_eq!(sb.record(id)["request"]["duration"], 8, "the default, as an integer");
}

/// `doctor --check-access` checks every catalog model of each provider whose key is
/// set, with one free metadata read each.
#[test]
fn doctor_checks_access_to_every_model_of_a_provider_with_a_key() {
    let sb = Sandbox::new();
    let api = MockApi::start();
    let models: Vec<&str> = iris::catalog::all()
        .filter(|m| m.provider == iris::domain::ProviderId::OpenAi)
        .map(|m| m.id)
        .collect();
    for id in &models {
        api.on(
            "GET",
            &format!("/v1/models/{id}"),
            json_response(200, serde_json::json!({ "id": id, "object": "model" })),
        );
    }
    // Only the OpenAI key is set, so Gemini is skipped and the mock sees every call.
    let v = sb.iris().openai(&api).args(["doctor", "--check-access", "--json"]).run().ok();
    let checks = v["result"]["checks"].as_array().unwrap();
    let access: Vec<&Value> =
        checks.iter().filter(|c| c["id"].as_str().unwrap().starts_with("access.")).collect();
    let ids: Vec<String> = access.iter().map(|c| c["id"].as_str().unwrap().to_string()).collect();
    let expected: Vec<String> =
        models.iter().map(|id| format!("access.openai.{id}")).chain(["access.gemini".to_string()]).collect();
    assert_eq!(ids, expected, "{v}");
    assert!(access[..models.len()].iter().all(|c| c["status"] == "ok"), "{v}");
    assert_eq!(v["result"]["healthy"], true, "{v}");
    for id in &models {
        assert_eq!(api.count("GET", &format!("/v1/models/{id}")), 1, "{id}");
    }
    assert_eq!(api.total(), models.len(), "{:?}", api.requests());
    api.assert_credentials_only_in(Some(("authorization", &format!("Bearer {OPENAI_KEY}"))));
}

// ----- the harness: no provider is reachable or keyed unless a test says so ------------------------

#[test]
fn every_provider_starts_without_a_key_and_with_an_unreachable_base_url() {
    let sb = Sandbox::new();
    let v = sb.iris().args(["config", "show", "--json"]).run().ok();
    let credentials = v["result"]["credentials"].as_array().unwrap();
    for &provider in iris::domain::ProviderId::ALL {
        let env = provider.credential_env();
        let credential = credentials.iter().find(|c| c["env"] == env);
        assert_eq!(credential.map(|c| &c["present"]), Some(&Value::Bool(false)), "{env}: {v}");
        let (value, source) = setting(&v, &format!("providers.{provider}.base_url"));
        assert_eq!(source, "env", "{provider}: {v}");
        assert!(value.as_str().unwrap().starts_with(DEAD_URL), "{provider}: {v}");
    }
}
