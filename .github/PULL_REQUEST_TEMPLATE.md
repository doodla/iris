## What

<!-- One or two sentences: what does this change, concretely? -->

## Why

<!-- The reasoning, not just the what — especially for anything non-obvious. Link an issue if
there is one. -->

## Contract changes

<!-- Does this change the CLI (flags, commands), the --json envelope, a result shape, an error
code, a warning code, the JSON Schema, or a persisted job-record field? If yes: describe it, and
confirm `schema/iris-output.v1.schema.json` was regenerated (a test enforces this) and the
relevant doc under docs/ was updated to match. If no, say "none." -->

## Checks run

<!-- Check the boxes for what you ran locally; see CONTRIBUTING.md for the exact commands. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `sh tests/installer/run.sh` (only if `install.sh` or `tests/installer/` changed)
- [ ] `shellcheck -s sh install.sh` and `shellcheck scripts/*.sh tests/installer/*.sh tests/live/*.sh`
      (only if shell scripts changed)
- [ ] `sh tests/live/mock-run.sh` (only if `scripts/live-verify.sh` or `tests/live/` changed)
- [ ] Documented commands were actually run against the built binary (only if `docs/`, `README.md`,
      or `--help` text changed)

## Secrets

- [ ] This PR adds no credentials, real API keys, or other secrets anywhere (code, tests,
      fixtures, or this description) — see `AGENTS.md`.

## Live testing

<!-- Only relevant if you touched provider request/response handling: did you run anything from
docs/live-testing.md? If so, note the approximate cost and what you verified. If not, say so —
that's expected for most changes; offline coverage is what CI checks. -->
