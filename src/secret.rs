//! A credential value that cannot be accidentally printed or serialized.

use std::fmt;

/// An API key read from the environment. `Debug`/`Display` never reveal it and it
/// does not implement `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Secret(value.into())
    }

    /// Read a non-empty value from an environment variable.
    pub fn from_env(var: &str) -> Option<Secret> {
        match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => Some(Secret(v.trim().to_string())),
            _ => None,
        }
    }

    /// The raw value. Only for building an HTTP header; never for messages or logs.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_display_hide_the_value() {
        let s = Secret::new("sk-very-secret-value");
        assert!(!format!("{s:?}").contains("very-secret"));
        assert!(!format!("{s}").contains("very-secret"));
        assert_eq!(s.expose(), "sk-very-secret-value");
    }
}
