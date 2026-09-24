# Adding a provider

Adding a provider to Iris is meant to be a focused, additive change: a new adapter module, one
line in the registry, the provider's identity in `ProviderId`, catalog declarations, and tests —
not edits scattered through `app` or `cli`. The
[checklist](#checklist-everything-a-new-provider-touches) at the end lists every file it touches,
including the schema, help text, and documents that name providers. This guide walks through it
step by step, using a **hypothetical** Seedance (video) provider as a worked example. Iris does not
implement Seedance; nothing here is a real adapter, and no Seedance API is called by Iris today.

Read [architecture.md](architecture.md) first for the module layout and the two provider traits
(`ImageProvider`, `VideoProvider`) this guide builds on.

## 1. Research and record decisions first

Before writing code: read the provider's *current* official developer documentation (not a
consumer app — see the README's note that ChatGPT/Gemini-app/Flow subscriptions are not API
access) for authentication, endpoints, request/response shapes, synchronous vs. asynchronous
behavior, supported inputs, retry/idempotency guidance, artifact retention, and pricing. Record
what you found and when you checked it, so the next person can tell what is verified against
current docs and what has drifted. Iris does this in the code itself: the module header of each
catalog file (`src/catalog/openai.rs`, `gemini.rs`, `veo.rs`) says which official documentation
its values come from and the date it was checked, the price tables carry their source (`PRICING_URL`) and
date (`CATALOG_AS_OF`, or `PRICING_AS_OF` for OpenAI) into every `pricing` entry and cost
estimate, the adapters' module headers explain their wire-format choices, and
[decisions.md](decisions.md) records the consequential choices with their sources and the date
they were checked. Do the same for a new provider.

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

Implement `Provider` (`id`, the credential header, `docs_url`, and the free `check_access`
metadata call; `default_base_url` comes from `ProviderId`) and `VideoProvider` (`submit`, `poll`,
`output_retention`, and optionally `validate` — the local checks `submit` makes, run before the
job record is written — and `check_output_uri`) — see
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
        inputs: InputSpec { /* image counts, media types, sizes, mask rules, frames, references,
                               inline request cap */ },
        options: OPTIONS,                              // every accepted option, typed (see below)
        outputs: OutputSpec { media_types: &["video/mp4"], max_count: 1 },
        limits: Limits { max_prompt_chars: Some(2000) },
        pricing: PRICING,                              // &[PriceRule { .. }], with source_url + as_of
        access_notes: &["Preview model", "Paid tier required"],
        docs_url: "https://…",
        validate: Some(RULES),                         // cross-field rules and their published constraints
        estimate: Some(estimate_pre_call),              // pre-call cost estimate, or None if unsupportable
        estimate_usage: None,                           // post-call estimate from reported usage, if any
    },
];
```

Then:

1. Give the provider its identity: a `Seedance` variant of the `ProviderId` enum
   (`src/domain.rs`), an entry in `ProviderId::ALL`, and a match arm in each of its identity
   methods: `as_str` (`"seedance"`, its `--provider` value and config table name),
   `display_name`, `credential_env` (the one environment variable its key is read from, e.g.
   `SEEDANCE_API_KEY`), `default_base_url`, and `base_url_env` (`IRIS_SEEDANCE_BASE_URL`).
   `as_str` must equal the variant's serde name: the enum's `#[serde(rename_all = "snake_case")]`
   names `Seedance` `seedance`, and a variant the rule would spell differently needs its own
   `#[serde(rename = "…")]`, as `OpenAi` has for `openai`. The compiler points at every missing
   match arm, but not at a missing `ALL` entry or a mismatched name; two unit tests in
   `src/domain.rs` catch those. `all_lists_every_provider_once` fails until `ALL` lists every
   variant exactly once, the registry has one adapter per provider in `ALL` order (step 4), and
   the catalog has models for each provider. `identity_names_follow_the_id` checks that the
   serde name, `--provider` parsing, and `IRIS_<ID>_BASE_URL` all follow `as_str`.
   Configuration, `doctor`, and credential redaction iterate `ProviderId::ALL`, `providers list`
   iterates the registry, and all of them read these methods, so they pick the provider up
   without further edits (see [the checklist below](#checklist-everything-a-new-provider-touches)).
   Then add the catalog module to `src/catalog/mod.rs`: `pub mod seedance;`, its `MODELS` in the
   `all()` chain, and a `model_id_syntax` arm for the ids its adapter can send.
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
4. Declare the input rules the provider documents in `InputSpec`: accepted types and sizes, mask
   rules (`MaskSpec`), and a cap on the whole request when inputs are sent inline
   (`RequestSizeLimit`, with allowances that bound the JSON your adapter adds). Iris enforces them
   locally before a dry run returns and before a credential is needed; checks inside the adapter
   are only a second line of defense, and a dry run must never accept a request the adapter would
   refuse.
5. If the model needs cross-field validation (Iris's Veo catalog has "1080p or 4k requires an
   8-second duration", "a last frame requires a first frame"), write a check function with the
   same shape as `catalog::veo::validate_video`, declare every rule it enforces as a `Constraint`
   next to it, and set `validate: Some(RequestRules { constraints, check })`. The check reports a
   broken rule with `Constraint::violation`, so the error names it, and `models show` publishes
   the constraints. Call `catalog_support::assert_constraints_cover_the_validator` from your
   catalog tests: it tries every combination of declared values and fails if the check rejects
   something that is not a declared constraint, or a declared constraint is never enforced.
6. Cite your sources: `pricing` entries carry `source_url` and `as_of`; `access_notes` state
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

Together with `pub mod seedance;` next to the other adapter modules, that is the only place
`app` learns about the adapter: it looks adapters up through the registry, by `ProviderId`, so no
command handler in `app` or `cli` changes to make the new provider reachable from every command
(`models list`, `providers list`, `image`/`video generate`, `jobs`, `doctor`, …).

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
- Test fakes that match on `ProviderId` exhaustively (`FakeProvider::credential_header` in
  `tests/app_support.rs`) need an arm for the new provider; the compiler shows where.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before
  committing — the same checks every commit in this repository passes (see
  [CONTRIBUTING.md](../CONTRIBUTING.md)).

## Checklist: everything a new provider touches

The complete list, for a provider like the Seedance example.

**The provider's own code:**

- `src/providers/seedance/` (the adapter), `pub mod seedance;` and one `Registry::builtin()` line
  in `src/providers/mod.rs`.
- `src/catalog/seedance.rs`, and in `src/catalog/mod.rs` its `pub mod`, its models in `all()`, and a
  `model_id_syntax` arm.
- `ProviderId` in `src/domain.rs`: the variant, the `ALL` entry, and one arm in each of `as_str`,
  `display_name`, `credential_env`, `default_base_url`, and `base_url_env`. The compiler checks
  the arms; the `domain` unit tests check the `ALL` entry and that the names agree (step 3).
- Adapter and catalog tests (step 5).

**Derived from those, with no further edits:** parsing `--provider seedance` and
`image.provider = "seedance"`; the `[providers.seedance]` config table with the same keys as the
others (`base_url`, `image_model`, `video_model`, `request_timeout`); the `IRIS_SEEDANCE_BASE_URL`
override; its rows in `config show`; the `non_default_base_url` warning; `doctor`'s
`credentials.seedance`, `base_url.seedance`, and `access.seedance.<model>` checks; the
`providers list` entry; redaction of its key from every message; and the config file's refusal of
credential-like keys, whose message lists every provider's variable.

**Contract, help text, and documents that name providers, updated by hand:**

- The JSON Schema. `ProviderId` is an enum in the published schema (every `provider` field), so
  regenerate it with `cargo run -q -- schema > schema/iris-output.v1.schema.json`
  (`tests/schema_contract.rs` fails until you do). A new provider value is an additive change
  under the [versioning policy](json-contract.md#schema-versioning-policy): no `schema_version`
  bump, but it gets a changelog entry.
- Help text in `src/cli/args.rs` that lists the providers or their variables: the top-level
  "Credentials are read only from the OPENAI_API_KEY and GEMINI_API_KEY environment variables",
  `--provider`'s "Provider: openai or gemini", and the `providers list` description.
- The default video provider. A video command without `--provider` or `--model` uses the provider
  whose catalog declares a default video model (`default_for` containing `video.generate`). If a
  second provider declares one, the first in `ProviderId` order wins; choose that order
  deliberately, or add a `video.provider` setting as a documented configuration change.
- Documentation: the README's setup section and support table,
  [configuration.md](configuration.md) (credential table, precedence table, full key set),
  `CHANGELOG.md`, and [decisions.md](decisions.md) (the API choices you made, with source links
  and the date you checked them; see section 1).
- `AGENTS.md`'s credential rule, which names the variables Iris reads: a new variable is a
  deliberate change to that rule.

Deliberately provider-specific, and not needed for a new provider: `doctor`'s warning that
`GOOGLE_API_KEY` is set but ignored, and each adapter's own error mapping.

## What you should not need to touch

If adding a provider requires changes beyond the checklist above (for example `cli/args.rs`
beyond the help text that names providers, or new branches in `app/image.rs` / `app/video.rs`
keyed on which provider was selected), that is a sign the abstraction in `providers` needs to grow
first — fix the trait or the shared catalog types, not the application layer, and record why as a
deliberate, documented CLI/JSON compatibility change (see
[AGENTS.md#compatibility](../AGENTS.md#compatibility)).
