//! Redaction of secrets and signed URLs before anything is printed, logged, or persisted.

use std::borrow::Cow;

use crate::domain::ProviderId;

/// Query parameters whose values are safe to show.
const SAFE_QUERY_KEYS: &[&str] = &["alt"];

/// Redact a URL for display: drop userinfo, replace every query value (except an
/// allowlist) with `REDACTED`, and drop the fragment. Unparseable input is replaced
/// entirely so nothing sensitive leaks through.
pub fn redact_url(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "[unparseable URL redacted]".to_string();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_fragment(None);
    if url.query().is_some() {
        let pairs: Vec<(String, String)> = url
            .query_pairs()
            .map(|(k, v)| {
                let keep = SAFE_QUERY_KEYS.iter().any(|s| s.eq_ignore_ascii_case(&k));
                (k.into_owned(), if keep { v.into_owned() } else { "REDACTED".to_string() })
            })
            .collect();
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    url.to_string()
}

/// Current credential values from the environment (only values long enough to be
/// meaningful, so short strings are never blanket-replaced).
fn secret_values() -> Vec<String> {
    ProviderId::ALL
        .iter()
        .filter_map(|p| std::env::var(p.credential_env()).ok())
        .map(|v| v.trim().to_string())
        .filter(|v| v.len() >= 8)
        .collect()
}

/// Replace every occurrence of a configured credential value with `[REDACTED]`.
pub fn scrub(text: &str) -> Cow<'_, str> {
    scrub_with(text, &secret_values())
}

/// `scrub` with an explicit secret list (for tests and callers holding secrets).
pub fn scrub_with<'a>(text: &'a str, secrets: &[String]) -> Cow<'a, str> {
    let mut out = Cow::Borrowed(text);
    for s in secrets {
        if s.len() >= 8 && out.contains(s.as_str()) {
            out = Cow::Owned(out.replace(s.as_str(), "[REDACTED]"));
        }
    }
    out
}

/// Scrub all string values inside a JSON value, recursively.
pub fn scrub_json(value: &mut serde_json::Value) {
    let secrets = secret_values();
    scrub_json_with(value, &secrets);
}

fn scrub_json_with(value: &mut serde_json::Value, secrets: &[String]) {
    match value {
        serde_json::Value::String(s) => {
            if let Cow::Owned(new) = scrub_with(s, secrets) {
                *s = new;
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| scrub_json_with(v, secrets)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| scrub_json_with(v, secrets)),
        _ => {}
    }
}

/// Truncate provider-supplied text to `max` characters (on a char boundary).
pub fn truncate(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((idx, _)) => format!("{}…", &text[..idx]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_query_values_and_userinfo() {
        let r = redact_url(
            "https://user:pw@storage.example.com/v/abc.mp4?X-Goog-Signature=deadbeef&alt=media#frag",
        );
        assert!(!r.contains("deadbeef"));
        assert!(!r.contains("pw"));
        assert!(r.contains("alt=media"));
        assert!(r.contains("X-Goog-Signature=REDACTED"));
        assert!(!r.contains("frag"));
    }

    #[test]
    fn unparseable_urls_are_fully_redacted() {
        assert_eq!(redact_url("not a url?token=abc"), "[unparseable URL redacted]");
    }

    #[test]
    fn scrub_replaces_known_secrets_only() {
        let secrets = vec!["sk-test-1234567890".to_string(), "short".to_string()];
        let s = scrub_with("key sk-test-1234567890 and short word", &secrets);
        assert_eq!(s, "key [REDACTED] and short word");
    }

    #[test]
    fn truncate_is_char_safe() {
        assert_eq!(truncate("héllo", 2), "hé…");
        assert_eq!(truncate("hi", 5), "hi");
    }
}
