#!/usr/bin/env bash
# live-verify.sh — OPT-IN, PAID live verification of Iris against the real OpenAI
# and Gemini APIs (SPEC §7 steps 1–8, budget rules of SPEC §8).
#
# THIS SCRIPT SPENDS MONEY. It is never run by CI or by `cargo test`. It refuses
# to run a step unless IRIS_LIVE_CONFIRM=yes-i-accept-charges is set.
#
# Documentation: tests/live/README.md. Quick reference: `scripts/live-verify.sh --help`.
#
# Credentials come only from OPENAI_API_KEY / GEMINI_API_KEY in the environment.
# The script checks them for presence only; it never prints, logs, writes, or
# passes them as arguments (the scan of saved outputs feeds the value to grep
# through a pipe, not argv).

set -euo pipefail

readonly CONFIRM_VALUE="yes-i-accept-charges"
readonly GEMINI_IMAGE_MODEL="gemini-3.1-flash-image"
readonly VEO_MODEL="veo-3.1-lite-generate-preview"
readonly PROMPT_IMAGE="A small red paper boat on a calm blue pond, simple flat illustration"
readonly PROMPT_EDIT="Add a small yellow sun in the top-right corner and keep everything else unchanged"
readonly PROMPT_VIDEO="A small red paper boat drifting slowly on a calm pond, gentle ripples, static camera"

usage() {
    cat <<'EOF'
Usage:
  IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh [--step N] [--dir DIR] [--bin PATH]
  scripts/live-verify.sh --plan [--dir DIR] [--bin PATH]
  scripts/live-verify.sh --help

PAID live verification of Iris through the built binary (SPEC section 7). Each
paid step first prints Iris's own cost estimate (from a free --dry-run of the
same command) and refuses to run if no estimate exists or it exceeds the
per-step cap.

Steps (cheapest settings; estimates as of the catalog date):
  1   OpenAI image generation      gpt-image-2.5-sunburst, 1024x1024, quality low   ~$0.006   paid
  2   Gemini image generation      gemini-3.1-flash-image, 512, 1:1                 ~$0.045   paid
  3a  OpenAI edit of step 1's image (same settings)                              ~$0.006+  paid
  3b  Gemini edit of step 2's image (same settings)                              ~$0.045+  paid
  3   = 3a then 3b
  4   Veo submission, --detach      veo-3.1-lite-generate-preview, 4 s, 720p, 16:9  ~$0.20    paid, ONCE
  5   resume the job from a separate invocation (jobs status)                     free
  6   download without resubmission (jobs wait)                                   free
  7   safe repeat retrieval (jobs download, then jobs download -d copy)           free
  8   JSON-mode, image-decode, and MP4-structure checks over the saved outputs    free (no network)
Without --step, steps 1..8 run in order and stop at the first failure.

Options:
  --step N     run one step: 1, 2, 3, 3a, 3b, 4, 5, 6, 7, 8, or all (default all)
  --dir DIR    working directory (default: $IRIS_LIVE_DIR or ./iris-live):
                 DIR/work      media, raw outputs, Iris state (IRIS_STATE_DIR), empty config
                 DIR/evidence  sanitized JSON outputs, stderr, ledger.txt (safe to keep)
  --bin PATH   iris binary (default: $IRIS_BIN, else target/release/iris, else iris on PATH)
  --plan       print each paid step's estimate using free dry runs; sends nothing,
               needs no keys and no confirmation
  -h, --help   this text

Environment:
  IRIS_LIVE_CONFIRM=yes-i-accept-charges   required for every step (not for --plan/--help)
  OPENAI_API_KEY, GEMINI_API_KEY           checked for presence only
  IRIS_LIVE_OPENAI_MODEL                   OpenAI model (default gpt-image-2.5-sunburst; flare costs the same)
  IRIS_LIVE_MAX_STEP_USD                   per-step estimate cap (default 0.50)
  IRIS_LIVE_WAIT                           caller wait limit of step 6 (default 15m)

Safety:
  * The Veo job is submitted at most once per DIR: a marker is written BEFORE the
    request, and step 4 refuses to run again once it exists.
  * Steps whose evidence shows success are not rerun (delete the evidence file to
    rerun a paid step deliberately).
  * IRIS_OPENAI_BASE_URL / IRIS_GEMINI_BASE_URL make a run non-live, so they are
    refused. IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only allows them to test this
    script against local mock servers; every ledger line then says mode=MOCK, which
    is never live evidence.
EOF
}

die() {
    printf 'live-verify: error: %s\n' "$*" >&2
    exit 1
}

say() {
    printf 'live-verify: %s\n' "$*"
}

# ----- arguments -----------------------------------------------------------------------

STEP=all
PLAN=0
DIR=${IRIS_LIVE_DIR:-./iris-live}
BIN=${IRIS_BIN:-}
while [ $# -gt 0 ]; do
    case $1 in
        --step) [ $# -ge 2 ] || die "--step needs a value"; STEP=$2; shift 2 ;;
        --step=*) STEP=${1#*=}; shift ;;
        --dir) [ $# -ge 2 ] || die "--dir needs a value"; DIR=$2; shift 2 ;;
        --dir=*) DIR=${1#*=}; shift ;;
        --bin) [ $# -ge 2 ] || die "--bin needs a value"; BIN=$2; shift 2 ;;
        --bin=*) BIN=${1#*=}; shift ;;
        --plan) PLAN=1; shift ;;
        -h | --help) usage; exit 0 ;;
        *) die "unknown argument '$1' (see --help)" ;;
    esac
done
case $STEP in
    all | 1 | 2 | 3 | 3a | 3b | 4 | 5 | 6 | 7 | 8) ;;
    *) die "unknown step '$STEP' (1, 2, 3, 3a, 3b, 4, 5, 6, 7, 8, or all)" ;;
esac

OPENAI_MODEL=${IRIS_LIVE_OPENAI_MODEL:-gpt-image-2.5-sunburst}
MAX_STEP_USD=${IRIS_LIVE_MAX_STEP_USD:-0.50}
WAIT_LIMIT=${IRIS_LIVE_WAIT:-15m}

command -v jq >/dev/null 2>&1 || die "jq is required (https://jqlang.org)"
command -v od >/dev/null 2>&1 || die "od is required"

if [ "$PLAN" = 0 ] && [ "${IRIS_LIVE_CONFIRM:-}" != "$CONFIRM_VALUE" ]; then
    die "this script makes PAID requests. Set IRIS_LIVE_CONFIRM=$CONFIRM_VALUE to accept the charges \
(see --help for the steps and estimates, or --plan for a free estimate-only run)"
fi

MODE=LIVE
if [ -n "${IRIS_OPENAI_BASE_URL:-}" ] || [ -n "${IRIS_GEMINI_BASE_URL:-}" ]; then
    if [ "${IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE:-}" = "mock-only" ]; then
        MODE=MOCK
        say "MOCK MODE: a provider base URL is overridden; nothing from this run is live evidence"
    else
        die "IRIS_OPENAI_BASE_URL or IRIS_GEMINI_BASE_URL is set, so this would not be a live verification; unset it"
    fi
fi

ROOT=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "$BIN" ]; then
    if [ -x "$ROOT/target/release/iris" ]; then
        BIN=$ROOT/target/release/iris
    elif command -v iris >/dev/null 2>&1; then
        BIN=$(command -v iris)
    else
        die "no iris binary: build one (cargo build --release) or pass --bin PATH"
    fi
fi
[ -x "$BIN" ] || die "not an executable: $BIN"

mkdir -p "$DIR"
DIR=$(cd "$DIR" && pwd)
WORK=$DIR/work
EVID=$DIR/evidence
RAW=$WORK/raw
STATE=$WORK/state
CONFIG=$WORK/config.toml
mkdir -p "$WORK" "$EVID" "$RAW" "$STATE"
chmod 700 "$WORK"
# An empty config file: a personal config (output_dir, base_url, models) must not
# change what is verified.
[ -e "$CONFIG" ] || : >"$CONFIG"

# Run the iris binary with an isolated configuration and state directory. Keys are
# inherited from the environment, never passed as arguments.
iris() {
    env -u IRIS_OUTPUT_DIR -u IRIS_IMAGE_PROVIDER -u IRIS_WAIT_TIMEOUT -u IRIS_POLL_INTERVAL \
        -u IRIS_STORE_PROMPTS -u IRIS_LOG -u GOOGLE_API_KEY \
        IRIS_CONFIG="$CONFIG" IRIS_STATE_DIR="$STATE" "$BIN" "$@"
}

VERSION=$("$BIN" --version 2>/dev/null) || die "$BIN --version failed"
say "binary: $BIN ($VERSION)"
say "mode: $MODE; directory: $DIR"

# ----- helpers ---------------------------------------------------------------------------

require_key() {
    local var=$1
    if [ -z "${!var:-}" ]; then
        die "$var is not set (checked for presence only)"
    fi
}

# True if FILE contains the value of OPENAI_API_KEY or GEMINI_API_KEY. The value
# reaches grep through a pipe (printf is a builtin), never through argv.
leaks_secret() {
    local file=$1 var val
    for var in OPENAI_API_KEY GEMINI_API_KEY; do
        val=${!var:-}
        [ "${#val}" -ge 8 ] || continue
        if grep -qF -f <(printf '%s\n' "$val") -- "$file"; then
            return 0
        fi
    done
    return 1
}

# Copy SRC to DST with local paths shortened ($DIR → <dir>, $HOME → ~), refusing
# (and deleting SRC) if it contains a credential value.
sanitize() {
    local src=$1 dst=$2 text
    if leaks_secret "$src"; then
        rm -f "$src" "$dst"
        die "refusing to keep $src: it contains a credential value (an Iris bug; report it)"
    fi
    text=$(cat "$src")
    text=${text//"$DIR"/<dir>}
    if [ -n "${HOME:-}" ]; then
        text=${text//"$HOME"/"~"}
    fi
    printf '%s\n' "$text" >"$dst"
}

now() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

# ledger STEP STATUS ESTIMATE EXIT NOTE — print and append one ledger line.
ledger() {
    local line
    line="at=$(now) step=$1 status=$2 estimated_usd=$3 exit=$4 mode=$MODE note=\"$5\""
    printf '%s\n' "$line" >>"$EVID/ledger.txt"
    printf 'LEDGER %s\n' "$line"
}

header() {
    printf '\n=== step %s: %s ===\n' "$1" "$2"
}

# True if the saved evidence of NAME is a successful envelope.
done_ok() {
    [ -s "$RAW/$1.json" ] && jq -e '.ok == true' "$RAW/$1.json" >/dev/null 2>&1
}

# run_json NAME ARGS... — run `iris ARGS... --json`, keep the raw envelope and
# stderr under work/raw, sanitized copies under evidence/, exit code in LAST_EXIT.
LAST_EXIT=0
run_json() {
    local name=$1
    shift
    say "running: iris $(printf '%q ' "$@")--json"
    set +e
    iris "$@" --json >"$RAW/$name.json" 2>"$RAW/$name.stderr"
    LAST_EXIT=$?
    set -e
    sanitize "$RAW/$name.json" "$EVID/$name.json"
    sanitize "$RAW/$name.stderr" "$EVID/$name.stderr.txt"
    say "exit $LAST_EXIT; $(jq -r 'if .ok then "ok" else "error \(.error.code): \(.error.message)" end' "$RAW/$name.json" 2>/dev/null || echo 'stdout is not JSON')"
}

# estimate ARGS... — free dry run of the generation command; prints Iris's cost
# estimate and sets ESTIMATE. Refuses (exit) when there is no estimate or it is
# above the per-step cap.
ESTIMATE=0
estimate() {
    local out amount basis code
    set +e
    out=$(iris "$@" --dry-run --json 2>/dev/null)
    code=$?
    set -e
    if [ "$code" != 0 ]; then
        die "the dry run failed (exit $code, nothing was sent): $(printf '%s' "$out" | jq -r '.error.message' 2>/dev/null || echo "$out")"
    fi
    amount=$(printf '%s' "$out" | jq -r '.result.cost_estimate.amount // empty')
    basis=$(printf '%s' "$out" | jq -r '.result.cost_estimate.basis // empty')
    [ -n "$amount" ] || die "Iris has no cost estimate for this request; refusing a request whose cost cannot be bounded"
    awk -v a="$amount" -v c="$MAX_STEP_USD" 'BEGIN { exit !(a <= c) }' ||
        die "estimated \$$amount exceeds the per-step cap \$$MAX_STEP_USD (IRIS_LIVE_MAX_STEP_USD); nothing was sent"
    say "ESTIMATED COST: \$$amount; $basis"
    ESTIMATE=$amount
}

# First 12 bytes of FILE as lowercase hex.
magic() {
    od -An -tx1 -N12 "$1" | tr -d ' \n'
}

file_size() {
    wc -c <"$1" | tr -d ' '
}

# check_images JSON JQPATH — every artifact exists with the reported size, was
# decoded by Iris (width/height), and starts with its type's magic bytes.
check_images() {
    local json=$1 path=$2 n=0 p mt bytes w h m
    while IFS=$'\t' read -r p mt bytes w h; do
        n=$((n + 1))
        [ -f "$p" ] || die "missing artifact $p"
        [ "$(file_size "$p")" = "$bytes" ] || die "$p: size differs from the reported $bytes bytes"
        if [ -z "$w" ] || [ -z "$h" ] || [ "$w" -le 0 ] || [ "$h" -le 0 ]; then
            die "$p: Iris reported no decoded dimensions"
        fi
        m=$(magic "$p")
        case $mt in
            image/png) [ "${m:0:16}" = 89504e470d0a1a0a ] || die "$p: not a PNG" ;;
            image/jpeg) [ "${m:0:6}" = ffd8ff ] || die "$p: not a JPEG" ;;
            image/webp) [ "${m:0:8}${m:16:8}" = 5249464657454250 ] || die "$p: not a WebP" ;;
            *) die "$p: unexpected media type $mt" ;;
        esac
        say "image ok: $mt ${w}x${h}, $bytes bytes, decoded by iris: ${p/#"$DIR"/<dir>}"
    done < <(jq -r "${path}[] | [.path, .media_type, (.bytes|tostring), (.width // \"\"|tostring), (.height // \"\"|tostring)] | @tsv" "$json")
    [ "$n" -gt 0 ] || die "no image artifacts in $json"
}

# check_video JSON JQPATH — MP4 with `ftyp` first and a `moov` box, size as reported.
check_video() {
    local json=$1 path=$2 n=0 p mt bytes dur m
    while IFS=$'\t' read -r p mt bytes dur; do
        n=$((n + 1))
        [ -f "$p" ] || die "missing artifact $p"
        [ "$mt" = video/mp4 ] || die "$p: unexpected media type $mt"
        [ "$(file_size "$p")" = "$bytes" ] || die "$p: size differs from the reported $bytes bytes"
        m=$(magic "$p")
        [ "${m:8:8}" = 66747970 ] || die "$p: no ISO-BMFF ftyp box at the start"
        LC_ALL=C grep -qa moov "$p" || die "$p: no moov box"
        say "video ok: $mt, $bytes bytes, duration ${dur:-unknown} s: ${p/#"$DIR"/<dir>}"
    done < <(jq -r "${path}[] | [.path, .media_type, (.bytes|tostring), (.duration_seconds // \"\"|tostring)] | @tsv" "$json")
    [ "$n" -gt 0 ] || die "no video artifacts in $json"
}

# A 1x1 PNG, used only as the input of the free --plan dry runs of steps 3a/3b
# before steps 1/2 have produced real inputs.
placeholder_png() {
    local p=$WORK/placeholder-1x1.png
    if [ ! -s "$p" ]; then
        printf '\211\120\116\107\015\012\032\012\000\000\000\015\111\110\104\122\000\000\000\001\000\000\000\001\010\002\000\000\000\220\167\123\336\000\000\000\014\111\104\101\124\170\234\143\370\317\300\000\000\003\001\001\000\311\376\222\357\000\000\000\000\111\105\116\104\256\102\140\202' >"$p"
    fi
    printf '%s\n' "$p"
}

# The first artifact path of a successful step (for reuse as an edit input).
artifact_of() {
    done_ok "$1" || die "step '$1' has not succeeded yet; run it first"
    jq -r '.result.artifacts[0].path' "$RAW/$1.json"
}

# paid_image_step NAME TITLE KEYVAR ARGS... — the shared flow of steps 1–3.
paid_image_step() {
    local name=$1 title=$2 keyvar=$3
    shift 3
    header "${name#step}" "$title"
    if [ "$PLAN" = 1 ]; then
        estimate "$@"
        return 0
    fi
    if done_ok "$name"; then
        say "already succeeded (evidence/$name.json); not repeating a paid request (delete $RAW/$name.json to rerun)"
        return 0
    fi
    require_key "$keyvar"
    estimate "$@"
    run_json "$name" "$@"
    if [ "$LAST_EXIT" != 0 ]; then
        ledger "${name#step}" failed "$ESTIMATE" "$LAST_EXIT" "$title"
        die "step ${name#step} failed; see $EVID/$name.json"
    fi
    check_images "$RAW/$name.json" .result.artifacts
    local post
    post=$(jq -r '.result.cost_estimate.amount // "n/a"' "$RAW/$name.json")
    ledger "${name#step}" ok "$ESTIMATE" 0 "$title; post-call estimate \$$post"
}

# ----- steps -----------------------------------------------------------------------------

step1() {
    paid_image_step step1 "OpenAI image generation" OPENAI_API_KEY \
        image generate "$PROMPT_IMAGE" --provider openai -m "$OPENAI_MODEL" \
        --size 1024x1024 --quality low -d "$WORK/step1"
}

step2() {
    paid_image_step step2 "Gemini image generation" GEMINI_API_KEY \
        image generate "$PROMPT_IMAGE" --provider gemini -m "$GEMINI_IMAGE_MODEL" \
        --resolution 512 --aspect-ratio 1:1 -d "$WORK/step2"
}

step3a() {
    local input
    if [ "$PLAN" = 1 ] && ! done_ok step1; then input=$(placeholder_png); else input=$(artifact_of step1); fi
    paid_image_step step3a "OpenAI edit reusing the step 1 image" OPENAI_API_KEY \
        image edit "$PROMPT_EDIT" --provider openai -m "$OPENAI_MODEL" -i "$input" \
        --size 1024x1024 --quality low -d "$WORK/step3a"
}

step3b() {
    local input
    if [ "$PLAN" = 1 ] && ! done_ok step2; then input=$(placeholder_png); else input=$(artifact_of step2); fi
    paid_image_step step3b "Gemini edit reusing the step 2 image" GEMINI_API_KEY \
        image edit "$PROMPT_EDIT" --provider gemini -m "$GEMINI_IMAGE_MODEL" -i "$input" \
        --resolution 512 --aspect-ratio 1:1 -d "$WORK/step3b"
}

veo_args() {
    printf '%s\n' video generate "$PROMPT_VIDEO" -m "$VEO_MODEL" --duration 4 --resolution 720p \
        --aspect-ratio 16:9 -d "$WORK/video"
}

job_id() {
    [ -s "$WORK/veo-job-id" ] || die "no Veo job id yet (run step 4 first)"
    cat "$WORK/veo-job-id"
}

step4() {
    local args=() line jobs
    while IFS= read -r line; do args+=("$line"); done < <(veo_args)
    header 4 "Veo submission without waiting (--detach)"
    if [ "$PLAN" = 1 ]; then
        estimate "${args[@]}"
        return 0
    fi
    if [ -e "$WORK/veo-submitted" ]; then
        if [ -s "$WORK/veo-job-id" ]; then
            say "the Veo job $(job_id) was already submitted from $DIR; SPEC §8 allows one submission, not submitting again"
            return 0
        fi
        die "a Veo submission was already attempted from $DIR ($WORK/veo-submitted) without a recorded job id; \
check usage in Google AI Studio before anything else; this script will not submit again"
    fi
    require_key GEMINI_API_KEY
    jobs=$(iris jobs list --json | jq '.result.jobs | length')
    [ "$jobs" = 0 ] || die "the state directory already holds $jobs job(s); refusing a second Veo submission"
    estimate "${args[@]}"
    # The marker comes BEFORE the request: even if this process dies mid-submit, a
    # rerun will not submit a second paid job.
    now >"$WORK/veo-submitted"
    run_json step4 "${args[@]}" --detach
    case $LAST_EXIT in
        0)
            jq -r '.result.job.job_id' "$RAW/step4.json" >"$WORK/veo-job-id"
            say "job $(job_id) is $(jq -r '.result.job.status' "$RAW/step4.json"); remote operation recorded"
            ledger 4 ok "$ESTIMATE" 0 "Veo submission (detached), job $(job_id)"
            ;;
        5)
            ledger 4 uncertain "$ESTIMATE" 5 "submission_uncertain: may have been accepted and billed"
            die "the outcome of the Veo submission is UNCERTAIN; check usage in Google AI Studio; do NOT resubmit"
            ;;
        *)
            local status
            status=$(jq -r '.error.job_status // "none"' "$RAW/step4.json" 2>/dev/null || echo unknown)
            ledger 4 failed "$ESTIMATE" "$LAST_EXIT" "Veo submission rejected (job status $status)"
            if [ "$LAST_EXIT" = 2 ] || [ "$LAST_EXIT" = 3 ] || [ "$status" = failed ]; then
                # A definite rejection: nothing was accepted, so a later retry is allowed.
                rm -f "$WORK/veo-submitted"
            fi
            die "step 4 failed; see $EVID/step4.json"
            ;;
    esac
}

step5() {
    header 5 "resume the job from a separate invocation (jobs status)"
    [ "$PLAN" = 0 ] || { say "free (a status read)"; return 0; }
    require_key GEMINI_API_KEY
    local id status
    id=$(job_id)
    run_json step5 jobs status "$id"
    status=$(jq -r '.result.job.status // empty' "$RAW/step5.json")
    if [ "$LAST_EXIT" = 0 ] && { [ "$status" = running ] || [ "$status" = succeeded ]; }; then
        ledger 5 ok 0 0 "jobs status from a new process: $status"
    else
        ledger 5 failed 0 "$LAST_EXIT" "jobs status: ${status:-error}"
        die "step 5 failed; see $EVID/step5.json"
    fi
}

step6() {
    header 6 "download without resubmission (jobs wait)"
    [ "$PLAN" = 0 ] || { say "free (polls and a download)"; return 0; }
    require_key GEMINI_API_KEY
    local id
    id=$(job_id)
    if done_ok step6; then
        say "already downloaded (evidence/step6.json)"
        return 0
    fi
    run_json step6 jobs wait "$id" --timeout "$WAIT_LIMIT"
    case $LAST_EXIT in
        0)
            check_video "$RAW/step6.json" .result.job.artifacts
            ledger 6 ok 0 0 "jobs wait downloaded the video; no resubmission"
            ;;
        4)
            ledger 6 pending 0 4 "still running after $WAIT_LIMIT; the job continues remotely"
            die "the job has not finished yet; run --step 6 again later (free; nothing is resubmitted)"
            ;;
        *)
            ledger 6 failed 0 "$LAST_EXIT" "jobs wait: $(jq -r '.error.code // "error"' "$RAW/step6.json" 2>/dev/null || echo error)"
            die "step 6 failed; see $EVID/step6.json (a download can be retried with --step 6 or 7; nothing is resubmitted)"
            ;;
    esac
}

step7() {
    header 7 "safe repeat retrieval (jobs download, twice)"
    [ "$PLAN" = 0 ] || { say "free (no network for an intact local file)"; return 0; }
    require_key GEMINI_API_KEY
    local id first repeat copy jobs
    id=$(job_id)
    done_ok step6 || die "step 6 has not downloaded the video yet"
    first=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step6.json")
    run_json step7 jobs download "$id"
    [ "$LAST_EXIT" = 0 ] || { ledger 7 failed 0 "$LAST_EXIT" "repeat jobs download"; die "step 7 failed; see $EVID/step7.json"; }
    jq -e '[.warnings[].code] | index("already_downloaded")' "$RAW/step7.json" >/dev/null ||
        die "the repeat download did not report already_downloaded"
    repeat=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step7.json")
    run_json step7-copy jobs download "$id" -d "$WORK/video-copy"
    [ "$LAST_EXIT" = 0 ] || { ledger 7 failed 0 "$LAST_EXIT" "copy jobs download"; die "step 7 failed; see $EVID/step7-copy.json"; }
    check_video "$RAW/step7-copy.json" .result.job.artifacts
    copy=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step7-copy.json")
    if [ "$first" != "$repeat" ] || [ "$first" != "$copy" ]; then
        die "sha256 differs between retrievals"
    fi
    jobs=$(iris jobs list --json | jq '.result.jobs | length')
    [ "$jobs" = 1 ] || die "expected exactly one job record, found $jobs"
    ledger 7 ok 0 0 "repeat download reported already_downloaded; copy matched sha256; one job"
}

step8() {
    header 8 "JSON mode and media checks over the saved outputs (no network)"
    [ "$PLAN" = 0 ] || { say "free (reads saved outputs)"; return 0; }
    local f ok=0 total=0
    for f in "$RAW"/*.json; do
        [ -e "$f" ] || continue
        total=$((total + 1))
        jq -e '.schema_version == 1 and (has("ok") and has("result") and has("error") and has("warnings"))' "$f" >/dev/null ||
            die "$f is not a v1 Iris envelope"
        if jq -e '.ok == true' "$f" >/dev/null; then ok=$((ok + 1)); fi
    done
    [ "$ok" -ge 1 ] || die "no successful JSON-mode command saved yet"
    for f in step1 step2 step3a step3b; do
        if done_ok "$f"; then check_images "$RAW/$f.json" .result.artifacts; fi
    done
    if done_ok step6; then check_video "$RAW/step6.json" .result.job.artifacts; fi
    ledger 8 ok 0 0 "$ok of $total saved JSON envelopes are successes (schema_version 1)"
}

run_step() {
    case $1 in
        1) step1 ;;
        2) step2 ;;
        3) step3a; step3b ;;
        3a) step3a ;;
        3b) step3b ;;
        4) step4 ;;
        5) step5 ;;
        6) step6 ;;
        7) step7 ;;
        8) step8 ;;
    esac
}

if [ "$STEP" = all ]; then
    for s in 1 2 3a 3b 4 5 6 7 8; do
        run_step "$s"
    done
else
    run_step "$STEP"
fi
if [ "$PLAN" = 1 ]; then
    say "plan only: nothing was sent"
else
    say "done; ledger: $EVID/ledger.txt"
fi
