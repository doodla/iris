# Live verification (opt-in, paid)

`scripts/live-verify.sh` checks Iris against the real OpenAI and Gemini APIs
through the built `iris` binary. It follows the steps and budget rules
documented below.

**This script spends money.** CI and `cargo test` never run it. Every step that
runs requires `IRIS_LIVE_CONFIRM=yes-i-accept-charges`. The one Veo submission
(step 4) also requires `IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job`. The ordinary
test suite is offline and free: `cargo test` runs the process-level scenarios in
`tests/e2e_*.rs` against local mock servers. Those runs are mock evidence, never
live evidence.

## Prerequisites

- A built binary: `cargo build --release`. The default is `target/release/iris`
  of this checkout; pass `--bin PATH` or set `IRIS_BIN` to use another (for
  example when `CARGO_TARGET_DIR` points elsewhere). An `iris` found on `PATH`
  is never used, because it may be a stale build.
- `jq`, plus the standard tools `od`, `awk`, `grep`, and `env`.
- `OPENAI_API_KEY` and `GEMINI_API_KEY` in the environment. The script checks
  that they are present and never prints, logs, or writes them.
- `HOME` or `XDG_STATE_HOME`: the default directory and the machine-wide Veo
  marker live under `${XDG_STATE_HOME:-~/.local/state}/iris-live/`.
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

The costs below come from the catalog as of 2026-09-24. Before every step, the
script prints its estimated cost (`ESTIMATED COST: $0 (free: …)` for steps 5–8).
For a paid step, the estimate is Iris's own, taken from a free `--dry-run` of
the exact command. A paid step is refused, with nothing sent, in any of these
cases:

- Iris has no estimate for it.
- The estimate is above the per-step cap (`IRIS_LIVE_MAX_STEP_USD`, default
  $0.50).
- The directory's estimated spend plus the estimate would exceed the budget
  (`IRIS_LIVE_BUDGET_USD`, default $10). Set it to what is left of your overall
  budget.
- Step 4 only: the clip's estimate is more than half of the remaining budget.

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
| 8 | JSON-mode, image, and MP4 checks over the saved outputs | no network | free |

A full run costs about $0.30. `--step 3` runs 3a and then 3b. `IRIS_LIVE_OPENAI_MODEL`
switches to `gpt-image-2.5-flare`, which costs the same.

## Commands

```sh
# Free: the steps and estimates, from dry runs (nothing is sent, no keys needed)
scripts/live-verify.sh --plan

# Paid: one step at a time (recommended; record each LEDGER line)
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh --step 1

# Paid: the one Veo submission, only if no Veo job was submitted for this budget yet
IRIS_LIVE_CONFIRM=yes-i-accept-charges IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job \
  scripts/live-verify.sh --step 4

# Paid: every step in order, stopping at the first failure
IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh
```

Without `--step`, step 4 is skipped unless `IRIS_LIVE_VEO_CONFIRM` is set, and
steps 5–7 are skipped while the directory has no Veo job.

Use the same directory for every step. Later steps read earlier results from it:
step 3 edits the images from steps 1 and 2, and steps 5–7 use step 4's job id.
The default is `$IRIS_LIVE_DIR`, else `${XDG_STATE_HOME:-~/.local/state}/iris-live/run`.
If you pass `--dir`, keep it outside any git checkout: its raw outputs, job
state, and media are not git-ignored. The script warns when the directory is
inside this checkout.

## Ledger

Each ledger line is printed with a `LEDGER` prefix and appended to
`evidence/ledger.txt`:

```
LEDGER at=… step=4 status=sent estimated_usd=0.2 total_usd=0.30216 exit=0 mode=LIVE note="paid request; pre-call estimate $0.2, post-call estimate $0.2"
LEDGER at=… step=4 status=ok estimated_usd=0 total_usd=0.30216 exit=0 mode=LIVE note="Veo submission accepted (detached), job job_…"
```

- `estimated_usd` is the estimated spend the line adds. Only `sent` lines add
  any. Their amount is the larger of Iris's pre-call estimate and its post-call
  estimate from the usage the provider reported.
- `total_usd` is the directory's running estimated spend. The budget checks use
  it.

`status` is one of:

- `sent`: a paid request went out. This line is written as soon as the request
  returns, before any check, whatever its outcome.
- `ok`: the step passed. For a paid step, its output also passed the checks.
- `failed`: the step failed.
- `verify-failed`: the request succeeded, but its output failed a check.
- `leak`: a saved output contained a credential value. It was deleted, and the
  run stopped. This is an Iris bug; report it. A rerun of that paid step refuses
  until `work/raw/stepN.leaked` is deleted.
- `uncertain`: step 4 only. The submission may have been accepted and billed.
- `pending`: step 6 only. The job is still running.

## Directory layout

- `DIR/work/` (mode 0700) holds the generated media, the raw JSON envelopes and
  stderr (`raw/`), Iris's state (`state/`, with the job record), the empty
  config, and the step markers.
- `DIR/evidence/` holds sanitized copies of every envelope and stderr file
  (`stepN.json`, `stepN.stderr.txt`) and `ledger.txt`. Before a file is saved,
  local paths are shortened (`<dir>`, `~`), and the file is checked for
  credential values; any file containing one is deleted and the step fails.
  These files are small and safe to keep as evidence.

## Safety properties

- **One Veo submission per machine.** Step 4 needs `IRIS_LIVE_VEO_CONFIRM`. It
  writes two markers *before* sending the request: `DIR/work/veo-submitted` and
  `${XDG_STATE_HOME:-~/.local/state}/iris-live/veo-submitted`. Both are created
  atomically. While either exists, step 4 never submits again, from any
  directory, even if the earlier process died mid-request. It also refuses if
  the state directory already holds a job.
  - The confirmation cannot see Veo jobs submitted by hand. Set it only if no
    Veo job was submitted for this budget yet.
  - Delete the machine-wide marker only when a new budget allows another
    submission.
  - A definite rejection (exit 2 or 3, or a record in `failed` state) removes
    both markers, so the step can be retried after the cause is fixed.
  - `submission_uncertain` (exit 5) keeps them. Check usage in Google AI Studio
    and do not resubmit.
- **No repeated paid images.** Every paid request is ledgered as `sent` before
  its output is checked.
  - A paid image step whose request succeeded is never sent again. If its output
    failed a check, a rerun repeats only the checks.
  - A marker written before each request makes a rerun refuse if an earlier run
    died while its request was in flight. Check the provider's usage page, then
    delete `work/raw/stepN.sending` to send it again.
  - To pay for a new request deliberately, delete `work/raw/stepN.json`.
- **Waiting is free and resumable.** If the job is still running after
  `IRIS_LIVE_WAIT` (default `15m`), step 6 records `pending` and stops. Run
  `--step 6` again later: it polls and downloads, and never resubmits.
- **Image checks.** Iris decodes every image when it saves it. For each image,
  the script checks that:
  - Iris reported its dimensions.
  - The dimensions match the request: 1024x1024 for OpenAI, square for Gemini's
    1:1.
  - The size matches the file.
  - The file starts with the right magic bytes.
  - For a PNG, the IHDR dimensions equal Iris's, and the file ends with IEND.
- **Video checks.** For the MP4, the script checks that:
  - The size matches the file.
  - The top-level boxes tile the file exactly, start with `ftyp`, and include
    `moov`.
  - The movie header (`mvhd` in `moov`, parsed by the script) gives the
    requested 4 s (3.5 s to 5.0 s).
  - Iris reported a duration in the same range.

## Testing the script without paying

`IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only` lets the script run against local
mock servers. Every ledger line of such a run says `mode=MOCK`. A mock run is
never live verification and must not be reported as one. Mock mode requires:

- Both `IRIS_OPENAI_BASE_URL` and `IRIS_GEMINI_BASE_URL`. A provider left at its
  default would be the real, paid API.
- Each base URL is an `http(s)` URL on `127.0.0.1`, `localhost`, or `[::1]`.
- `OPENAI_API_KEY` and `GEMINI_API_KEY` are unset or fake: they must start with
  `fake-` or `test-`. Run it under `env -i` so that no real key is inherited.

The Veo marker of a mock run is `iris-live/veo-submitted.mock`, so a mock run
never blocks a live one. Point `HOME` or `XDG_STATE_HOME` at a temporary
directory to keep it out of your own state directory.

`tests/live/mock-run.sh` does all of this for you, offline and for free, and CI
runs it on every change. It serves both APIs from `tests/live/mock_providers.py`
on `127.0.0.1` and runs the script under `env -i` with fake keys:

- `--plan`, with every proxy variable pointing at the mock so that any request
  trying to leave the machine would be recorded, must print the five paid
  estimates and send nothing.
- A full mock-mode run must pass all eight steps with `mode=MOCK` ledger lines,
  and the mock must receive exactly one OpenAI generation and one edit, two
  Gemini `generateContent` calls, one Veo submission, one poll, and one
  download.
- Running it again must send no request at all.

```sh
cargo build --locked
sh tests/live/mock-run.sh target/debug/iris   # or the path of another iris build
```

The offline process tests are the free way to check the same behavior against
mocks:

```sh
cargo test --test e2e_images --test e2e_video --test e2e_cli
```
