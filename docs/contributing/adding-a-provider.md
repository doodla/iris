# Add a provider

This guide shows how to add a provider to Iris. It uses a hypothetical Seedance video provider as a
worked example: Iris doesn't implement Seedance, and nothing on this page is a real adapter.

Adding a provider should be a focused change: an adapter module, one line in the registry, the
provider's identity in `ProviderId`, catalog declarations, and tests. It shouldn't need edits
throughout `app` or `cli`. The [checklist](#checklist) at the end lists every file that a new
provider touches, including the schema, help text, and pages that name providers.

Before you start, read [Architecture](architecture.md) for the module layers and the provider
traits.

## Step 1: Research the provider, and record what you find

Before you write code, read the provider's current developer documentation, not its consumer apps.
Look for authentication, endpoints, request and response shapes, whether requests are synchronous
or asynchronous, supported inputs, retry and idempotency guidance, output retention, and pricing.
A consumer subscription doesn't give API access, so the API documentation is the only source.

Record what you found and when you checked it, so the next person can tell what's verified and
what may have drifted:

- The module header of each catalog file, such as `src/catalog/openai.rs`, names the documentation
  that its values come from, and the date it was checked.
- The price tables carry their source (`PRICING_URL`) and date (`CATALOG_AS_OF`, or
  `PRICING_AS_OF` for OpenAI) into every `pricing` entry and cost estimate.
- The adapters' module headers explain their wire-format choices.
- [Decisions](decisions.md) records the consequential choices, with sources and the date that they
  were checked.

Support only what the provider documents. If its Rust SDK doesn't cover what you need, or lags the
API, write a thin REST client instead, as `providers/gemini/` does: Google has no official Rust
SDK, so that adapter is a plain `reqwest` client. Never claim a capability, an access guarantee, or
a way to recover or cancel a job that the provider doesn't document.

## Step 2: Add the adapter module

Keep the provider's wire types, such as request and response structs and error bodies, private to
its adapter. Nothing outside `providers/seedance/` should know Seedance's JSON shapes. Follow the
layout of `providers/gemini/`:

```text
src/providers/seedance/
  mod.rs      -- Provider + VideoProvider impl, module-level docs
  client.rs   -- endpoint URLs, the credential header, shared error-status mapping
  wire.rs     -- request/response types (private)
```

Implement `Provider`: `id`, the credential header, `docs_url`, and the free `check_access` metadata
call. The default base URL comes from `ProviderId`. Then implement `VideoProvider`: `submit`,
`poll`, and `output_retention`, and optionally `validate`, which runs the local checks that
`submit` makes before the job record is written, and `check_output_uri`. For the trait
definitions, see [Architecture](architecture.md#sync-and-async-two-provider-traits). A provider
with image models implements `image()` instead of `video()`, and a provider with both implements
both and returns `Some(self)` from each.

An adapter must uphold these rules. Review and Iris's tests enforce them; the compiler alone
doesn't:

- **Never silently drop an option.** `ResolvedOptions` contains only options that the catalog
  declared for the model and operation, because validation runs once, centrally, before any adapter
  is called. If your wire mapping meets an option that it doesn't know, the catalog declaration has
  a bug. Return `IrisError::internal(...)`, as the "unmapped option" branches of
  `providers/openai/wire.rs` and `providers/gemini/veo.rs` do.
- **Send a paid submission once.** `submit`, and any synchronous image call, uses the `PaidSubmit`
  retry class. It retries only a connection failure before the request was sent, a documented
  rate-limit rejection, or a documented "not processed" overload rejection: never a timeout or
  reset after sending, and never a bare 5xx. When a paid request's outcome is ambiguous, return
  `submission_uncertain` with `details.charge_possible: true`, and never resubmit it yourself.
  `providers/gemini/veo.rs::classify_submit` is the reference implementation.
- **Hand every paid item back.** A synchronous image adapter types each returned item by its bytes,
  and keeps every usable image with its position in the response (`GeneratedImage::item`). An item
  that isn't a usable image is reported with `output_item_unusable`, and its content, decoded or
  as received, goes in `ImageOutput::unusable`, or in `ImageFailure::unusable` when no item was
  usable, so that the app can keep it. Adapters never write files. An error built from a completed
  response carries the usage that the response reported (`providers::with_reported_usage`), and
  `details.charged: true` only when the provider documents that such a response is billed.
- **Build warnings from the registry.** Use `Warning::new(WarningCode::…, message)`, and reuse an
  existing code when one fits, such as `output_format_mismatch`, `output_item_unusable`, or
  `unexpected_output_count`. To add a code, add it to `WarningCode` in `src/domain.rs` and to the
  table in the [Errors reference](../reference/errors.md#warning-codes). A contract test checks
  that both lists agree, and that no other source file spells out a code.
- **Keep `poll` idempotent.** It uses the `IdempotentRead` retry class, which retries connection
  errors, timeouts, and HTTP 408, 429, and 5xx. It must never change anything that Iris can't
  safely repeat.
- **Report retention only as documented.** `output_retention()` returns `Some(duration)` only when
  the provider documents how long outputs stay downloadable, and `None` otherwise. Iris uses it to
  warn before an output expires (`retention_limited`), never to promise a time that no one
  published.
- **Leave out cancellation.** `VideoProvider` has no `cancel` method, and Iris has no
  `jobs cancel` command, because no provider that Iris supports offers one. If your provider
  documents a cancel endpoint, that's a new command: a deliberate change to the CLI contract under
  the [compatibility rules](https://github.com/doodla/iris/blob/main/AGENTS.md#compatibility), not
  something to add to the existing trait.
- **Use the shared credential-origin rule for downloads.** `http::download` attaches
  `Provider::credential_header()` only when the download URL's scheme, host, and port match the
  provider's configured base URL, and follows redirects itself, so a redirect to another origin
  drops the credential. `poll` returns every output URI of a finished job as given, in sample order.
  The job record then fails only an output whose URI no download could use (not an `http` or
  `https` URL, or with user information or a fragment), with `provider_bad_response` and an
  `output_item_unusable` warning, and keeps the others. A finished job is `failed` only when the
  provider reports an error, or none of its outputs has a usable URI. Which usable URIs Iris fetches
  is decided at download time, against the base URL configured then, by
  `VideoProvider::check_output_uri`. `providers/gemini/veo.rs::validate_output_uri` shows the
  pattern: the same origin and a narrow path shape. A refused URI fails that output's download,
  never the job.
- **Redact before an error can leak.** Pass provider error text through `redact::scrub`, which
  removes configured credential values, before it reaches `IrisError`.
  `providers/openai/client.rs::scrub_credential` is the reference: it cleans every string field of
  an error.

## Step 3: Give the provider its identity

1. In `src/domain.rs`, add a `Seedance` variant to the `ProviderId` enum and an entry to
   `ProviderId::ALL`. Then add a match arm to each identity method:
   - `as_str`: `"seedance"`, which is also the `--provider` filter value and the config table name.
   - `display_name`.
   - `credential_env`: the one environment variable that holds the key, such as
     `SEEDANCE_API_KEY`.
   - `default_base_url`.
   - `base_url_env`: `IRIS_SEEDANCE_BASE_URL`.

   `as_str` must equal the variant's serde name. The enum's `#[serde(rename_all = "snake_case")]`
   names `Seedance` `seedance`. A variant that the rule would spell differently needs its own
   `#[serde(rename = "…")]`, as `OpenAi` has for `openai`.
2. Give the adapter a `CredentialHeader`, with the header name and value prefix that the provider
   documents, such as `Authorization` and `Bearer `.

The compiler points out every missing match arm, but not a missing `ALL` entry or a mismatched
name. Two unit tests in `src/domain.rs` catch those:

- `all_lists_every_provider_once` fails until `ALL` lists every variant exactly once, the registry
  has one adapter per provider in `ALL` order (see [Step 5](#step-5-register-the-adapter)), and
  the catalog has models for each provider.
- `identity_names_follow_the_id` checks that the serde name, `--provider` parsing, and
  `IRIS_<ID>_BASE_URL` all follow `as_str`.

Configuration, `doctor`, and credential redaction iterate `ProviderId::ALL`, and `providers list`
iterates the registry. All of them read these methods, so they pick up the new provider without
further edits.

## Step 4: Declare the models in the catalog

Create `src/catalog/seedance.rs`. Every model is one `ModelSpec`:

```rust
pub static MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "seedance-1-pro",                       // sent to the provider verbatim
        provider: ProviderId::Seedance,
        display_name: "Seedance 1 Pro",
        summary: "…",                                 // what it is for and its trade-off, in one line
        aliases: &[],                                 // e.g. dated-snapshot aliases
        lifecycle: Lifecycle::Preview,                 // ga | preview | deprecated, as documented
        billing: Billing::Paid,                        // billed at the provider's published prices
        operations: &[Operation::VideoGenerate],
        inputs: InputSpec { /* image counts, media types, sizes, mask rules, frames, references,
                               inline request cap */ },
        options: OPTIONS,                              // every accepted option, typed (see below)
        outputs: OutputSpec { media_types: &["video/mp4"], max_count: 1 },
        limits: Limits { max_prompt_chars: Some(2000) },
        pricing: PRICING,                              // &[PriceRule { .. }], with source_url + as_of
        access_notes: &["Preview model", "Paid tier required"],
        docs_url: "https://…",
        validate: Some(RULES),                         // cross-field rules and their published constraints
        estimate: Some(Estimator {                      // pre-call cost estimate, or None if unsupportable
            estimate: estimate_pre_call,
            standard: &[&[("duration", "8"), ("resolution", "720p")]], // requests for the standard output
        }),
        estimate_usage: None,                           // post-call estimate from reported usage, if any
    },
];

/// Names Iris gives no model (retired or other-platform ids, a family of them, or a
/// nickname), with the provider's reason and date and the models to use instead.
pub const DECLINED: &[DeclinedName] = &[DeclinedName {
    names: &[],
    families: &["seedance-1.0"],                        // matches seedance-1.0 and seedance-1.0-…
    reason: "… shut down seedance-1.0 on …",           // as the provider documents it
    instead: &["seedance-1-pro"],                       // catalog ids or aliases
}];
```

Add the module to `src/catalog/mod.rs`: `pub mod seedance;`, its `MODELS` in the `all()` chain, its
`DECLINED` names in the `declined_names()` chain, and a `model_id_syntax` arm for the IDs that its
adapter can send. Then fill in the declarations:

- **Options.** Declare every option that the model accepts as an `OptionSpec`: `OptionKind::Enum`
  of strings, `IntegerEnum` of integers, `Integer { min, max }`, `Boolean`, `Text { max_chars }`,
  or `Pattern` with a custom validator. An option in its command's typed-flag table, such as
  `--count` or `--duration`, must use that flag's name. Every other option is available only with
  `-O name=value`. The tables are `IMAGE_FLAGS` and `VIDEO_FLAGS` in `src/cli/args.rs`, next to
  the clap structs that define the flags. Three tests keep them consistent:
  `tests/openai_catalog.rs::typed_flags_follow_the_cli_flag_table`,
  `tests/gemini_catalog.rs::typed_flags_follow_the_cli_flag_tables_for_every_gemini_provider_model`,
  and `tests/cli_process.rs::every_typed_flag_is_declared_by_some_model_for_its_command`. Never
  declare an option that your adapter can't send: if the provider accepts it but your adapter has
  nowhere to put it yet, leave it out of the catalog.
- **Inputs.** Declare the input rules that the provider documents in `InputSpec`: accepted types
  and sizes, mask rules (`MaskSpec`), and a limit on the whole request when inputs are sent inline
  (`RequestSizeLimit`, with allowances for the JSON that your adapter adds). Iris enforces them
  before a dry run returns and before it needs a credential. Checks inside the adapter are only a
  second line of defense: a dry run must never accept a request that the adapter would refuse.
- **Rules between options.** If the model has rules that relate options, such as Veo's "1080p or 4k
  requires an 8-second duration" and "a last frame requires a first frame", write a check function
  shaped like `catalog::veo::validate_video`. Declare every rule that it enforces as a `Constraint`,
  and set `validate: Some(RequestRules { constraints, check })`. The check reports a broken rule
  with `Constraint::violation`, so the error names it, and `models show` publishes the constraints.
  Call `catalog_support::assert_constraints_cover_the_validator` from your catalog tests. It tries
  every combination of declared values, and fails if the check rejects something that isn't a
  declared constraint, or a declared constraint is never enforced.
- **Sources.** `pricing` entries carry `source_url` and `as_of`. `access_notes` state documented
  account requirements in the provider's own words, never inferred ones. The `summary` says what the
  model is for and its trade-off, in the provider's documented terms. Where the provider says
  nothing that helps a choice, state factual differences, such as options and prices, not
  marketing.
- **Estimates.** If the model's prices support an estimate before the request, set `estimate` to an
  `Estimator`. It holds the estimate function, and in `standard`, the option values of the requests
  that give exactly the standard output of the model's operations (`StandardOutput`: one 1024x1024
  image for image operations, or one 8-second 720p video). List one request per setting that still
  changes the price of that output, as the OpenAI models list every quality. Options that you don't
  list keep their defaults. When the function can't estimate a request, as for OpenAI's `auto`
  quality or size, it returns why and which options to pass, which becomes the
  `cost_estimate_unavailable` warning. `models list`, `models show`, and the `model_required` and
  `unknown_model` candidates report the standard output's estimates as `standard_cost`, from the
  same function. Call `catalog_support::assert_standard_requests_give_the_standard_output` from your
  catalog tests, with a function that says which output a request gives from your catalog's own
  declarations. It checks that each declared request is valid for every operation of the model, asks
  for one output, gives the standard output, and is estimated by the model's own estimator.

## Step 5: Register the adapter

Add one line to `Registry::builtin()` in `src/providers/mod.rs`:

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

With `pub mod seedance;` next to the other adapter modules, that's the only place where `app` learns
about the adapter. `app` looks adapters up through the registry by `ProviderId`, so no command
handler in `app` or `cli` changes: every command can reach the new provider.

## Step 6: Write tests

Mirror the existing test files of each provider: `tests/openai_catalog.rs` and
`tests/openai_adapter.rs`, `tests/gemini_catalog.rs` and `tests/gemini_image.rs`, and
`tests/veo_catalog.rs` and `tests/veo_adapter.rs`.

- **Catalog tests** check that every declared option round-trips through validation with its
  documented default, limits, and values; that the typed-flag tables are respected; and that price
  and estimate functions return sane values labeled as estimates. When a request can't be
  estimated, such as with an `auto` quality, they check the reason and the options to pass.
- **Adapter tests** check that request encoding matches the documented wire shape exactly, with a
  snapshot or field-by-field assertions, not only a check that nothing panics. Response parsing must
  handle the documented success shape and the provider's error shapes: authentication failure, rate
  limit, content block, and a malformed body. Check that the `PaidSubmit` and `IdempotentRead` retry
  classification matches Step 2, against a local `wiremock` server, never a real endpoint. Live
  checks are separate, paid, and opt-in: see [Live testing](live-testing.md).
- Test fakes that match on `ProviderId` exhaustively, such as `FakeProvider::credential_header` in
  `tests/app_support.rs`, need an arm for the new provider. The compiler shows where.

Before you commit, run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test`, like every commit in this repository. See the
[contributing guide](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md).

## Checklist

Everything that a provider like the Seedance example touches.

**The provider's own code:**

- `src/providers/seedance/`, the adapter, and in `src/providers/mod.rs`, `pub mod seedance;` and
  one `Registry::builtin()` line.
- `src/catalog/seedance.rs`, and in `src/catalog/mod.rs`, its `pub mod`, its models in `all()`, its
  declined names in `declined_names()`, and a `model_id_syntax` arm.
- `ProviderId` in `src/domain.rs`: the variant, the `ALL` entry, and one arm in each of `as_str`,
  `display_name`, `credential_env`, `default_base_url`, and `base_url_env`. The compiler checks the
  arms, and the `domain` unit tests check the `ALL` entry and the names (see
  [Step 3](#step-3-give-the-provider-its-identity)).
- The adapter and catalog tests (see [Step 6](#step-6-write-tests)).

**Derived from those, with no further edits:**

- The `--provider seedance` filter of `models list` and `jobs list`.
- Its models in `-m`, `image.model`, and `video.model`.
- The `[providers.seedance]` config table, with the same keys as the others (`base_url`,
  `request_timeout`, `submit_timeout`), and the `IRIS_SEEDANCE_BASE_URL` override.
- Its rows in `config show`, and the `non_default_base_url` warning.
- `doctor`'s `credentials.seedance`, `base_url.seedance`, and `access.seedance.MODEL` checks.
- The `providers list` entry.
- Redaction of its key from every message, and the config file's refusal of credential-like keys,
  whose message lists every provider's variable.
- The offline test harness. Every `iris` process that `cargo test` starts has each provider's
  credential variable removed, and its base URL pointed at a dead local port, so a developer's real
  key never reaches the offline suite. See `configure` in `tests/cli_process.rs`, and `Iris::new`
  and `credential_vars` in `tests/support/process.rs`.

**Updated by hand:**

- **The JSON Schema.** It lists the known provider IDs, as an open set, so an unknown ID still
  validates. Regenerate it with `cargo run -q -- schema > schema/iris-output.v1.schema.json`;
  `tests/schema_contract.rs` fails until you do. A new provider is an additive change under the
  [versioning policy](../reference/json-output.md#versioning): no `schema_version` change, but a
  changelog entry.
- **Help text** in `src/cli/args.rs` that names the providers, their products, or their variables:
  `ABOUT`, `LONG_ABOUT` (its first paragraph names each provider's image and video products, and
  its credentials sentence names the variables), and the `providers list` description. A test in
  `tests/cli_process.rs` fails until the top-level help names the new credential variable. Then
  regenerate the [CLI reference](../reference/cli.md) with
  `IRIS_UPDATE_DOCS=1 cargo test --test cli_reference`.
- **Package metadata** in `Cargo.toml`: `description`, which names OpenAI and Google Gemini/Veo,
  and `keywords`, of which crates.io allows at most five.
- **Documentation:**
  - The README's model table.
  - The API key table and the settings table of the
    [Configuration reference](../reference/configuration.md).
  - The module table in [Architecture](architecture.md), which names `providers/openai/`,
    `providers/gemini/`, and each catalog file.
  - `CHANGELOG.md`.
  - [Decisions](decisions.md), with the API choices that you made, their sources, and the date
    that you checked them.
- **`AGENTS.md` and `SECURITY.md`**, which name the variables that Iris reads: a new variable is a
  deliberate change to the credential rule. The bug report template,
  `.github/ISSUE_TEMPLATE/bug_report.md`, lists the providers.
- **Test fixtures** for the provider's own tests:
  - For the process tests, add a fake key next to `OPENAI_KEY` and `GEMINI_KEY` in
    `tests/support/process.rs`. Add it to the key scans in `Out::assert_hygiene` there, and in
    `MockApi::assert_credentials_only_in` in `tests/support/mock.rs`. Add a helper like
    `Iris::gemini` that points the provider at a mock server with that key.
  - For the in-process app tests, add a fake key in `tests/app_support.rs`, set by `Sandbox::env`.
- **Live verification.** `scripts/live-verify.sh` names each provider's variables in its usage text
  and in three checks:
  - Its mock-mode guard, which requires every `IRIS_*_BASE_URL` to point at a local host, and every
    key to be fake.
  - Its refusal to run live while any `IRIS_*_BASE_URL` is set.
  - Its key-leak scan (`leaks_secret`).

  Add the new provider's variables to the usage text, all three checks, and
  [Live testing](live-testing.md). Add live steps for the provider, with their costs, only
  deliberately: they're paid.

Some code is deliberately specific to one provider, and a new provider doesn't need it: `doctor`'s
warning that `GOOGLE_API_KEY` is set but ignored, and each adapter's own error mapping.

## What you shouldn't need to change

If a provider needs changes beyond this checklist, such as in `cli/args.rs` beyond the help text
that names providers, or new branches in `app/image.rs` or `app/video.rs` that depend on which
provider was selected, the abstraction in `providers` needs to grow first. Change the trait or the
shared catalog types, not the application layer. If that changes the CLI or JSON output, treat it
as a deliberate, documented change under the
[compatibility rules](https://github.com/doodla/iris/blob/main/AGENTS.md#compatibility).
