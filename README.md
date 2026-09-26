# Iris

Iris is a polished, open-source command-line tool that lets agents and humans **generate and edit
images and generate videos** through OpenAI and Google, from one consistent, agent-friendly
interface. It normalizes prompts, inputs, outputs, and errors across providers while staying honest
about where they differ, and it makes provider-native asynchronous jobs (Google Veo video) durable:
submit, disconnect, and resume from another process without losing the job.

```console
$ iris image generate -m gpt-image-2.5-sunburst "a watercolor fox in a misty forest" -o fox.png --size 1024x1024 --quality low
Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
Saved /home/you/fox.png
Estimated cost: ~$0.00595 USD (estimate from reported usage (gpt-image-2.5-sunburst): 14 text input tokens × $5.00/1M + 0 image input tokens × $8.00/1M + 196 output tokens × $30.00/1M; cached-input discounts not reported)
```

(Output from a local mock server standing in for the OpenAI API. A real run prints the same lines,
with the cost estimated from the usage OpenAI reports for that request, so it varies with the
prompt, size, and quality.)

**Provider usage is billed separately by OpenAI and Google, to your own API account.** A ChatGPT
Plus/Pro subscription, the Gemini app, or a Google Flow subscription does **not** grant API access;
you need an API key with billing enabled on the provider's developer platform. A Google AI
(Gemini app) subscription doesn't include Gemini API access either, but **Google Developer
Program Cloud credits can be applied to API usage**. Iris reports cost *estimates*, from published
prices before a call or from the provider's own reported usage after one — see
[Limitations](#limitations).

## Contents

- [What Iris does](#what-iris-does)
- [Installation](#installation)
- [Setup](#setup)
- [First success](#first-success)
- [More examples](#more-examples)
- [Prompts, models, and output files](#prompts-models-and-output-files)
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
  resume it later with `iris jobs status|wait|download <JOB_ID>`, even from a different process
  that uses the same state directory.
- **Reference-image inputs** where a provider supports them (edit images, Veo first/last frame
  and reference images).
- **Machine-readable everything**: a versioned `--json` envelope, a published JSON Schema
  (`iris schema`), a stable error-code taxonomy, and a documented exit-code mapping, so an agent
  never has to parse human prose.
- **Honest capability reporting**: `iris models list` says what each model is for and what the
  same output costs with each (one 1024x1024 image, one 8-second 720p video), and `iris models
  show` lists exactly what a model accepts, its published prices, and documented access
  requirements, generated from Iris's built-in catalog — never invented.

Out of scope for v1: a GUI, a hosted backend or daemon, browser automation of consumer apps
(ChatGPT, the Gemini app, Google Flow), speculative future providers, automatic cross-provider
failover, and Windows.

## Installation

### From a release

Iris ships a small POSIX-sh installer with each release ([docs/guides/install.md](docs/guides/install.md) has
the full details: pinned installs, passing options through the pipe, checksum verification,
upgrade, and uninstall):

```console
$ curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
```

It installs to `~/.local/bin` (no sudo), detects your OS/CPU (Linux x86_64, macOS x86_64/ARM64),
verifies the downloaded archive's SHA-256 against the release's `SHA256SUMS`, and prints the
installed version plus any PATH change you need.

### Building from source

Building from source needs Rust 1.89 or newer (`rustup` is the easiest way to get a toolchain):

```console
$ git clone https://github.com/doodla/iris && cd iris
$ cargo install --locked --path .
```

This builds `iris` with exactly the dependency versions in the committed `Cargo.lock` and installs
it into Cargo's `bin` directory (`~/.cargo/bin` by default). The platforms release archives are
built for, and their minimum OS/kernel versions, are listed in
[docs/guides/install.md](docs/guides/install.md#supported-platforms-and-runtime-requirements).

## Setup

Iris reads credentials **only** from environment variables — never from the config file, never
from a command-line flag:

```console
$ export OPENAI_API_KEY=sk-...
$ export GEMINI_API_KEY=...
```

You need only the key for the provider(s) you use. Check what Iris sees (values are never
printed, only presence). Real output on a first run on Linux with `HOME=/home/you`; paths are
always absolute, never `~`:

```console
$ iris doctor
[ok]      config: no config file at /home/you/.config/iris/config.toml (it is optional)
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
[ok]      state_dir: state directory /home/you/.local/state/iris does not exist yet; it will be created on first use
[ok]      output_dir: output directory /home/you is writable
[ok]      base_url.openai: openai API base URL is the default (https://api.openai.com/v1)
[ok]      base_url.gemini: gemini API base URL is the default (https://generativelanguage.googleapis.com/)
[ok]      jobs: 0 local job record(s) readable
Healthy.
```

`iris doctor --check-access` additionally makes one free, unbilled metadata call for every model
of each provider whose key is set (the models `iris models list` shows), to see whether each one
is visible to your key, not just that a key is present. That read does not check billing tier,
prepaid credit, or OpenAI organization verification, so a paid request can still be refused.

`iris doctor` exits 0 whenever its checks ran, even when it finds problems: scripts should read
`healthy` (`result.healthy` with `--json`) or look for `[error]` lines, not the exit code. A key
that is not set is a warning while another provider's key is set; with no key set at all, the
`credentials` check is an error, since every generation command would fail with
`missing_credentials`.

Gemini image and Veo models have **no free tier**: the key's project needs a paid-tier billing plan
(on Prepay, a positive credit balance). Use an auth API key: Google says the Gemini API will reject
standard keys from September 2026 (no exact day given). Run `iris models show <model>` for a
model's exact, current access notes rather than assuming these generalize.

Non-secret settings (output directory, timeouts, a config file) follow
`flag > environment variable > config file > default`; see
[docs/reference/configuration.md](docs/reference/configuration.md). Iris has no default model: every generation
command names one with `-m`, or uses the one the config file names (see
[Prompts, models, and output files](#prompts-models-and-output-files)).

## First success

A small, low-quality image (output from a local mock server standing in for the OpenAI API; with
a real key the cost line reflects the usage OpenAI reports for your request):

```console
$ iris image generate -m gpt-image-2.5-sunburst "a red bicycle leaning against a brick wall" -o bike.png --size 1024x1024 --quality low
Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
Saved /home/you/bike.png
Estimated cost: ~$0.00595 USD (estimate from reported usage (gpt-image-2.5-sunburst): 14 text input tokens × $5.00/1M + 0 image input tokens × $8.00/1M + 196 output tokens × $30.00/1M; cached-input discounts not reported)
```

Validate a request locally, with no charge and no credentials required, before spending money:
`--dry-run` makes every local check the real run makes (model, options, input files, output
paths) and prints the plan instead of sending anything. With an explicit size and quality the plan
carries a pre-call estimate (with `auto`, the default, the model chooses them, so it is `null` and
a `cost_estimate_unavailable` warning names the options to pass for one). `billing: "paid"` says
the real run is billed to your provider account at its published prices, and a name the real run
generates is shown as its pattern (`iris-<ulid>.png`). The other fields are described under
[`plan`](docs/reference/json-output.md#dry-run-plan):

```console
$ iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low --dry-run --json
{"command":"image.generate","error":null,"ok":true,"result":{"async_job":false,"billing":"paid","cost_estimate":{"amount":0.00588,"as_of":"2026-09-24","basis":"estimate: 1 image × 196 output tokens × $30.00/1M (gpt-image-2.5-sunburst, low, 1024x1024); OpenAI calculator formula (indicative for GPT Image 2.5); prompt and input-image tokens not included","currency":"USD","estimated":true,"source_url":"https://developers.openai.com/api/docs/pricing"},"credential_present":false,"detach":false,"dry_run":true,"inputs":[],"label":null,"max_cost":null,"model":"gpt-image-2.5-sunburst","model_source":"flag","operation":"image.generate","options":{"background":"auto","compression":100,"count":1,"format":"png","moderation":"auto","quality":"low","size":"1024x1024"},"outputs":["/home/you/iris-<ulid>.png"],"prompt_fingerprint":{"chars":13,"sha256":"1191409152a26c2e3a7b6e7e0fc0f0dbc04a0c2aef096f8e20239abd84a7c3c6"},"provider":"openai","wait":null},"schema_version":1,"warnings":[]}
```

To cap what a command may spend, add `--max-cost <USD>`: a request whose pre-call estimate is
above the cap, or that has no estimate, fails with `cost_limit_exceeded` (exit 2) before anything
is sent, in a dry run too. The cap compares the estimate, which can leave out prompt, input-image,
and thinking tokens (its basis says what it leaves out), so the bill can be higher than the cap,
by what the basis leaves out:

```console
$ iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low --max-cost 0.005
error[cost_limit_exceeded]: the request is estimated at $0.00588 USD, above --max-cost $0.005
  hint: choose cheaper options or a cheaper model (`iris models list` compares the models' costs on the same output), or raise --max-cost
```

## More examples

Generate with Google's Nano Banana 2 instead of OpenAI's GPT Image (the provider is the model's):

```console
$ iris image generate -m nano-banana-2 "a watercolor fox" -o fox-gemini.jpg
```

Edit an image with a reference and a mask (only the OpenAI models take a mask):

```console
$ iris image edit -m gpt-image-2.5-sunburst -i room.png --mask window-mask.png "add a large window" -o room-window.png --size 1024x1024 --quality low
$ iris image edit -m nano-banana-2 -i a.png -i b.png "combine these into one poster"
```

Generate a video and wait for it (the default: wait, then save):

```console
$ iris video generate -m veo-lite "waves crashing at dusk, slow motion" --duration 4 -o waves.mp4
```

Submit and come back later — from any process that uses the same state directory (`iris config
path`), even after the terminal closed (output from a local mock server standing in for the Gemini
API):

```console
$ iris video generate -m veo-lite "a paper boat drifting on a pond" --duration 4 --detach
Submitting job job_01m3a59a5syx5aex0a0qv8qc3x to gemini (veo-3.1-lite-generate-preview); this is a paid request
Job job_01m3a59a5syx5aex0a0qv8qc3x accepted by gemini
warning[preview_model]: veo-3.1-lite-generate-preview is a preview model; its behavior, limits, and availability may change
Submitted job job_01m3a59a5syx5aex0a0qv8qc3x: running (gemini veo-3.1-lite-generate-preview)
Next: iris jobs status job_01m3a59a5syx5aex0a0qv8qc3x
Next: iris jobs wait job_01m3a59a5syx5aex0a0qv8qc3x

$ iris jobs status job_01m3a59a5syx5aex0a0qv8qc3x
$ iris jobs wait job_01m3a59a5syx5aex0a0qv8qc3x           # waits, then downloads
$ iris jobs download job_01m3a59a5syx5aex0a0qv8qc3x        # safe to repeat; never regenerates
```

A wait limit or Ctrl-C only stops *waiting* — the remote job keeps running and stays resumable
(transcript from a local mock server standing in for the Gemini API):

```console
$ iris jobs wait job_01m3asz412h3hncrs5hkyqh05r --timeout 1ms --poll-interval 10s
error[wait_timeout]: job job_01m3asz412h3hncrs5hkyqh05r did not finish within 1ms; it continues remotely
  hint: resume with `iris jobs wait job_01m3asz412h3hncrs5hkyqh05r` (or check with `iris jobs status job_01m3asz412h3hncrs5hkyqh05r`)
  job: job_01m3asz412h3hncrs5hkyqh05r (status running)
  remote operation: models/veo-3.1-fast-generate-preview/operations/op_mockjob002
$ echo $?
4
```

## Prompts, models, and output files

### Prompt

A prompt comes from exactly one of three mutually exclusive sources — Iris rejects two at once
with a clear `usage_error`: inline text (`iris image generate -m <MODEL> "a fox" ...`), a UTF-8
file (`-f/--prompt-file PATH`, trailing whitespace trimmed), or standard input (`--prompt-stdin`,
which must not be a terminal).

### Model

Every generation command names its model. `-m`/`--model` takes a model id or alias (`iris models
list` shows every one Iris knows); without it, the command uses the model the config file names for
its kind (`[image] model`, `[video] model`). Iris never picks one for you: with neither, the
command fails with `model_required` (exit 2) before anything is sent, and the error lists the
models that support the command, each with its summary and `standard_cost`
(`error.details.candidates`; see [docs/reference/configuration.md](docs/reference/configuration.md#config-file)).
A model Iris does not know is `unknown_model` with the same list. A near miss, such as
`gpt-image-2.5`, `veo-3.1-lite`, or `Nano-Banana-2`, also asks "did you mean …?" naming the models
it nearly names (`details.suggestions`); a name Iris declines, such as `dall-e-3`, `veo-3`, or the
bare `nano-banana` (Google's Nano Banana is `gemini-2.5-flash-image`), says why and what to use
instead (also as `details.suggestions`). An option or input the model does not take is
`unsupported_option` naming the models that take it (`details.supported_by`), and a value outside
an option's listed values is `invalid_argument` listing them (`details.allowed`). The provider is
the model's; results say which of the two named it (`model_source`: `flag` or `config`). For a
model Iris doesn't know yet, `--capabilities-from <KNOWN_MODEL>` declares that the unknown id has a
known model's capabilities (sent to the provider as given, validated as that known model, and
flagged with an `unverified_model_capabilities` warning, which also asks "did you mean -m …?" when
the id nearly names a catalog model) rather than refusing outright. Iris makes no cost estimate
for such a model, since the known model's prices may not apply, and the id must use characters the
provider's API accepts in a model id (for Gemini and Veo: letters, digits, `.`, `_`, and `-`),
checked before anything is sent. Any option a model accepts but has no typed flag for is reachable
through `-O key=value` (repeatable); `iris models show <model>` lists every option, typed or
`-O`-only. A mistyped flag is a `usage_error` whose hint asks "did you mean …?" too
(`details.suggestions`), and such an option given as a flag (`--background`) is pointed at its `-O`
form.

### Output files

Without `-o`/`-d`, Iris saves to the output directory (`IRIS_OUTPUT_DIR`, config `output_dir`, or
else the current directory) under a predictable name: images as `iris-<ulid>.<ext>` (the
extension follows the actual returned media type), and video job outputs
as `<job_id>.mp4`. `-o/--output PATH` names an exact file (with several outputs: `<stem>-<i>.<ext>`
with `i` from 1, so `-n 3 -o p.png` saves `p-1.png` to `p-3.png`, while `artifacts[].index` counts
from 0); `-d/--out-dir DIR` picks a directory and keeps the default naming. A video job records
where it was asked to save (`result.job.output_plan`): a later `iris jobs wait` or `iris jobs
download` without `-o` or `-d` saves there. Media is never written to standard output: `-o -`, a
name of a standard stream such as `/dev/stdout` (even when standard output is redirected to a
file), and a device such as `/dev/null` are refused, and the saved paths are what Iris prints
(`result.artifacts[].path` with `--json`). An existing file at the target path is refused as
`output_exists` unless `--overwrite` is passed — Iris never silently replaces a file. A paid image
is never thrown away either: if it cannot be written where you asked after the request was made
(say the disk filled up or the directory was deleted), Iris saves it under `<state_dir>/unsaved/`
instead and reports the path (`iris config path` shows the state directory). Returned content that
is not a valid image is kept there too, as received (`.bin`).

The extension of `-o` also picks the image type for models that take a format (OpenAI's `-o
fox.jpg` requests JPEG). Gemini image models take none: the provider chooses the type (live runs
returned JPEG), so the extension only names the file. The plan warns about it up front
(`output_extension_may_change`, in dry runs too), and if another type comes back Iris keeps the
stem and saves under the right extension (`-o fox.png` becomes `fox.jpg`, with
`output_extension_adjusted`). Without `--overwrite`, `output_exists` covers every name the image
may be saved under: `-o fox.png` is refused, naming the file, while `fox.jpg` exists (and `-o
fox.jpg` while `fox.png` does), so a rerun never pays for an image it would save next to one
already there. `--overwrite` covers only the path you named: an existing file under the adjusted
name is never replaced (the image goes to `<stem>.<n>.<ext>`, `output_renamed`).

## Agent usage (JSON mode)

With `--json`, every command prints **exactly one JSON document on stdout**; all progress and
diagnostics go to stderr, and interactive prompts are never used. The envelope always has a
`schema_version`, `ok`, `command`, `result`/`error` (exactly one non-null), and `warnings`:

```console
$ iris --json version
{"command":"version","error":null,"ok":true,"result":{"git_commit":null,"name":"iris","schema_version":1,"target":"x86_64-unknown-linux-gnu","version":"0.1.0"},"schema_version":1,"warnings":[]}
```

(A binary built from a checkout reports `git_commit: null`; see
[docs/guides/install.md](docs/guides/install.md#verifying-what-you-installed).)

Get the full schema (also published at `schema/iris-output.v1.schema.json` in this repo):

```console
$ iris schema > iris-output.v1.schema.json
```

Exit codes are a stable, documented contract — an agent can branch on them without parsing text
(the exit code of every error code is in
[docs/reference/json-output.md](docs/reference/errors.md#error-codes)):

| exit | meaning |
|---|---|
| 0 | success |
| 1 | runtime or provider failure |
| 2 | the request is invalid or conflicts as given; fix it before running again |
| 3 | credentials, access, or quota problem |
| 4 | not finished yet — the job continues remotely (wait timeout, or outputs not ready) |
| 5 | outcome uncertain — a paid submission may or may not have gone through; Iris never resubmits automatically |
| 130 | interrupted (Ctrl-C/SIGINT, SIGTERM, or SIGHUP) |

Exit 2 covers both local validation (nothing was sent) and a definite provider rejection of a
malformed request (e.g. an OpenAI HTTP 400) — the request itself was bad either way. Tell them
apart from `error.provider_status`: `null` means nothing was sent.

The video **recovery flow** an agent should implement, from any process that uses the same state
directory:

1. **Submit** with `--detach` and a `--label` that names the intended video (`iris video generate
   -m veo-lite "a paper boat" --label paper-boat-1 --detach --json`); `result.job.job_id` is the
   job. No two local jobs share a label, so the same command run again after a crash is refused
   with `label_in_use` (exit 2), naming the job, before anything is sent, and `iris jobs list
   --label paper-boat-1 --json` finds the job.
2. **Follow** it with `iris jobs status <id> --json`, which exits **0** and reports the job's state
   in `result.job.status` (`submitting`, `running`, `succeeded`, `failed`, `expired`, or
   `submission_unknown`): branch on that field, not on the exit code. Or loop on `iris jobs wait
   <id> --timeout <D> --json`: it exits **4** (`wait_timeout`) while the job is still running and
   **0** once the job has succeeded and its outputs are saved (with `--no-download`, once it has
   succeeded). Otherwise it exits with the code of the error it reports, as in the table above:
   usually **1** for a remote failure, an expired job, or a failed download (`remote_job_failed`,
   `content_blocked`, `artifact_expired`, `download_failed`), and **5** when the submission's
   outcome is unknown (`submission_uncertain`).
3. **Download** with `iris jobs download <id> --json` once the job reports `succeeded` (`jobs wait`
   without `--no-download` has already done it). A download checks a `running` record's status
   once first, so a stale local record is not a problem; if the job is still running it exits
   **4** (`job_not_ready`) rather than waiting or resubmitting.

Every step is idempotent: repeating a download never re-generates the video (see
[docs/concepts/video-jobs.md](docs/concepts/video-jobs.md)).

## Supported providers and models

Generated from `iris models list` / `iris models show` against Iris's built-in catalog
(`CATALOG_AS_OF = 2026-09-24`) — run those commands yourself for the current, authoritative list:

| provider | operation | inputs | async? | recoverable? | remote cancel? |
|---|---|---|---|---|---|
| OpenAI (GPT Image 2.5 Sunburst/Flare, GPT Image 2) | image generate, image edit | up to 16 reference images + optional mask (edit) | no (synchronous) | no — a lost connection after sending is unrecoverable | n/a |
| Google Gemini (Nano Banana 2, Nano Banana 2 Lite, Nano Banana Pro) | image generate, image edit | up to 14 reference images (edit); no mask | no (synchronous) | no | n/a |
| Google Veo (3.1, 3.1 Fast, 3.1 Lite — all **preview**) | video generate | first frame, last frame, up to 3 reference images (Standard/Fast) | **yes** — provider-native job | **yes** — durable local job record, resumable from any process that uses the same state directory | **no** — the provider offers no cancel/delete for Veo operations |

```console
$ iris models list
MODEL                          PROVIDER  LIFECYCLE  OPERATIONS                  ALIASES
gpt-image-2.5-sunburst         openai    ga         image.generate, image.edit  gpt-image-2.5-sunburst-2026-09-08
  OpenAI's most capable image model, for workflows where editing precision matters most
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.01317 (medium), ~$0.05268 (high),
  ~$0.09366 (xhigh), ~$0.21072 (max)
gpt-image-2.5-flare            openai    ga         image.generate, image.edit  gpt-image-2.5-flare-2026-09-08
  OpenAI's fastest image model, for fast, high-quality everyday generation, at the same token rates
  as Sunburst
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.01317 (medium), ~$0.05268 (high),
  ~$0.09366 (xhigh), ~$0.21072 (max)
gpt-image-2                    openai    ga         image.generate, image.edit  gpt-image-2-2026-04-21
  The earlier GPT Image model; OpenAI says to use a 2.5 model for new integrations. Quality up to
  high, and at medium and high about 4x the 2.5 models' output tokens (OpenAI's calculator,
  indicative for 2.5)
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.05268 (medium), ~$0.21072 (high)
gemini-3.1-flash-image         gemini    ga         image.generate, image.edit  nano-banana-2
  Google's most versatile image model, balancing speed with 4K output, world knowledge and text
  rendering; good with multiple reference images
  paid; one 1024x1024 image: ~$0.067
gemini-3.1-flash-lite-image    gemini    ga         image.generate, image.edit  nano-banana-2-lite
  Google's fastest and cheapest image model: 1K only, and not optimized for multiple reference
  images or multi-turn editing
  paid; one 1024x1024 image: ~$0.0336
gemini-3-pro-image             gemini    ga         image.generate, image.edit  nano-banana-pro
  Google's premium image model for the most complex visual tasks and professional assets; the
  highest per-image price at each resolution
  paid; one 1024x1024 image: ~$0.134
veo-3.1-fast-generate-preview  gemini    preview    video.generate              veo-fast
  Veo 3.1 optimized for speed: every Veo option Iris offers, 4k and reference images included, at a
  lower per-second price than Veo 3.1 Standard
  paid; one 8-second 720p video: ~$0.80
veo-3.1-generate-preview       gemini    preview    video.generate              veo
  Veo 3.1 Standard, which Google calls best for professional-grade 4K output and complex camera
  movements; every Veo option Iris offers, at the highest per-second price
  paid; one 8-second 720p video: ~$3.20
veo-3.1-lite-generate-preview  gemini    preview    video.generate              veo-lite
  The lowest-priced Veo model: up to 1080p, with no 4k, no reference images, and no negative prompt
  paid; one 8-second 720p video: ~$0.40
```

Each model's row is followed by its summary (what it is for and its trade-off, from the
provider's documentation), its billing (`paid`: requests are billed to your provider account at
its published prices), and what a standard output costs with it: one 1024x1024 image for the
image models, one 8-second 720p video for the video models. A comparison needs the same output
from every model, so each model is priced on it with the options that give it, by the same
estimator as a real request's (`standard_cost` with `--json`, which lists those options). The
OpenAI models are priced at every quality with an estimate, since at one size the quality sets
their price. Their price at other sizes follows OpenAI's calculator formula, by which a larger
non-square size can cost less than a smaller square one (the `--size` description in `iris models
show` says why).

Choose a model by what it is for, what it accepts, and what it costs: `iris models list` shows
the summaries and what the same output costs with each model, `iris models show <model>` every
option and published price, and `--dry-run` the estimate of the exact request before you pay for
it. Then pass the model's id or alias with `-m`, or name it once in the config file so commands
without `-m` use it:

```toml
[image]
model = "gpt-image-2.5-sunburst"

[video]
model = "veo-lite"  # alias of veo-3.1-lite-generate-preview; config show shows the id
```

`iris models show <model>` prints one model's full contract: every accepted option (with its
typed flag or `-O key=value` form), input/output limits, published prices, and documented access
requirements (e.g. "API Organization Verification may be required for GPT Image models", "No
free tier: the key's project needs a paid-tier billing plan"). `iris providers list` shows which
credential each provider reads and whether it's set.

## Limitations

- **Synchronous image calls cannot be recovered.** If the connection is lost after OpenAI or
  Gemini accepts an image request, Iris cannot resume or query it later — there is no job to
  recover (unlike video). A timeout or dropped connection after sending (and an OpenAI 408/5xx
  answer) is reported as `submission_uncertain` (exit 5, `retryable: false`) with
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

- [docs/contributing/architecture.md](docs/contributing/architecture.md) — module layout, sync vs. async traits, where
  invariants live
- [docs/contributing/decisions.md](docs/contributing/decisions.md) — why Iris uses the endpoints, models, retry rules, and
  tools it does, with the official sources and the date they were checked
- [docs/contributing/adding-a-provider.md](docs/contributing/adding-a-provider.md) — how to add a provider (worked example: Seedance)
- [docs/reference/json-output.md](docs/reference/json-output.md) — the `--json` envelope, every result type, the
  error taxonomy, exit codes, and schema versioning
- [docs/concepts/video-jobs.md](docs/concepts/video-jobs.md) — job lifecycle, submission uncertainty, downloads, retention,
  local deletion vs. remote
- [docs/reference/configuration.md](docs/reference/configuration.md) — config file, environment variables, precedence
- [docs/guides/install.md](docs/guides/install.md) — the installer in detail: pinning, integrity, upgrade,
  uninstall
- [docs/contributing/live-testing.md](docs/contributing/live-testing.md) — opt-in, paid live verification: what it checks
  and how to run it by hand
- [CONTRIBUTING.md](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md),
  [SECURITY.md](https://github.com/doodla/iris/blob/main/SECURITY.md), [CHANGELOG.md](CHANGELOG.md)

## License

[MIT](LICENSE).
