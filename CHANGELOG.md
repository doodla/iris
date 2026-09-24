# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Iris intends to follow
[Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

No release has been tagged yet. The entries below describe v1's capabilities as implemented at
this commit; this section will be dated and renamed to a `[0.1.0]` release heading once a `v0.1.0`
tag is actually cut, per [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

Iris's planned first release: a Rust CLI that generates and edits images and generates videos through
OpenAI and Google, with a durable job model for provider-native asynchronous work and a
machine-readable contract for agents.

### Added

- **Image generation and editing** on OpenAI's Images API (GPT Image 2.5 Sunburst, GPT Image 2.5
  Flare, GPT Image 2) and Google's Gemini API (Nano Banana 2, Nano Banana 2 Lite, Nano Banana
  Pro), including editing/composing from local reference images and, on OpenAI, a mask. Both are
  synchronous.
- **Video generation** on Google Veo (3.1, 3.1 Fast, 3.1 Lite — all preview), a provider-native
  asynchronous job: durable local job records, `--detach` submit-and-return, and
  `jobs status`/`wait`/`download`/`delete` recovery from any later process. First-frame,
  last-frame, and reference-image inputs where the model documents support for them.
  Submission uncertainty (an ambiguous paid submit) is reported as `submission_uncertain` (exit 5;
  the job's own status field reads `submission_unknown`) and never resubmitted automatically.
- **A versioned `--json` contract**: one JSON envelope per command, a published JSON Schema
  (`iris schema`), a stable error-code taxonomy with a documented exit-code mapping, and
  structured warnings. See [docs/json-contract.md](docs/json-contract.md).
- **`iris models list`/`show`** describing every built-in model's declared capabilities, options,
  limits, published pricing, and documented access requirements, plus `--check-access` for a free
  per-account metadata check.
- **`iris jobs`** (`list`, `status`, `wait`, `download`, `delete`) for provider-native async job
  recovery, with atomic, versioned, lock-protected local persistence. Downloads are safe to
  repeat and never re-trigger generation. See [docs/jobs.md](docs/jobs.md).
- **`iris config`** (`show`, `path`) and **`iris doctor`** for configuration inspection and
  credential/access diagnostics (presence only — values are never printed). See
  [docs/configuration.md](docs/configuration.md).
- **`iris completions`** for bash, zsh, fish, and elvish, and **`iris version`**.
- `iris --json version` reports `git_commit`, the commit the binary was built from: set at build
  time from `IRIS_GIT_COMMIT` when it is 7 to 40 hex digits (the release and CI workflows set it
  to the commit they checked out), and `null` for any other build. `GITHUB_SHA` alone is not
  used, since in another project's workflow it names that project's commit. See
  [docs/install.md](docs/install.md#verifying-what-you-installed).
- **Cost estimates** (pre-call where supportable, post-call from reported usage otherwise),
  always explicitly labeled as estimates, never an invoice.
- A one-command installer (`install.sh`) for Linux x86_64 and macOS x86_64/ARM64, with checksum
  verification, pinned installs, and no `sudo`. See [docs/install.md](docs/install.md). (No
  release has been published yet — see that document for what works today.)
- Release archives hold `docs/` next to the binary, `LICENSE`, `README.md`, and `CHANGELOG.md`,
  so the README's links into `docs/` work in an unpacked archive. The installer accepts archives
  with or without `docs/` and still installs only the binary.
- CI (formatting, Clippy, offline tests on Linux and macOS, a pinned MSRV check, dependency
  license/advisory scanning, repeated weekly for new advisories) and a tag-triggered release
  workflow producing checksummed archives.
  CI also runs the release path on every change: it builds the Linux musl binary, checks that
  packaging it twice gives byte-identical archives, and smoke-tests the archive and `install.sh`
  with it. The release workflow publishes only after format, Clippy, and tests pass again on the
  tagged commit and every target's archive passes the same smoke test.
- `AGENTS.md`/`CLAUDE.md` durable agent instructions, and this project's documentation set under
  `docs/`.

### Changed

- A paid synchronous image request whose outcome is unknown — a timeout or dropped connection
  after it was sent, or an OpenAI HTTP 408/5xx answer other than the documented
  `server_is_overloaded` 503 — is now `submission_uncertain` (exit 5, `retryable: false`,
  `details.charge_possible: true`, `job_id: null`) instead of a retryable `request_timeout` or
  `provider_error`. A dropped connection is no longer labeled a timeout. Gemini HTTP error answers
  keep their codes without `charge_possible` (Google does not charge failed requests), and Ctrl-C
  during a paid image call reports `retryable: false`. No image error with
  `details.charge_possible: true` says `retryable: true`.
- Paid image responses are decoded item by item and judged by their bytes: one unusable item
  (a URL, bad base64, content that is not an image) no longer discards the valid images next to
  it; it gets the new `output_item_unusable` warning instead. A Gemini image whose `mimeType` is
  wrong or missing is kept under its real type with `output_format_mismatch` instead of failing
  the command. Only a response with no usable image is `provider_bad_response`.
- A valid paid image that cannot be written where it was requested (an I/O failure after
  preflight) is saved under `<state_dir>/unsaved/` instead, reported in `artifacts` with the new
  `output_saved_elsewhere` warning. Errors after a paid image call that could not save an image
  now carry `details.charge_possible: true`, a billing hint, `details.saved`, and
  `details.fallback_paths`.
- `image generate`/`image edit` check that the provider's API key is present before creating any
  output directory, so a run that fails with `missing_credentials` leaves nothing on disk.
- A Gemini image call that returns no image (and was not blocked) is now `provider_error`
  (retryable; running it again is billed again) instead of `remote_job_failed`: a synchronous call
  has no remote job.
- Rewriting a job record keeps what a newer Iris wrote at every level: unknown fields inside
  persisted error bodies, `usage`, and `cost_estimate`, and error codes this version does not
  know. Views show such a code as `internal_error` with the original in `details.recorded_code`.
- A finished Veo job is `succeeded` with its output URIs recorded even when Iris will not fetch
  them with the current base URL (for example behind a pass-through proxy). Download trust is
  checked on every `jobs wait`/`jobs download`; a refused URI fails that output with
  `download_failed` and a hint, and a later download with a corrected base URL succeeds.
  Previously such a job was recorded as `failed` and could not be recovered.
- Veo retention counts from submission: `remote_expires_at` is `submitted_at` plus the documented
  retention, and `retention_limited` says the outputs are kept "at least until about" that time.
  `completed_at` is documented as the time Iris observed completion.
- A 404 while checking a running Veo job expires it only when Google answers `NOT_FOUND` after the
  retention period; any other 404, or an earlier one, leaves the job `running` (a
  `status_refresh_failed` warning from `jobs status`; `permission_denied` with
  `provider_status: 404` and a hint from `jobs wait`). An expired job's error keeps the provider's
  status and code.
- Downloads always ask the file host instead of refusing on the local retention estimate. 410 is
  `artifact_expired`; 403/404 are `artifact_expired` only after the retention period and a
  retryable `download_failed` (output left re-downloadable) before it.
- `jobs download` on a record that still says `running` checks the job's status once first, so a
  job that finished since the last check downloads instead of reporting `job_not_ready`.
- SIGTERM and SIGHUP are handled like Ctrl-C: during a paid Veo submission the first one is
  deferred until the operation id is recorded, and every interrupted command prints exactly one
  `interrupted` envelope and exits 130 (previously SIGTERM killed the process silently, possibly
  losing the operation id).
- An interrupted Veo submission (the deferred first interrupt, or a second one) reports
  `retryable: false` and `details.charge_possible: true`.
- An interrupt that arrives while `video generate` writes the job record, or after that but before
  it starts sending the paid request, stops without sending it: the record is deleted and the
  result is `interrupted` with `retryable: true` and no job id.
- Partial download files (`.<name>.iris-part-*`) that a killed process left for a target are
  removed before the next download of that target; docs/jobs.md lists what SIGKILL can leave.
- Artifact downloads are capped at 4 GiB: a larger declared `Content-Length` or body is
  `download_failed` (not retryable) and nothing partial is kept.
- A local copy for `jobs download -o/-d` whose source file changes while being copied now falls
  back to fetching the output, as documented, instead of failing with `io_error`.
- Errors about a job (`wait_timeout`, `interrupted`, `output_exists`, `job_not_ready`, ...) now
  carry the job's `provider`, and `invalid_media` from a job download is `retryable: true` with a
  hint to download again.
- `video generate` runs the adapter's local checks (such as model-id syntax and the inline
  request size) before writing the job record, and a submission the adapter still refuses before
  sending no longer leaves a `failed` job. It also checks the credential before creating output
  directories, as do `jobs wait` and `jobs download` when they must fetch from the provider.
- `iris --help` no longer says exit code 2 means "nothing was sent": exit 2 is an invalid request
  that must be fixed before retrying, rejected either locally (`error.provider_status` is null,
  nothing was sent) or by the provider as given.
- The Gemini and Veo access notes (`iris models show`) and key-related error hints now state only
  what Google documents: no free tier (a paid-tier billing plan is needed), and standard API keys
  "will" be rejected from September 2026 with no exact day given, so Iris recommends an auth key
  instead of claiming the cutoff is already enforced. Both catalogs share one wording.
- `iris doctor --check-access` gives each check a unique id (`access.<provider>.<model>`) and
  reports a successful metadata read as "visible to this key" rather than "this account can use"
  it: billing tier, prepaid credit, and organization verification are not checked. `doctor`'s
  help, the README, and the JSON contract now say that it exits 0 whenever its checks ran, so
  callers must read `healthy`.
- An output location that cannot be used as given (a file in the way, no permission, a read-only
  file system, a directory that cannot be created) is now `invalid_argument` (exit 2) with
  `details.path` and a hint, found before anything is sent; it was `io_error` (exit 1) for
  everything except a file in the way.
- Planned output paths are lexically normalized (`-o ../x.png` plans `/parent/x.png`, not
  `/cwd/../x.png`; a video plan without `-o` still shows the output directory as configured), the
  JSON contract explains that default names in a `--dry-run` plan are indicative, and the
  `output_extension_adjusted` warning for `-o` without an extension names the files actually
  written when there are several outputs.
- A model resolved with `--capabilities-from` no longer gets a cost estimate computed from the
  template model's prices: `cost_estimate` is `null` (in plans, results, and job records) with a
  `cost_estimate_unavailable` warning saying the template's prices are not assumed.
- An unknown model id given with `--capabilities-from` is checked against the id syntax of the
  template's provider (declared in the catalog; for Gemini and Veo the rule the adapter applies
  before sending), so `--dry-run` rejects ids such as `a:b` that the real run would refuse, and a
  rejected Veo id no longer leaves a failed job record.
- Input rules that only the adapters enforced are declared in the catalog and checked before a
  `--dry-run` returns and before the credential check: OpenAI's mask rules (PNG with an alpha
  channel, under 4 MB, the dimensions of the first `--image`) and the Gemini and Veo caps on an
  inline request. A dry run now fails where the real run would, and a real run without a key
  reports the input problem (exit 2) instead of `missing_credentials`. `models show` reports them
  as `inputs.mask_requirements` and `inputs.max_request_bytes` (additive fields).
- An `-o` extension that contradicts the requested format names the flag actually given
  (`-O format=jpeg` or `--format jpeg`) instead of always saying `--format`.
- `models show` publishes a machine-readable `constraints` array: the rules that relate several
  options or inputs (e.g. 1080p/4k and reference images require an 8-second Veo duration,
  `compression` only with jpeg/webp), each with an id and the options and inputs involved. A
  request breaking one fails with `invalid_argument` and the id in `details.constraint`
  (additive fields).
- The published JSON Schema now matches the documented contract: every always-present key is
  `required` (nullable where it may be `null`), `ok` decides which of `result`/`error` is null,
  `result` is tied to `command`, and an error's `category` to its `code`. Outputs are unchanged;
  the schema only rejects documents Iris never prints.
- `default_for` in `models list`/`models show` reports the model a command uses without `--model`:
  the configured `providers.<provider>.image_model`/`video_model` when set, else the catalog
  default (it always showed the catalog default). `doctor --check-access` checks those same models,
  once each, and reports a configured default the catalog does not know as an `error` check (it
  always checked the catalog defaults).
- Human output: `jobs status` no longer prints a `completed:` time for a job that is still running
  or whose submission outcome is unknown, and the `provider_text_output` warning points at the
  "Model text" line instead of a JSON field.

### Removed

- The `--seed`, `--audio`, and `--no-audio` flags: no built-in model accepts them, so they could
  only fail (`-O name=value` reaches options of future models). Image commands now offer only image
  flags (`--count`, `--size`, `--aspect-ratio`, `--resolution`, `--quality`, `--format`) and
  `video generate` only video flags (`--count`, `--duration`, `--resolution`, `--aspect-ratio`,
  `--negative-prompt`); a flag of the other kind is a usage error. Help and shell completions
  follow.

### Known limitations

- A synchronous image call cannot be recovered if the connection is lost after the provider
  accepted it — there is no job to resume, unlike video (see
  [docs/jobs.md](docs/jobs.md#why-synchronous-calls-have-no-job-record)).
- Veo audio cannot be disabled (not an option the Gemini API offers), Veo outputs are retained by
  the provider for about 2 days, and all Veo models are labeled preview by Google.
- No remote job cancellation of any kind — `jobs delete` removes only the local record.
- No Windows support (builds, CI, or installer) in v1; Linux and macOS only.
