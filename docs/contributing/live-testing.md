# Live testing

Live verification checks Iris against the real OpenAI and Gemini APIs, through the built `iris`
binary, with your API keys. This page describes what it checks, what it costs, how to run it, and
the rules that keep it from spending more than you intend.

> [!WARNING]
> Live verification sends real, billed requests. CI never runs it, `cargo test` never runs it, and
> nothing runs it automatically. The default test suite is offline and free; see the
> [contributing guide](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md).

## What it checks

The smallest set of real requests that exercises every integration:

1. OpenAI image generation.
2. Gemini image generation.
3. An edit with a reference image on each image provider, reusing the images from steps 1 and 2.
4. One Veo video submission, without waiting (`--detach`).
5. Resuming that job from a separate `iris` process (`jobs status`).
6. Waiting for the job and downloading it without submitting it again (`jobs wait`).
7. Repeating the download safely (`jobs download`, then to another directory).
8. JSON mode throughout, plus independent checks that the images decode and the MP4 is valid.

Steps 4 to 7 share one Veo job, because video is the expensive part: a run submits at most one.

## Before you begin

- **A built binary.** Run `cargo build --release`. The script uses `target/release/iris` of this
  checkout. To use another binary, for example when `CARGO_TARGET_DIR` points elsewhere, pass
  `--bin PATH` or set `IRIS_BIN`. The script never uses an `iris` from your `PATH`, because it might
  be a stale build.
- **Tools.** `jq`, and the standard tools `od`, `awk`, `grep`, and `env`.
- **API keys.** `OPENAI_API_KEY` and `GEMINI_API_KEY`. The script checks that they're set, and
  never prints, logs, or writes them.
- **A state location.** `HOME` or `XDG_STATE_HOME`: the default run directory and the machine-wide
  Veo marker are under `${XDG_STATE_HOME:-~/.local/state}/iris-live/`.
- **Provider access.** OpenAI's GPT Image models may require API Organization Verification. The
  Gemini image models and Veo have no free tier: the key's project needs a paid-tier billing plan,
  and on Prepay, a positive credit balance. Veo is a preview model.

Iris reads no other configuration during a live run. The script points `IRIS_CONFIG` at an empty
file and `IRIS_STATE_DIR` at its own directory, and unsets the `IRIS_*` setting overrides for each
`iris` process. It refuses to run at all if `IRIS_OPENAI_BASE_URL` or `IRIS_GEMINI_BASE_URL` is set,
because the run wouldn't be live.

## Steps and costs

The costs come from the catalog as checked on 2026-09-24. Before every step, the script prints the
step's estimated cost, which is `ESTIMATED COST: $0 (free: …)` for steps 5 to 8. For a paid step,
the estimate is Iris's own, from a free `--dry-run` of the exact command.

| Step | What | Settings | Estimate |
|---|---|---|---|
| 1 | OpenAI image generation | `gpt-image-2.5-sunburst --size 1024x1024 --quality low` | ~$0.006 |
| 2 | Gemini image generation | `gemini-3.1-flash-image --resolution 512 --aspect-ratio 1:1` | ~$0.045 |
| 3a | OpenAI edit of step 1's image | Same as step 1 | ~$0.006, plus input tokens |
| 3b | Gemini edit of step 2's image | Same as step 2 | ~$0.045, plus input tokens |
| 4 | Veo submission with `--detach`, only once | `veo-3.1-lite-generate-preview --duration 4 --resolution 720p --aspect-ratio 16:9` | ~$0.20 |
| 5 | Resume from a separate process (`jobs status`) | | Free |
| 6 | Download without submitting again (`jobs wait`) | | Free |
| 7 | Repeat the download safely (`jobs download`, then `jobs download -d COPY`) | | Free |
| 8 | JSON, image, and MP4 checks of the saved outputs | No network | Free |

A full run costs about $0.30. `--step 3` runs 3a, then 3b. `IRIS_LIVE_OPENAI_MODEL` switches to
`gpt-image-2.5-flare`, which costs the same.

The script refuses a paid step, and sends nothing, in any of these cases:

- Iris has no estimate for it.
- The estimate is above the per-step cap, `IRIS_LIVE_MAX_STEP_USD`, $0.50 by default.
- The run directory's estimated spend plus the estimate would exceed the budget,
  `IRIS_LIVE_BUDGET_USD`, $10 by default. Set it to what's left of your overall budget.
- Step 4 only: the video's estimate is more than half of the remaining budget.

## Run the live verification

Every step requires `IRIS_LIVE_CONFIRM=yes-i-accept-charges`. The one Veo submission, step 4, also
requires `IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job`.

```sh
# Free: the steps and their estimates, from dry runs (nothing is sent, no keys needed)
scripts/live-verify.sh --plan

# Paid: one step at a time (recommended; record each LEDGER line)
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh --step 1

# Paid: the one Veo submission, only if no Veo job was submitted for this budget yet
IRIS_LIVE_CONFIRM=yes-i-accept-charges IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job \
  scripts/live-verify.sh --step 4

# Paid: every step in order, stopping at the first failure
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh
```

Without `--step`, the script skips step 4 unless `IRIS_LIVE_VEO_CONFIRM` is set, and skips steps 5
to 7 while the run directory has no Veo job.

Use the same run directory for every step, because later steps read earlier results: step 3 edits
the images from steps 1 and 2, and steps 5 to 7 use step 4's job. The directory is `$IRIS_LIVE_DIR`,
or else `${XDG_STATE_HOME:-~/.local/state}/iris-live/run`. If you pass `--dir`, keep the directory
outside any git checkout, because its raw outputs, job state, and media aren't git-ignored. The
script warns when the directory is inside this checkout.

### Run the steps by hand

The same steps, with the plain CLI. Every command except `--dry-run` sends a real, billed request.
Run each paid command with `--dry-run` first to read its estimate. Always pass explicit sizes and
qualities: with `auto`, Iris has no estimate.

```sh
# 1-2: generation
iris --json image generate "a red paper kite over a hill" -m gpt-image-2.5-sunburst --size 1024x1024 --quality low -o openai.png
iris --json image generate "a red paper kite over a hill" -m gemini-3.1-flash-image --resolution 512 --aspect-ratio 1:1 -o gemini.jpg

# 3: edits reusing those images
iris --json image edit "add a small yellow sun" -m gpt-image-2.5-sunburst -i openai.png --size 1024x1024 --quality low -o openai-edit.png
iris --json image edit "add a small yellow sun" -m gemini-3.1-flash-image -i gemini.jpg --resolution 512 -o gemini-edit.jpg

# 4: the one Veo submission, returning immediately
iris --json video generate "a slow aerial shot over a calm lake at sunrise" -m veo-lite --duration 4 --resolution 720p --aspect-ratio 16:9 --detach

# 5-7: later processes; nothing here submits again
iris --json jobs status JOB_ID
iris --json jobs wait JOB_ID
iris --json jobs download JOB_ID              # warning already_downloaded, no network
iris --json jobs download JOB_ID -d copy/     # local copy, no network
```

Gemini chooses its output type. If it returns PNG for a `.jpg` path, Iris saves the image as `.png`
and reports an `output_extension_adjusted` warning.

## Budget rules

- Check the providers' current pricing pages before each call, because prices change.
- Record each call's estimated cost before you send it, and keep a running total.
- Never send a request whose cost can't be bounded in advance.
- Submit at most one Veo job. Skip it if its estimate is more than half of the remaining budget, or
  if your account doesn't have the paid tier that Veo requires.
- If access, quota, billing, or budget blocks a step, finish the others, name exactly what wasn't
  verified, and label offline (mock) evidence and live evidence as what they are.

## The ledger

The script prints each ledger line with a `LEDGER` prefix, and appends it to `evidence/ledger.txt`:

```text
LEDGER at=… step=4 status=sent estimated_usd=0.2 total_usd=0.30216 exit=0 mode=LIVE note="paid request; pre-call estimate $0.2, post-call estimate $0.2"
LEDGER at=… step=4 status=ok estimated_usd=0 total_usd=0.30216 exit=0 mode=LIVE note="Veo submission accepted (detached), job job_…"
```

- `estimated_usd` is the estimated spend that the line adds. Only `sent` lines add any: the larger
  of Iris's estimate before the request and its estimate from the usage that the provider reported.
- `total_usd` is the run directory's running estimated spend, which the budget checks use.

`status` is one of these values:

| Status | Meaning |
|---|---|
| `sent` | A paid request went out. The script writes this line as soon as the request returns, before any check, whatever its outcome. |
| `ok` | The step passed. For a paid step, its output also passed the checks. |
| `failed` | The step failed. |
| `verify-failed` | The request succeeded, but its output failed a check. |
| `leak` | A saved output contained a credential value. The script deleted it and stopped. This is an Iris bug: report it. A rerun of that paid step refuses to run until you delete `work/raw/stepN.leaked`. |
| `uncertain` | Step 4 only: the submission may have been accepted and billed. |
| `pending` | Step 6 only: the job is still running. |

## Run directory layout

- `DIR/work/`, with mode `0700`, holds the generated media, the raw JSON envelopes and stderr
  (`raw/`), Iris's state (`state/`, with the job record), the empty config file, and the step
  markers.
- `DIR/evidence/` holds sanitized copies of every envelope and stderr file (`stepN.json`,
  `stepN.stderr.txt`) and `ledger.txt`. Before the script saves a file, it shortens local paths
  (`<dir>`, `~`) and checks the file for credential values. It deletes any file that contains one,
  and fails the step. These files are small and safe to keep as evidence.

## Safety rules

- **One Veo submission per machine.** Step 4 needs `IRIS_LIVE_VEO_CONFIRM`. Before it sends the
  request, it writes two markers, atomically: `DIR/work/veo-submitted` and
  `${XDG_STATE_HOME:-~/.local/state}/iris-live/veo-submitted`. While either exists, step 4 never
  submits again, from any directory, even if the earlier process died during the request. It also
  refuses if the state directory already holds a job.
  - The confirmation can't see Veo jobs that you submitted by hand. Set it only if no Veo job was
    submitted for this budget yet.
  - Delete the machine-wide marker only when a new budget allows another submission.
  - A definite rejection, meaning exit code 2 or 3, or a record in the `failed` state, removes both
    markers, so you can retry the step after you fix the cause.
  - `submission_uncertain` (exit code 5) keeps them. Check your usage in Google AI Studio, and
    don't submit again.
- **No repeated paid images.** The ledger records every paid request as `sent` before its output is
  checked.
  - A paid image step whose request succeeded is never sent again. If its output failed a check, a
    rerun repeats only the checks.
  - A marker written before each request makes a rerun refuse if an earlier run died while its
    request was in flight. Check the provider's usage page, then delete `work/raw/stepN.sending` to
    send it again.
  - To pay for a new request deliberately, delete `work/raw/stepN.json`.
- **Waiting is free and resumable.** If the job is still running after `IRIS_LIVE_WAIT`, 15 minutes
  by default, step 6 records `pending` and stops. Run `--step 6` again later: it polls and
  downloads, and never submits again.
- **Image checks.** Iris decodes every image when it saves it. For each image, the script also
  checks that:
  - Iris reported its dimensions.
  - The dimensions match the request: 1024x1024 for OpenAI, and square for Gemini's 1:1.
  - The reported size matches the file.
  - The file starts with the right magic bytes.
  - For a PNG, the IHDR dimensions equal Iris's, and the file ends with IEND.
- **Video checks.** For the MP4, the script checks that:
  - The reported size matches the file.
  - The top-level boxes tile the file exactly, start with `ftyp`, and include `moov`.
  - The movie header (`mvhd` in `moov`, which the script parses) gives the requested 4 seconds,
    between 3.5 and 5.0.
  - Iris reported a duration in the same range.

## Test the script without paying

`IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only` lets the script run against local mock servers. Every
ledger line of such a run says `mode=MOCK`. A mock run is never live verification, and must not be
reported as one. Mock mode requires:

- Both `IRIS_OPENAI_BASE_URL` and `IRIS_GEMINI_BASE_URL`, because a provider left at its default
  would be the real, paid API.
- Each base URL is an `http` or `https` URL on `127.0.0.1`, `localhost`, or `[::1]`.
- `OPENAI_API_KEY` and `GEMINI_API_KEY` are unset or fake: they must start with `fake-` or `test-`.
  Run the script under `env -i`, so that it inherits no real key.

The Veo marker of a mock run is `iris-live/veo-submitted.mock`, so a mock run never blocks a live
one. Point `HOME` or `XDG_STATE_HOME` at a temporary directory to keep the run out of your own state
directory.

`tests/live/mock-run.sh` does all of this for you, offline and for free, and CI runs it on every
change. It serves both APIs from `tests/live/mock_providers.py` on `127.0.0.1`, and runs the script
under `env -i` with fake keys. It checks that:

- `--plan`, with every proxy variable pointing at the mock so that any request that tries to leave
  the machine is recorded, prints the five paid estimates and sends nothing.
- A full mock-mode run passes all eight steps with `mode=MOCK` ledger lines, and the mock receives
  exactly one OpenAI generation and one edit, two Gemini `generateContent` calls, one Veo
  submission, one poll, and one download.
- Running it again sends no request at all.

```sh
cargo build --locked
sh tests/live/mock-run.sh   # the debug build (honors CARGO_TARGET_DIR), or pass another iris
```

The offline process tests check the same behavior against mocks, for free:

```sh
cargo test --test e2e_images --test e2e_video --test e2e_cli
```

## Live run log

Each run was done by hand, through a release build, newest first. Costs are Iris's estimates from
reported usage, not invoices. None of the runs verified reference images on Veo 3.1 Standard,
OpenAI qualities above `low`, or anything on macOS.

| Date | Commits | Scope | Estimated spend |
|---|---|---|---|
| 2026-09-25 | e387959 (the same code as 7209b39) and a8e7884 | Every model, input, and option that the earlier runs didn't exercise | about $5.83 |
| 2026-09-25 | 7209b39 | Steps 1 to 3 and 5 to 8, reusing the Veo job from 0aff663 | about $0.11 |
| 2026-09-25 | 4e9d568 | The same steps and results as the run of 7209b39 | about $0.11 |
| 2026-09-24 | 0aff663 | The first full run, including the Veo submission that the later runs reused | about $0.31 |

No run found a key value in any saved output, log, or job record.

### 2026-09-25: every other model, input, and option

Run through release builds of e387959 and, for the last video, a8e7884. Four requests were refused
or failed without being charged. What three of them showed is reflected in the catalog and the
hints.

| Area | Request | Result | Estimated cost |
|---|---|---|---|
| OpenAI edit | `--mask` (transparent upper-right quadrant), 1024x1024, low | 1024×1024 PNG | $0.0142 |
| OpenAI count | `-n 2`, 1024x1024, low | two 1024×1024 PNGs | $0.0119 |
| OpenAI options | `gpt-image-2.5-flare`, `--format webp`, `-O background=transparent -O compression=60 -O moderation=low` | 1024×1024 WebP with alpha | $0.0059 |
| OpenAI model | `gpt-image-2`, 1536x1024, low | 1536×1024 PNG | $0.0048 |
| Gemini models | `gemini-3.1-flash-lite-image` 1K; `gemini-3-pro-image` 4K 16:9 | 1408×768 JPEG; 5504×3072 JPEG | $0.2769 |
| Gemini options | `gemini-3.1-flash-image` 2K with `-O thinking_level=high` | 2816×1536 JPEG | $0.1047 |
| Gemini references | edit with three input images, 1K | 848×1264 JPEG | $0.0689 |
| Veo frames | Lite, 8 s, 1080p, `--image` + `--last-frame`, `-O person_generation=allow_adult`, waited in one command | 1920×1080 MP4, 8.0 s, in 73 s | $0.64 |
| Veo references | Fast, 8 s, 4k, `--ref` (the `ASSET` wire format), `--detach`; a 3-minute `jobs wait` ended with exit 4 and the job still running, and a second `jobs wait` downloaded it | 3840×2160 MP4, 8.0 s | $2.40 |
| Veo models | Standard, 4 s, 720p, 9:16, `--negative-prompt`; Fast, 4 s, 720p, `--negative-prompt` | 720×1280 and 1280×720 MP4s, 4.0 s | $2.00 |
| Veo people | Lite, 6 s, 720p, `-O person_generation=allow_all` | 1280×720 MP4, 6.0 s; the first attempt failed at the provider with INTERNAL and wasn't charged | $0.30 |

What these requests showed, which the catalog and the hints reflect:

- Veo 3.1 Lite refuses `negativePrompt` ("isn't supported by this model"), so Lite declares no
  negative prompt.
- Veo 3.1 Fast refuses a negative prompt next to a reference image ("not supported in your use
  case"), although it accepts one for text-to-video. Iris refuses that combination before sending,
  on Fast and Standard.
- A job that failed at the provider with INTERNAL succeeded when submitted again, and the hint for
  such a failure says so.

### 2026-09-25: commit 7209b39

The paid image steps were repeated. The Veo steps reused the job from the run of 0aff663 instead of
submitting a second video.

| Step | Model and settings | Result | Estimated cost |
|---|---|---|---|
| 1 OpenAI generate | `gpt-image-2.5-sunburst`, 1024x1024, low | 1024×1024 PNG, decoded; 196 output tokens | $0.0059 |
| 2 Gemini generate | `gemini-3.1-flash-image`, 512, 1:1 | 512×512 JPEG, valid; `output_extension_may_change` warned that Gemini picks the type | $0.0460 |
| 3a OpenAI edit | step 1 image as input, 1024x1024, low | 1024×1024 PNG, decoded | $0.0142 |
| 3b Gemini edit | step 2 image as reference, 512 | 512×512 JPEG, valid | $0.0456 |
| 5 Resume | `jobs status` on the earlier job, with its record reset to `running` so the poll path runs | one poll of the real operation, `succeeded`; retention reported as "at least until" submission + 48 h | Free |
| 6 Download | `jobs download` in a new process | fetched directly from the API host (no redirect); SHA-256 identical to the first run | Free |
| 7 Repeat | `jobs download` again, then `-d` to another directory | `already_downloaded` with no request; local copy with no request | Free |
| 8 JSON | all steps in `--json` mode | one envelope per command | |

A record that the build of 0aff663 wrote was also read unchanged by this one.

### 2026-09-24: commit 0aff663

The first full run, including the Veo submission that the runs of 4e9d568 and 7209b39 reused.

| Step | Model and settings | Result | Estimated cost |
|---|---|---|---|
| 1 OpenAI generate | `gpt-image-2.5-sunburst`, 1024x1024, low | 1024×1024 PNG, decoded; 196 output tokens | $0.0060 |
| 2 Gemini generate | `gemini-3.1-flash-image`, 512, 1:1 | 512×512 JPEG, valid | $0.0460 |
| 3a OpenAI edit | step 1 image as input, 1024x1024, low | 1024×1024 PNG; JSON data-URL edit encoding confirmed | $0.0142 |
| 3b Gemini edit | step 2 image as reference, 512 | 512×512 JPEG, valid | $0.0456 |
| 4 Veo submit | `veo-3.1-lite-generate-preview`, 4 s, 720p, 16:9, `--detach` | accepted in under a second; job recorded | $0.20 |
| 5 Resume | `jobs status` in a new process | `running`, polled remotely | Free |
| 6 Wait + download | `jobs wait` in a new process | done after about 20 s; 1280×720 H.264 + AAC MP4, 4.0 s | Free |
| 7 Repeat | `jobs download` ×3 | `already_downloaded`; local copy; a fetch again gave an identical SHA-256; one submission in total | Free |
| 8 JSON | all steps in `--json` mode | one envelope per command | |

This run showed three facts that the offline tests can't. Gemini returned JPEG for every call. Veo
honored the 4-second duration, so the charge matched the estimate. The API host served the Veo
download directly, with no redirect.
