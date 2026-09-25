# Live verification (paid, opt-in)

The normal test suite (`cargo test`) runs offline against local mock servers, needs no
credentials, and costs nothing — see
[CONTRIBUTING.md](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md). Live verification is
different: it sends real, billed requests to OpenAI and Google with your keys. CI never runs it,
and nothing runs it automatically.

## What it checks

The smallest set of real requests that exercises every integration:

1. OpenAI image generation.
2. Gemini image generation.
3. Editing with a reference image on each image provider, reusing the images from steps 1–2.
4. One Veo video submission, without waiting (`--detach`).
5. Resuming that job from a **separate** `iris` process (`jobs status`).
6. Waiting and downloading it without resubmitting (`jobs wait`).
7. Repeating the download safely (`jobs download`, then to another directory).
8. JSON mode throughout, plus independent checks that images decode and the MP4 is valid.

Steps 4–7 share **one** Veo job: video is the expensive part, so a run submits at most one.

## Running it

[`scripts/live-verify.sh`](https://github.com/doodla/iris/blob/main/scripts/live-verify.sh) runs
the steps through the built binary with the cheapest settings, one step at a time or all in order.
Read [tests/live/README.md](https://github.com/doodla/iris/blob/main/tests/live/README.md) and the
script before running it.

```console
$ cargo build --release
$ scripts/live-verify.sh --plan                      # free: prints each step's estimated cost
$ IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh --step 1
```

Safeguards built into the script: every step needs `IRIS_LIVE_CONFIRM=yes-i-accept-charges`, the
Veo step additionally needs `IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job` and refuses a second
submission, each paid step prints Iris's own `--dry-run` estimate first and is refused if there is
no estimate or it exceeds the per-step cap, keys are checked for presence only, and saved evidence
is scanned for key values.

### By hand

The same steps with the plain CLI. Every command below except `--dry-run` sends a real, billed
request. Run each paid command with `--dry-run` first to read its estimate; an `auto` size or
quality produces `cost_estimate_unavailable`, so always pass explicit values.

```console
# 1–2: generation
$ iris --json image generate "a red paper kite over a hill" --size 1024x1024 --quality low -o openai.png
$ iris --json image generate "a red paper kite over a hill" --provider gemini --resolution 512 --aspect-ratio 1:1 -o gemini.jpg

# 3: edits reusing those images
$ iris --json image edit "add a small yellow sun" -i openai.png --size 1024x1024 --quality low -o openai-edit.png
$ iris --json image edit "add a small yellow sun" --provider gemini -i gemini.jpg --resolution 512 -o gemini-edit.jpg

# 4: the one Veo submission, returning immediately
$ iris --json video generate "a slow aerial shot over a calm lake at sunrise" -m veo-lite --duration 4 --resolution 720p --aspect-ratio 16:9 --detach

# 5–7: later processes; nothing here resubmits
$ iris --json jobs status <job_id>
$ iris --json jobs wait <job_id>
$ iris --json jobs download <job_id>              # warning already_downloaded, no network
$ iris --json jobs download <job_id> -d copy/     # local copy, no network
```

Gemini chooses its output format; if it returns PNG for a `.jpg` path, Iris saves it as `.png` and
says so with an `output_extension_adjusted` warning.

## Budget rules

- Check the providers' current pricing pages before each call; prices change.
- Record each call's estimated cost before sending it and keep a running total.
- Never send a request whose cost cannot be bounded up front.
- Submit at most one Veo job, and skip it if its estimate exceeds half of the remaining budget or
  your account lacks the paid tier Veo requires.
- If access, quota, billing, or budget blocks a step, finish the others, name exactly what stayed
  unverified, and keep offline (mock) evidence and live evidence labeled as what they are.

## Last live runs

Every run was done by hand through a release build, following the steps above. Costs are Iris's
usage-based estimates, not invoices.

### 2026-09-25, commits e387959 and a8e7884: everything not yet verified live

Every model, input kind and option that the earlier runs had not exercised, run by hand through
release builds of e387959 (the same code as 7209b39) and, for the last video, a8e7884. Four
requests were refused or failed without being charged; three of them led to fixes.

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
| Veo references | Fast, 8 s, 4k, `--ref` (the `ASSET` wire format), `--detach`; a 3-minute `jobs wait` ended with exit 4 and the job still running, a second `jobs wait` downloaded it | 3840×2160 MP4, 8.0 s | $2.40 |
| Veo models | Standard, 4 s, 720p, 9:16, `--negative-prompt`; Fast, 4 s, 720p, `--negative-prompt` | 720×1280 and 1280×720 MP4s, 4.0 s | $2.00 |
| Veo people | Lite, 6 s, 720p, `-O person_generation=allow_all` | 1280×720 MP4, 6.0 s (the first attempt failed at the provider with INTERNAL and was not charged) | $0.30 |

Findings, fixed in a8e7884 and 345125e:

- Veo 3.1 Lite refuses `negativePrompt` ("isn't supported by this model"), so Lite no longer
  declares a negative prompt.
- Veo 3.1 Fast refuses a negative prompt next to a reference image ("not supported in your use
  case"), although it accepts one for text-to-video. Iris now refuses that combination before
  sending, on Fast and Standard.
- A job that failed at the provider with INTERNAL succeeded when resubmitted. Its hint now says
  so, instead of pointing at the prompt.

Estimated spend: about $5.83. No key values were found in any saved output, log, or job record.

### 2026-09-25, commit 7209b39

Run against the code at commit 7209b39, after the final review fixes. The paid image steps were
repeated; the Veo steps reused the job from the first run instead of submitting a second video.

| Step | Model and settings | Result | Estimated cost |
|---|---|---|---|
| 1 OpenAI generate | `gpt-image-2.5-sunburst`, 1024x1024, low | 1024×1024 PNG, decoded; 196 output tokens | $0.0059 |
| 2 Gemini generate | `gemini-3.1-flash-image`, 512, 1:1 | 512×512 JPEG, valid; `output_extension_may_change` warned that Gemini picks the type | $0.0460 |
| 3a OpenAI edit | step 1 image as input, 1024x1024, low | 1024×1024 PNG, decoded | $0.0142 |
| 3b Gemini edit | step 2 image as reference, 512 | 512×512 JPEG, valid | $0.0456 |
| 5 Resume | `jobs status` on the earlier job, with its record reset to `running` so the poll path runs | one poll of the real operation → `succeeded`; retention reported as "at least until" submission + 48 h | free |
| 6 Download | `jobs download` in a new process | fetched directly from the API host (no redirect); SHA-256 identical to the first run | free |
| 7 Repeat | `jobs download` again, then `-d` to another directory | `already_downloaded` with no request; local copy with no request | free |
| 8 JSON | all steps in `--json` mode | one envelope per command | — |

A record written by the first build was also read unchanged by this one. Estimated spend: about
$0.11. No key values were found in any saved output, log, or job record.

### 2026-09-25, commit 4e9d568

An intermediate re-run between review rounds, with the same steps and results as the run above
(estimated spend about $0.11).

### 2026-09-24, commit 0aff663

The first full run, including the only Veo submission.

| Step | Model and settings | Result | Estimated cost |
|---|---|---|---|
| 1 OpenAI generate | `gpt-image-2.5-sunburst`, 1024x1024, low | 1024×1024 PNG, decoded; 196 output tokens | $0.0060 |
| 2 Gemini generate | `gemini-3.1-flash-image`, 512, 1:1 | 512×512 JPEG, valid | $0.0460 |
| 3a OpenAI edit | step 1 image as input, 1024x1024, low | 1024×1024 PNG; JSON data-URL edit encoding confirmed | $0.0142 |
| 3b Gemini edit | step 2 image as reference, 512 | 512×512 JPEG, valid | $0.0456 |
| 4 Veo submit | `veo-3.1-lite-generate-preview`, 4 s, 720p, 16:9, `--detach` | accepted in under a second; job recorded | $0.20 |
| 5 Resume | `jobs status` in a new process | `running`, polled remotely | free |
| 6 Wait + download | `jobs wait` in a new process | done after about 20 s; 1280×720 H.264 + AAC MP4, 4.0 s | free |
| 7 Repeat | `jobs download` ×3 | `already_downloaded`; local copy; re-fetch gave an identical SHA-256; one submission total | free |
| 8 JSON | all steps in `--json` mode | one envelope per command | — |

Estimated spend: about $0.31. No key values were found in any saved output, log, or job record.

Observed facts that the offline tests cannot show: Gemini returned JPEG for every call, Veo honored
the 4-second duration (so the charge matches the estimate), and the Veo file download was served
directly by the API host with no redirect. Not verified live: reference images on Veo 3.1
Standard, OpenAI qualities above `low`, and anything on macOS.
