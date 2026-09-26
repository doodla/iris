## What

<!-- One or two sentences: what does this change? -->

## Why

<!-- The reasoning, especially for anything that isn't obvious. Link an issue if there is one. -->

## Contract changes

<!-- Does this change the CLI (commands or flags), the JSON output, an error or warning code, an
exit code, the JSON Schema, or a job record field? If so, describe the change, and confirm that you
regenerated `schema/iris-output.v1.schema.json` or `docs/reference/cli.md` as needed (tests enforce
both) and updated the docs. If not, write "None." -->

## Checks run

<!-- Check what you ran locally. CONTRIBUTING.md has the exact commands. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --all-targets --locked -- -D warnings`
- [ ] `cargo test --locked`
- [ ] `sh tests/installer/run.sh` (only if `install.sh` or `tests/installer/` changed)
- [ ] `shellcheck -s sh install.sh` and `shellcheck scripts/*.sh tests/installer/*.sh tests/live/*.sh`
      (only if shell scripts changed)
- [ ] `sh tests/live/mock-run.sh` (only if `scripts/live-verify.sh` or `tests/live/` changed)
- [ ] Ran every documented command against the built binary (only if the docs, the README, or
      `--help` text changed)

## Secrets

- [ ] This PR adds no credentials, real API keys, or other secrets anywhere: code, tests, fixtures,
      or this description. See `AGENTS.md`.

## Live testing

<!-- Only if you changed how Iris sends requests to a provider or reads its responses: did you run
anything from docs/contributing/live-testing.md? If so, note the approximate cost and what you
verified. If not, say so. That's expected for most changes: CI runs the offline tests. -->
