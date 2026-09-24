# Live verification (paid, opt-in)

The normal test suite (`cargo test`) runs offline against local mock servers, needs no
credentials, and costs nothing — see [CONTRIBUTING.md](../CONTRIBUTING.md). Live verification is
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

[`scripts/live-verify.sh`](../scripts/live-verify.sh) runs the steps through the built binary with
the cheapest settings, one step at a time or all in order. Read
[tests/live/README.md](../tests/live/README.md) and the script before running it.

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

## Last live run

Run on 2026-09-24 against version 0.1.0, by hand through the release binary, following the steps
above. Costs are Iris's usage-based estimates, not invoices.

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

Total estimated spend: about $0.31. A scan of every saved output, log, and job record found no key
values. Observed facts that the offline tests cannot show: Gemini returned JPEG for both calls,
Veo honored the 4-second duration (so the charge matches the estimate), and the Veo file download
was served directly by the API host with no redirect.
