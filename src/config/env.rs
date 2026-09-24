//! A snapshot of the process environment, so resolution is a pure function of its
//! inputs and tests never mutate the real environment.

use std::collections::{BTreeMap, BTreeSet};
use std::env::VarError;
use std::fmt;
use std::path::{Path, PathBuf};

use super::paths::Platform;
use crate::domain::ProviderId;
use crate::error::IrisError;
use crate::secret::Secret;

/// `IRIS_CONFIG`: config file path.
pub const ENV_CONFIG: &str = "IRIS_CONFIG";
/// `IRIS_OUTPUT_DIR`: default output directory.
pub const ENV_OUTPUT_DIR: &str = "IRIS_OUTPUT_DIR";
/// `IRIS_STATE_DIR`: job state directory.
pub const ENV_STATE_DIR: &str = "IRIS_STATE_DIR";
/// `IRIS_IMAGE_PROVIDER`: default image provider.
pub const ENV_IMAGE_PROVIDER: &str = "IRIS_IMAGE_PROVIDER";
/// `IRIS_WAIT_TIMEOUT`: caller wait limit for video jobs.
pub const ENV_WAIT_TIMEOUT: &str = "IRIS_WAIT_TIMEOUT";
/// `IRIS_POLL_INTERVAL`: poll interval for video jobs.
pub const ENV_POLL_INTERVAL: &str = "IRIS_POLL_INTERVAL";
/// `IRIS_STORE_PROMPTS`: keep prompt text in job records.
pub const ENV_STORE_PROMPTS: &str = "IRIS_STORE_PROMPTS";
/// `IRIS_OPENAI_BASE_URL`: OpenAI API base URL override.
pub const ENV_OPENAI_BASE_URL: &str = "IRIS_OPENAI_BASE_URL";
/// `IRIS_GEMINI_BASE_URL`: Gemini API base URL override.
pub const ENV_GEMINI_BASE_URL: &str = "IRIS_GEMINI_BASE_URL";
/// `IRIS_LOG`: tracing filter directives.
pub const ENV_LOG: &str = "IRIS_LOG";

/// Non-secret variables captured by [`EnvSnapshot::from_process`].
pub const SETTING_VARS: &[&str] = &[
    ENV_CONFIG,
    ENV_OUTPUT_DIR,
    ENV_STATE_DIR,
    ENV_IMAGE_PROVIDER,
    ENV_WAIT_TIMEOUT,
    ENV_POLL_INTERVAL,
    ENV_STORE_PROMPTS,
    ENV_OPENAI_BASE_URL,
    ENV_GEMINI_BASE_URL,
    ENV_LOG,
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "HOME",
];

/// The environment Iris resolves settings from: the platform, home and current
/// directories, the non-secret variables in [`SETTING_VARS`], and the credentials
/// `OPENAI_API_KEY` / `GEMINI_API_KEY` (held as [`Secret`]s, never as plain strings).
///
/// Empty or whitespace-only variables count as unset. `Debug` shows variable names
/// only, never values.
#[derive(Clone)]
pub struct EnvSnapshot {
    platform: Platform,
    home: Option<PathBuf>,
    cwd: PathBuf,
    vars: BTreeMap<String, String>,
    non_unicode: BTreeSet<String>,
    credentials: BTreeMap<ProviderId, Secret>,
}

impl EnvSnapshot {
    /// Capture the current process environment. The home directory comes from the
    /// `directories` crate (`$HOME`, else the password database).
    pub fn from_process() -> Result<Self, IrisError> {
        let cwd = std::env::current_dir()
            .map_err(|e| IrisError::io("cannot determine the current directory", &e))?;
        let home = directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf());
        let mut snap = EnvSnapshot::new(Platform::current(), home, cwd);
        for name in SETTING_VARS {
            match std::env::var(name) {
                Ok(value) => snap = snap.with_var(name, &value),
                Err(VarError::NotUnicode(_)) => {
                    snap.non_unicode.insert((*name).to_string());
                }
                Err(VarError::NotPresent) => {}
            }
        }
        for provider in ProviderId::ALL {
            if let Some(secret) = Secret::from_env(provider.credential_env()) {
                snap.credentials.insert(*provider, secret);
            }
        }
        Ok(snap)
    }

    /// An empty environment (for tests and embedding): no variables, no credentials.
    pub fn new(platform: Platform, home: Option<PathBuf>, cwd: PathBuf) -> Self {
        EnvSnapshot {
            platform,
            home,
            cwd,
            vars: BTreeMap::new(),
            non_unicode: BTreeSet::new(),
            credentials: BTreeMap::new(),
        }
    }

    /// Set a variable. `OPENAI_API_KEY` / `GEMINI_API_KEY` are stored as [`Secret`]s
    /// (trimmed; empty means absent) and are not readable through [`EnvSnapshot::var`].
    pub fn with_var(mut self, name: &str, value: &str) -> Self {
        if let Some(provider) = ProviderId::ALL.iter().find(|p| p.credential_env() == name) {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                self.credentials.remove(provider);
            } else {
                self.credentials.insert(*provider, Secret::new(trimmed));
            }
        } else {
            self.non_unicode.remove(name);
            self.vars.insert(name.to_string(), value.to_string());
        }
        self
    }

    /// A non-secret variable, or `None` if unset or blank.
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str).filter(|v| !v.trim().is_empty())
    }

    /// True if the variable is set but not valid UTF-8 (captured by `from_process`).
    pub fn is_non_unicode(&self, name: &str) -> bool {
        self.non_unicode.contains(name)
    }

    /// The credential for `provider`, if its environment variable is set.
    pub fn credential(&self, provider: ProviderId) -> Option<&Secret> {
        self.credentials.get(&provider)
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Home directory: `$HOME` from the snapshot if absolute, else the one captured
    /// at construction.
    pub fn home(&self) -> Option<&Path> {
        match self.var("HOME").map(Path::new) {
            Some(p) if p.is_absolute() => Some(p),
            _ => self.home.as_deref(),
        }
    }

    /// Current directory (relative paths from flags and variables resolve against it).
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(crate) fn credentials(&self) -> &BTreeMap<ProviderId, Secret> {
        &self.credentials
    }
}

impl fmt::Debug for EnvSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvSnapshot")
            .field("platform", &self.platform)
            .field("home", &self.home)
            .field("cwd", &self.cwd)
            .field("vars", &self.vars.keys().collect::<Vec<_>>())
            .field("credentials", &self.credentials.keys().map(|p| p.credential_env()).collect::<Vec<_>>())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_secrets_and_not_plain_vars() {
        let env = EnvSnapshot::new(Platform::Linux, None, PathBuf::from("/w"))
            .with_var("OPENAI_API_KEY", " test-openai-key-000 ")
            .with_var("GEMINI_API_KEY", "   ")
            .with_var("IRIS_LOG", "debug");
        assert_eq!(env.credential(ProviderId::OpenAi).unwrap().expose(), "test-openai-key-000");
        assert!(env.credential(ProviderId::Gemini).is_none());
        assert_eq!(env.var("OPENAI_API_KEY"), None);
        let dbg = format!("{env:?}");
        assert!(!dbg.contains("test-openai-key-000"), "{dbg}");
        assert!(!dbg.contains("debug\""), "{dbg}");
        assert!(dbg.contains("OPENAI_API_KEY"));
    }

    #[test]
    fn blank_vars_are_unset_and_home_prefers_absolute_home_var() {
        let env = EnvSnapshot::new(Platform::Linux, Some(PathBuf::from("/fallback")), PathBuf::from("/w"))
            .with_var("IRIS_OUTPUT_DIR", "  ")
            .with_var("HOME", "relative/home");
        assert_eq!(env.var("IRIS_OUTPUT_DIR"), None);
        assert_eq!(env.home(), Some(Path::new("/fallback")));
        let env = env.with_var("HOME", "/home/me");
        assert_eq!(env.home(), Some(Path::new("/home/me")));
    }
}
