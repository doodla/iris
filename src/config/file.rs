//! The TOML config file: strict parsing, credential-key rejection, and positions.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::domain::ProviderId;
use crate::error::{ErrorCode, IrisError};
use crate::redact;

/// Parsed config file. Every table rejects unknown keys.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    pub output_dir: Option<String>,
    pub state_dir: Option<String>,
    #[serde(default)]
    pub image: ImageSection,
    #[serde(default)]
    pub video: VideoSection,
    #[serde(default)]
    pub jobs: JobsSection,
    /// `[providers.<id>]` tables, keyed by provider id. [`parse`] rejects any id
    /// that is not in [`ProviderId::ALL`], so a new provider gets its table without
    /// a new field here.
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSection>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImageSection {
    pub model: Option<String>,
}

/// Durations are strings (`"10m"`) or integer seconds; validated during resolution
/// so errors can name the key.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoSection {
    pub model: Option<String>,
    pub wait_timeout: Option<toml::Value>,
    pub poll_interval: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JobsSection {
    pub store_prompts: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderSection {
    pub base_url: Option<String>,
    pub request_timeout: Option<toml::Value>,
    pub submit_timeout: Option<toml::Value>,
}

/// A `config_invalid` error about the file at `path`.
pub(crate) fn file_error(path: &Path, message: impl std::fmt::Display) -> IrisError {
    IrisError::new(
        ErrorCode::ConfigInvalid,
        redact::scrub(&format!("config file {}: {message}", path.display())).into_owned(),
    )
    .with_detail("path", path.display().to_string())
    .with_hint("fix the config file, or point --config / IRIS_CONFIG at another file")
}

/// A `config_invalid` error about `key` in the file at `path`.
pub(crate) fn key_error(path: &Path, key: &str, message: impl std::fmt::Display) -> IrisError {
    file_error(path, format!("`{key}`: {message}")).with_detail("key", key)
}

/// Read and parse the config file. `Ok(None)` if it does not exist.
pub(crate) fn load(path: &Path) -> Result<Option<FileConfig>, IrisError> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(file_error(path, format!("cannot be read: {e}"))),
    };
    let text = String::from_utf8(bytes).map_err(|_| file_error(path, "is not valid UTF-8"))?;
    parse(path, &text).map(Some)
}

/// Parse config text. Errors never quote source lines (a misplaced key could be on
/// them); they give line/column and the key path instead.
pub(crate) fn parse(path: &Path, text: &str) -> Result<FileConfig, IrisError> {
    let table = text.parse::<toml::Table>().map_err(|e| {
        let at = e.span().map(|s| position(text, s.start)).map(|(l, c)| format!(" at line {l}, column {c}"));
        file_error(path, format!("invalid TOML{}: {}", at.unwrap_or_default(), e.message()))
    })?;
    if let Some(key) = find_credential_key(&table, "") {
        let vars: Vec<&str> = ProviderId::ALL.iter().map(|p| p.credential_env()).collect();
        return Err(key_error(
            path,
            &key,
            format!("credentials are read only from {}, never from the config file", vars.join(" / ")),
        ));
    }
    // Checked before the typed pass, which reads each table's contents first: a
    // misspelled `[providers.<id>]` table is reported as unknown itself, never as a
    // problem with a key inside it.
    if let Some(toml::Value::Table(providers)) = table.get("providers")
        && let Some(name) = providers.keys().find(|k| !ProviderId::ALL.iter().any(|p| p.as_str() == *k))
    {
        let known: Vec<String> = ProviderId::ALL.iter().map(|p| format!("`{p}`")).collect();
        return Err(key_error(
            path,
            &format!("providers.{name}"),
            format!("unknown key; expected one of {}", known.join(", ")),
        ));
    }
    toml::Value::Table(table).try_into::<FileConfig>().map_err(|e| schema_error(path, &e))
}

/// A `config_invalid` error for the typed pass (unknown key or wrong type): the key
/// path goes to `details.key` (as [`key_error`] does), and the message names the
/// expected and found *types* but never quotes the offending value, which could be
/// a misplaced secret.
fn schema_error(path: &Path, e: &toml::de::Error) -> IrisError {
    // Without source input, the error renders as "<message>\nin `<key path>`"; toml
    // offers no other accessor for the key path.
    let rendered = e.to_string();
    let table = rendered
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("in `")?.strip_suffix('`'))
        .filter(|k| !k.is_empty());
    let message = e.message().trim();
    let join = |field: &str| match table {
        Some(t) => format!("{t}.{field}"),
        None => field.to_string(),
    };
    let (key, text) = if let Some(rest) = message.strip_prefix("unknown field `")
        && let Some((field, tail)) = rest.split_once('`')
    {
        let tail = tail.trim_start_matches(',').trim();
        let text = if tail.is_empty() { "unknown key".to_string() } else { format!("unknown key; {tail}") };
        (Some(join(field)), text)
    } else if let Some(rest) = message.strip_prefix("invalid type: ") {
        (table.map(str::to_string), found_expected("wrong type", rest))
    } else if let Some(rest) = message.strip_prefix("invalid value: ") {
        (table.map(str::to_string), found_expected("invalid value", rest))
    } else {
        (table.map(str::to_string), without_quoted(message))
    };
    let text = redact::truncate(&text, 300);
    match key {
        Some(key) => key_error(path, &key, text),
        None => file_error(path, text),
    }
}

/// `"<what> (found <kind>, expected <type>)"` from serde's `"<kind> <value>, expected
/// <type>"`, dropping the value (serde quotes it after the kind).
fn found_expected(what: &str, rest: &str) -> String {
    let (found, expected) = rest.split_once(", expected ").unwrap_or((rest, ""));
    let kind = found.split(['"', '`']).next().unwrap_or("").trim();
    let kind = if kind.is_empty() { "a value" } else { kind };
    if expected.is_empty() {
        format!("{what} (found {kind})")
    } else {
        format!("{what} (found {kind}, expected {})", without_quoted(expected))
    }
}

/// Replace every `"…"` and `` `…` `` quoted span with `…`.
fn without_quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut open: Option<char> = None;
    for c in text.chars() {
        match open {
            Some(q) if c == q => {
                out.push('…');
                out.push(c);
                open = None;
            }
            Some(_) => {}
            None if c == '"' || c == '`' => {
                out.push(c);
                open = Some(c);
            }
            None => out.push(c),
        }
    }
    if open.is_some() {
        out.push('…');
    }
    out
}

/// 1-based line and column of a byte offset.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
    (line, column)
}

/// True for key names that look like credentials: `key`, `*_key`, `*-key`, `apikey`,
/// and `token` / `secret` / `password` / `passwd` alone or as a suffix.
pub(crate) fn is_credential_like(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if n == "key" || n == "apikey" || n.ends_with("_key") || n.ends_with("-key") {
        return true;
    }
    ["token", "secret", "password", "passwd"]
        .iter()
        .any(|w| n == *w || n.ends_with(&format!("_{w}")) || n.ends_with(&format!("-{w}")))
}

/// Path of the first credential-like key at any depth (tables, arrays of tables).
fn find_credential_key(table: &toml::Table, prefix: &str) -> Option<String> {
    for (name, value) in table {
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}.{name}") };
        if is_credential_like(name) {
            return Some(path);
        }
        if let Some(found) = find_in_value(value, &path) {
            return Some(found);
        }
    }
    None
}

fn find_in_value(value: &toml::Value, path: &str) -> Option<String> {
    match value {
        toml::Value::Table(t) => find_credential_key(t, path),
        toml::Value::Array(items) => {
            items.iter().enumerate().find_map(|(i, v)| find_in_value(v, &format!("{path}[{i}]")))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: &str = "/cfg/config.toml";

    fn err(text: &str) -> IrisError {
        parse(Path::new(P), text).unwrap_err()
    }

    #[test]
    fn full_example_parses() {
        let cfg = parse(
            Path::new(P),
            r#"
output_dir = "~/Pictures/iris"
state_dir = "/custom/state"
[image]
model = "gpt-image-2.5-flare"
[video]
model = "veo-3.1-lite-generate-preview"
wait_timeout = "10m"
poll_interval = 15
[jobs]
store_prompts = false
[providers.openai]
base_url = "https://api.openai.com/v1"
request_timeout = "300s"
[providers.gemini]
base_url = "https://generativelanguage.googleapis.com"
"#,
        )
        .unwrap();
        assert_eq!(cfg.output_dir.as_deref(), Some("~/Pictures/iris"));
        assert_eq!(cfg.video.poll_interval, Some(toml::Value::Integer(15)));
        assert_eq!(cfg.jobs.store_prompts, Some(false));
    }

    #[test]
    fn credential_like_keys_are_detected() {
        for k in [
            "api_key",
            "API_KEY",
            "key",
            "openai-key",
            "apikey",
            "token",
            "access_token",
            "secret",
            "password",
        ] {
            assert!(is_credential_like(k), "{k}");
        }
        for k in ["base_url", "model", "keyboard", "store_prompts", "tokens_per_minute"] {
            assert!(!is_credential_like(k), "{k}");
        }
    }

    #[test]
    fn credential_keys_are_rejected_at_any_depth_without_echoing_values() {
        let e = err("[providers.openai]\napi_key = \"sk-live-abcdefghijkl\"\n");
        assert_eq!(e.code, ErrorCode::ConfigInvalid);
        assert!(e.message.contains("providers.openai.api_key"), "{}", e.message);
        assert!(e.message.contains("OPENAI_API_KEY"), "{}", e.message);
        assert!(!e.message.contains("sk-live"), "{}", e.message);
        let e = err("[[extra]]\nname = \"a\"\n[[extra]]\nToken = \"t-123456789\"\n");
        assert!(e.message.contains("extra[1].Token"), "{}", e.message);
    }

    #[test]
    fn syntax_errors_give_positions_but_not_source_lines() {
        let e = err("output_dir = \"/x\"\nbroken = sk-live-abcdefghijkl\n");
        assert!(e.message.contains("line 2"), "{}", e.message);
        assert!(!e.message.contains("sk-live"), "{}", e.message);
    }

    fn key(e: &IrisError) -> Option<&str> {
        e.details.get("key").and_then(|v| v.as_str())
    }

    #[test]
    fn unknown_keys_name_the_key_and_table() {
        let e = err("[video]\nwait = \"1m\"\n");
        assert_eq!(key(&e), Some("video.wait"));
        assert!(e.message.contains("`video.wait`: unknown key"), "{}", e.message);
        assert!(e.message.contains("wait_timeout"), "the expected keys are listed: {}", e.message);
        let e = err("[providers.other]\nbase_url = \"https://x\"\n");
        assert_eq!(key(&e), Some("providers.other"));
        assert!(e.message.contains("expected one of `openai`, `gemini`"), "{}", e.message);
        let e = err("outptu_dir = \"/x\"\n");
        assert_eq!(key(&e), Some("outptu_dir"));
    }

    #[test]
    fn an_unknown_provider_table_is_reported_before_its_contents() {
        // An unknown key inside it: the table name is the error, not the key.
        let e = err("[providers.other]\nx = 1\n");
        assert_eq!(e.code, ErrorCode::ConfigInvalid);
        assert_eq!(key(&e), Some("providers.other"));
        assert!(e.message.contains("`providers.other`: unknown key; expected one of"), "{}", e.message);
        // A misspelled provider with a wrongly typed value: the misspelling is the error.
        let e = err("[providers.opneai]\nbase_url = 5\n");
        assert_eq!(key(&e), Some("providers.opneai"));
        assert!(!e.message.contains("wrong type"), "{}", e.message);
        // Dotted keys and inline tables name the same table.
        assert_eq!(key(&err("providers.opneai.base_url = \"https://x\"\n")), Some("providers.opneai"));
        assert_eq!(key(&err("providers = { other = { x = 1 } }\n")), Some("providers.other"));
        // A known provider's table is still checked key by key.
        let e = err("[providers.openai]\nx = 1\n");
        assert_eq!(key(&e), Some("providers.openai.x"));
        // `providers` itself of the wrong type is a type error, not an unknown table.
        let e = err("providers = 5\n");
        assert_eq!(key(&e), Some("providers"));
        assert!(e.message.contains("wrong type"), "{}", e.message);
    }

    #[test]
    fn an_array_of_providers_is_a_type_error() {
        // `providers` holds one table per provider id, so an array is rejected by the
        // `providers` key itself, even an empty one, and even as an array of tables.
        for text in ["providers = []\n", "providers = [{}]\n", "[[providers]]\nx = 1\n"] {
            let e = err(text);
            assert_eq!(e.code, ErrorCode::ConfigInvalid, "{text}");
            assert_eq!(key(&e), Some("providers"), "{text}");
            assert!(
                e.message.contains("`providers`: wrong type (found sequence, expected a map)"),
                "{text}: {}",
                e.message
            );
        }
    }

    #[test]
    fn wrong_types_name_the_key_and_never_quote_the_value() {
        let e = err("[jobs]\nstore_prompts = \"sk-live-abcdefghijkl\"\n");
        assert_eq!(e.code, ErrorCode::ConfigInvalid);
        assert_eq!(key(&e), Some("jobs.store_prompts"));
        assert!(e.message.contains("wrong type (found string, expected a boolean)"), "{}", e.message);
        assert!(!e.message.contains("sk-live"), "{}", e.message);
        let e = err("[providers.openai]\nbase_url = 12345678901\n");
        assert_eq!(key(&e), Some("providers.openai.base_url"));
        assert!(e.message.contains("found integer"), "{}", e.message);
        assert!(!e.message.contains("12345678901"), "{}", e.message);
        let e = err("output_dir = [\"sk-live-abcdefghijkl\"]\n");
        assert_eq!(key(&e), Some("output_dir"));
        assert!(!e.message.contains("sk-live"), "{}", e.message);
    }

    #[test]
    fn quoted_spans_are_elided() {
        assert_eq!(without_quoted(r#"a "secret" and `x` end"#), "a \"…\" and `…` end");
        assert_eq!(without_quoted(r#"open "rest"#), "open \"…");
        assert_eq!(
            found_expected("wrong type", r#"string "v", expected a boolean"#),
            "wrong type (found string, expected a boolean)"
        );
        assert_eq!(
            found_expected("invalid value", "integer `-5`, expected u64"),
            "invalid value (found integer, expected u64)"
        );
    }

    #[test]
    fn positions_are_one_based() {
        assert_eq!(position("a\nbc\n", 3), (2, 2));
        assert_eq!(position("", 0), (1, 1));
    }
}
