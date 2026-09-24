//! The published JSON output schema (see docs/json-contract.md): the committed file equals the schema
//! generated from the Rust DTOs, and real outputs of every command validate
//! against it (the envelope, and the `$defs` type of the command's result).

#[path = "app_support.rs"]
mod support;

use std::collections::BTreeSet;
use std::sync::Arc;

use iris::error::{ErrorCategory, ErrorCode};
use iris::output::{SCHEMA_VERSION, human};
use serde_json::Value;
use support::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SCHEMA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/iris-output.v1.schema.json");

#[test]
fn committed_schema_matches_the_generated_schema() {
    let generated = human::schema_document(&iris::output::schema());
    let committed = std::fs::read_to_string(SCHEMA_PATH).expect("schema/iris-output.v1.schema.json exists");
    assert!(
        committed == generated,
        "schema/iris-output.v1.schema.json is out of date with the output DTOs.\nRegenerate it with\n    \
         cargo run -q -- schema > schema/iris-output.v1.schema.json\nand review the diff: removing or renaming \
         a field, or changing its meaning, needs a schema_version bump (see docs/json-contract.md)."
    );
}

#[test]
fn the_schema_is_valid_and_enumerates_the_stable_codes() {
    let schema = committed_schema();
    jsonschema::validator_for(schema).expect("the committed schema is a valid JSON Schema");
    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(SCHEMA_VERSION, 1);

    let enum_of = |def: &str| -> BTreeSet<String> {
        let d = &schema["$defs"][def];
        let values = d.get("enum").and_then(Value::as_array).cloned().unwrap_or_else(|| {
            d["oneOf"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|v| v["enum"].as_array().cloned().unwrap_or_default())
                .collect()
        });
        values.iter().map(|v| v.as_str().unwrap().to_string()).collect()
    };
    let codes: BTreeSet<String> = ErrorCode::ALL.iter().map(|c| c.as_str().to_string()).collect();
    assert_eq!(enum_of("ErrorCode"), codes);
    let categories: BTreeSet<String> = ErrorCategory::ALL
        .iter()
        .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_string())
        .collect();
    assert_eq!(enum_of("ErrorCategory"), categories);
    let commands: BTreeSet<String> = "image.generate image.edit video.generate jobs.list jobs.status jobs.wait \
                                      jobs.download jobs.delete models.list models.show providers.list config.show \
                                      config.path doctor schema completions version"
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(enum_of("CommandName"), commands, "docs/json-contract.md command names");
}

/// Run every command of the tree in JSON mode (successes and failures); `run_cli`
/// validates each output against the committed schema.
#[tokio::test]
async fn outputs_of_every_command_match_the_schema() {
    let sandbox = Sandbox::new();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).insert_header("content-type", "video/mp4").set_body_bytes(mp4(4)),
        )
        .mount(&server)
        .await;
    let openai = Arc::new(FakeProvider::openai());
    let gemini = Arc::new(FakeProvider::gemini());
    let env = sandbox.env().with_var("IRIS_GEMINI_BASE_URL", &server.uri());
    let run = |args: Vec<String>| {
        let setup = CliSetup::new(env.clone(), vec![openai.clone(), gemini.clone()]);
        async move {
            let argv: Vec<&str> = args.iter().map(String::as_str).collect();
            run_cli(setup, &argv).await.json()
        }
    };
    let s = |items: &[&str]| items.iter().map(|x| x.to_string()).collect::<Vec<_>>();

    let input = sandbox.path("in.png");
    std::fs::write(&input, png(8, 8)).unwrap();
    let input = input.to_str().unwrap().to_string();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut record = |v: &Value| {
        if let Some(c) = v["command"].as_str() {
            seen.insert(c.to_string());
        }
    };

    record(&run(s(&["image", "generate", "a fox", "--quality", "low", "--json"])).await);
    record(&run(s(&["image", "generate", "a fox", "--dry-run", "--json"])).await);
    record(&run(s(&["image", "edit", "-i", &input, "--mask", &input, "make it blue", "--json"])).await);
    record(&run(s(&["image", "edit", "-i", &input, "x", "--dry-run", "--json"])).await);
    record(&run(s(&["image", "generate", "x", "-O", "nope=1", "--json"])).await);
    let detached = run(s(&["video", "generate", "waves", "--detach", "--json"])).await;
    record(&detached);
    let id = detached["result"]["job"]["job_id"].as_str().unwrap().to_string();
    record(&run(s(&["video", "generate", "waves", "--image", &input, "--dry-run", "--json"])).await);
    record(&run(s(&["jobs", "list", "--json"])).await);
    record(&run(s(&["jobs", "status", &id, "--no-refresh", "--json"])).await);
    gemini.videos().push_poll(Ok(remote_success(&format!("{}/v1beta/files/x:download", server.uri()))));
    record(&run(s(&["jobs", "wait", &id, "--no-download", "--json"])).await);
    record(&run(s(&["jobs", "download", &id, "--json"])).await);
    record(&run(s(&["jobs", "download", &id, "--json"])).await); // already_downloaded
    record(&run(s(&["jobs", "delete", &id, "--json"])).await);
    record(&run(s(&["jobs", "status", &id, "--json"])).await); // job_not_found
    gemini.videos().push_poll(Ok(remote_success(&format!("{}/v1beta/files/y:download", server.uri()))));
    record(&run(s(&["video", "generate", "boat", "--poll-interval", "2s", "--json"])).await);
    gemini.videos().push_submit(Err(iris::error::IrisError::new(ErrorCode::SubmissionUncertain, "unknown")));
    record(&run(s(&["video", "generate", "boat", "--json"])).await);
    record(&run(s(&["models", "list", "--json"])).await);
    record(&run(s(&["models", "show", "fake-video-1", "--check-access", "--json"])).await);
    record(&run(s(&["models", "show", "nope", "--json"])).await);
    record(&run(s(&["providers", "list", "--json"])).await);
    record(&run(s(&["config", "show", "--json"])).await);
    record(&run(s(&["config", "path", "--json"])).await);
    record(&run(s(&["doctor", "--check-access", "--json"])).await);
    record(&run(s(&["schema", "--json"])).await);
    record(&run(s(&["completions", "zsh", "--json"])).await);
    record(&run(s(&["version", "--json"])).await);
    record(&run(s(&["--version", "--json"])).await);
    record(&run(s(&["jobs", "wait", "--help", "--json"])).await);
    let none = run(s(&["--json", "frobnicate"])).await;
    assert!(none["command"].is_null());

    let all: BTreeSet<String> = "image.generate image.edit video.generate jobs.list jobs.status jobs.wait \
                                 jobs.download jobs.delete models.list models.show providers.list config.show \
                                 config.path doctor schema completions version"
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(seen, all, "every command's output was validated");
}

#[test]
fn the_contract_check_rejects_malformed_envelopes() {
    let rejects = |v: Value| std::panic::catch_unwind(|| assert_matches_schema(&v)).is_err();
    let good = serde_json::json!({
        "schema_version": 1, "ok": true, "command": "jobs.list", "result": {"jobs": []}, "error": null, "warnings": []
    });
    assert!(!rejects(good.clone()));
    let mut bad = good.clone();
    bad["ok"] = Value::String("yes".into());
    assert!(rejects(bad));
    let mut bad = good.clone();
    bad["command"] = Value::String("jobs.frobnicate".into());
    assert!(rejects(bad));
    let mut bad = good.clone();
    bad["result"] = serde_json::json!({"deleted": [], "remote_effect": "none", "note": ""});
    assert!(rejects(bad), "a result of another command's type");
    let mut bad = good.clone();
    bad.as_object_mut().unwrap().remove("warnings");
    assert!(rejects(bad));
    let bad = serde_json::json!({
        "schema_version": 1, "ok": false, "command": null, "result": null,
        "error": {"code": "not_a_code", "category": "usage", "message": "m"}, "warnings": []
    });
    assert!(rejects(bad));
}
