# Releasing Iris

This page is for maintainers. It describes how to cut a release, what a release archive contains,
and how to test the installer and the release path before you publish.

## Cut a release

Cut releases from `main`:

1. On a branch, set the new `version` in `Cargo.toml`, and run `cargo check` without `--locked`, so
   that `Cargo.lock` records it too. Commit both files.
2. In `CHANGELOG.md`, move the entries under `## [Unreleased]` to a new
   `## [X.Y.Z] - YYYY-MM-DD` heading below it, and leave `[Unreleased]` empty. That section becomes
   the text of the GitHub release.
3. Merge the change, and wait until CI passes on the resulting `main` commit. Only tag a `main`
   commit whose CI is green.
4. Run the Release workflow by hand on that commit: **Actions** > **Release** > **Run workflow**.
   Wait for it to pass. A manual run never publishes. CI covers only the Linux release path, and
   this run also builds, packages, and smoke-tests both macOS archives with the pinned release
   toolchain, so a problem there doesn't use up a version number.
5. Tag the commit and push the tag:

   ```sh
   git tag vX.Y.Z COMMIT
   git push origin vX.Y.Z
   ```

The release workflow (`.github/workflows/release.yml`) reads the release notes with
[parse-changelog](https://github.com/taiki-e/parse-changelog), at the version pinned in the
workflow. It fails if the version's section is missing or empty, or if any version has two
headings. To preview the notes, install the same version and run it:

```sh
cargo install --locked parse-changelog@0.6.17
parse-changelog CHANGELOG.md X.Y.Z
```

A manual run of the Release workflow also shows the notes in the run's summary.

Pushing a `v*` tag starts the release workflow, which does the following:

1. Checks that the tag is `v` followed by the `Cargo.toml` version, and that `CHANGELOG.md` has that
   version's section.
2. Runs formatting, Clippy, and the tests again on the tagged commit.
3. Builds the Linux musl binary and both macOS binaries, each on its own runner.
4. Packages each binary as `iris-vX.Y.Z-TARGET.tar.gz`, and smoke-tests every archive, including
   installing it with `install.sh`.
5. Creates the GitHub release, with the three archives, `SHA256SUMS`, `install.sh`, and the
   version's changelog section as its notes. A version with a `-` suffix, such as `1.2.0-rc.1`,
   becomes a pre-release.

The release is created only when every job passes. If a job fails, nothing is published. Run a job
again if it failed for a temporary reason. Otherwise, fix the problem on `main` and release a new
version, rather than moving a pushed tag.

## Change the release toolchain

The release workflow checks and builds with one pinned Rust version, `RELEASE_RUST_TOOLCHAIN` at the
top of `.github/workflows/release.yml`, not with whatever `stable` is when a tag is pushed. Every
job that uses it logs `rustc -Vv`, and fails if the compiler is a different version. CI on `main`
keeps testing the current stable version and the minimum supported version.

To move releases to a newer Rust version:

1. On a branch, set `RELEASE_RUST_TOOLCHAIN` to an exact `X.Y.Z` release, not `stable` or `X.Y`,
   and no older than `rust-version` in `Cargo.toml`.
2. Run the Release workflow by hand on that branch. It runs the release checks, builds, and smoke
   tests with the new toolchain, and publishes nothing.
3. Merge the change once the run passes, before you tag the release that should use it.

## Update the license notices

Packaging generates `THIRD-PARTY-LICENSES` with cargo-about (`about.toml`, `about.hbs`), and fails
on any problem that cargo-about reports. A new dependency whose license `deny.toml` allows needs no
change there.

- If an update to `aws-lc-sys` or `aws-lc-rs` changes that crate's `LICENSE` file, read the new
  file, and put its SHA-256 in `about.toml`. Its comments explain why those two files are listed.
- Packaging accepts only one cargo-about version, `cargo_about_version` in
  `scripts/package-release.sh`, so that the same commit always gives the same archive. To move to a
  newer version, change it there, in the cargo-about install steps of `ci.yml` and `release.yml`,
  and on this page. Then compare the file that it generates with the old one.

## What a release archive contains

Each release has one archive per target, named `iris-vX.Y.Z-TARGET.tar.gz`, and one `SHA256SUMS`
file. An archive holds a single directory, `iris-vX.Y.Z-TARGET/`, with these files:

- `iris`, the binary.
- `LICENSE`, the MIT license of Iris.
- `THIRD-PARTY-LICENSES`: the license text and copyright notices of every crate compiled into that
  target's binary, and the crates.io address of each crate's source. It includes `option-ext`, the
  one dependency under the MPL-2.0. `scripts/package-release.sh` generates it with
  [cargo-about](https://github.com/EmbarkStudios/cargo-about) (`about.toml`, `about.hbs`).
- `README.md`, `CHANGELOG.md`, and `docs/`, for reading offline.

Archives are reproducible: the same inputs give the same bytes on the same kind of host.
`scripts/package-release.sh` fixes the entry order, owners, permissions, and timestamps.

A binary reports the commit that it was built from as `git_commit`, from the `IRIS_GIT_COMMIT`
variable at build time. Iris uses the value, in lowercase, when it's 7 to 40 hexadecimal digits,
and reports `null` otherwise. Nothing else sets it: not `git`, and not `GITHUB_SHA`, which is the
commit of the repository that runs a workflow, not necessarily of the Iris source. The release
workflow sets `IRIS_GIT_COMMIT` to the tagged commit.

## Test the installer

The installer's test suite, `tests/installer/run.sh`, builds fake release archives and `SHA256SUMS`
files with `make-fixtures.sh`, and serves them from `127.0.0.1` with `server.py`. It runs
`install.sh` against them in a clean environment (`env -i`), with `uname` and `sysctl` shims that
simulate Linux and macOS:

```sh
sh tests/installer/run.sh
```

The suite covers the following, prints one line per case, and exits with a nonzero code if any case
fails:

- Platform detection, including unsupported systems and Rosetta.
- Version selection: `latest`, a pinned version, a pre-release, and an invalid version.
- Network failures: HTTP errors; dropped, truncated, and refused connections; and a `TERM` signal
  during a download.
- Checksum failures, and archive validation: path traversal, links and special files, unexpected
  entries, and a binary that doesn't run.
- Install directories, `PATH` hints, and upgrades that keep the old `iris` when anything fails.
- `curl` against GNU and BusyBox `wget`, including a `wget` that doesn't verify certificates, and
  `sha256sum` against `shasum`.
- Never reading from standard input.

To run the installer under another shell, set `INSTALLER_SHELL`, such as
`INSTALLER_SHELL='bash --posix'` or `INSTALLER_SHELL=dash`. The BusyBox cases use a `busybox` binary
for every tool when one is on `PATH`, or when `BUSYBOX` names one, as on Alpine. Otherwise, they run
GNU `wget` behind a shim with BusyBox's exit codes.

## Test the release path

CI runs the Linux release path on every change, with the pinned release toolchain. It builds the
static `x86_64-unknown-linux-musl` binary and packages it twice: the two archives must be identical.
Then it runs `scripts/smoke-test-release.sh`, which runs the packaged binary, and installs the
archive with `install.sh` from a local server.

The macOS archives are built and smoke-tested only by the release workflow, which runs the same
smoke test for every target before it publishes anything. A manual run of the workflow never
publishes, so run it before you push a tag.

To replay the Linux run locally, you need the musl target, your distribution's `musl-tools`, and the
exact cargo-about version that `scripts/package-release.sh` requires:

```sh
rustup target add x86_64-unknown-linux-musl
cargo install --locked --features cli cargo-about@0.9.2
cargo build --release --locked --target x86_64-unknown-linux-musl
sh scripts/package-release.sh x86_64-unknown-linux-musl dist
sh scripts/smoke-test-release.sh "$(sh scripts/check-tag-version.sh)" x86_64-unknown-linux-musl dist
```
