# Contributing to Iris

Thanks for looking at Iris. This file covers how to build it, what to check before opening a
pull request, and where the durable rules live. For the module layout and where invariants are
enforced, see [docs/architecture.md](docs/architecture.md); for adding a provider, see
[docs/providers.md](docs/providers.md).

## Dev setup

You need a Rust toolchain — Rust 1.89 or newer (`rust-version` in `Cargo.toml`); `rustup` is the
easiest way to get one, and will pick up the pinned toolchain automatically.

```console
$ git clone https://github.com/doodla/iris && cd iris
$ cargo build
$ cargo test
```

No credentials, no network access beyond crates.io during the build, and no cost are needed for
any of the above — see [Tests](#tests) below.

## Checks to run before opening a pull request

These are the same checks CI runs on every push and pull request; running them locally first
saves a round trip:

```console
$ cargo fmt --all --check
$ cargo clippy --all-targets --locked -- -D warnings
$ cargo test --locked
$ cargo check --locked --all-targets     # also run on the pinned MSRV toolchain in CI
$ cargo deny check                        # dependency licenses and advisories
```

If you touched `install.sh`, also run its offline test suite and `shellcheck`:

```console
$ shellcheck -s sh install.sh
$ sh tests/installer/run.sh
```

If you touched anything under `schema/` or the DTOs it's generated from, make sure the committed
schema file still matches what `cargo test` regenerates (a test enforces this — a mismatch fails
the build, not just a lint).

CI also builds and tests on macOS (Intel and Apple silicon) in addition to Linux, and packages
the crate (`cargo package --list`) to catch anything accidentally included or excluded — those
jobs are hosted-only; running the Linux checks above locally covers the parts you can verify
before pushing.

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

## Reporting a security issue

Do not open a public issue for a security vulnerability — see [SECURITY.md](SECURITY.md).

## Scope

Preserve unrelated work: a pull request that fixes one thing should not reformat, rename, or
"clean up" code it doesn't need to touch. If you're adding a provider, [docs/providers.md](docs/providers.md)
describes the expected shape of that change (a focused adapter, a registry line, catalog
declarations, and tests) — if it looks like it needs edits scattered through `app` or `cli`
instead, that's usually a sign the shared abstraction needs to grow first.
