# Releasing Iris

This page is for maintainers. It describes what a release archive contains, and how to test the
installer and the release path before you publish a release.

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
