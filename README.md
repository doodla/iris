# Iris

Iris is a polished, open-source command-line tool that lets agents and humans **generate and edit
images and generate videos** through OpenAI and Google, from one consistent, agent-friendly
interface. It normalizes prompts, inputs, outputs, and errors across providers while staying honest
about where they differ, and it makes provider-native asynchronous jobs (Google Veo video) durable:
submit, disconnect, and resume from another process without losing the job.

```console
$ iris image generate "a watercolor fox in a misty forest" -o fox.png
Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
Saved /home/you/fox.png
Estimated cost: ~$0.0082 USD (estimate from reported usage ...)
```

**Provider usage is billed separately by OpenAI and Google, to your own API account.** A ChatGPT
Plus/Pro subscription, the Gemini app, or a Google Flow subscription does **not** grant API access;
you need an API key with billing enabled on the provider's developer platform. Iris only reports
cost *estimates* from what the provider tells it — see [Limitations](#limitations).

## Contents

- [What Iris does](#what-iris-does)
- [Installation](#installation)
- [Setup](#setup)
- [First success](#first-success)
- [More examples](#more-examples)
- [Agent usage](#agent-usage-json-mode)
- [Supported providers and models](#supported-providers-and-models)
- [Limitations](#limitations)
- [Documentation](#documentation)

## What Iris does

- **Image generation and editing** on OpenAI's Images API (GPT Image) and Google's Gemini API
  (Nano Banana), including editing/composing from one or more local reference images and,
  on OpenAI, a mask. Both are synchronous: the image is saved before the command returns.
- **Video generation** on Google Veo, a provider-native *asynchronous* job. Iris writes a local
  job record before submitting, so a job survives Ctrl-C, a wait timeout, or the process exiting:
  resume it later with `iris jobs status|wait|download <JOB_ID>`, even from a different process.
- **Reference-image inputs** where a provider supports them (edit images, Veo first/last frame
  and reference images).
- **Machine-readable everything**: a versioned `--json` envelope, a published JSON Schema
  (`iris schema`), a stable error-code taxonomy, and a documented exit-code mapping, so an agent
  never has to parse human prose.
- **Honest capability reporting**: `iris models show` lists exactly what a model accepts, its
  published prices, and documented access requirements, generated from Iris's built-in catalog —
  never invented.

Out of scope for v1: a GUI, a hosted backend or daemon, browser automation of consumer apps
(ChatGPT, the Gemini app, Google Flow), speculative future providers, automatic cross-provider
failover, and Windows.

## Installation

### From a published release (once one exists)

Iris ships a small POSIX-sh installer per release ([docs/install.md](docs/install.md) has the
full details: pinned installs, passing options through the pipe, checksum verification, upgrade,
and uninstall). **As of this writing no release has been published yet**, so the one-liner below
documents the intended, tested path (the installer itself is verified offline against local
fixtures — see [docs/install.md](docs/install.md)) rather than something you can run today:

```console
$ curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
```

It installs to `~/.local/bin` (no sudo), detects your OS/CPU (Linux x86_64, macOS x86_64/ARM64),
verifies the downloaded archive's SHA-256 against the release's `SHA256SUMS`, and prints the
installed version plus any PATH change you need.

### From a checkout (works today)

```console
$ git clone https://github.com/doodla/iris && cd iris
$ cargo install --locked --path .
```

This builds and installs the `iris` binary with `cargo` (Rust 1.89 or newer; `rustup` is the
easiest way to get a toolchain). Verified for this release: `cargo build --locked --release`
succeeds against the committed `Cargo.lock` and produces a working `iris --version`.

## Setup

Iris reads credentials **only** from environment variables — never from the config file, never
from a command-line flag:

```console
$ export OPENAI_API_KEY=sk-...
$ export GEMINI_API_KEY=...
```

You need only the key for the provider(s) you use. Check what Iris sees (values are never
printed, only presence):

```console
$ iris doctor
[ok]      config: no config file at ~/.config/iris/config.toml; built-in defaults apply
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
[ok]      state_dir: state directory ~/.local/state/iris is writable
[ok]      output_dir: output directory /home/you is writable
[ok]      jobs: 0 local job record(s) readable
Healthy.
```

`iris doctor --check-access` additionally makes one free, unbilled metadata call per provider to
confirm your account can actually reach the model, not just that a key is present.

Non-secret settings (default models, output directory, timeouts, a config file) follow
`flag > environment variable > config file > built-in default`; see
[docs/configuration.md](docs/configuration.md).

## First success

```console
$ iris image generate "a red bicycle leaning against a brick wall" -o bike.png
Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
Saved /home/you/bike.png
Estimated cost: ~$0.0082 USD (estimate from reported usage (gpt-image-2.5-sunburst): ...)
```

Validate a request locally, with no charge and no credentials required, before spending money:

```console
$ iris image generate "a red bicycle" --dry-run --json
{"command":"image.generate","result":{"dry_run":true,"provider":"openai","model":"gpt-image-2.5-sunburst",
 "operation":"image.generate","async_job":false,"options":{...},"inputs":[],
 "outputs":["/home/you/iris-01m3a2qat6apz4cwwhx37fkewe.png"],"credential_present":true,
 "cost_estimate":null}, "ok":true, "schema_version":1, "warnings":[...]}
```

## More examples

Generate with Gemini's "Nano Banana" instead of OpenAI:

```console
$ iris image generate "a watercolor fox" --provider gemini -o fox-gemini.png
```

Edit an image with a reference and a mask (OpenAI only supports masks):

```console
$ iris image edit -i room.png --mask window-mask.png "add a large window" -o room-window.png
$ iris image edit -i a.png -i b.png "combine these into one poster" --provider gemini
```

Generate a video and wait for it (the default: wait, then save):

```console
$ iris video generate "waves crashing at dusk, slow motion" --duration 4 -o waves.mp4
```

Submit and come back later — from any process, even after the terminal closed:

```console
$ iris video generate "a paper boat drifting on a pond" --duration 4 --detach
Job job_01m3a333vp4pc80nb37svfgq7x accepted by gemini
Submitted job job_01m3a333vp4pc80nb37svfgq7x: running (gemini veo-3.1-fast-generate-preview)
Next: iris jobs status job_01m3a333vp4pc80nb37svfgq7x
Next: iris jobs wait job_01m3a333vp4pc80nb37svfgq7x

$ iris jobs status job_01m3a333vp4pc80nb37svfgq7x
$ iris jobs wait job_01m3a333vp4pc80nb37svfgq7x           # waits, then downloads
$ iris jobs download job_01m3a333vp4pc80nb37svfgq7x        # safe to repeat; never regenerates
```

A wait limit or Ctrl-C only stops *waiting* — the remote job keeps running and stays resumable:

```console
$ iris jobs wait job_01m3a3g5wb5mkqg2whke2e4k3q --timeout 1ms --poll-interval 10s
error[wait_timeout]: job job_01m3a3g5wb5mkqg2whke2e4k3q did not finish within 1ms; it continues remotely
  hint: resume with `iris jobs wait job_01m3a3g5wb5mkqg2whke2e4k3q` (or check with `iris jobs status ...`)
$ echo $?
4
```

## Agent usage (JSON mode)

With `--json`, every command prints **exactly one JSON document on stdout**; all progress and
diagnostics go to stderr, and interactive prompts are never used. The envelope always has a
`schema_version`, `ok`, `command`, `result`/`error` (exactly one non-null), and `warnings`:

```console
$ iris --json version
{"command":"version","error":null,"ok":true,
 "result":{"git_commit":null,"name":"iris","schema_version":1,
           "target":"x86_64-unknown-linux-gnu","version":"0.1.0"},
 "schema_version":1,"warnings":[]}
```

Get the full schema (also published at `schema/iris-output.v1.schema.json` in this repo):

```console
$ iris schema > iris-output.v1.schema.json
```

Exit codes are a stable, documented contract — an agent can branch on them without parsing text
(full table in [docs/json-contract.md](docs/json-contract.md)):

| exit | meaning |
|---|---|
| 0 | success |
| 1 | runtime or provider failure |
| 2 | usage/validation/conflict error — nothing was sent |
| 3 | credentials, access, or quota problem |
| 4 | not finished yet — the job continues remotely (wait timeout, or outputs not ready) |
| 5 | outcome uncertain — a paid submission may or may not have gone through; Iris never resubmits automatically |
| 130 | interrupted (Ctrl-C) |

The video **recovery flow** an agent should implement: `--detach` to get a `job_id` immediately,
then poll with `iris jobs status <id> --json` (exit 4 while `status: running`), and finally
`iris jobs wait <id>` or `iris jobs download <id>` once it reports `succeeded`. Every step is
idempotent: repeating a download never re-generates the video (see
[docs/jobs.md](docs/jobs.md)).

## Supported providers and models

Generated from `iris models list` / `iris models show` against Iris's built-in catalog
(`CATALOG_AS_OF = 2026-09-24`) — run those commands yourself for the current, authoritative list:

| provider | operation | inputs | async? | recoverable? | remote cancel? |
|---|---|---|---|---|---|
| OpenAI (GPT Image 2.5 Sunburst/Flare, GPT Image 2) | image generate, image edit | up to 16 reference images + optional mask (edit) | no (synchronous) | no — a lost connection after sending is unrecoverable | n/a |
| Google Gemini (Nano Banana 2, Nano Banana 2 Lite, Nano Banana Pro) | image generate, image edit | up to 14 reference images (edit); no mask | no (synchronous) | no | n/a |
| Google Veo (3.1, 3.1 Fast, 3.1 Lite — all **preview**) | video generate | first frame, last frame, up to 3 reference images (Standard/Fast) | **yes** — provider-native job | **yes** — durable local job record, resumable from any process | **no** — the provider offers no cancel/delete for Veo operations |

```console
$ iris models list
MODEL                          PROVIDER  LIFECYCLE  OPERATIONS                  DEFAULT FOR                 ALIASES
gpt-image-2.5-sunburst         openai    ga         image.generate, image.edit  image.generate, image.edit  gpt-image-2.5-sunburst-2026-09-08
gpt-image-2.5-flare            openai    ga         image.generate, image.edit  -                           gpt-image-2.5-flare-2026-09-08
gpt-image-2                    openai    ga         image.generate, image.edit  -                           gpt-image-2-2026-04-21
gemini-3.1-flash-image         gemini    ga         image.generate, image.edit  image.generate, image.edit  nano-banana-2, nano-banana
gemini-3.1-flash-lite-image    gemini    ga         image.generate, image.edit  -                           nano-banana-2-lite
gemini-3-pro-image             gemini    ga         image.generate, image.edit  -                           nano-banana-pro
veo-3.1-fast-generate-preview  gemini    preview    video.generate              video.generate              veo-fast
veo-3.1-generate-preview       gemini    preview    video.generate              -                           veo
veo-3.1-lite-generate-preview  gemini    preview    video.generate              -                           veo-lite
```

`iris models show <model>` prints one model's full contract: every accepted option (with its
typed flag or `-O key=value` form), input/output limits, published prices, and documented access
requirements (e.g. "API Organization Verification may be required for GPT Image models", "No
free tier for image models: billing (Prepay) required"). `iris providers list` shows which
credential each provider reads and whether it's set.

## Limitations

- **Synchronous image calls cannot be recovered.** If the connection is lost after OpenAI or
  Gemini accepts an image request, Iris cannot resume or query it later — there is no job to
  recover (unlike video). A timeout after sending is reported as `request_timeout` with
  `details.charge_possible: true`; Iris never retries it automatically.
- **Veo models are all "preview"** per Google's own lifecycle labeling — behavior, limits, and
  availability may change upstream without notice.
- **Veo audio is always on** and cannot be disabled; it is not an option the Gemini API offers.
- **Veo outputs are retained by the provider for about 2 days** — download before then, or the
  download will fail with `artifact_expired`.
- **No remote cancellation of anything.** `iris jobs delete` removes only the *local* job record;
  it never cancels or deletes a remote OpenAI/Gemini job, and Veo operations offer no cancel/delete
  method to begin with.
- **No Windows support** (builds, CI, releases, or the installer) in v1; Linux and macOS only.
- Cost figures are **estimates** derived from published prices or the provider's own reported
  usage, explicitly labeled as estimates (`cost_estimate.estimated: true`) — never an invoice.

## Documentation

- [docs/architecture.md](docs/architecture.md) — module layout, sync vs. async traits, where
  invariants live
- [docs/providers.md](docs/providers.md) — how to add a provider (worked example: Seedance)
- [docs/json-contract.md](docs/json-contract.md) — the `--json` envelope, every result type, the
  error taxonomy, exit codes, and schema versioning
- [docs/jobs.md](docs/jobs.md) — job lifecycle, submission uncertainty, downloads, retention,
  local deletion vs. remote
- [docs/configuration.md](docs/configuration.md) — config file, environment variables, precedence
- [docs/install.md](docs/install.md) — the installer in detail: pinning, integrity, upgrade,
  uninstall
- [docs/live-testing.md](docs/live-testing.md) — opt-in, paid live verification: what it checks
  and how to run it by hand
- [CONTRIBUTING.md](CONTRIBUTING.md), [SECURITY.md](SECURITY.md), [CHANGELOG.md](CHANGELOG.md)

## License

[MIT](LICENSE).
