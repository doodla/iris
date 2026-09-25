//! Provider base URL overrides, end to end: plain `http` is accepted only for a
//! loopback host, and every command that sends a provider's key to a non-default
//! base URL says so with a `non_default_base_url` warning, not only `config show`
//! and `doctor`. Each step is a real `iris` process against 127.0.0.1 mocks, with
//! fake keys.

mod support;

use serde_json::Value;
use support::*;

const PROMPT: &str = "a lighthouse at dusk";

/// The `non_default_base_url` warnings of an envelope.
fn base_url_warnings(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["code"] == "non_default_base_url")
        .map(|w| w["message"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn plain_http_is_refused_for_any_host_but_loopback() {
    let sb = Sandbox::new();

    // From the environment: config_invalid naming the variable, before any command runs.
    for url in ["http://api.example.invalid", "http://10.0.0.7:8080", "http://localhost.example"] {
        let v = sb
            .iris()
            .env("IRIS_GEMINI_BASE_URL", url)
            .args(["config", "show", "--json"])
            .run()
            .err(2, "config_invalid");
        let e = &v["error"];
        assert_eq!(e["details"]["env_var"], "IRIS_GEMINI_BASE_URL", "{v}");
        assert!(e["message"].as_str().unwrap().contains("loopback"), "{v}");
        // The paid command fails the same way, before anything is sent.
        sb.iris()
            .env("IRIS_GEMINI_BASE_URL", url)
            .keys()
            .args(["video", "generate", "-m", VEO_LITE, PROMPT, "--detach", "--json"])
            .run()
            .err(2, "config_invalid");
    }

    // From the config file: config_invalid naming the key.
    let config = sb.config("insecure.toml", "[providers.openai]\nbase_url = \"http://proxy.example/v1\"\n");
    let v = sb
        .iris()
        .env_remove("IRIS_OPENAI_BASE_URL")
        .arg("--config")
        .arg(&config)
        .args(["config", "show", "--json"])
        .run()
        .err(2, "config_invalid");
    assert_eq!(v["error"]["details"]["key"], "providers.openai.base_url", "{v}");

    // Loopback hosts and https anywhere are fine, and flagged as non-default.
    for (url, insecure) in [
        ("http://localhost:8080", true),
        ("http://127.0.0.2:8080", true),
        ("http://[::1]:8080", true),
        ("https://proxy.example/gemini", false),
    ] {
        let v = sb.iris().env("IRIS_GEMINI_BASE_URL", url).args(["config", "show", "--json"]).run().ok();
        let warnings = base_url_warnings(&v);
        let gemini: Vec<&String> = warnings.iter().filter(|m| m.contains("GEMINI_API_KEY")).collect();
        assert_eq!(gemini.len(), 1, "{url}: {v}");
        assert_eq!(gemini[0].contains("unencrypted HTTP"), insecure, "{url}: {v}");
    }
}

#[test]
fn every_command_that_sends_a_key_to_a_non_default_host_warns_once() {
    let sb = Sandbox::new();
    let veo = VeoMock::start();
    let gemini_warning = |v: &Value| {
        let warnings = base_url_warnings(v);
        assert_eq!(warnings.len(), 1, "exactly one warning, for the provider used: {v}");
        assert!(
            warnings[0].contains("IRIS_GEMINI_BASE_URL") && warnings[0].contains("GEMINI_API_KEY"),
            "{v}"
        );
    };

    // Submitting a job.
    let v = sb
        .iris()
        .gemini(&veo.api)
        .args(["video", "generate", PROMPT, "-m", VEO_LITE, "--duration", "4", "--resolution", "720p"])
        .args(["--aspect-ratio", "16:9", "--detach", "--json"])
        .run()
        .ok();
    gemini_warning(&v);
    let id = v["result"]["job"]["job_id"].as_str().unwrap().to_string();

    // Checking its status (a poll with the key).
    let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--json"]).run().ok();
    gemini_warning(&v);
    // Without a refresh nothing is sent: no warning.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "status", &id, "--no-refresh", "--json"]).run().ok();
    assert!(base_url_warnings(&v).is_empty(), "{v}");

    // Waiting and downloading: several requests with the key, one warning.
    veo.succeed();
    let v = sb.iris().gemini(&veo.api).args(["jobs", "wait", &id, "--json"]).run().ok();
    gemini_warning(&v);
    assert_eq!(v["result"]["job"]["outputs"][0]["download_state"], "downloaded", "{v}");

    // A repeated download that needs no request: no key sent, no warning.
    let v = sb.iris().gemini(&veo.api).args(["jobs", "download", &id, "--json"]).run().ok();
    assert!(base_url_warnings(&v).is_empty(), "{v}");

    // A model access check sends the key too.
    veo.api.on("GET", &format!("/v1beta/models/{VEO_LITE}"), json_response(200, serde_json::json!({})));
    let v =
        sb.iris().gemini(&veo.api).args(["models", "show", VEO_LITE, "--check-access", "--json"]).run().ok();
    gemini_warning(&v);
    let v = sb.iris().gemini(&veo.api).args(["models", "show", VEO_LITE, "--json"]).run().ok();
    assert!(base_url_warnings(&v).is_empty(), "no access check, no key sent: {v}");
    veo.assert_no_credential_leaks();

    // An image request names the provider it went to.
    let api = MockApi::start();
    api.on("POST", OPENAI_GENERATIONS, openai_images(&[&png(8, 8)], "req_base_url_1"));
    let v = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, PROMPT, "-o", "a.png", "--json"])
        .run()
        .ok();
    let warnings = base_url_warnings(&v);
    assert_eq!(warnings.len(), 1, "{v}");
    assert!(warnings[0].contains("OPENAI_API_KEY") && warnings[0].contains("unencrypted HTTP"), "{v}");
    // A dry run sends nothing.
    let v = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, PROMPT, "--dry-run", "--json"])
        .run()
        .ok();
    assert!(base_url_warnings(&v).is_empty(), "{v}");
    assert_eq!(api.total(), 1);

    // Human mode: the warning goes to stderr with the result.
    let out = sb
        .iris()
        .openai(&api)
        .args(["image", "generate", "-m", OPENAI_IMAGE_MODEL, PROMPT, "-o", "b.png"])
        .run();
    out.human();
    assert!(
        out.stderr.contains("warning[non_default_base_url]: providers.openai.base_url is"),
        "{}",
        out.stderr
    );
}
