//! The published JSON output schema (see docs/reference/json-output.md): the committed file equals the schema
//! generated from the Rust DTOs, and real outputs of every command validate
//! against it (the envelope, and the `$defs` type of the command's result).

#[path = "app_support.rs"]
mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iris::domain::{Billing, WarningCode};
use iris::error::{ErrorCategory, ErrorCode};
use iris::output::{SCHEMA_VERSION, envelope, human};
use serde_json::Value;
use support::*;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SCHEMA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/iris-output.v1.schema.json");
const CONTRACT_DOC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/reference/errors.md");

/// The body rows of the first Markdown table under the heading line `heading` of
/// docs/reference/errors.md: trimmed cells, with surrounding backticks removed.
fn contract_table(heading: &str) -> Vec<Vec<String>> {
    let doc = std::fs::read_to_string(CONTRACT_DOC).expect("docs/reference/errors.md exists");
    let section = doc
        .split_once(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("docs/reference/errors.md has no heading {heading:?}"))
        .1;
    let rows: Vec<Vec<String>> = section
        .lines()
        .take_while(|line| !line.starts_with("## "))
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            line.trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches('`').to_string())
                .collect()
        })
        .collect();
    assert!(rows.len() > 2, "no table under {heading:?}");
    assert!(rows[1].iter().all(|cell| cell.chars().all(|c| c == '-' || c == ':')), "{:?}", rows[1]);
    rows[2..].to_vec()
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
    files
}

#[test]
fn committed_schema_matches_the_generated_schema() {
    let generated = human::schema_document(&iris::output::schema());
    let committed = std::fs::read_to_string(SCHEMA_PATH).expect("schema/iris-output.v1.schema.json exists");
    assert!(
        committed == generated,
        "schema/iris-output.v1.schema.json is out of date with the output DTOs.\nRegenerate it with\n    \
         cargo run -q -- schema > schema/iris-output.v1.schema.json\nand review the diff: removing or renaming \
         a field, or changing its meaning, needs a schema_version bump (see docs/reference/json-output.md)."
    );
}

#[test]
fn the_schema_is_valid_and_enumerates_the_stable_codes() {
    let schema = committed_schema();
    jsonschema::validator_for(schema).expect("the committed schema is a valid JSON Schema");
    assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(schema["$id"], envelope::SCHEMA_ID);
    assert!(envelope::SCHEMA_ID.ends_with("/schema/iris-output.v1.schema.json"));
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(schema["properties"]["schema_version"]["const"], 1);

    let codes: BTreeSet<String> = ErrorCode::ALL.iter().map(|c| c.as_str().to_string()).collect();
    assert_eq!(known_values(&schema["$defs"]["ErrorCode"]), codes);
    let categories: BTreeSet<String> = ErrorCategory::ALL
        .iter()
        .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_string())
        .collect();
    let listed: BTreeSet<String> = schema["$defs"]["ErrorCategory"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().into())
        .collect();
    assert_eq!(listed, categories);
    let commands: BTreeSet<String> = "image.generate image.edit video.generate jobs.list jobs.status jobs.wait \
                                      jobs.download jobs.delete models.list models.show providers.list config.show \
                                      config.path doctor schema completions version"
        .split_whitespace()
        .map(str::to_string)
        .collect();
    assert_eq!(
        known_values(&schema["$defs"]["CommandName"]),
        commands,
        "docs/reference/json-output.md command names"
    );
    let warnings: BTreeSet<String> = WarningCode::ALL.iter().map(|c| c.as_str().to_string()).collect();
    assert_eq!(known_values(&schema["$defs"]["Warning"]["properties"]["code"]), warnings);
    let billing: BTreeSet<String> = Billing::ALL.iter().map(|b| b.as_str().to_string()).collect();
    assert_eq!(known_values(&schema["$defs"]["Billing"]), billing);
    // The meaning of each billing value, and how to read one this schema does not list.
    let described = schema["$defs"]["Billing"]["description"].as_str().unwrap();
    for b in Billing::ALL {
        assert!(described.contains(&format!("{}: {}", b.as_str(), b.description())), "{described}");
    }
    assert!(described.contains("Read a value you do not know as: requests may cost money."), "{described}");
}

/// The known values of an open set: `anyOf: [{enum: [...]}, {pattern}]`.
fn known_values(open_set: &Value) -> BTreeSet<String> {
    let branches = open_set["anyOf"].as_array().unwrap_or_else(|| panic!("not an open set: {open_set}"));
    assert_eq!(branches.len(), 2, "{open_set}");
    assert!(branches[1]["pattern"].is_string(), "{open_set}");
    branches[0]["enum"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

/// Error codes, commands, warning codes, provider ids, and billing values are open sets, so
/// adding one stays an additive change: an envelope of a later 1.x Iris with a value this
/// schema does not list still validates, as long as it has the documented form. Everything
/// else stays closed: a malformed value and another `schema_version` are rejected.
#[test]
fn open_sets_accept_later_values_of_the_documented_form_only() {
    let validator = jsonschema::validator_for(committed_schema()).expect("valid schema");
    let error = |code: &str, category: &str| {
        serde_json::json!({
            "code": code, "category": category, "message": "m", "retryable": null,
            "retry_after_seconds": null, "hint": null, "provider": null, "provider_status": null,
            "provider_code": null, "provider_request_id": null, "job_id": null, "remote_operation_id": null,
            "job_status": null, "details": null
        })
    };
    let failure = |command: Value, error: Value, warning: &str| {
        serde_json::json!({
            "schema_version": 1, "ok": false, "command": command, "result": null, "error": error,
            "warnings": [{"code": warning, "message": "m"}]
        })
    };
    let known = failure("jobs.list".into(), error("quota_exceeded", "quota"), "preview_model");
    assert!(validator.is_valid(&known));
    assert!(validator.is_valid(&failure(
        "videos.extend".into(),
        error("a_future_code", "provider"),
        "a_future_warning_2"
    )));
    let success = serde_json::json!({
        "schema_version": 1, "ok": true, "command": "jobs.archive", "result": {"jobs": []}, "error": null,
        "warnings": []
    });
    assert!(validator.is_valid(&success), "a later command's result is one of the result types");
    // A later provider (for example one added through the extension guide).
    let mut later_provider = error("quota_exceeded", "quota");
    later_provider["provider"] = "seedance".into();
    assert!(validator.is_valid(&failure("jobs.list".into(), later_provider.clone(), "preview_model")));
    later_provider["provider"] = "Seedance".into();
    assert!(!validator.is_valid(&failure("jobs.list".into(), later_provider, "preview_model")));
    // A later billing value, in a dry-run plan.
    let plan = |billing: &str| {
        serde_json::json!({
            "schema_version": 1, "ok": true, "command": "image.generate", "error": null, "warnings": [],
            "result": {
                "dry_run": true, "provider": "openai", "model": "m", "model_source": "flag",
                "operation": "image.generate", "async_job": false, "detach": false, "label": null, "wait": null,
                "billing": billing, "options": {}, "inputs": [], "outputs": [], "credential_present": false,
                "cost_estimate": null, "max_cost": null, "prompt_fingerprint": { "sha256": "00", "chars": 1 }
            }
        })
    };
    assert!(validator.is_valid(&plan("paid")));
    assert!(validator.is_valid(&plan("free")));
    assert!(!validator.is_valid(&plan("Free")));

    for malformed in ["", "Quota", "a-b", "1st", "a b", "a.b"] {
        let v = failure("jobs.list".into(), error(malformed, "quota"), "preview_model");
        assert!(!validator.is_valid(&v), "error code {malformed:?}");
        let v = failure("jobs.list".into(), error("quota_exceeded", "quota"), malformed);
        assert!(!validator.is_valid(&v), "warning code {malformed:?}");
    }
    for malformed in ["", "Jobs.list", "jobs..list", "jobs.", ".jobs", "jobs list", "jobs-list.x"] {
        let v = failure(malformed.into(), error("quota_exceeded", "quota"), "preview_model");
        assert!(!validator.is_valid(&v), "command {malformed:?}");
    }
    for version in [0, 2] {
        let mut v = known.clone();
        v["schema_version"] = version.into();
        assert!(!validator.is_valid(&v), "schema_version {version}");
    }
    // The category rule still holds for every known code.
    assert!(!validator.is_valid(&failure("jobs.list".into(), error("quota_exceeded", "provider"), "x")));
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

    record(
        &run(s(&["image", "generate", "-m", "fake-image-1", "a fox", "--quality", "low", "--json"])).await,
    );
    record(&run(s(&["image", "generate", "-m", "fake-image-1", "a fox", "--dry-run", "--json"])).await);
    record(
        &run(s(&[
            "image",
            "edit",
            "-m",
            "fake-image-1",
            "-i",
            &input,
            "--mask",
            &input,
            "make it blue",
            "--json",
        ]))
        .await,
    );
    record(&run(s(&["image", "edit", "-m", "fake-image-1", "-i", &input, "x", "--dry-run", "--json"])).await);
    record(&run(s(&["image", "generate", "-m", "fake-image-1", "x", "-O", "nope=1", "--json"])).await);
    record(&run(s(&["image", "generate", "x", "--json"])).await); // model_required
    let detached = run(s(&["video", "generate", "-m", "fake-video-1", "waves", "--detach", "--json"])).await;
    record(&detached);
    let id = detached["result"]["job"]["job_id"].as_str().unwrap().to_string();
    record(
        &run(s(&[
            "video",
            "generate",
            "-m",
            "fake-video-1",
            "waves",
            "--image",
            &input,
            "--dry-run",
            "--json",
        ]))
        .await,
    );
    record(&run(s(&["jobs", "list", "--json"])).await);
    record(&run(s(&["jobs", "status", &id, "--no-refresh", "--json"])).await);
    gemini.videos().push_poll(Ok(remote_success(&format!("{}/v1beta/files/x:download", server.uri()))));
    record(&run(s(&["jobs", "wait", &id, "--no-download", "--json"])).await);
    record(&run(s(&["jobs", "download", &id, "--json"])).await);
    record(&run(s(&["jobs", "download", &id, "--json"])).await); // already_downloaded
    record(&run(s(&["jobs", "delete", &id, "--json"])).await);
    record(&run(s(&["jobs", "status", &id, "--json"])).await); // job_not_found
    gemini.videos().push_poll(Ok(remote_success(&format!("{}/v1beta/files/y:download", server.uri()))));
    record(
        &run(s(&["video", "generate", "-m", "fake-video-1", "boat", "--poll-interval", "2s", "--json"]))
            .await,
    );
    gemini.videos().push_submit(Err(iris::error::IrisError::new(ErrorCode::SubmissionUncertain, "unknown")));
    record(&run(s(&["video", "generate", "-m", "fake-video-1", "boat", "--json"])).await);
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
    bad["command"] = Value::String("Jobs.List".into());
    assert!(rejects(bad));
    let mut bad = good.clone();
    bad["result"] = serde_json::json!({"deleted": [], "remote_effect": "none", "note": ""});
    assert!(rejects(bad), "a result of another command's type");
    let mut bad = good.clone();
    bad.as_object_mut().unwrap().remove("warnings");
    assert!(rejects(bad));
    let bad = serde_json::json!({
        "schema_version": 1, "ok": false, "command": null, "result": null,
        "error": {"code": "Not A Code", "category": "usage", "message": "m"}, "warnings": []
    });
    assert!(rejects(bad));
}

/// The schema on its own (no helper checks) encodes the documented contract: every
/// always-present key is required, `ok` decides which of `result`/`error` is null,
/// `result` has its command's type, and an error's category matches its code.
#[test]
fn the_schema_alone_rejects_envelopes_that_break_the_contract() {
    let validator = jsonschema::validator_for(committed_schema()).expect("valid schema");
    let valid = |v: &Value| validator.is_valid(v);
    let error_body = serde_json::json!({
        "code": "invalid_argument", "category": "validation", "message": "m", "retryable": false,
        "retry_after_seconds": null, "hint": null, "provider": null, "provider_status": null,
        "provider_code": null, "provider_request_id": null, "job_id": null, "remote_operation_id": null,
        "job_status": null, "details": null
    });
    let image = serde_json::json!({
        "provider": "openai", "model": "m", "model_source": "flag", "operation": "image.generate",
        "status": "succeeded",
        "created_at": "t", "completed_at": "t", "provider_request_id": null, "artifacts": [],
        "text": null, "usage": null, "cost_estimate": null
    });
    let plan = serde_json::json!({
        "dry_run": true, "provider": "openai", "model": "m", "model_source": "config",
        "operation": "image.generate",
        "async_job": false, "detach": false, "label": null, "wait": null, "billing": "paid", "options": {}, "inputs": [],
        "outputs": [], "credential_present": false, "cost_estimate": null, "max_cost": null,
        "prompt_fingerprint": { "sha256": "00", "chars": 1 }
    });
    let envelope = |ok: bool, command: Value, result: Value, error: Value| {
        serde_json::json!({
            "schema_version": 1, "ok": ok, "command": command, "result": result, "error": error, "warnings": []
        })
    };

    // Accepted: the shapes Iris prints.
    let ok_image = envelope(true, "image.generate".into(), image.clone(), Value::Null);
    assert!(valid(&ok_image));
    assert!(valid(&envelope(true, "image.edit".into(), plan.clone(), Value::Null)), "a dry-run plan");
    assert!(valid(&envelope(true, Value::Null, serde_json::json!({"help": "..."}), Value::Null)));
    assert!(valid(&envelope(false, "jobs.list".into(), Value::Null, error_body.clone())));
    assert!(valid(&envelope(false, Value::Null, Value::Null, error_body.clone())));

    // Missing keys: envelope, result, and error keys are all required.
    assert!(!valid(&serde_json::json!({"schema_version": 1, "ok": true, "warnings": []})));
    for key in ["command", "result", "error", "warnings"] {
        let mut v = ok_image.clone();
        v.as_object_mut().unwrap().remove(key);
        assert!(!valid(&v), "envelope without {key}");
    }
    for key in ["text", "usage", "cost_estimate", "provider_request_id"] {
        let mut result = image.clone();
        result.as_object_mut().unwrap().remove(key);
        assert!(
            !valid(&envelope(true, "image.generate".into(), result, Value::Null)),
            "result without {key}"
        );
    }
    for key in ["retryable", "hint", "provider", "provider_status", "job_id", "details"] {
        let mut error = error_body.clone();
        error.as_object_mut().unwrap().remove(key);
        assert!(!valid(&envelope(false, Value::Null, Value::Null, error)), "error without {key}");
    }

    // ok decides which of result/error is null.
    assert!(!valid(&envelope(true, "image.generate".into(), image.clone(), error_body.clone())));
    assert!(!valid(&envelope(true, "image.generate".into(), Value::Null, Value::Null)));
    assert!(!valid(&envelope(false, "image.generate".into(), image.clone(), error_body.clone())));
    assert!(!valid(&envelope(false, "image.generate".into(), Value::Null, Value::Null)));
    assert!(!valid(&envelope(
        true,
        "jobs.list".into(),
        serde_json::json!({"help": "x"}),
        error_body.clone()
    )));

    // The result must be of the command's type.
    let deleted = serde_json::json!({"deleted": [], "remote_effect": "none", "note": ""});
    assert!(valid(&envelope(true, "jobs.delete".into(), deleted.clone(), Value::Null)));
    assert!(!valid(&envelope(true, "jobs.list".into(), deleted, Value::Null)));
    assert!(!valid(&envelope(true, "jobs.list".into(), serde_json::json!({"help": "x"}), Value::Null)));
    assert!(!valid(&envelope(true, "jobs.status".into(), plan.clone(), Value::Null)), "no plan for jobs");
    assert!(!valid(&envelope(true, "image.generate".into(), serde_json::json!({"jobs": []}), Value::Null)));
    assert!(
        !valid(&envelope(true, Value::Null, serde_json::json!({"jobs": []}), Value::Null)),
        "null is help"
    );

    // An error's category follows its code.
    let mut mismatched = error_body.clone();
    mismatched["code"] = "usage_error".into();
    mismatched["category"] = "quota".into();
    assert!(!valid(&envelope(false, Value::Null, Value::Null, mismatched)));
    let mut matched = error_body.clone();
    matched["code"] = "quota_exceeded".into();
    matched["category"] = "quota".into();
    assert!(valid(&envelope(false, Value::Null, Value::Null, matched)));
    // internal_error is no exception: an unknown code read back from a job record
    // is shown with category internal (the original goes to details.recorded_code).
    let mut unknown = error_body;
    unknown["code"] = "internal_error".into();
    unknown["category"] = "quota".into();
    assert!(!valid(&envelope(false, Value::Null, Value::Null, unknown)));
}

/// Every command has a result mapping, and every mapped type is in `$defs`.
#[test]
fn every_command_maps_to_result_types_in_the_schema() {
    use iris::output::CommandName;
    let schema = committed_schema();
    let commands = known_values(&schema["$defs"]["CommandName"]);
    assert_eq!(commands.len(), CommandName::ALL.len());
    for command in CommandName::ALL {
        let name = serde_json::to_value(command).unwrap();
        assert!(commands.contains(name.as_str().unwrap()), "{name}");
        for def in command.result_types() {
            assert!(schema["$defs"].get(&def).is_some(), "{name}: no $defs/{def}");
        }
    }
}

/// A job record written by a newer Iris can hold an error code this binary does not
/// know; it reads as internal_error with the stored category, and what Iris then
/// prints must still match the published schema.
#[tokio::test]
async fn a_job_error_with_a_newer_code_still_matches_the_schema() {
    let sandbox = Sandbox::new();
    let gemini = Arc::new(FakeProvider::gemini());
    let setup = || CliSetup::new(sandbox.env(), vec![Arc::new(FakeProvider::openai()), gemini.clone()]);
    gemini.videos().push_submit(Err(iris::error::IrisError::new(ErrorCode::QuotaExceeded, "out of quota")));
    let v = run_cli(setup(), &["video", "generate", "-m", "fake-video-1", "boat", "--json"]).await.json();
    let id = v["error"]["job_id"].as_str().unwrap().to_string();
    let path = sandbox.state().join("jobs").join(format!("{id}.json"));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"quota_exceeded\""), "{text}");
    std::fs::write(&path, text.replace("\"quota_exceeded\"", "\"quota_exceeded_for_a_new_reason\"")).unwrap();

    // run_cli validates every --json output against the committed schema.
    let v = run_cli(setup(), &["jobs", "status", &id, "--no-refresh", "--json"]).await.json();
    assert_eq!(v["result"]["job"]["error"]["code"], "internal_error", "{v}");
    assert_eq!(v["result"]["job"]["error"]["category"], "internal", "{v}");
    assert_eq!(
        v["result"]["job"]["error"]["details"]["recorded_code"], "quota_exceeded_for_a_new_reason",
        "{v}"
    );
}

/// The warning codes docs/reference/errors.md lists are exactly the registry's.
#[test]
fn the_documented_warning_codes_are_the_registry() {
    let rows = contract_table("## Warning codes");
    let documented: Vec<String> = rows.iter().map(|row| row[0].clone()).collect();
    let unique: BTreeSet<String> = documented.iter().cloned().collect();
    assert_eq!(unique.len(), documented.len(), "a warning code is documented twice: {documented:?}");
    let registry: BTreeSet<String> = WarningCode::ALL.iter().map(|c| c.as_str().to_string()).collect();
    assert_eq!(unique, registry, "docs/reference/errors.md \"Warning codes\" vs WarningCode::ALL");
    assert!(rows.iter().all(|row| row.len() == 2 && !row[1].is_empty()), "every code has a meaning");
}

/// Warnings are built only from the registry: no source file besides the registry
/// (src/domain.rs) spells out a warning code or builds a `Warning` literally (both
/// would let a code outside the registry reach the output), and every registered
/// code has an emitter.
#[test]
fn warnings_are_only_built_from_the_registry() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let registry = src.join("domain.rs");
    let sources: Vec<(PathBuf, String)> = rust_files(&src)
        .into_iter()
        .filter(|path| *path != registry)
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            (path, text)
        })
        .collect();
    assert!(sources.len() > 10, "found the sources under {}", src.display());
    for (path, text) in &sources {
        for code in WarningCode::ALL {
            assert!(
                !text.contains(&format!("\"{code}\"")),
                "{} spells out the warning code \"{code}\"; use WarningCode::{code:?}",
                path.display()
            );
        }
        for (at, _) in text.match_indices("Warning {") {
            let named = text[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
            let rest = text[at + "Warning {".len()..].trim_start();
            assert!(
                named || !(rest.starts_with("code") || rest.starts_with("message") || rest.starts_with("..")),
                "{} builds a Warning literally; use Warning::new(WarningCode::…, …)",
                path.display()
            );
        }
    }
    for code in WarningCode::ALL {
        let emitter = format!("WarningCode::{code:?}");
        assert!(
            sources.iter().any(|(_, text)| text.contains(&emitter)),
            "{code} is registered (and documented) but nothing emits it"
        );
    }
}

/// docs/reference/errors.md's table of error codes and ErrorCode agree both ways:
/// every code is documented once, every row is a code, each row's category,
/// exit code, and default retryability (the first word of its cell) are the code's,
/// and every code has a meaning.
#[test]
fn the_documented_error_table_is_the_error_codes() {
    let rows = contract_table("## Error codes");
    let documented: Vec<&str> = rows.iter().map(|row| row[0].as_str()).collect();
    let unique: BTreeSet<&str> = documented.iter().copied().collect();
    assert_eq!(unique.len(), documented.len(), "a code is documented twice: {documented:?}");
    let codes: BTreeSet<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(unique, codes, "docs/reference/errors.md error table vs ErrorCode::ALL");
    for row in &rows {
        assert_eq!(row.len(), 5, "{row:?}");
        assert!(!row[4].is_empty(), "{} has no meaning", row[0]);
        let code = *ErrorCode::ALL.iter().find(|c| c.as_str() == row[0]).unwrap();
        let category = serde_json::to_value(code.category()).unwrap();
        assert_eq!(row[1], category.as_str().unwrap(), "category of {code}");
        assert_eq!(row[2], code.exit_code().to_string(), "exit code of {code}");
        let default = match row[3].split([' ', '(']).next().unwrap() {
            "true" => Some(true),
            "false" => Some(false),
            "unspecified" => None,
            other => {
                panic!("{code}: the retryable cell must start with true, false, or unspecified: {other:?}")
            }
        };
        assert_eq!(default, code.default_retryable(), "default retryability of {code}");
    }
}
