//! The tool versions pinned by hand (docs/contributing/releasing.md, "Update the pinned
//! tools") agree everywhere they are named, so a pull request that bumps one and misses
//! a place fails here, not at release time. Also runs the release toolchain's age check,
//! scripts/check-release-toolchain-age.sh, on fixed versions.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(root().join(path)).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// The files of `dir` (relative to the repository root), and of its subdirectories when
/// `recursive`, whose names end with `suffix`.
fn files_in(dir: &Path, suffix: &str, recursive: bool, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root().join(dir)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = dir.join(&name);
        if entry.file_type().unwrap().is_dir() {
            if recursive {
                files_in(&path, suffix, true, out);
            }
        } else if name.ends_with(suffix) {
            out.push(path);
        }
    }
}

/// Every file that can name a pinned version: the Markdown files at the root and in
/// docs/, the workflows, and the scripts. The changelog is left out: its entries describe
/// earlier releases, which can name earlier versions.
fn pin_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    files_in(Path::new(""), ".md", false, &mut out);
    out.retain(|p| p != Path::new("CHANGELOG.md"));
    files_in(Path::new("docs"), ".md", true, &mut out);
    files_in(Path::new(".github/workflows"), ".yml", false, &mut out);
    files_in(Path::new("scripts"), ".sh", false, &mut out);
    out.sort();
    out
}

/// The version that follows each `prefix` in `text`: its digits and dots, without a
/// sentence's final dot. A prefix followed by no digit, as in `cargo-about@<version>`,
/// names no version.
fn versions_after(text: &str, prefix: &str) -> Vec<String> {
    text.match_indices(prefix)
        .map(|(at, _)| {
            let digits: String =
                text[at + prefix.len()..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            digits.trim_end_matches('.').to_string()
        })
        .filter(|v| !v.is_empty())
        .collect()
}

/// The minimum Rust version that `text` states: in each `Rust X.Y or later`, and each
/// `Minimum Rust version X.Y`. Other versions of Rust that a page names, such as the
/// release that stabilized an API, are not the minimum.
fn stated_minimums(text: &str) -> Vec<String> {
    let mut out: Vec<String> = text
        .match_indices("Rust ")
        .filter_map(|(at, _)| {
            let rest = &text[at + "Rust ".len()..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            (!digits.is_empty() && rest[digits.len()..].starts_with(" or later")).then_some(digits)
        })
        .collect();
    out.extend(versions_after(text, "Minimum Rust version "));
    out
}

/// Check that every version that `extract` finds in the [`pin_files`] is `pinned`, and
/// that each file of `required` names one: those are the places a bump must change, so
/// the check can't pass by finding nothing. Lines are joined first, so a phrase wrapped
/// across two lines still counts.
fn assert_all_name(what: &str, pinned: &str, extract: impl Fn(&str) -> Vec<String>, required: &[&str]) {
    let mut named: Vec<(PathBuf, String)> = Vec::new();
    for file in pin_files() {
        let text = read(&file).split_whitespace().collect::<Vec<_>>().join(" ");
        named.extend(extract(&text).into_iter().map(|v| (file.clone(), v)));
    }
    let wrong: Vec<String> = named
        .iter()
        .filter(|(_, v)| v != pinned)
        .map(|(file, v)| format!("{} names {what} {v}", file.display()))
        .collect();
    assert!(wrong.is_empty(), "{what} is pinned to {pinned}, but:\n{}", wrong.join("\n"));
    for file in required {
        assert!(named.iter().any(|(f, _)| f == Path::new(file)), "{file} names no version of {what}");
    }
}

/// `(major, minor, patch)` of `X.Y` or `X.Y.Z`.
fn version(text: &str) -> (u32, u32, u32) {
    let parts: Vec<u32> =
        text.split('.').map(|p| p.parse().unwrap_or_else(|_| panic!("not a version: {text}"))).collect();
    match parts.as_slice() {
        [major, minor] => (*major, *minor, 0),
        [major, minor, patch] => (*major, *minor, *patch),
        _ => panic!("not a version: {text}"),
    }
}

/// `rust-version` of the `[package]` table of Cargo.toml.
fn minimum_rust_version() -> String {
    let manifest: toml::Table = read(Path::new("Cargo.toml")).parse().unwrap();
    manifest["package"]["rust-version"].as_str().expect("package.rust-version").to_string()
}

/// `RELEASE_RUST_TOOLCHAIN` of release.yml, from its one line in the exact form that
/// ci.yml's release dry run and the Release toolchain workflow read with `sed`.
fn release_toolchain() -> String {
    let workflow = read(Path::new(".github/workflows/release.yml"));
    let pins: Vec<&str> = workflow
        .lines()
        .filter_map(|line| line.strip_prefix("  RELEASE_RUST_TOOLCHAIN: \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect();
    let [pin] = pins.as_slice() else {
        panic!("release.yml must set RELEASE_RUST_TOOLCHAIN once, as `  RELEASE_RUST_TOOLCHAIN: \"X.Y.Z\"`");
    };
    pin.to_string()
}

#[test]
fn cargo_about_is_pinned_to_the_version_that_packaging_requires() {
    let script = read(Path::new("scripts/package-release.sh"));
    let pinned = script
        .lines()
        .find_map(|line| line.strip_prefix("cargo_about_version="))
        .expect("scripts/package-release.sh sets cargo_about_version");
    assert_all_name(
        "cargo-about",
        pinned,
        |text| versions_after(text, "cargo-about@"),
        &[".github/workflows/ci.yml", ".github/workflows/release.yml", "docs/contributing/releasing.md"],
    );
}

#[test]
fn parse_changelog_is_pinned_to_the_version_that_the_release_installs() {
    let workflow = read(Path::new(".github/workflows/release.yml"));
    let installed = versions_after(&workflow, "tool: parse-changelog@");
    let [pinned] = installed.as_slice() else {
        panic!("release.yml must install one parse-changelog version, found {installed:?}");
    };
    assert_all_name(
        "parse-changelog",
        pinned,
        |text| versions_after(text, "parse-changelog@"),
        &["docs/contributing/releasing.md"],
    );
}

#[test]
fn the_documented_minimum_rust_version_is_the_one_cargo_toml_declares() {
    assert_all_name(
        "the minimum Rust version",
        &minimum_rust_version(),
        stated_minimums,
        &["CONTRIBUTING.md", "docs/guides/install.md", "docs/contributing/decisions.md"],
    );
}

#[test]
fn the_release_toolchain_is_an_exact_release_no_older_than_the_minimum() {
    let pin = release_toolchain();
    assert_eq!(pin.split('.').count(), 3, "RELEASE_RUST_TOOLCHAIN must be an exact X.Y.Z release, got {pin}");
    let minimum = minimum_rust_version();
    assert!(
        version(&pin) >= version(&minimum),
        "RELEASE_RUST_TOOLCHAIN {pin} is older than the minimum Rust version {minimum}"
    );
}

#[test]
fn the_age_check_flags_a_release_toolchain_too_far_behind_stable() {
    let check = |pinned: &str, stable: &str, max_behind: &str| {
        Command::new("sh")
            .arg(root().join("scripts/check-release-toolchain-age.sh"))
            .args([pinned, stable, max_behind])
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_eq!(check("1.95.0", "1.98.1", "3"), Some(0), "3 minor versions behind is within the limit");
    assert_eq!(check("1.98.1", "1.98.1", "3"), Some(0));
    assert_eq!(check("1.99.0", "1.98.1", "3"), Some(0), "newer than stable");
    assert_eq!(check("1.94.1", "1.98.1", "3"), Some(2), "4 minor versions behind is too far");
    for bad in [
        ["1.94", "1.98.1", "3"],
        ["1.94.1", "stable", "3"],
        ["1.94.1", "2.0.0", "3"],
        ["1.94.1", "1.98.1", "three"],
    ] {
        assert_eq!(check(bad[0], bad[1], bad[2]), Some(1), "{bad:?}");
    }
}

/// The extractors find what the pages write, and nothing else.
#[test]
fn the_extractors_read_the_pinned_forms() {
    assert_eq!(
        versions_after("install cargo-about@0.9.2 and cargo-about@<version>.", "cargo-about@"),
        ["0.9.2"]
    );
    assert_eq!(versions_after("parse-changelog@0.6.17.", "parse-changelog@"), ["0.6.17"]);
    assert_eq!(
        stated_minimums(
            "You need Rust 1.89 or later. Since Rust 1.89, ... **Minimum Rust version 1.89, edition"
        ),
        ["1.89", "1.89"]
    );
    assert_eq!(stated_minimums("locking since Rust 1.89, so"), Vec::<String>::new());
}
