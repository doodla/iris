//! Small informational commands: `version`, `config show`, `config path`.

use crate::domain::Warning;
use crate::output::SCHEMA_VERSION;
use crate::output::results::{ConfigPathResult, ConfigShowResult, VersionResult};

use super::context::AppContext;

/// The compilation target triple (captured by `build.rs`; a best-effort
/// description when built without it).
pub fn target() -> &'static str {
    option_env!("IRIS_TARGET").unwrap_or(FALLBACK_TARGET)
}

#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"))]
const FALLBACK_TARGET: &str = "x86_64-unknown-linux-gnu";
#[cfg(all(target_arch = "x86_64", target_os = "linux", target_env = "musl"))]
const FALLBACK_TARGET: &str = "x86_64-unknown-linux-musl";
#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
const FALLBACK_TARGET: &str = "aarch64-unknown-linux-gnu";
#[cfg(all(target_arch = "x86_64", target_os = "macos"))]
const FALLBACK_TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_arch = "aarch64", target_os = "macos"))]
const FALLBACK_TARGET: &str = "aarch64-apple-darwin";
#[cfg(not(any(
    all(target_arch = "x86_64", target_os = "linux", target_env = "gnu"),
    all(target_arch = "x86_64", target_os = "linux", target_env = "musl"),
    all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"),
    all(target_os = "macos", any(target_arch = "x86_64", target_arch = "aarch64")),
)))]
const FALLBACK_TARGET: &str = "unknown";

/// `version`.
pub fn version() -> VersionResult {
    VersionResult {
        name: "iris".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        schema_version: SCHEMA_VERSION,
        target: target().to_string(),
        git_commit: None,
    }
}

/// `config show`: effective settings with their sources, credential presence,
/// and warning `non_default_base_url` for overridden base URLs.
pub fn config_show(ctx: &AppContext, warnings: &mut Vec<Warning>) -> ConfigShowResult {
    warnings.extend(ctx.settings.warnings());
    ctx.settings.config_show()
}

/// `config path`.
pub fn config_path(ctx: &AppContext) -> ConfigPathResult {
    ctx.settings.config_path()
}
