# AGENTS.md

Shared instructions for coding agents (and humans) working on Iris, a Rust CLI for
generating and editing media through provider APIs. These are invariants and steering
rules, not a description of the code; read `docs/architecture.md` and the source for that.

## Compatibility

- The CLI command tree, flags, JSON envelope, error `code`s, warning codes, and exit
  codes are public contracts. Change them only deliberately: additive JSON fields and
  codes are fine; renames, removals, or semantic changes require bumping
  `schema_version`. Any change regenerates `schema/` (a test enforces this), updates
  the docs, and gets a changelog entry.
- Persisted job records are versioned. Readers must keep loading older records and
  preserve fields they do not know.
- `--json` mode prints exactly one JSON document on stdout. Everything else goes to stderr.
- Provider error strings are never part of the public taxonomy; map them to Iris codes.

## Providers

- Provider request/response types stay inside `src/providers/<provider>/`. The rest of
  the application sees only the shared traits and domain types.
- Capabilities, defaults, and price tables are declared in the catalog, with the date
  and source they were checked against. Validate every option, input, and operation
  against the resolved model before any network call. Reject unsupported options
  explicitly; never drop, coerce, or silently remap them.
- Keep synchronous generation and provider-native async jobs distinct. Never offer
  detach, resume, or cancellation for an operation the provider cannot recover.
- Do not claim capabilities, access, cancellation, or recovery that a provider does not
  document. When providers differ, implement the honest subset.
- Adding a provider means: an adapter, one registry entry, its `ProviderId` identity,
  catalog declarations, tests, and the regenerated schema and docs. If it requires edits
  across the app, fix the abstraction first.

## Money, credentials, and safety

- Credentials come only from `OPENAI_API_KEY` and `GEMINI_API_KEY`. Never print, log,
  persist, or put them in URLs, argv, fixtures, or error messages. Check presence only.
- Paid submissions are never retried automatically unless the request provably did not
  reach the provider, or the provider explicitly rejected it before processing (rate
  limit, documented overload). Ambiguous outcomes are reported as uncertain and never
  resubmitted, even when a vendor's guidance says to retry.
- Generation and download are separate outcomes. A download failure must never trigger
  another generation. Downloads write to temp files and finalize atomically; repeating
  a download must be safe.
- Never send credentials to a host other than the provider's configured API origin,
  including across redirects. Redact signed URLs and secrets in all output.
- A paid output that reached Iris is never discarded: if it cannot be saved as
  requested, save it elsewhere and say so.
- Persist provider-native async jobs before and after submission so any later process
  can resume them. Ctrl-C or a local wait limit never marks a remote job as failed.
  Persisted state is versioned and written atomically under a lock.
- Do not log prompts or input contents by default.

## Dependencies

- Prefer stable, maintained crates and established CI/release tools over hand-rolled
  code. Hand-roll only trivial code or when nothing suitable exists, and say why in the
  commit message.
- Dependencies must pass `cargo deny check` (MIT-compatible licenses, no known
  vulnerabilities).

## Tests

- The default test suite runs offline, needs no credentials, and costs nothing. Use
  mock servers and fixtures; use fake keys set through the environment, never real ones.
- Live tests are opt-in, paid, and run deliberately with the smallest practical
  requests. Never add them to ordinary CI and never run them to "see if it works".
- Test observable behavior (CLI output, exit codes, files, persisted state), not
  implementation details.

## Working in this repository

- Respect unrelated work: do not revert, reformat, or "clean up" code outside your task.
- Before claiming something works, run it: `cargo fmt --check`, `cargo clippy
  --all-targets -- -D warnings`, `cargo test`, and any command you document. Say
  plainly what you did not verify.
- Commits: one coherent, reviewable change per commit, including its tests and docs;
  each commit builds and passes tests; concise imperative subject, reasoning in the
  body. Stage explicit paths and review the staged diff. Never commit secrets,
  generated media, or local state. Never amend, squash, or rewrite commits you did
  not create without permission.
- Never fabricate a git identity: if none is configured, finish and verify the work,
  then report that committing is blocked. Never reset or discard working-tree changes
  you did not make.
