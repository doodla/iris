# Adding a provider

Adding a provider to Iris is meant to be a focused, additive change: a new adapter module, one
line in the registry, catalog declarations, and tests — not edits scattered through `app` or
`cli`. This guide walks through it step by step, using a **hypothetical** Seedance (video)
provider as a worked example. Iris does not implement Seedance; nothing here is a real adapter,
and no Seedance API is called by Iris today.

Read [architecture.md](architecture.md) first for the module layout and the two provider traits
(`ImageProvider`, `VideoProvider`) this guide builds on.

## 1. Research and record decisions first

Before writing code: read the provider's *current* official developer documentation (not a
consumer app — see the README's note that ChatGPT/Gemini-app/Flow subscriptions are not API
access) for authentication, endpoints, request/response shapes, synchronous vs. asynchronous
behavior, supported inputs, retry/idempotency guidance, artifact retention, and pricing. Record
what you found and when you checked it — Iris's own catalog comments cite `research.md`/
`verification.md` files with dates and source URLs for exactly this reason; do the same for a new
provider so the next person can tell what is verified against current docs and what has drifted.

Decide honestly what Iris can support: if the provider's Rust SDK (if one exists) does not cover
what you need or lags the API, write a thin REST client instead of waiting on or working around
it (see `providers/gemini/` for a worked example: no official Google Rust SDK, so it is a plain
`reqwest` client). Never claim a capability, access guarantee, or recovery/cancellation path the
provider does not document.

## 2. Add the adapter module: `src/providers/seedance/`

Provider wire types (request/response structs, error bodies) are private to the adapter — nothing
outside `providers/seedance/` should know Seedance's JSON shapes. Following the existing
`providers/gemini/` layout:

```
src/providers/seedance/
  mod.rs      -- Provider + VideoProvider impl, module-level docs
  client.rs   -- endpoint URLs, the credential header, shared error-status mapping
  wire.rs     -- request/response types (private)
```

Implement `Provider` (identity, default base URL, credential header, the free `check_access`
metadata call) and `VideoProvider` (`submit`, `poll`, `output_retention`, and optionally
`validate` — the local checks `submit` makes, run before the job record is written — and
`check_output_uri`) — see
[architecture.md](architecture.md#sync-vs-async-two-provider-traits-on-purpose) for the exact
trait shapes. A provider that only does images implements `image()` instead of `video()`; a
provider that does both implements both and returns `Some(self)` from each.

Invariants an adapter must uphold (enforced by review and by Iris's own tests, not by the
compiler alone):

- **Never silently drop an option.** `ResolvedOptions` only ever contains options the catalog
  declared for this model and operation (validation happens once, centrally, before any adapter
  is called — see step 3). If your wire-mapping `match` hits an option name it does not know what
  to do with, that is a bug in the catalog declaration, not something to ignore: return
  `IrisError::internal(...)`, the same way `providers/openai/wire.rs` and
  `providers/gemini/veo.rs` do for their own "unmapped option" branches.
- **A paid submission is sent once.** `submit` (and any synchronous image call) uses the
  `PaidSubmit` retry class, which retries *only* a connection failure before the request was sent,
  a provider-documented rate-limit rejection, or a provider-documented "not processed" overload
  rejection — never a timeout or reset after sending, and never a bare 5xx. If a paid request's
  outcome is ambiguous, return an error with code `submission_uncertain` and
  `details.charge_possible: true` (video or synchronous image); never resubmit it
  yourself. See `providers/gemini/veo.rs::classify_submit` for the reference implementation of
  this rule.
- **`poll` is idempotent** and uses the `IdempotentRead` retry class (retried on connect errors,
  timeouts, 408/429/5xx). It must never mutate anything Iris cannot safely repeat.
- **Report retention honestly.** `output_retention()` returns `Some(duration)` only when the
  provider documents how long outputs stay downloadable, and `None` otherwise — Iris uses this to
  warn before an artifact expires (`retention_limited`), never to promise a number no one
  published.
- **Declare cancellation honestly, by omission.** There is no `cancel` method on `VideoProvider`
  and no `jobs cancel` command in the CLI (see [jobs.md](jobs.md#local-deletion-vs-remote-state))
  because no provider Iris implements today offers one. If your provider *does* document a cancel
  endpoint, that is a CLI-contract change (a new command), not something to bolt onto the existing
  trait — treat it as a deliberate, documented CLI/JSON compatibility change (see
  [AGENTS.md#compatibility](../AGENTS.md#compatibility)) rather than improvising.
- **Downloads use the shared credential-origin rule**, not a bespoke one: `http::download`
  already attaches `Provider::credential_header()` only when the download URL's scheme+host+port
  match the provider's configured base URL, and follows redirects manually so a redirect off that
  origin drops the credential. `poll` records every output URI of a finished job as given (after
  at most a structural check: a URL, http(s), no userinfo or fragment), so a job the provider
  finished is always `succeeded`. Which URIs Iris is willing to fetch is decided at download time,
  against the base URL configured then, by `VideoProvider::check_output_uri` (see
  `providers/gemini/veo.rs::validate_output_uri` for the pattern: same origin, a narrow allowed
  path shape); a refusal fails that output's download, never the job.
- **Redact before an error can leak.** Route provider error text through `redact::scrub` (removes
  configured credential values) before it reaches `IrisError` — see
  `providers/openai/client.rs::scrub_credential` for the reference pass over every string field of
  an error.

## 3. Declare the model(s) in the catalog: `src/catalog/seedance.rs`

Every model is one `ModelSpec`:

```rust
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "seedance-1-pro",                       // sent to the provider verbatim
        provider: ProviderId::Seedance,
        display_name: "Seedance 1 Pro",
        aliases: &[],                                 // e.g. dated-snapshot aliases
        lifecycle: Lifecycle::Preview,                 // ga | preview | deprecated, as documented
        operations: &[Operation::VideoGenerate],
        default_for: &[],                              // set once this is the provider's default
        inputs: InputSpec { /* max_input_images, media types, mask, first/last frame, references */ },
        options: OPTIONS,                              // every accepted option, typed (see below)
        outputs: OutputSpec { media_types: &["video/mp4"], max_count: 1 },
        limits: Limits { max_prompt_chars: Some(2000) },
        pricing: PRICING,                              // &[PriceRule { .. }], with source_url + as_of
        access_notes: &["Preview model", "Paid tier required"],
        docs_url: "https://…",
        validate: Some(validate_options),              // cross-field rules (e.g. "1080p requires duration=8")
        estimate: Some(estimate_pre_call),              // pre-call cost estimate, or None if unsupportable
        estimate_usage: None,                           // post-call estimate from reported usage, if any
    },
];
```

Then:

1. Add a `Seedance` variant to the `ProviderId` enum (`src/domain.rs`): its serde name
   (`"seedance"`), an entry in `ProviderId::ALL` (config, doctor, and redaction all iterate this
   to cover every provider), and a `credential_env()` match arm naming its environment variable.
   Iris reads provider keys only from named environment variables (`OPENAI_API_KEY`,
   `GEMINI_API_KEY` today); document the new one in [configuration.md](configuration.md). Add the
   new module to the catalog's `all()` chain (`src/catalog/mod.rs`).
2. Give the adapter a `CredentialHeader` (header name and value prefix, e.g. `Authorization` /
   `Bearer `) matching how the provider documents authentication.
3. Declare every option the model accepts as an `OptionSpec` (`OptionKind::Enum`, `Integer { min,
   max }`, `Boolean`, `Text { max_chars }`, or `Pattern` with a custom validator). An option in
   the typed-flag table of its command (`--count`, `--duration`, …) must use that flag name;
   anything else is reachable only through `-O name=value`. The tables are `IMAGE_FLAGS` (image
   commands) and `VIDEO_FLAGS` (`video generate`) in `src/cli/args.rs`, each next to the clap
   struct that defines the flags. `tests/openai_catalog.rs::typed_flags_follow_the_cli_flag_table`
   and `tests/gemini_catalog.rs::typed_flags_follow_the_cli_flag_tables_for_every_gemini_provider_model`
   keep declared models consistent with them, and
   `tests/cli_process.rs::every_typed_flag_is_declared_by_some_model_for_its_command` keeps any
   command from offering a flag no model accepts. **Never accept an option Iris cannot map on the wire side** — if the
   provider takes it but your adapter has nowhere to put it yet, leave it out of the catalog
   rather than declaring it and dropping it.
4. If the model needs cross-field validation (Iris's Veo catalog has "1080p or 4k requires an
   8-second duration", "a last frame requires a first frame"), write a `validate` function with
   the same shape as `catalog::veo::validate_video`.
5. Cite your sources: `pricing` entries carry `source_url` and `as_of`; `access_notes` state
   documented account requirements in the provider's own words, never inferred ones.

## 4. Register the adapter

One line in `Registry::builtin()` (`src/providers/mod.rs`):

```rust
pub fn builtin() -> Self {
    Registry {
        providers: vec![
            Arc::new(openai::OpenAiProvider::new()),
            Arc::new(gemini::GeminiProvider::new()),
            Arc::new(seedance::SeedanceProvider::new()),
        ],
    }
}
```

That is the only place `app` learns a new provider exists — it looks up adapters solely through
the registry, by `ProviderId`, so nothing in `app` or `cli` needs to change to make the new
provider reachable from every command (`models list`, `providers list`, `image`/`video generate`,
`doctor`, …).

## 5. Tests

Mirror the existing per-provider test files (`tests/openai_catalog.rs` /
`tests/openai_adapter.rs`, `tests/gemini_catalog.rs` / `tests/gemini_image.rs`,
`tests/veo_catalog.rs` / `tests/veo_adapter.rs`):

- **Catalog tests**: every declared option round-trips through validation with its documented
  default, min/max, and enum values; the typed-flag mapping table is respected; pricing/estimate
  functions return sane, labeled-as-estimate values (or `None` with `cost_estimate_unavailable`
  when a point estimate is not supportable, e.g. an `auto` quality).
- **Adapter tests**: request encoding matches the documented wire shape exactly (a snapshot or
  field-by-field assertion against a built request, not just "it doesn't panic"); response
  parsing handles the documented success shape *and* the provider's error shapes (auth failure,
  rate limit, content block, malformed body); the `PaidSubmit` vs. `IdempotentRead` retry
  classification matches what you decided in step 2, tested against a local `wiremock`
  server — **never** a real endpoint (see [live-testing.md](live-testing.md) for how the small,
  budgeted, opt-in live check is separated from the ordinary offline suite).
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before
  committing — the same checks every commit in this repository passes (see
  [CONTRIBUTING.md](../CONTRIBUTING.md)).

## What you should not need to touch

If adding a provider requires changing `cli/args.rs` beyond adding the provider to a documented
enum, or requires new branches in `app/image.rs` / `app/video.rs` keyed on which provider was
selected, that is a sign the abstraction in `providers` needs to grow first — fix the trait or the
shared catalog types, not the application layer, and record why as a deliberate, documented
CLI/JSON compatibility change (see [AGENTS.md#compatibility](../AGENTS.md#compatibility)).
