//! Validation of JSON envelopes against the committed schema
//! `schema/iris-output.v1.schema.json` (see docs/reference/json-output.md): the envelope itself, then `result`
//! against the `$defs` type of its command (or `error` against `ErrorBody`).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

/// The committed schema document.
pub fn committed_schema() -> &'static Value {
    static SCHEMA: OnceLock<Value> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/schema/iris-output.v1.schema.json");
        serde_json::from_str(&std::fs::read_to_string(path).expect("committed schema"))
            .expect("schema is JSON")
    })
}

/// Validate `instance` against `#/$defs/<name>` (`None` = the envelope). Compiled
/// validators are cached per definition.
fn check(def: Option<&str>, instance: &Value) {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static jsonschema::Validator>>> = OnceLock::new();
    let key = def.unwrap_or("").to_string();
    let validator = {
        let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
        *cache.entry(key).or_insert_with(|| {
            let schema = committed_schema();
            let doc = match def {
                None => schema.clone(),
                Some(name) => {
                    assert!(schema["$defs"].get(name).is_some(), "the schema has no $defs/{name}");
                    json!({ "$schema": schema["$schema"], "$ref": format!("#/$defs/{name}"), "$defs": schema["$defs"] })
                }
            };
            Box::leak(Box::new(jsonschema::validator_for(&doc).expect("valid JSON Schema")))
        })
    };
    let errors: Vec<String> =
        validator.iter_errors(instance).map(|e| format!("{e} at {}", e.instance_path())).collect();
    assert!(errors.is_empty(), "does not match {}: {errors:?}\n{instance}", def.unwrap_or("the envelope"));
}

/// The `$defs` result type of a successful envelope for `command` (see docs/reference/json-output.md).
pub fn result_def(command: Option<&str>, result: &Value) -> &'static str {
    let only_help = result.as_object().is_some_and(|o| o.len() == 1 && o.contains_key("help"));
    if only_help {
        return "HelpResult";
    }
    if result.get("dry_run").is_some() {
        return "PlanResult";
    }
    match command.expect("successful envelopes name their command") {
        "image.generate" | "image.edit" => "ImageResult",
        "video.generate" | "jobs.status" | "jobs.wait" | "jobs.download" => "JobResult",
        "jobs.list" => "JobListResult",
        "jobs.delete" => "JobDeleteResult",
        "models.list" => "ModelListResult",
        "models.show" => "ModelShowResult",
        "providers.list" => "ProviderListResult",
        "config.show" => "ConfigShowResult",
        "config.path" => "ConfigPathResult",
        "doctor" => "DoctorResult",
        "schema" => "SchemaResult",
        "completions" => "CompletionsResult",
        "version" => "VersionResult",
        other => panic!("unknown command {other}"),
    }
}

/// Validate one envelope: its shape (both `result` and `error` keys, exactly one
/// non-null), the envelope schema, and the command's result type or `ErrorBody`.
pub fn assert_matches_schema(envelope: &Value) {
    check(None, envelope);
    let obj = envelope.as_object().expect("envelope is an object");
    for key in ["schema_version", "ok", "command", "result", "error", "warnings"] {
        assert!(obj.contains_key(key), "envelope lacks {key}: {envelope}");
    }
    let ok = envelope["ok"].as_bool().unwrap();
    assert_eq!(envelope["result"].is_null(), !ok, "exactly one of result/error: {envelope}");
    assert_eq!(envelope["error"].is_null(), ok, "exactly one of result/error: {envelope}");
    if ok {
        let def = result_def(envelope["command"].as_str(), &envelope["result"]);
        check(Some(def), &envelope["result"]);
    } else {
        check(Some("ErrorBody"), &envelope["error"]);
    }
}

/// Codes of the envelope's warnings.
pub fn warning_codes(envelope: &Value) -> Vec<String> {
    envelope["warnings"].as_array().unwrap().iter().map(|w| w["code"].as_str().unwrap().to_string()).collect()
}
