# Contributing to Iris

Thanks for looking at Iris. This file covers how to build it, what to check before opening a
pull request, and where the durable rules live. For the module layout and where invariants are
enforced, see [docs/architecture.md](docs/architecture.md); for adding a provider, see
[docs/providers.md](docs/providers.md).

## Dev setup

You need a Rust toolchain — Rust 1.89 or newer (the `rust-version` declared in `Cargo.toml`, which
CI's MSRV job also checks against); `rustup` is the easiest way to get one. This repository has no
`rust-toolchain.toml`, so nothing is pinned automatically — install 1.89+ yourself (`rustup install
1.89` or your usual up-to-date stable toolchain both work).

```console
$ git clone https://github.com/doodla/iris && cd iris
$ cargo build
$ cargo test
```

No credentials, no network access beyond crates.io during the build, and no cost are needed for
any of the above — see [Tests](#tests) below.

## Checks to run before opening a pull request

These are the same checks CI runs on pushes to `main` and on pull requests; running them locally
first saves a round trip:

```console
$ cargo fmt --all --check
$ cargo clippy --all-targets --locked -- -D warnings
$ cargo test --locked
$ cargo check --locked --all-targets     # also run on the pinned MSRV toolchain in CI
$ cargo deny check                        # dependency licenses and advisories
```

If you touched `install.sh`, `scripts/`, or `tests/installer/`, also run `shellcheck` and the
installer's offline test suite:

```console
$ shellcheck -s sh install.sh
$ shellcheck scripts/*.sh tests/installer/*.sh
$ sh tests/installer/run.sh
```

If you changed anything that appears in `--json` output (the result and error types the schema is
generated from), regenerate the committed schema and review its diff:

```console
$ cargo run -q -- schema > schema/iris-output.v1.schema.json
```

`cargo test` never writes that file: it only compares it with the schema generated from the code
and fails on any difference. Removing or renaming a field, or changing its meaning, also needs a
`schema_version` bump (see [docs/json-contract.md](docs/json-contract.md)).

CI also builds and tests on macOS (Intel and Apple silicon) in addition to Linux, packages the
crate (`cargo package --list`) to catch anything accidentally included or excluded, and runs a
release dry run: it builds the Linux musl binary, packages it, and smoke-tests the archive and
`install.sh` with it. The macOS jobs are hosted-only; the release dry run can be replayed locally
as shown in [docs/install.md](docs/install.md#testing-the-installer-without-a-real-release).

## Tests

- **The default test suite is entirely offline**: no credentials are read from your real
  environment, no network call reaches a real provider, and nothing costs money. Provider
  behavior is exercised against local mock HTTP servers (`wiremock`) with fake keys set through
  the process environment (`Command::env`, never argv). This is a hard rule, not a convenience —
  see [AGENTS.md](AGENTS.md).
- **Live verification is separate, paid, and opt-in.** It is never run in CI and never run as
  part of `cargo test`. See [docs/live-testing.md](docs/live-testing.md) for what it verifies,
  the cost budget, and how to run it deliberately by hand.
- Prefer testing observable behavior — CLI output, exit codes, files written, persisted job
  records — over asserting internal implementation details. Look at the existing test files
  (`tests/*.rs`) for the established patterns (`tests/app_support.rs` holds the shared fake
  catalog, fake providers, and media fixtures used across the others) before adding a new one.

## Commit policy

Iris's commit policy is stated once, concisely, in [AGENTS.md](AGENTS.md#working-in-this-repository)
— read it there rather than here, so it stays in exactly one place. In short: coherent, reviewable
commits that build and pass tests, with an imperative subject and any non-obvious reasoning in the
body; explicit `git add` of the paths a commit actually touches; no secrets, generated media, or
local state ever committed.

## Releasing

Maintainers cut a release from `main`:

1. On a branch, set the new `version` in `Cargo.toml` and run `cargo check` (without `--locked`)
   so that `Cargo.lock` records it too. Commit both.
2. In `CHANGELOG.md`, move the entries under `## [Unreleased]` to a new `## [X.Y.Z] - YYYY-MM-DD`
   heading below it, and leave `[Unreleased]` empty. That section becomes the text of the GitHub
   release, and the release workflow fails if it is missing or empty; preview it with
   `sh scripts/release-notes.sh X.Y.Z`.
3. Merge that change, and wait until CI has passed on the resulting `main` commit. Only ever tag
   a `main` commit whose CI is green.
4. Tag that commit and push the tag:

   ```console
   $ git tag vX.Y.Z <commit>
   $ git push origin vX.Y.Z
   ```

Pushing a `v*` tag starts the release workflow (`.github/workflows/release.yml`). It fails unless
the tag is `v` followed by the `Cargo.toml` version and `CHANGELOG.md` has that version's section;
reruns formatting, Clippy, and the tests on the tagged commit; builds the Linux musl binary and
both macOS binaries on their own runners; packages each as `iris-vX.Y.Z-<target>.tar.gz` (the
binary, `LICENSE`, `README.md`, `CHANGELOG.md`, and `docs/`); and smoke-tests every archive,
including installing it with `install.sh`. Only when all of that passes does it create the GitHub
release, with the three archives, `SHA256SUMS`, and `install.sh`, and the version's `CHANGELOG.md`
section as its notes; a version with a `-` suffix (such as `1.2.0-rc.1`) becomes a pre-release. If
any job fails, nothing is published: re-run a job that failed for a transient reason, and otherwise
fix the problem on `main` and release a new version rather than moving a pushed tag. Running the
workflow by hand (workflow_dispatch) does everything except publishing.

### The release toolchain

The release workflow checks and builds with one pinned Rust version, `RELEASE_RUST_TOOLCHAIN` at
the top of `.github/workflows/release.yml`, not with whatever `stable` is current when a tag is
pushed; every job that uses it logs `rustc -Vv` and fails if the compiler is a different version.
CI on `main` keeps testing current stable and the minimum supported version. To move releases to a
newer Rust:

1. On a branch, set `RELEASE_RUST_TOOLCHAIN` to an exact `X.Y.Z` release (not `stable` or `X.Y`),
   no older than `rust-version` in `Cargo.toml`.
2. Run the Release workflow by hand on that branch (Actions → Release → Run workflow). It runs the
   release checks, builds, and smoke tests with the new toolchain and publishes nothing.
3. Merge once it passes, before tagging the release that should use it.

## Reporting a security issue

Do not open a public issue for a security vulnerability — see [SECURITY.md](SECURITY.md).

## Scope

Preserve unrelated work: a pull request that fixes one thing should not reformat, rename, or
"clean up" code it doesn't need to touch. If you're adding a provider, [docs/providers.md](docs/providers.md)
describes the expected shape of that change (a focused adapter, a registry line, catalog
declarations, and tests) — if it looks like it needs edits scattered through `app` or `cli`
instead, that's usually a sign the shared abstraction needs to grow first.
