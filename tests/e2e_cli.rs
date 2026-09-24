//! End-to-end configuration and command-line scenarios: the built `iris` binary as
//! a real process, with 127.0.0.1 mock providers attached so that any request that
//! should not happen is recorded.
//!
//! Covers scenarios 11 (settings precedence flag > env > file > default, and
//! credentials refused in the config file), 12 (usage errors in JSON mode), and 13
//! (`--dry-run` never contacts a provider and needs no keys).

mod support;

use std::path::Path;

use serde_json::Value;
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
    api.on("POST", &gemini_generate_path(GEMINI_DEFAULT_IMAGE_MODEL), gemini_parts(part));
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
    let generate = ["image", "generate", "a fox", "--size", "1024x1024", "--quality", "low", "--json"];

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
    let v = default.clone().args(["image", "generate", "a fox", "--dry-run", "--json"]).run().ok();
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
        format!("[image]\nprovider = \"openai\"\n\n[image.auth]\nToken = \"{secret}\"\n"),
        format!("openai_key = \"{secret}\"\n"),
    ] {
        let sb = Sandbox::new();
        let api = answering_api();
        let config = sb.config("iris.toml", &toml);
        let out = sb
            .iris()
            .openai(&api)
            .env("IRIS_CONFIG", &config)
            .args(["image", "generate", "a fox", "--size", "1024x1024", "--quality", "low", "--json"])
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
        (&["image", "generate", "a fox", "--bogus", "--json"], None, Some("image.generate")),
        (&["--json", "image", "generate", "a fox", "-f", "p.txt"], None, Some("image.generate")),
        (
            &["image", "generate", "a fox", "--prompt-stdin", "--json"],
            Some("from stdin"),
            Some("image.generate"),
        ),
        (
            &["video", "generate", "-f", "p.txt", "--prompt-stdin", "--json"],
            Some("x"),
            Some("video.generate"),
        ),
        (&["image", "generate", "a fox", "-o", "a.png", "-d", "out", "--json"], None, Some("image.generate")),
        (&["image", "edit", "a fox", "--json"], None, Some("image.edit")),
        (&["image", "generate", "--json"], None, Some("image.generate")),
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
            for out in p["outputs"].as_array().unwrap() {
                assert!(out.as_str().unwrap().starts_with(sb.work().to_str().unwrap()), "{v}");
            }
            p
        };

        let p = plan(&["image", "generate", "a fox", "--size", "1024x1024", "--quality", "low"]);
        assert_eq!(
            (p["provider"].as_str(), p["model"].as_str()),
            (Some("openai"), Some(OPENAI_DEFAULT_MODEL))
        );
        assert_eq!(p["async_job"], false);
        assert_eq!(p["options"]["quality"], "low");
        assert_eq!(p["cost_estimate"]["estimated"], true);
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.00588).abs() < 1e-9, "{p}");

        let p = plan(&["image", "generate", "a fox", "--provider", "gemini", "--resolution", "512"]);
        assert_eq!(p["model"], GEMINI_DEFAULT_IMAGE_MODEL);
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.045).abs() < 1e-9, "{p}");

        let p = plan(&["image", "edit", "add a hat", "-i", "a.png", "--mask", "mask.png"]);
        let inputs = p["inputs"].as_array().unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!((inputs[0]["role"].as_str(), inputs[1]["role"].as_str()), (Some("image"), Some("mask")));
        assert_eq!(inputs[0]["path"], sb.path("a.png").to_str().unwrap());
        assert_eq!(inputs[0]["media_type"], "image/png");
        assert_eq!(inputs[1]["bytes"], mask_png(64, 64).len() as u64);

        let p = plan(&["video", "generate", "waves", "-m", VEO_LITE, "--duration", "4"]);
        assert_eq!(p["async_job"], true);
        assert_eq!(p["options"]["duration"], "4");
        assert_eq!(p["options"]["resolution"], "720p", "effective defaults are shown");
        assert!((p["cost_estimate"]["amount"].as_f64().unwrap() - 0.20).abs() < 1e-9, "{p}");

        let out = base.clone().args(["image", "generate", "a fox", "--dry-run"]).run();
        assert!(out.human().starts_with("Dry run: nothing was sent"), "{}", out.stdout);
    }
    assert_eq!(api.total(), 0, "a dry run never contacts a provider");
    assert_eq!(files_in(&sb.work()), ["a.png", "mask.png"], "nothing was written");
    assert!(!sb.jobs_dir().exists(), "no job record");
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
        run(&image_args(id, "nano-banana"), true).ok();
        run(&image_args(id, "nano-banana"), false).ok();
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
            let args = ["image", "generate", "a fox", &model, "--capabilities-from", "nano-banana"];
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

    // Veo predictLongRunning: reference images plus every parameter, and the
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
        ("negative_prompt", negative),
        ("person_generation", "allow_adult"),
        ("resolution", "1080p"),
    ];
    sb.iris()
        .gemini(&api)
        .args([
            "video", "generate", "-m", "veo", prompt, "--ref", "a.png", "--ref", "b.jpg", "--ref", "c.png",
        ])
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
    check(limit.upper_bound(prompt, &options(&pairs), sizes), body_len(&route));
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

/// `default_for` shows the model a command actually uses without `--model`: the
/// configured `providers.<p>.image_model`/`video_model` when set, else the catalog
/// default, in both `models list` and `models show`.
#[test]
fn models_report_the_effective_default_model() {
    let sb = Sandbox::new();
    let default_for = |v: &Value, id: &str| -> Vec<String> {
        let models = v["result"]["models"].as_array().unwrap();
        let m = models.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("{id}: {v}"));
        m["default_for"].as_array().unwrap().iter().map(|o| o.as_str().unwrap().to_string()).collect()
    };
    let both = ["image.generate".to_string(), "image.edit".to_string()];

    let v = sb.iris().args(["models", "list", "--json"]).run().ok();
    assert_eq!(default_for(&v, "gpt-image-2.5-sunburst"), both);
    assert!(default_for(&v, "gpt-image-2").is_empty());
    assert_eq!(default_for(&v, "veo-3.1-fast-generate-preview"), ["video.generate"]);

    let config = sb.config(
        "iris.toml",
        "[providers.openai]\nimage_model = \"gpt-image-2\"\n\n[providers.gemini]\nvideo_model = \"veo-lite\"\n",
    );
    let mut configured = sb.iris();
    configured.env("IRIS_CONFIG", &config);
    let v = configured.clone().args(["models", "list", "--json"]).run().ok();
    assert_eq!(default_for(&v, "gpt-image-2"), both);
    assert!(default_for(&v, "gpt-image-2.5-sunburst").is_empty());
    assert_eq!(default_for(&v, "veo-3.1-lite-generate-preview"), ["video.generate"]);
    assert!(default_for(&v, "veo-3.1-fast-generate-preview").is_empty());
    assert_eq!(default_for(&v, "gemini-3.1-flash-image"), both, "not configured: the catalog default");

    let v = configured.clone().args(["models", "show", "gpt-image-2", "--json"]).run().ok();
    assert_eq!(v["result"]["model"]["default_for"], serde_json::json!(both));
    let plan = configured.clone().args(["image", "generate", "x", "--dry-run", "--json"]).run().ok();
    assert_eq!(plan["result"]["model"], "gpt-image-2", "the listed default is the one used");
    let human = configured.args(["models", "list"]).run();
    let line = human.human().lines().find(|l| l.starts_with("gpt-image-2 ")).unwrap().to_string();
    assert_eq!(line.matches("image.generate, image.edit").count(), 2, "operations and default for: {line}");
}
