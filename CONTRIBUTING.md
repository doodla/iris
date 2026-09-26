# Contributing to Iris

Thanks for your interest in Iris. This guide explains how to report a problem, set up a development
environment, check a change, and open a pull request.

## Ways to contribute

- **Report a bug.** Open an issue with the bug report template. Include the `iris --json version`
  output and, if a command failed, its JSON error. For a security issue, follow the
  [security policy](SECURITY.md) instead.
- **Request a feature.** Open an issue with the feature request template. For a provider capability,
  link the provider's current documentation for it.
- **Improve the documentation.** Follow the
  [documentation style guide](docs/contributing/style-guide.md).
- **Change the code.** Open a focused pull request: one fix or feature, with its tests and docs.

## Scope

Iris is a command-line tool for the documented image and video APIs of its providers. These are out
of scope:

- A graphical interface, a hosted service, or a daemon.
- Browser automation of consumer apps, such as ChatGPT, the Gemini app, or Google Flow.
- Providers or features that the provider's API doesn't document.
- Automatic failover from one provider to another.
- Windows support.

Some rules hold for every change. Iris never chooses a model for the caller, and never retries a
paid request whose outcome is uncertain. It never claims a capability that a provider doesn't
document. [`AGENTS.md`](AGENTS.md) lists these invariants, for people and coding agents alike. To
add a provider, follow [Add a provider](docs/contributing/adding-a-provider.md).

## Set up a development environment

You need Rust 1.89 or later, the `rust-version` in `Cargo.toml`. [rustup](https://rustup.rs) is the
easiest way to get it. The repository has no `rust-toolchain.toml`, so any recent stable toolchain
works.

```sh
git clone https://github.com/doodla/iris && cd iris
cargo build
cargo test
```

The build and the test suite need no API keys, no network access beyond crates.io, and no money.
For how the code is organized, see [Architecture](docs/contributing/architecture.md).

## Test your change

The default test suite is entirely offline. It reads no credentials from your environment, never
reaches a real provider, and costs nothing: tests run against local mock servers (`wiremock`), with
fake keys set through the process environment, never through arguments. Keep it that way.

- Test observable behavior, such as CLI output, exit codes, files, and job records, rather than
  implementation details.
- Follow the patterns of the existing tests in `tests/`. `tests/app_support.rs` holds the shared
  fake catalog, fake providers, and media fixtures.
- Live verification against the real APIs is separate, paid, and opt-in. Never add it to CI. See
  [Live testing](docs/contributing/live-testing.md).

## Before you open a pull request

Run the same checks as CI:

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo check --locked --all-targets   # CI also runs this on the minimum Rust version
cargo deny check                     # dependency licenses and advisories
```

If you changed `install.sh`, `scripts/`, `tests/installer/`, or `tests/live/`, also run ShellCheck,
the installer's offline tests, and the offline test of `scripts/live-verify.sh`:

```sh
shellcheck -s sh install.sh
shellcheck scripts/*.sh tests/installer/*.sh tests/live/*.sh
sh tests/installer/run.sh
cargo build --locked && sh tests/live/mock-run.sh
```

Some changes need generated files updated. `cargo test` checks both files and fails until you
regenerate them:

- **The JSON output.** If you changed a result or error type, regenerate the JSON Schema and review
  its diff:

  ```sh
  cargo run -q -- schema > schema/iris-output.v1.schema.json
  ```

- **The help text.** If you changed `--help` text, regenerate the CLI reference:

  ```sh
  IRIS_UPDATE_DOCS=1 cargo test --test cli_reference
  ```

If you changed documented behavior, update the page that's its home, as the
[style guide](docs/contributing/style-guide.md#give-every-fact-one-home) lists them, and run every
command that you document against the built binary. `cargo test` also checks the docs' links and
parts of their style.

CI also tests on macOS, checks the crate's package contents, and runs the Linux release path. See
[Releasing Iris](docs/contributing/releasing.md#test-the-release-path).

## Compatibility

The CLI's commands and flags, the JSON output, the error and warning codes, and the exit codes are
public contracts. Additive changes, such as a new field or a new code, are fine. Renaming or
removing something, or changing its meaning, needs a `schema_version` increment; see
[Versioning](docs/reference/json-output.md#versioning). Every contract change gets a changelog
entry.

## Commits and pull requests

- Make each commit one coherent, reviewable change, with its tests and docs. Every commit builds and
  passes the tests.
- Write a short, imperative subject, and explain the reasoning in the body.
- Stage the paths that you changed by name, and review the staged diff.
- Never commit secrets, generated media, or local state.
- Keep unrelated code as it is: don't reformat, rename, or clean up code that your change doesn't
  need to touch.

The pull request template asks which contract changes the PR makes and which checks you ran.

## Report a security issue

Don't open a public issue for a vulnerability. Follow the [security policy](SECURITY.md).

## Release Iris

Maintainers cut releases. See [Releasing Iris](docs/contributing/releasing.md).
