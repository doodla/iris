//! The versioned JSON envelope (see docs/json-contract.md) and the error object.

use schemars::JsonSchema;
use serde::Serialize;

use crate::domain::{Billing, JobStatus, ProviderId, Warning, WarningCode};
use crate::error::{ErrorCategory, ErrorCode, IrisError};
use crate::redact;

use super::results::*;

/// Major version of the JSON output contract.
pub const SCHEMA_VERSION: u32 = 1;

/// The published schema's `$id`: where the committed file of this major version is
/// served from.
pub const SCHEMA_ID: &str =
    "https://raw.githubusercontent.com/doodla/iris/main/schema/iris-output.v1.schema.json";

/// The form of every error code and warning code. Both are open sets: a later
/// version of the same `schema_version` may add values of this form.
pub const CODE_PATTERN: &str = "^[a-z][a-z0-9_]*$";

/// The form of every envelope `command`: codes joined by dots (`image.generate`).
/// An open set like the codes.
pub const COMMAND_PATTERN: &str = "^[a-z][a-z0-9_]*(\\.[a-z][a-z0-9_]*)*$";

/// Command identifiers used in the envelope's `command` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, JsonSchema)]
pub enum CommandName {
    #[serde(rename = "image.generate")]
    ImageGenerate,
    #[serde(rename = "image.edit")]
    ImageEdit,
    #[serde(rename = "video.generate")]
    VideoGenerate,
    #[serde(rename = "jobs.list")]
    JobsList,
    #[serde(rename = "jobs.status")]
    JobsStatus,
    #[serde(rename = "jobs.wait")]
    JobsWait,
    #[serde(rename = "jobs.download")]
    JobsDownload,
    #[serde(rename = "jobs.delete")]
    JobsDelete,
    #[serde(rename = "models.list")]
    ModelsList,
    #[serde(rename = "models.show")]
    ModelsShow,
    #[serde(rename = "providers.list")]
    ProvidersList,
    #[serde(rename = "config.show")]
    ConfigShow,
    #[serde(rename = "config.path")]
    ConfigPath,
    #[serde(rename = "doctor")]
    Doctor,
    #[serde(rename = "schema")]
    Schema,
    #[serde(rename = "completions")]
    Completions,
    #[serde(rename = "version")]
    Version,
}

impl CommandName {
    /// Every command, in the order of the command tree.
    pub const ALL: &'static [CommandName] = &[
        CommandName::ImageGenerate,
        CommandName::ImageEdit,
        CommandName::VideoGenerate,
        CommandName::JobsList,
        CommandName::JobsStatus,
        CommandName::JobsWait,
        CommandName::JobsDownload,
        CommandName::JobsDelete,
        CommandName::ModelsList,
        CommandName::ModelsShow,
        CommandName::ProvidersList,
        CommandName::ConfigShow,
        CommandName::ConfigPath,
        CommandName::Doctor,
        CommandName::Schema,
        CommandName::Completions,
        CommandName::Version,
    ];

    /// Schema names (`$defs`) of the results a successful envelope of this command
    /// carries. (`--help` results have `command: null`.)
    pub fn result_types(self) -> Vec<String> {
        fn name<T: JsonSchema>() -> String {
            T::schema_name().into_owned()
        }
        match self {
            CommandName::ImageGenerate | CommandName::ImageEdit => {
                vec![name::<ImageResult>(), name::<PlanResult>()]
            }
            CommandName::VideoGenerate => vec![name::<JobResult>(), name::<PlanResult>()],
            CommandName::JobsStatus | CommandName::JobsWait | CommandName::JobsDownload => {
                vec![name::<JobResult>()]
            }
            CommandName::JobsList => vec![name::<JobListResult>()],
            CommandName::JobsDelete => vec![name::<JobDeleteResult>()],
            CommandName::ModelsList => vec![name::<ModelListResult>()],
            CommandName::ModelsShow => vec![name::<ModelShowResult>()],
            CommandName::ProvidersList => vec![name::<ProviderListResult>()],
            CommandName::ConfigShow => vec![name::<ConfigShowResult>()],
            CommandName::ConfigPath => vec![name::<ConfigPathResult>()],
            CommandName::Doctor => vec![name::<DoctorResult>()],
            CommandName::Schema => vec![name::<SchemaResult>()],
            CommandName::Completions => vec![name::<CompletionsResult>()],
            CommandName::Version => vec![name::<VersionResult>()],
        }
    }
}

/// Every possible `result` payload. Serialized untagged; the envelope's `command`
/// tells which variant applies (see the `$defs` in the published schema).
// Built once per process for printing; variant size is irrelevant.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum ResultPayload {
    Image(ImageResult),
    Job(JobResult),
    JobList(JobListResult),
    JobDelete(JobDeleteResult),
    ModelList(ModelListResult),
    ModelShow(ModelShowResult),
    ProviderList(ProviderListResult),
    ConfigShow(ConfigShowResult),
    ConfigPath(ConfigPathResult),
    Doctor(DoctorResult),
    Schema(SchemaResult),
    Completions(CompletionsResult),
    Version(VersionResult),
    Help(HelpResult),
    Plan(PlanResult),
}

/// The error object of a failed command.
#[derive(Debug, Clone, Serialize, serde::Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub category: ErrorCategory,
    pub message: String,
    /// Whether running the same command again may succeed; null if unknown.
    pub retryable: Option<bool>,
    pub retry_after_seconds: Option<u64>,
    pub hint: Option<String>,
    pub provider: Option<ProviderId>,
    pub provider_status: Option<u16>,
    /// Provider's own code (informational, unstable).
    pub provider_code: Option<String>,
    pub provider_request_id: Option<String>,
    pub job_id: Option<String>,
    pub remote_operation_id: Option<String>,
    pub job_status: Option<JobStatus>,
    pub details: Option<serde_json::Map<String, serde_json::Value>>,
}

impl From<&IrisError> for ErrorBody {
    fn from(e: &IrisError) -> Self {
        let s = |t: &str| redact::scrub(t).into_owned();
        let details = if e.details.is_empty() {
            None
        } else {
            let mut v = serde_json::Value::Object(e.details.clone());
            redact::scrub_json(&mut v);
            match v {
                serde_json::Value::Object(m) => Some(m),
                _ => None,
            }
        };
        ErrorBody {
            code: e.code,
            category: e.code.category(),
            message: s(&e.message),
            retryable: e.retryable,
            retry_after_seconds: e.retry_after.map(|d| d.as_secs().max(1)),
            hint: e.hint.as_deref().map(s),
            provider: e.provider,
            provider_status: e.provider_status,
            provider_code: e.provider_code.as_deref().map(s),
            provider_request_id: e.provider_request_id.as_deref().map(s),
            job_id: e.job_id.clone(),
            remote_operation_id: e.remote_operation_id.as_deref().map(s),
            job_status: e.job_status,
            details,
        }
    }
}

/// The single JSON document printed on stdout in `--json` mode.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Envelope {
    pub schema_version: u32,
    pub ok: bool,
    pub command: Option<CommandName>,
    pub result: Option<ResultPayload>,
    pub error: Option<ErrorBody>,
    pub warnings: Vec<Warning>,
}

impl Envelope {
    pub fn success(command: CommandName, result: ResultPayload, warnings: Vec<Warning>) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            ok: true,
            command: Some(command),
            result: Some(result),
            error: None,
            warnings,
        }
    }

    pub fn failure(command: Option<CommandName>, error: &IrisError, warnings: Vec<Warning>) -> Self {
        Envelope {
            schema_version: SCHEMA_VERSION,
            ok: false,
            command,
            result: None,
            error: Some(ErrorBody::from(error)),
            warnings,
        }
    }

    /// Serialize as one line of JSON (secret-scrubbed as a final safety net).
    pub fn to_json_line(&self) -> String {
        let mut value = serde_json::to_value(self).expect("envelope serializes");
        redact::scrub_json(&mut value);
        let mut s = serde_json::to_string(&value).expect("value serializes");
        s.push('\n');
        s
    }
}

/// The published JSON Schema for the envelope, including every result type in `$defs`.
///
/// It is derived from the serialized types for the *serialize* contract, so every
/// field that is always written is `required` (and nullable when it is an
/// `Option`); output types never skip a field. A deterministic transform then adds
/// what the types cannot express (see [`add_contract_rules`]).
pub fn schema() -> serde_json::Value {
    let generator = schemars::generate::SchemaSettings::default().for_serialize().into_generator();
    let schema = generator.into_root_schema_for::<Envelope>();
    let mut value = serde_json::to_value(schema).expect("schema serializes");
    add_contract_rules(&mut value);
    value
}

/// Rules of the documented contract beyond the shapes of the types:
///
/// * the document's `$id`, and `schema_version` fixed at [`SCHEMA_VERSION`];
/// * `ok: true` ⇔ `result` is not null and `error` is null (`ok: false` ⇔ the
///   reverse);
/// * a successful envelope's `result` has its command's type
///   ([`CommandName::result_types`]), and one with `command: null` is `--help`;
/// * an error's `category` is the one of its `code` (an error read back from a job
///   record written by a newer Iris shows an unknown code as `internal_error` with
///   category `internal`, keeping the original in `details.recorded_code`);
/// * error codes, commands, warning codes, provider ids, and billing values are open
///   sets (see [`open_set`]): adding a value is an additive change, which a consumer
///   validating with this version's schema keeps accepting; the rules above apply to
///   the known values.
fn add_contract_rules(schema: &mut serde_json::Value) {
    use serde_json::json;
    let def = |name: &str| json!({ "$ref": format!("#/$defs/{name}") });
    let success_with = |command: serde_json::Value| json!({ "properties": { "ok": { "const": true }, "command": command }, "required": ["ok", "command"] });
    let mut rules = vec![json!({
        "if": { "properties": { "ok": { "const": true } }, "required": ["ok"] },
        "then": { "properties": { "result": { "not": { "type": "null" } }, "error": { "type": "null" } } },
        "else": { "properties": { "result": { "type": "null" }, "error": { "not": { "type": "null" } } } }
    })];
    rules.push(json!({
        "if": success_with(json!({ "type": "null" })),
        "then": { "properties": { "result": def(&HelpResult::schema_name()) } }
    }));
    for command in CommandName::ALL {
        let name = serde_json::to_value(command).expect("command names serialize");
        let types: Vec<serde_json::Value> = command.result_types().iter().map(|t| def(t)).collect();
        let result = if types.len() == 1 { types[0].clone() } else { json!({ "anyOf": types }) };
        rules.push(json!({
            "if": success_with(json!({ "const": name })),
            "then": { "properties": { "result": result } }
        }));
    }
    schema["allOf"] = serde_json::Value::Array(rules);

    let categories: Vec<serde_json::Value> = ErrorCode::ALL
        .iter()
        .map(|code| {
            json!({
                "if": { "properties": { "code": { "const": code.as_str() } }, "required": ["code"] },
                "then": { "properties": { "category": { "const": code.category() } } }
            })
        })
        .collect();
    let name = ErrorBody::schema_name();
    schema["$defs"][name.as_ref()]["allOf"] = serde_json::Value::Array(categories);

    schema["$id"] = json!(SCHEMA_ID);
    schema["properties"]["schema_version"] = json!({
        "description": "Major version of the JSON output contract; this schema describes version 1.",
        "type": "integer",
        "const": SCHEMA_VERSION,
    });
    let codes: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    schema["$defs"][ErrorCode::schema_name().as_ref()] = open_set(
        "Stable, public error code (docs/json-contract.md): one of the listed codes, or a code a later version \
         of this schema_version adds.",
        &codes,
        CODE_PATTERN,
    );
    let commands: Vec<serde_json::Value> =
        CommandName::ALL.iter().map(|c| serde_json::to_value(c).expect("command names serialize")).collect();
    let commands: Vec<&str> = commands.iter().filter_map(serde_json::Value::as_str).collect();
    schema["$defs"][CommandName::schema_name().as_ref()] = open_set(
        "Command identifier: one of the listed commands, or a command a later version of this schema_version adds.",
        &commands,
        COMMAND_PATTERN,
    );
    let providers: Vec<&str> = ProviderId::ALL.iter().map(|p| p.as_str()).collect();
    schema["$defs"][ProviderId::schema_name().as_ref()] = open_set(
        "Provider identifier: one of the listed providers, or a provider a later version of this schema_version \
         adds.",
        &providers,
        CODE_PATTERN,
    );
    let billing: Vec<&str> = Billing::ALL.iter().map(|b| b.as_str()).collect();
    let meanings: Vec<String> = Billing::ALL.iter().map(|b| format!("{b}: {}", b.description())).collect();
    schema["$defs"][Billing::schema_name().as_ref()] = open_set(
        &format!(
            "Whether a model's requests cost money: one of the listed values ({}), or a value a later version of \
             this schema_version adds. Read a value you do not know as: requests may cost money.",
            meanings.join("; ")
        ),
        &billing,
        CODE_PATTERN,
    );
    let warnings: Vec<&str> = WarningCode::ALL.iter().map(|c| c.as_str()).collect();
    schema["$defs"][Warning::schema_name().as_ref()]["properties"]["code"] = open_set(
        "Stable warning code (docs/json-contract.md): one of the listed codes, or a code a later version of this \
         schema_version adds. Treat a code you do not know as informational text.",
        &warnings,
        CODE_PATTERN,
    );
}

/// An open set of strings: the `known` values are listed (for readers and code
/// generators), and any other value of the form `pattern` is accepted too.
fn open_set(description: &str, known: &[&str], pattern: &str) -> serde_json::Value {
    serde_json::json!({
        "description": description,
        "type": "string",
        "anyOf": [{ "enum": known }, { "pattern": pattern }],
    })
}
