//! Platform default locations and `~` expansion.

use std::path::{Path, PathBuf};

/// Directory-layout convention. Iris targets Linux and macOS only; other Unix
/// systems use the Linux (XDG) layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// XDG base directories.
    Linux,
    /// `~/Library/Application Support`.
    MacOs,
}

impl Platform {
    /// The layout of the platform Iris was compiled for.
    pub fn current() -> Platform {
        if cfg!(target_os = "macos") { Platform::MacOs } else { Platform::Linux }
    }
}

/// Default locations for a platform (see docs/configuration.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPaths {
    /// Linux: `$XDG_CONFIG_HOME/iris/config.toml`, else `~/.config/iris/config.toml`.
    /// macOS: `~/Library/Application Support/iris/config.toml`.
    pub config_file: PathBuf,
    /// Linux: `$XDG_STATE_HOME/iris`, else `~/.local/state/iris`.
    /// macOS: `~/Library/Application Support/iris`.
    pub state_dir: PathBuf,
}

/// Compute the default locations. XDG variables are honored only when they hold an
/// absolute path, as the XDG Base Directory specification requires.
pub fn platform_paths(
    platform: Platform,
    home: &Path,
    xdg_config_home: Option<&str>,
    xdg_state_home: Option<&str>,
) -> PlatformPaths {
    let xdg = |v: Option<&str>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    match platform {
        Platform::Linux => PlatformPaths {
            config_file: xdg(xdg_config_home)
                .unwrap_or_else(|| home.join(".config"))
                .join("iris")
                .join("config.toml"),
            state_dir: xdg(xdg_state_home).unwrap_or_else(|| home.join(".local").join("state")).join("iris"),
        },
        Platform::MacOs => {
            let base = home.join("Library").join("Application Support").join("iris");
            PlatformPaths { config_file: base.join("config.toml"), state_dir: base }
        }
    }
}

/// Expand a leading `~` component (`~` or `~/…`) to `home`. Other paths, including
/// `~user/…`, are returned unchanged.
pub fn expand_tilde(path: &Path, home: Option<&Path>) -> Result<PathBuf, String> {
    match path.strip_prefix("~") {
        Ok(rest) => match home {
            Some(home) if rest.as_os_str().is_empty() => Ok(home.to_path_buf()),
            Some(home) => Ok(home.join(rest)),
            None => Err("cannot expand '~': the home directory is unknown".to_string()),
        },
        Err(_) => Ok(path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_defaults_follow_xdg() {
        let home = Path::new("/home/u");
        let p = platform_paths(Platform::Linux, home, None, None);
        assert_eq!(p.config_file, Path::new("/home/u/.config/iris/config.toml"));
        assert_eq!(p.state_dir, Path::new("/home/u/.local/state/iris"));
        let p = platform_paths(Platform::Linux, home, Some("/xdg/conf"), Some("/xdg/state"));
        assert_eq!(p.config_file, Path::new("/xdg/conf/iris/config.toml"));
        assert_eq!(p.state_dir, Path::new("/xdg/state/iris"));
        // Relative XDG values are invalid per the spec and ignored.
        let p = platform_paths(Platform::Linux, home, Some("conf"), Some("state"));
        assert_eq!(p.config_file, Path::new("/home/u/.config/iris/config.toml"));
    }

    #[test]
    fn macos_defaults_use_application_support() {
        let p = platform_paths(Platform::MacOs, Path::new("/Users/u"), Some("/ignored"), None);
        assert_eq!(p.config_file, Path::new("/Users/u/Library/Application Support/iris/config.toml"));
        assert_eq!(p.state_dir, Path::new("/Users/u/Library/Application Support/iris"));
    }

    #[test]
    fn tilde_expansion() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(expand_tilde(Path::new("~"), home).unwrap(), Path::new("/home/u"));
        assert_eq!(
            expand_tilde(Path::new("~/Pictures/iris"), home).unwrap(),
            Path::new("/home/u/Pictures/iris")
        );
        assert_eq!(expand_tilde(Path::new("~other/x"), home).unwrap(), Path::new("~other/x"));
        assert_eq!(expand_tilde(Path::new("/abs/~"), home).unwrap(), Path::new("/abs/~"));
        assert!(expand_tilde(Path::new("~/x"), None).is_err());
    }
}
