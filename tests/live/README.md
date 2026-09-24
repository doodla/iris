# Live verification (opt-in, paid)

`scripts/live-verify.sh` checks Iris against the real OpenAI and Gemini APIs
through the built `iris` binary. It follows the live-verification steps in
SPEC §7 and the budget rules in §8.

**This script spends money.** CI and `cargo test` never run it. Every step that
runs requires `IRIS_LIVE_CONFIRM=yes-i-accept-charges`. The ordinary test suite
is offline and free: `cargo test` runs the process-level scenarios in
`tests/e2e_*.rs` against local mock servers. Those runs are mock evidence, never
live evidence.

## Prerequisites

- A built binary: `cargo build --release` (the default is `target/release/iris`;
  pass `--bin PATH` or set `IRIS_BIN` to use another).
- `jq`, plus the standard tools `od`, `awk`, `grep`, and `env`.
- `OPENAI_API_KEY` and `GEMINI_API_KEY` in the environment. The script checks
  that they are present and never prints, logs, or writes them.
- Provider access:
  - OpenAI GPT Image models may require API Organization Verification.
  - Gemini image models and Veo need a paid tier with Prepay credits.
  - Veo is a preview model.

Iris reads no other configuration during a live run. The script points
`IRIS_CONFIG` at an empty file and `IRIS_STATE_DIR` at its own directory. It
unsets the `IRIS_*` setting overrides for each `iris` invocation. It refuses to
run at all if `IRIS_OPENAI_BASE_URL` or `IRIS_GEMINI_BASE_URL` is set, because
the run would then not be live.

## Steps and estimated costs

The costs below come from the catalog as of 2026-09-24. Before each paid step,
the script prints Iris's own estimate, taken from a free `--dry-run` of the
exact command. A step is refused if it has no estimate, or if the estimate is
above the per-step cap (`IRIS_LIVE_MAX_STEP_USD`, default $0.50).

| step | what | settings | estimate |
|---|---|---|---|
| 1 | OpenAI image generation | `gpt-image-2.5-sunburst --size 1024x1024 --quality low` | ~$0.006 |
| 2 | Gemini image generation | `gemini-3.1-flash-image --resolution 512 --aspect-ratio 1:1` | ~$0.045 |
| 3a | OpenAI edit of step 1's image | same as step 1 | ~$0.006 plus input tokens |
| 3b | Gemini edit of step 2's image | same as step 2 | ~$0.045 plus input tokens |
| 4 | Veo submission, `--detach` (only once) | `veo-3.1-lite-generate-preview --duration 4 --resolution 720p --aspect-ratio 16:9` | ~$0.20 |
| 5 | resume from a separate invocation (`jobs status`) | | free |
| 6 | download without resubmission (`jobs wait`) | | free |
| 7 | safe repeat retrieval (`jobs download`, then `jobs download -d <copy>`) | | free |
| 8 | JSON-mode, image-decode, and MP4-structure checks over the saved outputs | no network | free |

A full run costs about $0.30. `--step 3` runs 3a and then 3b. `IRIS_LIVE_OPENAI_MODEL`
switches to `gpt-image-2.5-flare`, which costs the same.

## Commands

```sh
# Free: the steps and estimates, from dry runs (nothing is sent, no keys needed)
scripts/live-verify.sh --plan

# Paid: one step at a time (recommended; record each LEDGER line)
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh --step 1 --dir ./iris-live

# Paid: every step in order, stopping at the first failure
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh --dir ./iris-live
```

Use the same `--dir` for every step. Later steps read earlier results from it:
step 3 edits the images from steps 1 and 2, and steps 5–7 use step 4's job id.

Each finished step prints one line, which is also appended to
`evidence/ledger.txt`:

```
LEDGER at=… step=4 status=ok estimated_usd=0.2 exit=0 mode=LIVE note="Veo submission (detached), job job_…"
```

`status` is one of:

- `ok`: the step passed.
- `failed`: the step failed.
- `uncertain`: step 4 only.
- `pending`: step 6 only. The job is still running.

The paid image steps also record Iris's post-call estimate, which is computed
from the usage the provider reported.

## Directory layout

- `DIR/work/` (mode 0700) holds the generated media, the raw JSON envelopes and
  stderr (`raw/`), Iris's state (`state/`, with the job record), and the empty
  config.
- `DIR/evidence/` holds sanitized copies of every envelope and stderr file
  (`stepN.json`, `stepN.stderr.txt`) and `ledger.txt`. Before a file is saved,
  local paths are shortened (`<dir>`, `~`), and the file is checked for
  credential values; any file containing one is deleted and the step fails.
  These files are small and safe to keep as evidence.

## Safety properties

- **One Veo submission per directory.** Step 4 writes `work/veo-submitted`
  *before* sending the request. After that, step 4 never submits again, even if
  the earlier process died mid-request. It also refuses if the state directory
  already holds a job.
  - A definite rejection (exit 2 or 3, or a record in `failed` state) removes the
    marker, so the step can be retried after the cause is fixed.
  - `submission_uncertain` (exit 5) keeps the marker. Check usage in Google AI
    Studio and do not resubmit.
- **No repeated paid images.** A paid image step whose saved envelope shows
  success is skipped. To repeat it deliberately (and pay again), delete
  `work/raw/stepN.json`.
- **Waiting is free and resumable.** If the job is still running after
  `IRIS_LIVE_WAIT` (default `15m`), step 6 records `pending` and stops. Run
  `--step 6` again later: it polls and downloads, and never resubmits.
- **Media checks.** Iris decodes every image when it saves it; the script checks
  that dimensions were reported, that sizes match, and that each file starts
  with the right magic bytes. For the video, the script checks that the MP4
  starts with `ftyp`, contains `moov`, and has the reported size and a parsed
  duration.

## Testing the script without paying

`IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only` lets the script run with
`IRIS_OPENAI_BASE_URL` and `IRIS_GEMINI_BASE_URL` pointing at local mock
servers. Use it with fake keys to exercise the script itself. Every ledger line
of such a run says `mode=MOCK`. A mock run is never live verification and must
not be reported as one.

The offline process tests are the free way to check the same behavior against
mocks:

```sh
cargo test --test e2e_images --test e2e_video --test e2e_cli
```
