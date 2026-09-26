# Architecture

This page describes how the Iris code is organized: the module layers, what each module owns, the
two provider traits, and where each invariant is enforced. For the command surface, see the
[CLI reference](../reference/cli.md) and the [JSON output reference](../reference/json-output.md).
For why Iris uses the provider APIs, retries, and dependencies that it does, see
[Decisions](decisions.md). To add a provider, see [Add a provider](adding-a-provider.md).

Iris is one Cargo package with two crates, both named `iris`:

- The binary crate, `src/main.rs`, only calls `iris::cli::run()` and exits with its code.
- The library crate, `src/lib.rs`, holds everything else, so integration tests can reach the
  internals.

The crate root sets `#![forbid(unsafe_code)]`, and Iris's own code has no `unsafe` blocks.

## Layering

The top-level modules form these layers, as the `use crate::…` imports of the non-test code show. A
module depends only on modules in lower rows, and modules in the same row don't depend on each
other, with one exception, `providers` and `artifacts`:

```text
cli
app
jobs        config
output
providers   artifacts
catalog     http
redact      error       secret
domain
```

| Module | Depends on |
|---|---|
| `cli` | `app`, `jobs`, `config`, `output`, `catalog`, `redact`, `error`, `domain` |
| `app` | `jobs`, `config`, `output`, `providers`, `artifacts`, `catalog`, `http`, `redact`, `error`, `domain` |
| `jobs` | `output`, `providers`, `artifacts`, `catalog`, `http`, `redact`, `error`, `domain` |
| `config` | `output`, `catalog`, `http`, `redact`, `secret`, `error`, `domain` |
| `output` | `providers`, `catalog`, `redact`, `error`, `domain` |
| `providers` | `artifacts`, `catalog`, `http`, `redact`, `secret`, `error`, `domain` |
| `artifacts` | `providers`, `catalog`, `error`, `domain` |
| `catalog` | `redact`, `error`, `domain` |
| `http` | `redact`, `secret`, `error`, `domain` |
| `redact`, `error` | `domain` |
| `secret`, `domain` | Nothing |

These edges aren't obvious from the module names:

- **`providers` and `artifacts` depend on each other.** This is the only cycle. `artifacts::input`
  reads and validates local input files into the adapters' request type, `providers::InputImage`
  with its `InputRole`. Adapters call the pure helpers in `artifacts::media`, such as magic-byte
  sniffing and image inspection, to check a provider's payload. Adapters still never touch output
  paths, job state, or the file layout that `artifacts` and `jobs` own. Moving `InputImage` and
  `InputRole` into `domain` would remove the cycle.
- **`cli` uses `config`, `catalog`, `jobs`, and `output` directly.** It resolves `Settings` from its
  flags (`CliOverrides`), turns typed flags and `-O key=value` into the catalog's `RawOption`s,
  parses `--label` into a `jobs::JobLabel`, and renders the JSON envelope and human text. Workflow
  logic stays in `app`.
- **`jobs` depends on five lower modules.** It uses `providers` for the adapter results that a
  record applies (`RemoteStatus`, `SubmittedOperation`), and `output` because a record renders
  itself as the public `JobView` and error body. It uses `artifacts` for the state of a downloaded
  file, `catalog` to persist resolved options (free-text options as a hash), and `http::Timeouts` to
  size how long a record may stay `submitting`.
- **`config` depends on `catalog`, `output`, and `http`.** A configured model must be a known model,
  `config show` and `config path` return `output::results` types, and `config` resolves the HTTP
  client settings and each provider's time limits.
- **`output` depends on `providers` and `catalog`** only for types that appear in results, such as
  `AccountAccess`, `Lifecycle`, and `OptionValue`, so that the published schema is generated from
  the same types.
- **`providers` and `artifacts` depend on `catalog`** for the resolved options that an adapter maps
  onto the wire, and the input rules (`InputSpec`) that a model declares.

The layering protects two invariants:

- `providers` never touches the file layout of jobs or artifacts. An adapter returns bytes for an
  image call, or an operation ID and status for a video call, never a path.
- Only adapters know a provider's wire format. Every request to a provider's API is built inside an
  adapter: generation, video submission, status checks, and the metadata call behind
  `--check-access`. The one network call that `app` makes itself is the download of a finished
  job's output. `app::jobs` asks the adapter whether it may fetch the recorded URI
  (`VideoProvider::check_output_uri`), attaches the provider's credential header only when the URI
  has the configured base URL's origin, and streams the file through `http::download`. That
  function follows redirects itself, and sends the credential only to hops on the same origin.

## Modules

| Module | Responsibility |
|---|---|
| `domain` | Plain types that every module shares. `ProviderId` is also each provider's fixed identity: its ID, credential variable, default base URL, and base URL variable. Also `Operation`, `ModelSource`, `Billing`, the job and download status enums, `Artifact`, `Usage`, `CostEstimate`, and `Warning` with the `WarningCode` registry that every warning comes from. |
| `error` | `IrisError`, `ErrorCode`, `ErrorCategory`, and the mapping to exit codes. See the [Errors reference](../reference/errors.md). |
| `secret` | The `Secret` type. Its `Debug` and `Display` print `***`, and it never implements `Serialize`. Credentials are `Secret` values from the moment Iris reads them from the environment. |
| `redact` | `redact_url`, which removes user information and replaces query values with `REDACTED` except for an allowlist; `scrub`, which removes any configured credential value from text; and `truncate`. Every error message, provider message, log line, and persisted `last_error` passes through them before it reaches stdout, stderr, or disk. |
| `catalog` | The static model catalog: each model's summary, operations, inputs, typed options with defaults and allowed values, outputs, prices, cost estimators, and access notes, and the names that Iris declines. Each provider's declarations are in `catalog/openai.rs`, `catalog/gemini.rs`, and `catalog/veo.rs`. |
| `providers` | The `Provider`, `ImageProvider`, and `VideoProvider` traits, `ProviderContext`, and `Registry::builtin()`. Each provider's wire types and HTTP calls are private to `providers/openai/` or `providers/gemini/`. |
| `http` | The shared HTTP client, the retry executor with its retry classes (see [Where invariants live](#where-invariants-live)), streaming downloads with the credential-origin rule, and error classification helpers. |
| `jobs` | Persisted job records (`JobRecord`, versioned) and `JobStore`, which owns `STATE_DIR/jobs/`: atomic writes, per-job locks, listing without locks, and local deletion. Only `video.generate` creates records. |
| `artifacts` | Output path planning and file names (`paths`); media sniffing and validation, including image decoding and the ISO-BMFF structure of videos (`media`); input image validation (`input`); atomic finalization through `.NAME.iris-part-RANDOM` temporary files, without replacing an existing file unless `--overwrite` is given (`finalize`, `download`); and the `unsaved` fallback for paid outputs that can't be saved where requested (`fallback`). |
| `config` | The config file, environment variables, the resolution order (flag, environment variable, config file, default), and platform paths. It resolves per-provider settings for every `ProviderId`. |
| `app` | The workflows, one submodule per area: `image`, `video`, `jobs`, `models` (which also serves `providers list`), `info` (`version`, `config show`, and `config path`), and `doctor`. `app::catalog` is the `Catalog` type, for model lookup and resolution. `app::context` is the `AppContext` that every workflow receives. `app::request` holds the steps that the generation commands share: model resolution, prompt and option checks, cost estimates, and dry-run plans. `app` has no clap types and prints nothing: it takes typed arguments, reports progress through a `Progress` trait, collects warnings, and returns results or an `IrisError`. |
| `output` | The JSON envelope and result types, built with `serde` and `schemars` so that the published schema comes from the serialized types, and the human-readable rendering. |
| `cli` | The clap definitions, prompt sources (argument, file, or standard input), dispatch to `app`, and output as JSON or text. `cli::run` is the process entry point. |

## Sync and async: two provider traits

Image generation and video generation are different kinds of operation, so they have different
traits. A provider implements the shared `Provider` trait, for its identity, credential header,
documentation link, and the free `check_access` metadata call. It then provides `image()`,
`video()`, or both:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// Its default base URL and credential variable are the `ProviderId`'s.
    fn id(&self) -> ProviderId;
    fn credential_header(&self) -> CredentialHeader;
    /// The provider's official documentation (shown by `providers list`).
    fn docs_url(&self) -> &'static str;
    async fn check_access(&self, model_id: &str, ctx: &ProviderContext) -> Result<AccountAccess, IrisError>;
    fn image(&self) -> Option<&dyn ImageProvider> { None }
    fn video(&self) -> Option<&dyn VideoProvider> { None }
}

#[async_trait]
pub trait ImageProvider: Send + Sync {
    /// `ImageFailure` is the `IrisError` plus the content of returned items that are
    /// not usable images, when a completed response had no usable image at all.
    async fn generate(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure>;
    async fn edit(&self, req: &ImageRequest, ctx: &ProviderContext) -> Result<ImageOutput, ImageFailure>;
}

#[async_trait]
pub trait VideoProvider: Send + Sync {
    /// Local checks `submit` would make, run before the job record exists (default: none).
    fn validate(&self, req: &VideoRequest) -> Result<(), IrisError> { Ok(()) }
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

`ImageProvider::generate` and `edit` return the images themselves: usable images in
`ImageOutput::images`, and the content of any other returned item, as received, in `unusable`, which
the app keeps in the `unsaved` folder. Nothing needs to persist between the request and the
response, so image calls create no job record. A lost connection after the provider accepted the
request can't be recovered, and is reported as `submission_uncertain` with `job_id: null` and
`details.charge_possible: true`. See
[How Iris handles paid requests](../concepts/paid-requests.md#when-the-outcome-is-uncertain).

`VideoProvider` splits `submit`, which is paid and sent once, from `poll`, which is idempotent and
safe to repeat from any process. Iris writes the job record before it submits, so a crash during
the submission still leaves a record to diagnose (`submission_unknown`). This split is what makes
`--detach` and `jobs status`, `wait`, and `download` possible. Image calls get none of these,
because the image APIs return no operation ID to recover.

`RemoteStatus` is `Running { progress }`, `Succeeded { outputs, usage, warnings }`,
`Failed { error }`, or `Gone { error }`. `Gone` means that the provider doesn't know the operation;
the job record turns it into `expired` only after the retention period. The shape is the same for
every provider, so `app::jobs` runs one poll loop for all of them.

`VideoProvider` has no `cancel` method, and Iris has no `jobs cancel` command, because no provider
that Iris supports lets you cancel an accepted job. See
[Deleting job records](../concepts/video-jobs.md#deleting-job-records).

Downloads go through the generic `http::download`. `app::jobs` attaches a provider's
`credential_header()` only when the download URL's scheme, host, and port match the provider's
configured base URL, and `http::download` drops it on every redirect to another origin. The adapter
decides only which output URIs Iris may fetch at all (`check_output_uri`), so the credential rule
needs no per-provider download code.

## Where invariants live

- **Options are never silently dropped.** `catalog::validate_request` checks every option, typed or
  `-O`, against the resolved model's declared `OptionSpec`s for the operation, once, before any
  provider is called. An adapter that receives an option without a wire mapping returns
  `internal_error` instead of dropping it: that case is a bug, because validation should have
  rejected the option.
- **Credentials never reach disk, argv, or another host.** `Secret` has no `Serialize` and a
  redacting `Debug`, so printing a credential by accident doesn't compile into output. `config`
  rejects config keys that look like credentials. The HTTP layer attaches a provider's credential
  header only when a request's scheme, host, and port match that provider's configured base URL,
  including across redirects. See [Security and privacy](../concepts/security-and-privacy.md).
- **A download failure is never a generation failure.** Downloads in `app::jobs` are a separate step
  from submitting and polling in `app::video`. `artifacts::finalize` writes through a temporary file
  and renames it into place, so a failed or repeated download can't damage a file that already
  succeeded, and a download never submits anything. See
  [Downloads](../concepts/video-jobs.md#downloads).
- **An ambiguous paid submission is never retried.** `http::retry` has three retry classes:
  `PaidSubmit`, `IdempotentRead`, and `Download`. `PaidSubmit` retries only outcomes that provably
  didn't reach the provider, such as a connection failure before sending, or that the provider
  explicitly rejected before processing, such as a documented rate limit or overload. Every other
  open outcome is reported as `submission_uncertain`, with no job for an image call and a
  `submission_unknown` job for a video, and Iris never sends it again.
- **Persisted state survives crashes and concurrent processes.** `jobs::JobStore` writes to a
  temporary file in the same directory, calls `sync_all`, and renames it into place. A per-job lock
  (`.lock`), held only for each read-modify-write, serializes updates. The store's lock
  (`labels.lock`) makes the label check and the new record's creation one step. `jobs list` reads
  without locks, so a stuck writer can't block it.

## Extending Iris

Adding a provider means a new adapter module, one line in `Registry::builtin()`, the provider's
identity in `ProviderId`, catalog declarations, and tests, not changes throughout `app` or `cli`.
Configuration, `doctor`, and redaction iterate `ProviderId::ALL`, and `providers list` iterates the
registry. The compiler doesn't check that `ALL` lists every variant, but a unit test in `domain`
does, and it also checks that `ALL` agrees with the registry and the catalog. The published schema,
a few help texts and pages that name providers, test fixtures, and the live verification script
are updated by hand. For the steps and the complete checklist, see
[Add a provider](adding-a-provider.md).
