//! The TOML config file: strict parsing, credential-key rejection, and positions.

use std::path::Path;

use serde::Deserialize;

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
    #[serde(default)]
    pub providers: ProvidersSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImageSection {
    pub provider: Option<String>,
}

/// Durations are strings (`"10m"`) or integer seconds; validated during resolution
/// so errors can name the key.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VideoSection {
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
pub(crate) struct ProvidersSection {
    #[serde(default)]
    pub openai: ProviderSection,
    #[serde(default)]
    pub gemini: ProviderSection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProviderSection {
    pub base_url: Option<String>,
    pub image_model: Option<String>,
    pub video_model: Option<String>,
    pub request_timeout: Option<toml::Value>,
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
        return Err(key_error(
            path,
            &key,
            "credentials are read only from OPENAI_API_KEY / GEMINI_API_KEY, never from the config file",
        ));
    }
    toml::Value::Table(table).try_into::<FileConfig>().map_err(|e| {
        // Without source input, the error renders as "<message>\nin `<key path>`".
        let rendered = e.to_string();
        let text = rendered.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        file_error(path, redact::truncate(&text, 300))
    })
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
provider = "openai"
[video]
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
        for k in ["base_url", "image_model", "provider", "keyboard", "store_prompts", "tokens_per_minute"] {
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

    #[test]
    fn unknown_keys_name_the_key_and_table() {
        let e = err("[video]\nwait = \"1m\"\n");
        assert!(e.message.contains("unknown field `wait`"), "{}", e.message);
        assert!(e.message.contains("video"), "{}", e.message);
        let e = err("[providers.other]\nbase_url = \"https://x\"\n");
        assert!(e.message.contains("unknown field `other`"), "{}", e.message);
        let e = err("[jobs]\nstore_prompts = \"yes\"\n");
        assert!(e.message.contains("store_prompts"), "{}", e.message);
    }

    #[test]
    fn positions_are_one_based() {
        assert_eq!(position("a\nbc\n", 3), (2, 2));
        assert_eq!(position("", 0), (1, 1));
    }
}
