//! Build-time facts for `iris --version` / `iris version`: the compilation
//! target triple and, when the build names it, the git commit.

use std::env;

/// The variable that names the commit being built. Iris's CI and release
/// workflows set it to the commit they checked out; anyone building from a
/// checkout can set it too.
const COMMIT_VAR: &str = "IRIS_GIT_COMMIT";

fn main() {
    if let Ok(target) = env::var("TARGET") {
        println!("cargo:rustc-env=IRIS_TARGET={target}");
    }

    // The commit comes from a variable the builder sets on purpose, not from
    // running `git` or from an ambient CI variable, so a build never reports
    // a commit nobody vouched for: `git` would describe whatever repository
    // happens to enclose the source (or none, for a crate package), and
    // GitHub Actions' GITHUB_SHA names the commit of the repository whose
    // workflow is running, which is not Iris's source when another project's
    // workflow runs `cargo install`.
    //
    // A value that is not a hex object name is dropped (the version then
    // reports null) instead of passed on: besides being meaningless, a
    // newline in it would end the `cargo:` line below early. An empty value
    // counts as unset.
    let mut commit = String::new();
    if let Some(value) = env::var(COMMIT_VAR).ok().filter(|v| !v.is_empty()) {
        if is_commit_id(&value) {
            commit = value.to_ascii_lowercase();
        } else {
            println!(
                "cargo:warning={COMMIT_VAR} is not 7 to 40 hexadecimal characters; `iris version` will report git_commit: null"
            );
        }
    }
    // Always emitted, even when empty, so a variable of the same name in the
    // build environment can never reach `option_env!` unchecked.
    println!("cargo:rustc-env=IRIS_BUILD_GIT_COMMIT={commit}");

    println!("cargo:rerun-if-env-changed={COMMIT_VAR}");
    println!("cargo:rerun-if-changed=build.rs");
}

/// A git object name: 7 (git's shortest abbreviation) to 40 (a full SHA-1)
/// hexadecimal digits.
fn is_commit_id(value: &str) -> bool {
    (7..=40).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_hexdigit())
}
