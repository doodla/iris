# Architecture

Iris is a single Cargo package: binary crate `iris` (`src/main.rs`, thin — it calls
`iris::cli::run()` and exits with its code) and library crate `iris` (`src/lib.rs`), so the
integration tests can reach internals through the library. `#![forbid(unsafe_code)]` is set at the
crate root; there is no `unsafe` anywhere in Iris's own code.

This document describes the module layout and where each kind of invariant is enforced. For the
exact command surface see [json-contract.md](json-contract.md) and `iris <command> --help`; for how
to add a provider see [providers.md](providers.md).

## Layering

```
cli -> app -> {providers, jobs, artifacts, catalog, config, output} -> {http, redact, error, domain}
```

A module depends only on modules to its right. Two deliberate, narrow exceptions:

- `config` may depend on `catalog` (to validate a configured default model id) and on
  `output::results` (to build `config show` rows) — configuration needs to describe the catalog
  and render itself, without the rest of the app depending on config internals.
- Provider adapters may call the pure, I/O-free helpers in `artifacts::media` (magic-byte
  sniffing, image inspection) to verify a provider's payload before returning it to `app`; they
  still never touch output paths, job state, or the filesystem layout `artifacts`/`jobs` own.

Two invariants the layering itself protects:

- `providers` never touches the filesystem layout of jobs or artifacts — an adapter returns bytes
  (image calls) or a remote operation id/status (video calls), never a path.
- `app` never constructs an HTTP request itself; every network call goes through a provider
  adapter, and adapters are the only place a provider's wire format is known.

## Modules

| module | responsibility |
|---|---|
| `domain` | Shared plain types used everywhere: `ProviderId`, `Operation`, job/download status enums, `Artifact`, `Usage`, `CostEstimate`, `Warning`. |
| `error` | `IrisError`, `ErrorCode`, `ErrorCategory`, and the exit-code mapping (see [json-contract.md](json-contract.md)). |
| `secret` | The `Secret` newtype: `Debug`/`Display` print `***`, and it is never `Serialize`. Credentials are held as `Secret` from the moment they are read from the environment. |
| `redact` | `redact_url` (strips userinfo, replaces query values with `REDACTED` except an allowlist), `scrub` (removes any configured credential value from text), `truncate`. Every error message, provider message, log line, and persisted `last_error` passes through these before it can reach stdout, stderr, or disk. |
| `catalog` | The static model catalog: every model Iris knows, its declared operations, inputs, options (typed, with defaults and allowed values/ranges), outputs, pricing, and access notes. `catalog::default_model` centralizes model defaults per (provider, operation). Per-provider declarations live in `catalog/{openai,gemini,veo}.rs`. |
| `providers` | The `ImageProvider` and `VideoProvider` traits, `ProviderContext`, and `Registry::builtin()`. Per-provider wire types and HTTP calls are private to `providers/{openai,gemini}/`. |
| `http` | Shared HTTP client construction, the retry executor (operation-aware retry classes; see [Where invariants live](#where-invariants-live)), streaming downloads with the credential-origin rule, and error classification helpers. |
| `jobs` | Persisted job records (`JobRecord`, versioned, v1) and `JobStore` (`<state_dir>/jobs/`: atomic writes, per-job locks, listing without locking, local deletion). Only `video.generate` creates records; synchronous image calls never do. |
| `artifacts` | Output path planning and filename rules (`paths`), media sniffing/validation (`media`: magic bytes, image decode, ISO-BMFF structure for video), local input-image validation (`input`), and atomic, no-clobber (or `--overwrite`) finalization through `.<name>.iris-part-*` temp files (`finalize`, `download`). |
| `config` | Config file (TOML), environment variables, precedence resolution (flag > env > file > default), and platform-appropriate paths. |
| `app` | Application workflows, one submodule per area: `image`, `video`, `jobs`, `models` (models list/show *and* providers list), `info` (version, config show/path), `doctor`. `app::catalog` is not a command handler — it is the `Catalog` type (model lookup/resolution/defaults) the other submodules use. No clap types, no printing — it takes typed arguments and an `AppContext`, reports progress through a `Progress` trait, collects warnings, and returns result DTOs or an `IrisError`. |
| `output` | The JSON envelope and result DTOs (`serde` + `schemars`, so the published schema is generated from the same types that are serialized) and human-text rendering. |
| `cli` | clap argument definitions, prompt-source resolution (inline/file/stdin), dispatch to `app`, and presentation (JSON envelope or human text). `cli::run` is the process entry point. |

## Sync vs. async: two provider traits, on purpose

Image generation and video generation are not the same shape of operation, and Iris does not
pretend otherwise. A provider implements the shared `Provider` trait (identity, base URL,
credential header, the free `check_access` metadata call) and opts into `image()` and/or `video()`:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn default_base_url(&self) -> &'static str;
    fn credential_header(&self) -> CredentialHeader;
    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError>;
    fn image(&self) -> Option<&dyn ImageProvider> { None }
    fn video(&self) -> Option<&dyn VideoProvider> { None }
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, IrisError>;
    async fn edit(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, IrisError>;
}

#[async_trait]
pub trait VideoProvider: Send + Sync {
    /// Paid, non-idempotent. On an ambiguous outcome returns `submission_uncertain`.
    async fn submit(&self, req: &VideoRequest, ctx: &ProviderContext) -> Result<SubmittedOperation, IrisError>;
    /// Idempotent status read (`IdempotentRead` retry class).
    async fn poll(&self, remote_id: &str, ctx: &ProviderContext) -> Result<RemoteStatus, IrisError>;
    /// Documented server-side retention of generated outputs, if any.
    fn output_retention(&self) -> Option<std::time::Duration>;
    /// Whether to fetch an output URI given the base URL configured now (default: yes).
    fn check_output_uri(&self, uri: &str, base_url: &url::Url) -> Result<(), IrisError> { Ok(()) }
}
```

`ImageProvider::generate`/`edit` return the generated bytes directly: there is nothing to persist
between the request and the response, so no job record is created, and a lost connection after the
provider accepted the request is simply unrecoverable (`submission_uncertain` with `job_id: null`,
`details.charge_possible: true` — see [jobs.md](jobs.md#why-synchronous-calls-have-no-job-record)).

`VideoProvider` is split into `submit` (paid, sent once, never blindly retried) and `poll`
(idempotent, safe to retry and to call again from a different process). Iris persists the job
*before* submitting so that even a crash in the submission's uncertainty window leaves a
diagnosable local record (`submission_unknown`) instead of silence. This split is what makes
`--detach` plus `jobs status`/`wait`/`download` possible, and why Iris never offers `--detach` or
recovery for image calls: the provider itself gives image generation no operation id to recover.

`RemoteStatus` is `Running { progress }`, `Succeeded { outputs, usage, warnings }`,
`Failed { error }`, or `Gone` (the provider no longer knows the operation — retention passed) —
the same shape regardless of provider, so `app::jobs` drives the poll loop once, independent of
which provider a job belongs to. There is no `cancel` method on `VideoProvider` and no `jobs
cancel` command: no provider Iris implements offers a way to cancel a job it accepted, so nothing
in the codebase pretends otherwise (see [jobs.md](jobs.md#local-deletion-vs-remote-state)).
Downloading an artifact goes through a generic `http::download`, which attaches a provider's
`credential_header()` only when the download URL's scheme, host, and port equal that provider's
configured base URL origin — no per-download adapter method is needed for that rule to hold.

## Where invariants live

- **"Never silently discard an option."** Enforced once, in `catalog::validate_request`, before
  any provider is called: every CLI option and every `-O key=value` is checked against the
  resolved model's declared `OptionSpec`s for the requested operation. An adapter that receives an
  option it does not have a wire mapping for returns `internal_error` rather than dropping it —
  that should be unreachable if validation ran, and treating it as a bug (not a silent no-op) is
  deliberate.
- **"Credentials never touch disk, argv, or an unrelated host."** `Secret` makes accidental
  printing a compile-time non-issue (no `Serialize`, redacting `Debug`); `config` rejects any
  config-file key that looks like a credential; the HTTP layer attaches a provider's credential
  header only when a request's scheme+host+port matches that provider's configured base URL
  origin, including across redirects (see [configuration.md](configuration.md) and
  [json-contract.md](json-contract.md)).
- **"A download failure is never a generation failure."** `app::jobs` downloads are a separate
  step from `app::video` submission/polling; `artifacts::finalize` writes through a temp file and
  finalizes atomically, so a failed or repeated download can never touch a file that already
  succeeded, and downloading never re-submits anything to a provider (see
  [jobs.md](jobs.md#downloads)).
- **"An ambiguous paid submission is never retried automatically."** `http::retry` has three
  retry classes (`PaidSubmit`, `IdempotentRead`, `Download`); `PaidSubmit` retries only outcomes
  that provably did not reach the provider (a connection failure before sending) or that the
  provider explicitly says were rejected before processing (a documented rate limit or overload
  rejection). Everything else that leaves the outcome open is reported as `submission_uncertain`
  (with no job for a synchronous image call, a `submission_unknown` job for a video submit) and
  never resent by Iris.
- **"Persisted state survives a crash and a concurrent process."** `jobs::JobStore` writes go
  through a temp file in the same directory, `sync_all`, then an atomic rename; a per-job lock
  (`.lock`, held only for the read-modify-write) serializes concurrent updates; `jobs list` reads
  without locking so it can never block on a stuck writer.

## Extending Iris

Adding a provider means a new adapter module, one line in `Registry::builtin()`, catalog
declarations, and tests — not changes scattered through `app` or `cli`. See
[providers.md](providers.md) for the step-by-step guide, worked through a hypothetical Seedance
adapter.
