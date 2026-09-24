//! Build-time facts for `iris --version` / `iris version`: the compilation
//! target triple and, when the build environment names it, the git commit.

use std::env;

/// Variables that may name the commit being built, in order of precedence:
/// an explicit override, then the commit GitHub Actions checked out.
const COMMIT_VARS: [&str; 2] = ["IRIS_GIT_COMMIT", "GITHUB_SHA"];

fn main() {
    if let Ok(target) = env::var("TARGET") {
        println!("cargo:rustc-env=IRIS_TARGET={target}");
    }

    // The commit comes from the environment rather than from running `git`,
    // so a build from a source tarball or a crate package never reports a
    // commit it cannot vouch for. The first variable that is set and
    // non-empty decides. A value that is not a hex object name is dropped
    // (the version then reports null) instead of passed on: besides being
    // meaningless, a newline in it would end the `cargo:` line below early.
    let mut commit = String::new();
    let named =
        COMMIT_VARS.iter().find_map(|name| env::var(name).ok().filter(|v| !v.is_empty()).map(|v| (name, v)));
    if let Some((name, value)) = named {
        if is_commit_id(&value) {
            commit = value.to_ascii_lowercase();
        } else {
            println!(
                "cargo:warning={name} is not 7 to 40 hexadecimal characters; `iris version` will report git_commit: null"
            );
        }
    }
    // Always emitted, even when empty, so a variable of the same name in the
    // build environment can never reach `option_env!` unchecked.
    println!("cargo:rustc-env=IRIS_BUILD_GIT_COMMIT={commit}");

    for name in COMMIT_VARS {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// A git object name: 7 (git's shortest abbreviation) to 40 (a full SHA-1)
/// hexadecimal digits.
fn is_commit_id(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
