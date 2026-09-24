# Live verification (paid, opt-in)

Everything else in this repository's test suite runs offline, needs no credentials, and costs
nothing — see [CONTRIBUTING.md](../CONTRIBUTING.md). Live verification is different: it makes
real, billed requests to OpenAI and Google with real credentials, so it is **never** run by CI,
never run automatically, and never run by accident.

## Status in this checkout

As of this writing, the dedicated live-verification entry point (`scripts/live-verify.sh` and
`tests/live/README.md`, tracked separately from this documentation) is **not present** in this
checkout. This page documents the live-verification *policy* — what must be checked, the cost
budget, and how to run each step by hand with the real `iris` binary — so that policy is usable
today, and so it matches whatever `scripts/live-verify.sh` implements once it lands (this page
does not get to invent a different policy). Do not run `scripts/live-verify.sh` unless you have
read it yourself and understand exactly what it will charge; the same caution applies to running
any of the manual commands below.

## What must be verified, and why

Iris's own offline tests exercise every code path against mock servers, but they cannot prove
Iris's understanding of the *real* wire format is correct — a provider's documentation can be
wrong, incomplete, or have changed. Live verification closes that gap with the smallest set of
real requests that actually exercises every integration:

1. OpenAI image generation.
2. Gemini image generation.
3. Editing/reference input on each implemented image provider — reusing the images generated in
   steps 1–2 as inputs, so no extra generation is paid for just to get an input image.
4. Veo video generation, submitted without waiting (`--detach`).
5. Resuming that same job from a **separate** `iris` invocation (`jobs status` / `jobs wait`).
6. Downloading it, without resubmission.
7. A safe repeat retrieval of the same download (proving it doesn't re-fetch or re-charge).
8. At least one of the above run in `--json` mode, to prove the JSON contract holds against a
   real response, not just a mock one.

Steps 4–7 all reuse the **same single** Veo job — Veo generation is the expensive part of this
list, so the budget (below) allows only one submission. Every image produced is validated as
real, decodable media before Iris saves it under its final name, and every video is validated as
a structurally valid MP4 (`ftyp`/`moov` boxes present, duration read where parseable) — see
[architecture.md](architecture.md#modules) (`artifacts::media`).

## Budget: an estimated $10 total

Check official, current pricing before every call — provider prices change. Choose the cheapest
settings that still exercise the feature:

- OpenAI: the default model (`gpt-image-2.5-sunburst`) with `--size 1024x1024 --quality low`.
- Gemini images: `gemini-3.1-flash-image --resolution 512 --aspect-ratio 1:1` (the cheapest
  documented resolution).
- Veo: `veo-3.1-lite-generate-preview` (the cheapest model), the shortest supported duration
  (`--duration 4`), the lowest resolution (`--resolution 720p`) — audio cannot be disabled on the
  Gemini API, so there is no audio-off option to reduce cost further.

Reserve (mentally or in a tracking sheet) the estimated cost of a call **before** sending it, and
keep a running total. Never send a request whose cost cannot be reasonably bounded up front (e.g.
an unconstrained `--count` or an `auto` size/quality that Iris itself already flags with
`cost_estimate_unavailable` — treat that warning as a signal to add an explicit, boundable value
before running it live). Never exceed the budget, and never add billing headroom just to keep
going — if a step would blow the budget, stop and report it as a blocker instead.

**Veo, specifically:** make **at most one submission**, ever, per verification pass. If its
estimated cost would exceed half of whatever budget remains, or if your account lacks the
required billing tier or region access, do not submit it — record that as a blocker rather than
guessing at whether it would work.

## Running it by hand today

Every command below is the real `iris` CLI — nothing here is simulated. Running any of them
sends a real, billed request. Do not run them without your own `OPENAI_API_KEY` /
`GEMINI_API_KEY`, without having checked current pricing yourself, and without intending to spend
real money.

```console
# 1-2: image generation, cheapest bounded settings
$ iris image generate "..." --size 1024x1024 --quality low -o openai.png --json
$ iris image generate "..." --provider gemini --resolution 512 --aspect-ratio 1:1 -o gemini.png --json

# 3: editing, reusing the images just generated (no extra generation cost)
$ iris image edit -i openai.png "..." -o openai-edit.png --json
$ iris image edit -i gemini.png "..." --provider gemini -o gemini-edit.png --json

# 4: Veo submit-and-return (the one allowed submission)
$ iris video generate "..." --model veo-3.1-lite-generate-preview --duration 4 --resolution 720p --detach --json
# note the job_id printed above, then from here on nothing resubmits anything:

# 5: resume from a separate invocation
$ iris jobs status <job_id> --json
$ iris jobs wait <job_id> --json

# 6-7: download, then repeat (both safe, neither resubmits or re-charges)
$ iris jobs download <job_id> --json
$ iris jobs download <job_id> --json
```

After each step, sanity-check the result: `ok: true`, a real `artifacts[].sha256`/`bytes`, and (for
images) dimensions that make sense. `iris` itself already rejects a payload that fails media
validation before saving it, so a `succeeded` result with an `artifacts[]` entry is meaningful
evidence, not just "the HTTP call didn't error."

## Recording results

Store sanitized evidence (the `--json` output of each step, with nothing secret in it — Iris
already never prints credential values) rather than a narrative claim of success. Never claim a
live success based on a mock-server run; conversely, mock-server evidence (like the transcripts
throughout this repository's other docs) is clearly distinguishable from live evidence because it
points at `127.0.0.1`, not a real provider host, in `providers.list`/`config show` output.

If access, quota, billing setup, pricing uncertainty, or the budget prevents completing a step:
finish everything else that isn't blocked, name the exact behavior that is unverified, keep
offline evidence and live evidence clearly labeled as what they are, and record the opt-in
command that would finish the job once the blocker is resolved — don't mark the behavior
"supported" on the strength of a mock alone.
