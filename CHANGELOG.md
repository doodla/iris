# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Iris intends to follow
[Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

## [0.1.0] - 2026-09-25

Iris's first release: a Rust CLI that generates and edits images and generates videos through
OpenAI and Google, with a durable job model for provider-native asynchronous work and a
machine-readable contract for agents.

### Added

**Generation**

- **Image generation and editing** on OpenAI's Images API (GPT Image 2.5 Sunburst, GPT Image 2.5
  Flare, GPT Image 2) and Google's Gemini API (Nano Banana 2, Nano Banana 2 Lite, Nano Banana
  Pro), including editing from local reference images and, on OpenAI, a mask. Both are
  synchronous calls.
- **Video generation** on Google Veo (3.1, 3.1 Fast, 3.1 Lite, all preview) as a provider-native
  asynchronous job, with first-frame, last-frame and reference-image inputs where the model
  documents them. `--detach` submits and returns; `jobs status`/`wait`/`download` recover the job
  from any later process that uses the same state directory.
- **Explicit models.** Every generation command names its model: `-m`/`--model`, or the model
  the config file names (`[image] model`, `[video] model`); Iris never chooses one. Without
  either, the command fails with `model_required` (exit 2) before anything is sent, listing the
  models that support the operation with their summaries and what the same output costs with
  each. The provider is the model's, and results report `model_source` (`flag` or `config`), as
  do errors about the model's options, inputs, and limits (`details.model_source`, and `(config
  image.model)` after the model's name when the config file chose it).
- **Local validation before anything is sent.** Options, inputs and cross-option rules are declared
  per model in the catalog and checked identically by `--dry-run` and a real run (mask rules,
  inline request caps, model-id syntax for `--capabilities-from` models included). The API key is
  checked before any output directory is created. An unusable output location (a file in the way,
  a directory that cannot be created or written) is `invalid_argument` (exit 2) with
  `details.path`, in a dry run too, which creates no directory and leaves nothing behind (it
  creates and removes one check file); `-o -` and the names of standard streams are refused, since
  Iris writes files and prints their paths. When the provider chooses the image type, `-o` gets an
  `output_extension_may_change` warning, and without `--overwrite` a file under any extension the
  provider may return is `output_exists` too, so a rerun does not pay again. A dry-run plan shows a
  name the real run generates as its pattern (`iris-<ulid>.png`, `<job_id>.mp4`), says whether the
  real run would detach (`detach`), and, for a video run that waits, the wait limit and poll
  interval it would use and where each came from (`wait`).
- **Cost estimates**, always labeled as estimates: before the call where supportable, from the
  reported usage afterwards. Without one, `cost_estimate_unavailable` says why and which options
  to pass for one (on OpenAI, `--quality` and `--size`, naming only those that are `auto`).
  Models resolved with `--capabilities-from` get none, since the template model's prices are not
  assumed. Every model reports its `billing` (`paid`) in `models list`, `models show`, and
  dry-run plans.
- **A spending cap per command**: `--max-cost <USD>` on `image generate`, `image edit` and `video
  generate` refuses a request whose pre-call estimate is above the cap, or that has no estimate,
  with `cost_limit_exceeded` (exit 2) before anything is sent, in a dry run too. The cap compares
  the estimate, which can leave out prompt, input-image and thinking tokens, so the bill can be
  higher, by what the estimate leaves out; the plan reports the cap it applied (`max_cost`).

**Paid requests are never retried or discarded behind your back**

- A paid request is retried automatically only when it provably did not reach the provider or
  was rejected before processing (rate limit, documented overload). Any ambiguous outcome (a
  timeout or dropped connection after sending, an OpenAI 408/5xx, an interrupt during the call)
  is `submission_uncertain` (exit 5) or `interrupted` (exit 130) with `retryable: false` and
  `details.charge_possible: true`, and is never resubmitted.
- Paid image responses are decoded item by item: one unusable item never discards the valid
  images next to it (`output_item_unusable`), a mislabeled image is kept under its real type
  (`output_format_mismatch`), and an image that cannot be written where requested, or content
  that is not a valid image, is kept in `<state_dir>/unsaved/` (`output_saved_elsewhere`, or
  `details.fallback_paths` on errors).
- An answer that completed without a usable image reports what it cost: `details.usage` and,
  when prices are known, `details.cost_estimate`; a Gemini answer is also marked
  `details.charged: true`, an OpenAI one `details.charge_possible: true`.

**Durable jobs**

- Job records are written atomically under a lock, versioned, and preserve fields and error codes
  written by newer versions of Iris.
- Generation and download are separate outcomes: a finished job is `succeeded` with its outputs
  recorded even when a download is refused or fails, and download trust (credentials only to the
  configured API origin, https-only redirects) is decided at download time.
- Retention is counted from submission (`remote_expires_at` is the earliest time the provider may
  stop serving outputs). A job is `expired` only when Google answers `NOT_FOUND` after that time;
  a download 403/404 before it is a retryable `download_failed`.
- Downloads are safe to repeat and never re-trigger generation: already-downloaded outputs are
  reused, `--overwrite` fetches again and replaces the file atomically, and a saved file that does
  not validate as media is fetched again. Temporary files are locked while written, downloads are
  capped at 4 GiB, and videos are validated structurally (media data present, chunk offsets inside
  the file, overflow-safe box parsing).
- Every job view carries the prompt's fingerprint (`prompt_fingerprint`: its SHA-256 and length,
  never its text), so a job whose submitting process was killed can be found with `jobs list
  --status submitting` by model, creation time and prompt; a dry-run plan shows the same
  fingerprint before the paid call.
- A job records where `video generate` was asked to save (its `-o`, or the output directory in
  effect, and `--overwrite`), shown as `output_plan` in every job view: `jobs wait` and `jobs
  download` save there unless given their own `-o` or `-d`.
- `video generate --label <LABEL>` records a label no other local job has, checked and written under
  the job store's lock: a second submission with the label, in any status, is refused with
  `label_in_use` (exit 2) before anything is sent, a dry run too, with a hint for that job's status,
  so a script that labels each intended video can rerun after a crash without paying twice. While a
  local record cannot be read, a labeled submission is refused (`state_invalid`). `jobs list
  --label` finds the job, and the dry-run plan shows the label.
- `jobs download` checks a job that still reads `running` once before deciding, and reports a
  failed check (missing key, 401, 403, quota) as that error, not as "not ready".
- `jobs delete` is all or nothing, deletes only local records, and without `--force` refuses a
  job that is active or whose outputs were not downloaded while the provider still keeps them.
- The recorded error of an ended job is shown with `retryable: false`: retrying means a new,
  billed request. `next_steps` and hints repeat `--config` when one was given.
- `jobs status --help`, `jobs wait --help`, and `jobs download --help` state their exit codes:
  `jobs status` exits 0 for a job in any state (branch on `result.job.status`); `jobs wait` 4
  while the job runs on and `jobs download` 4 (`job_not_ready`) while it is still running, and
  each of them the recorded error's code once the job ended without success. The `--timeout` and
  `--poll-interval` help names each default.
- Ctrl-C (SIGINT), SIGTERM and SIGHUP print one `interrupted` envelope and exit 130; during a Veo
  submission the first one is deferred until the operation id is recorded. Neither an interrupt
  nor a `--timeout` ever marks a remote job failed.

**Agent-facing contract**

- `--json` prints exactly one envelope per command. The published JSON Schema (`iris schema`,
  with `$id` and `schema_version` 1) requires every always-present key, ties `result` to
  `command` and each known error code to its category, and keeps error codes, commands, warning
  codes, provider ids and billing values open to later additions.
- A stable error taxonomy with documented exit codes (0, 1, 2, 3, 4, 5, 130) and a registry of
  warning codes; tests check the documented tables and every help example against the code.
- Errors name the way forward: `unknown_model` lists the models the command can use
  (`details.candidates`), a near miss such as `gpt-image-2.5` or `Nano-Banana-2` asks "did you mean
  …?" with the models it nearly names (`details.suggestions`), or says which operation they are for
  when they belong to another command, and a name Iris declines (a deprecated, retired, limited, or
  served-elsewhere model such as `dall-e-3`, `gpt-image-1`, `imagen-4` or `veo-3`, or the bare
  `nano-banana`) says why, with the provider's date, and what to use instead (also as
  `details.suggestions`). A model of another operation is `unsupported_operation` with the models
  that do support it (`details.candidates`). The same near miss given with `--capabilities-from` is
  sent as typed, and its `unverified_model_capabilities` warning asks "did you mean -m …?". An
  option or input the model does not take names the models that do (`details.supported_by`), and a
  value outside an option's listed values lists them (`details.allowed`), naming the listed value it
  matches but for case (`--resolution 4k`: "did you mean 4K?"). A mistyped flag, subcommand, or
  value close to a real one is a `usage_error` whose hint asks "did you mean …?"
  (`details.suggestions`), and a model option typed as a flag (`--background`) points at its `-O`
  form.
- `iris models list`/`show`: a one-line summary of what each model is for, what the same output
  costs with it (`standard_cost`: one 1024x1024 image, at each quality for the OpenAI models, or
  one 8-second 720p video, estimated by the model's own estimator), declared operations, options
  (typed defaults, `max_chars`), machine-readable `constraints`, input requirements, output types,
  published prices with their source and date, and documented access requirements.
  `--check-access` is a free check of whether a model is visible to your key.
- `iris config show`/`path`, `iris doctor` (exits 0 whenever its checks ran; read `healthy`,
  which is false when no provider key is set at all; `--check-access` checks every model of each
  provider whose key is set),
  `iris completions` (bash, zsh, fish, elvish) and `iris version` (`git_commit` is set by release
  and CI builds through `IRIS_GIT_COMMIT`).

**Configuration and security**

- Credentials come only from `OPENAI_API_KEY` and `GEMINI_API_KEY` and are never printed, logged
  or persisted. Signed URLs and secrets are redacted in all output; prompts are not logged, and job
  records keep a prompt's hash and length, not its text, unless `jobs.store_prompts` is set.
- Base URLs must use https (plain http only for loopback hosts, which never go through a proxy),
  and every command that sends a key to a non-default base URL warns `non_default_base_url`.
- API answers are read only up to fixed limits, request time limits allow for uploading large
  inputs, and `[providers.gemini] submit_timeout` sets the Veo submission timeout.
- Path environment variables (`IRIS_CONFIG`, `IRIS_STATE_DIR`, `IRIS_OUTPUT_DIR`) must be absolute
  or start with `~/`, like their config-file keys.

**Distribution**

- `install.sh`: one-command install for Linux x86_64 and macOS x86_64/ARM64, with checksum
  verification, pinned versions, no `sudo`, and curl, GNU wget or BusyBox wget.
- Reproducible release archives holding the binary, `LICENSE`, `THIRD-PARTY-LICENSES`,
  `README.md`, `CHANGELOG.md` and `docs/`. Releases are built with a pinned Rust toolchain, smoke
  tested before publishing, and their notes come from this file.
- CI: formatting, Clippy, offline tests on Linux and macOS, an MSRV check, weekly dependency
  license and advisory scanning, the Linux release path (musl build with the pinned release
  toolchain, packaging, installer) on every change, installer tests, and an offline run of the live-verification script.
- Documentation under `docs/`, including a decisions log with official sources and the date each
  was checked, and `AGENTS.md`/`CLAUDE.md` instructions for coding agents.

### Known limitations

- A synchronous image call cannot be recovered if the connection is lost after the provider
  accepted it — there is no job to resume, unlike video (see
  [docs/concepts/paid-requests.md](https://github.com/doodla/iris/blob/main/docs/concepts/paid-requests.md#when-the-outcome-is-uncertain)).
- Veo audio cannot be disabled (not an option the Gemini API offers), Veo outputs are retained by
  the provider for about 2 days, and all Veo models are labeled preview by Google.
- Veo 3.1 Lite takes no negative prompt, and no Veo model takes one together with reference
  images: the Gemini API refuses both, so Iris rejects them before sending.
- No remote job cancellation of any kind — `jobs delete` removes only the local record.
- No Windows support (builds, CI, or installer) in v1; Linux and macOS only.
- macOS archives are not signed or notarized; see [docs/guides/install.md](https://github.com/doodla/iris/blob/main/docs/guides/install.md) for installing
  one downloaded in a browser.

[Unreleased]: https://github.com/doodla/iris/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doodla/iris/releases/tag/v0.1.0
