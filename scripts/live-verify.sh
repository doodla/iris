#!/usr/bin/env bash
# live-verify.sh — OPT-IN, PAID live verification of Iris against the real OpenAI
# and Gemini APIs, following the steps and budget rules in
# docs/contributing/live-testing.md.
#
# THIS SCRIPT SPENDS MONEY. It is never run by CI or by `cargo test`. It refuses
# to run a step unless IRIS_LIVE_CONFIRM=yes-i-accept-charges is set, and the one
# Veo submission (step 4) also needs IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job.
#
# Documentation: docs/contributing/live-testing.md. Quick reference:
# `scripts/live-verify.sh --help`.
#
# Credentials come only from OPENAI_API_KEY / GEMINI_API_KEY in the environment.
# The script never prints, logs, writes, or passes them as arguments (the scan of
# saved outputs feeds the value to grep through a pipe, not argv). It checks them
# for presence, and in mock mode also that they are fake (a `fake-`/`test-` prefix).

set -euo pipefail

readonly CONFIRM_VALUE="yes-i-accept-charges"
readonly VEO_CONFIRM_VALUE="submit-one-veo-job"
readonly GEMINI_IMAGE_MODEL="gemini-3.1-flash-image"
readonly VEO_MODEL="veo-3.1-lite-generate-preview"
readonly VEO_SECONDS=4
# The accepted duration of the 4 s clip, in milliseconds.
readonly VEO_MIN_MS=3500 VEO_MAX_MS=5000
readonly PROMPT_IMAGE="A small red paper boat on a calm blue pond, simple flat illustration"
readonly PROMPT_EDIT="Add a small yellow sun in the top-right corner and keep everything else unchanged"
readonly PROMPT_VIDEO="A small red paper boat drifting slowly on a calm pond, gentle ripples, static camera"

usage() {
    cat <<'EOF'
Usage:
  IRIS_LIVE_CONFIRM=yes-i-accept-charges scripts/live-verify.sh [--step N] [--dir DIR] [--bin PATH]
  scripts/live-verify.sh --plan [--dir DIR] [--bin PATH]
  scripts/live-verify.sh --help

PAID live verification of Iris through the built binary. Before
each step the script prints its estimated cost. A paid step's estimate is Iris's
own (from a free --dry-run of the same command); the step is refused if there is
no estimate, if it exceeds the per-step cap, or if it would take the directory's
estimated spend over the budget.

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
  8   JSON-mode, image, and MP4 checks over the saved outputs                     free (no network)
Without --step, the steps run in order and stop at the first failure. Step 4 is
then skipped unless IRIS_LIVE_VEO_CONFIRM is set, and steps 5-7 are skipped while
the directory has no Veo job.

Options:
  --step N     run one step: 1, 2, 3, 3a, 3b, 4, 5, 6, 7, 8, or all (default all)
  --dir DIR    working directory; keep it outside any git checkout (default:
               $IRIS_LIVE_DIR, else ${XDG_STATE_HOME:-~/.local/state}/iris-live/run):
                 DIR/work      media, raw outputs, Iris state (IRIS_STATE_DIR), empty config
                 DIR/evidence  sanitized JSON outputs, stderr, ledger.txt (safe to keep)
  --bin PATH   iris binary (default: $IRIS_BIN, else target/release/iris of this
               checkout; an iris found on PATH is never used)
  --plan       print each step's estimate using free dry runs; sends nothing,
               needs no keys and no confirmation
  -h, --help   this text

Environment:
  IRIS_LIVE_CONFIRM=yes-i-accept-charges    required for every step (not for --plan/--help)
  IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job  also required for step 4, the one Veo submission
  OPENAI_API_KEY, GEMINI_API_KEY            checked for presence only (mock mode: must be fake)
  IRIS_LIVE_BUDGET_USD                      estimated budget of DIR (default 10); set it to
                                            what is left of your overall budget
  IRIS_LIVE_MAX_STEP_USD                    per-step estimate cap (default 0.50)
  IRIS_LIVE_OPENAI_MODEL                    OpenAI model (default gpt-image-2.5-sunburst;
                                            gpt-image-2.5-flare costs the same)
  IRIS_LIVE_WAIT                            caller wait limit of step 6 (default 15m)

Safety:
  * One Veo submission per machine. Step 4 needs IRIS_LIVE_VEO_CONFIRM, and it
    writes a marker in DIR and one in ${XDG_STATE_HOME:-~/.local/state}/iris-live/
    BEFORE the request. While either marker exists, no directory submits again.
    Set the confirmation only if no Veo job was submitted for this budget yet, by
    this script or by hand.
  * Every paid request is written to the ledger (status=sent, with its estimate)
    as soon as it returns, before any check. A paid step whose request succeeded
    is never sent again; if its output failed a check, a rerun repeats the checks.
  * IRIS_OPENAI_BASE_URL / IRIS_GEMINI_BASE_URL make a run non-live, so they are
    refused. IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only tests this script against
    local mock servers instead. It requires BOTH base URLs, each an http(s) URL on
    127.0.0.1, localhost, or [::1], and fake keys (prefix fake- or test-). Every
    ledger line then says mode=MOCK, which is never live evidence.
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
DIR=
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
BUDGET_USD=${IRIS_LIVE_BUDGET_USD:-10}
WAIT_LIMIT=${IRIS_LIVE_WAIT:-15m}
for amount in "$MAX_STEP_USD" "$BUDGET_USD"; do
    [[ $amount =~ ^[0-9]+([.][0-9]+)?$ ]] ||
        die "IRIS_LIVE_MAX_STEP_USD and IRIS_LIVE_BUDGET_USD must be plain USD amounts, such as 0.50"
done

command -v jq >/dev/null 2>&1 || die "jq is required (https://jqlang.org)"
command -v od >/dev/null 2>&1 || die "od is required"
command -v awk >/dev/null 2>&1 || die "awk is required"

if [ "$PLAN" = 0 ] && [ "${IRIS_LIVE_CONFIRM:-}" != "$CONFIRM_VALUE" ]; then
    die "this script makes PAID requests. Set IRIS_LIVE_CONFIRM=$CONFIRM_VALUE to accept the charges \
(see --help for the steps and estimates, or --plan for a free estimate-only run)"
fi

# ----- mode: LIVE, or MOCK against local mock servers only -------------------------------

# True if $1 is an http(s) URL whose host is a loopback address.
is_loopback_url() {
    local re='^https?://(127\.0\.0\.1|localhost|\[::1\])(:[0-9]{1,5})?(/[^[:space:]]*)?$'
    [[ $1 =~ $re ]]
}

MODE=LIVE
case ${IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE:-} in
    "") ;;
    mock-only) MODE=MOCK ;;
    *) die "IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE accepts only the value 'mock-only'" ;;
esac
if [ "$MODE" = MOCK ]; then
    # A provider left at its default base URL would be the real, paid API, and a
    # real key would go with it; so both must point at loopback, and keys must be fake.
    for var in IRIS_OPENAI_BASE_URL IRIS_GEMINI_BASE_URL; do
        [ -n "${!var:-}" ] ||
            die "mock mode needs BOTH IRIS_OPENAI_BASE_URL and IRIS_GEMINI_BASE_URL; $var is not set, so that provider would be the real, paid API"
        is_loopback_url "${!var}" ||
            die "mock mode: $var must be an http(s) URL on 127.0.0.1, localhost, or [::1]"
    done
    for var in OPENAI_API_KEY GEMINI_API_KEY; do
        case ${!var:-} in
            "" | fake-* | test-*) ;;
            *) die "mock mode: $var must be unset or a fake key starting with 'fake-' or 'test-' (a real key never goes to a mock; run under env -i)" ;;
        esac
    done
    say "MOCK MODE: both provider base URLs point at local mock servers; nothing from this run is live evidence"
elif [ -n "${IRIS_OPENAI_BASE_URL:-}" ] || [ -n "${IRIS_GEMINI_BASE_URL:-}" ]; then
    die "IRIS_OPENAI_BASE_URL or IRIS_GEMINI_BASE_URL is set, so this would not be a live verification; unset it"
fi

# ----- binary and directories ------------------------------------------------------------

ROOT=$(cd "$(dirname "$0")/.." && pwd)
if [ -z "$BIN" ]; then
    [ -x "$ROOT/target/release/iris" ] ||
        die "no iris binary at $ROOT/target/release/iris: build one (cargo build --release), or pass --bin PATH or set IRIS_BIN (an iris on PATH is never used: it may be a stale build)"
    BIN=$ROOT/target/release/iris
fi
[ -x "$BIN" ] || die "not an executable: $BIN"

STATE_HOME=${XDG_STATE_HOME:-${HOME:+$HOME/.local/state}}
[ -n "$STATE_HOME" ] || die "set HOME or XDG_STATE_HOME: the machine-wide Veo submission marker lives there"
LIVE_HOME=$STATE_HOME/iris-live
if [ "$MODE" = LIVE ]; then
    VEO_GLOBAL=$LIVE_HOME/veo-submitted
else
    VEO_GLOBAL=$LIVE_HOME/veo-submitted.mock
fi
DIR=${DIR:-${IRIS_LIVE_DIR:-$LIVE_HOME/run}}
mkdir -p "$LIVE_HOME" "$DIR"
chmod 700 "$LIVE_HOME"
DIR=$(cd "$DIR" && pwd)
case $DIR/ in
    "$ROOT"/*) say "WARNING: $DIR is inside the checkout $ROOT, where its files are not git-ignored; never commit them (prefer a directory outside the checkout)" ;;
esac
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
    env -u IRIS_OUTPUT_DIR -u IRIS_WAIT_TIMEOUT -u IRIS_POLL_INTERVAL \
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

now() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

# The estimated spend recorded in DIR's ledger: the sum of its estimated_usd fields
# (only `sent` lines, one per paid request, carry spend).
spent_total() {
    local ledger=$EVID/ledger.txt
    if [ ! -s "$ledger" ]; then
        echo 0
        return 0
    fi
    awk '{ for (i = 1; i <= NF; i++) { if ($i ~ /^note=/) break; if ($i ~ /^estimated_usd=/) s += substr($i, 15) } }
         END { printf "%.6g\n", s + 0 }' "$ledger"
}

# ledger STEP STATUS SPEND EXIT NOTE — print and append one ledger line. SPEND is the
# estimated cost the line adds to DIR's running total (total_usd); only `sent`, the
# line of a paid request, adds any.
ledger() {
    local total line
    total=$(awk -v t="$(spent_total)" -v s="$3" 'BEGIN { printf "%.6g", t + s }')
    line="at=$(now) step=$1 status=$2 estimated_usd=$3 total_usd=$total exit=$4 mode=$MODE note=\"$5\""
    printf '%s\n' "$line" >>"$EVID/ledger.txt"
    printf 'LEDGER %s\n' "$line"
}

# sanitize SRC DST STEP — copy SRC to DST with local paths shortened ($DIR → <dir>,
# $HOME → ~). A file that contains a credential value is deleted instead, the leak
# is written to the ledger and marked (so a rerun does not pay for the step again),
# and the run stops.
sanitize() {
    local src=$1 dst=$2 step=$3 text
    if leaks_secret "$src"; then
        rm -f "$src" "$dst"
        now >"$RAW/step$step.leaked"
        ledger "$step" leak 0 "$LAST_EXIT" "${src##*/} contained a credential value and was deleted"
        die "refusing to keep $src: it contained a credential value (an Iris bug; report it)"
    fi
    text=$(cat "$src")
    text=${text//"$DIR"/<dir>}
    if [ -n "${HOME:-}" ]; then
        text=${text//"$HOME"/"~"}
    fi
    printf '%s\n' "$text" >"$dst"
}

header() {
    printf '\n=== step %s: %s ===\n' "$1" "$2"
}

# The cost line of a free step.
free_step() {
    say "ESTIMATED COST: \$0 (free: $1)"
}

# True if NAME's saved envelope is a success.
envelope_ok() {
    [ -s "$RAW/$1.json" ] && jq -e '.ok == true' "$RAW/$1.json" >/dev/null 2>&1
}

# True if NAME succeeded and its output passed the checks.
done_ok() {
    envelope_ok "$1" && [ -e "$RAW/$1.verified" ]
}

# run_raw NAME ARGS... — run `iris ARGS... --json`, keeping stdout and stderr under
# work/raw; the exit code goes to LAST_EXIT. Prints nothing of the output.
LAST_EXIT=0
run_raw() {
    local name=$1
    shift
    say "running: iris $(printf '%q ' "$@")--json"
    set +e
    iris "$@" --json >"$RAW/$name.json" 2>"$RAW/$name.stderr"
    LAST_EXIT=$?
    set -e
}

# keep_evidence NAME STEP — sanitized copies of NAME's output under evidence/ (the
# run stops on a credential value), then a one-line summary.
keep_evidence() {
    local name=$1 step=$2
    sanitize "$RAW/$name.json" "$EVID/$name.json" "$step"
    sanitize "$RAW/$name.stderr" "$EVID/$name.stderr.txt" "$step"
    say "exit $LAST_EXIT; $(jq -r 'if .ok then "ok" else "error \(.error.code): \(.error.message)" end' "$RAW/$name.json" 2>/dev/null || echo 'stdout is not JSON')"
}

# run_json NAME STEP ARGS... — a free command with its evidence.
run_json() {
    local name=$1 step=$2
    shift 2
    run_raw "$name" "$@"
    keep_evidence "$name" "$step"
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

# check_budget STEP — refuse a paid request whose estimate (ESTIMATE) would take DIR's
# estimated spend over IRIS_LIVE_BUDGET_USD. The Veo clip (step 4) may use at most
# half of the remaining budget.
check_budget() {
    local total
    total=$(spent_total)
    awk -v t="$total" -v e="$ESTIMATE" -v b="$BUDGET_USD" 'BEGIN { exit !(t + e <= b) }' ||
        die "the estimate \$$ESTIMATE on top of the \$$total already spent from $DIR exceeds the budget \$$BUDGET_USD (IRIS_LIVE_BUDGET_USD); nothing was sent"
    if [ "$1" = 4 ]; then
        awk -v t="$total" -v e="$ESTIMATE" -v b="$BUDGET_USD" 'BEGIN { exit !(e <= (b - t) / 2) }' ||
            die "the Veo clip's estimate \$$ESTIMATE exceeds half of the remaining budget (\$$BUDGET_USD minus \$$total spent); the live-test budget rules say not to submit; nothing was sent"
    fi
    say "BUDGET: \$$total of \$$BUDGET_USD spent so far (estimated); this step adds about \$$ESTIMATE"
}

# paid_request NAME STEP ARGS... — send one paid request and write it to the ledger
# as `sent` at once, before any check. Its spend is the larger of the pre-call
# estimate (ESTIMATE) and Iris's post-call estimate from the usage the provider
# reported. A marker written before the request makes a rerun refuse to send it
# again if this process dies before the ledger line.
paid_request() {
    local name=$1 step=$2 post spend
    shift 2
    now >"$RAW/$name.sending"
    run_raw "$name" "$@"
    post=$(jq -r '(.result.cost_estimate // .result.job.cost_estimate // {}).amount // empty' "$RAW/$name.json" 2>/dev/null || true)
    spend=$(awk -v a="$ESTIMATE" -v b="${post:-0}" 'BEGIN { print (b > a ? b : a) }')
    ledger "$step" sent "$spend" "$LAST_EXIT" "paid request; pre-call estimate \$$ESTIMATE, post-call estimate \$${post:-n/a}"
    rm -f "$RAW/$name.sending"
    keep_evidence "$name" "$step"
}

# hex_at FILE OFFSET COUNT — COUNT bytes of FILE from OFFSET, as lowercase hex.
hex_at() {
    od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n'
}

file_size() {
    wc -c <"$1" | tr -d ' '
}

# The four-character code of a box type given as hex (for messages).
fourcc() {
    printf '%b' "\\x${1:0:2}\\x${1:2:2}\\x${1:4:2}\\x${1:6:2}" | LC_ALL=C tr -c 'A-Za-z0-9 ' '?'
}

# check_images JSON JQPATH EXPECT — every artifact exists with the reported size, was
# decoded by Iris (width/height), and has the requested dimensions (EXPECT: WxH, or
# `square` for a 1:1 aspect ratio). A PNG must have the right signature, an IHDR
# whose dimensions match Iris's, and IEND last; a JPEG must start with SOI.
check_images() {
    local json=$1 path=$2 expect=$3 n=0 p mt bytes w h m
    while IFS=$'\t' read -r p mt bytes w h; do
        n=$((n + 1))
        [ -f "$p" ] || die "missing artifact $p"
        [ "$(file_size "$p")" = "$bytes" ] || die "$p: size differs from the reported $bytes bytes"
        [[ $w =~ ^[1-9][0-9]*$ && $h =~ ^[1-9][0-9]*$ ]] || die "$p: Iris reported no decoded dimensions"
        case $expect in
            square) [ "$w" = "$h" ] || die "$p is ${w}x${h}, not square (1:1 was requested)" ;;
            *) [ "${w}x${h}" = "$expect" ] || die "$p is ${w}x${h}, but $expect was requested" ;;
        esac
        m=$(hex_at "$p" 0 24)
        [ "${#m}" = 48 ] || die "$p: too short to be an image"
        case $mt in
            image/png)
                [ "${m:0:16}" = 89504e470d0a1a0a ] || die "$p: not a PNG"
                [ "${m:24:8}" = 49484452 ] || die "$p: the PNG does not start with IHDR"
                [ "$((16#${m:32:8}))x$((16#${m:40:8}))" = "${w}x${h}" ] ||
                    die "$p: the PNG header says $((16#${m:32:8}))x$((16#${m:40:8})), Iris reported ${w}x${h}"
                [ "$(hex_at "$p" $((bytes - 12)) 12)" = 0000000049454e44ae426082 ] ||
                    die "$p: the PNG does not end with IEND (truncated?)"
                ;;
            image/jpeg) [ "${m:0:6}" = ffd8ff ] || die "$p: not a JPEG" ;;
            image/webp) [ "${m:0:8}${m:16:8}" = 5249464657454250 ] || die "$p: not a WebP" ;;
            *) die "$p: unexpected media type $mt" ;;
        esac
        say "image ok: $mt ${w}x${h} (requested $expect), $bytes bytes, decoded by iris: ${p/#"$DIR"/<dir>}"
    done < <(jq -r "${path}[] | [.path, .media_type, (.bytes|tostring), (.width // \"\"|tostring), (.height // \"\"|tostring)] | @tsv" "$json")
    [ "$n" -gt 0 ] || die "no image artifacts in $json"
}

# The requested dimensions of a paid image step, for check_images.
expect_of() {
    case $1 in
        step1 | step3a) echo 1024x1024 ;;
        *) echo square ;;
    esac
}

# mp4_box FILE OFFSET END — parse the ISO-BMFF box header at OFFSET; sets BOX_TYPE
# (hex), BOX_HDR (header bytes), and BOX_SIZE. Fails unless the box fits by END.
mp4_box() {
    local f=$1 off=$2 end=$3 hex
    [ $((end - off)) -ge 8 ] || return 1
    hex=$(hex_at "$f" "$off" 16)
    [ "${#hex}" -ge 16 ] || return 1
    BOX_TYPE=${hex:8:8}
    BOX_SIZE=$((16#${hex:0:8}))
    BOX_HDR=8
    if [ "$BOX_SIZE" = 1 ]; then
        [ "${#hex}" = 32 ] || return 1
        BOX_SIZE=$((16#${hex:16:16}))
        BOX_HDR=16
    elif [ "$BOX_SIZE" = 0 ]; then
        BOX_SIZE=$((end - off))
    fi
    [ "$BOX_SIZE" -ge "$BOX_HDR" ] && [ $((off + BOX_SIZE)) -le "$end" ]
}

# mp4_walk FILE START END — the boxes that exactly tile START..END, one
# "TYPE OFFSET HEADER SIZE" line each; fails on an inconsistent box.
mp4_walk() {
    local f=$1 off=$2 end=$3 count=0
    while [ "$off" -lt "$end" ]; do
        count=$((count + 1))
        [ "$count" -le 10000 ] || return 1
        mp4_box "$f" "$off" "$end" || return 1
        printf '%s %s %s %s\n' "$BOX_TYPE" "$off" "$BOX_HDR" "$BOX_SIZE"
        off=$((off + BOX_SIZE))
    done
}

# mvhd_ms FILE OFFSET SIZE — the movie duration in milliseconds, from the payload of
# an mvhd box (version 0 or 1) at OFFSET of SIZE bytes.
mvhd_ms() {
    local f=$1 off=$2 size=$3 hex scale dur
    hex=$(hex_at "$f" "$off" 32)
    case ${hex:0:2} in
        00)
            [ "$size" -ge 20 ] && [ "${#hex}" -ge 40 ] || return 1
            scale=$((16#${hex:24:8}))
            dur=$((16#${hex:32:8}))
            ;;
        01)
            [ "$size" -ge 32 ] && [ "${#hex}" = 64 ] || return 1
            scale=$((16#${hex:40:8}))
            dur=$((16#${hex:48:16}))
            ;;
        *) return 1 ;;
    esac
    [ "$scale" -gt 0 ] && [ "$dur" -ge 0 ] || return 1
    echo $((dur * 1000 / scale))
}

# check_video JSON JQPATH — an MP4 of the reported size whose top-level boxes tile
# the file, starting with `ftyp` and including `moov`; the movie header (mvhd)
# inside moov and Iris's reported duration both give the requested 4 s.
check_video() {
    local json=$1 path=$2 n=0 p mt bytes dur boxes moov children mvhd moff mhdr msize voff vhdr vsize ms types
    while IFS=$'\t' read -r p mt bytes dur; do
        n=$((n + 1))
        [ -f "$p" ] || die "missing artifact $p"
        [ "$mt" = video/mp4 ] || die "$p: unexpected media type $mt"
        [ "$(file_size "$p")" = "$bytes" ] || die "$p: size differs from the reported $bytes bytes"
        boxes=$(mp4_walk "$p" 0 "$bytes") || die "$p: the top-level MP4 boxes are inconsistent (truncated, or not ISO-BMFF)"
        [ "${boxes%% *}" = 66747970 ] || die "$p: the first box is not ftyp"
        moov=$(printf '%s\n' "$boxes" | awk '$1 == "6d6f6f76" { print $2, $3, $4; exit }')
        [ -n "$moov" ] || die "$p: no top-level moov box"
        read -r moff mhdr msize <<<"$moov"
        children=$(mp4_walk "$p" $((moff + mhdr)) $((moff + msize))) || die "$p: the moov box is inconsistent"
        mvhd=$(printf '%s\n' "$children" | awk '$1 == "6d766864" { print $2, $3, $4; exit }')
        [ -n "$mvhd" ] || die "$p: no mvhd box in moov"
        read -r voff vhdr vsize <<<"$mvhd"
        ms=$(mvhd_ms "$p" $((voff + vhdr)) $((vsize - vhdr))) || die "$p: unreadable mvhd box"
        ((ms >= VEO_MIN_MS && ms <= VEO_MAX_MS)) ||
            die "$p: the MP4 header gives $ms ms; a $VEO_SECONDS s clip was requested"
        [ -n "$dur" ] || die "$p: Iris reported no duration"
        awk -v d="$dur" -v lo="$VEO_MIN_MS" -v hi="$VEO_MAX_MS" 'BEGIN { exit !(d * 1000 >= lo && d * 1000 <= hi) }' ||
            die "$p: Iris reported $dur s; a $VEO_SECONDS s clip was requested"
        types=$(printf '%s\n' "$boxes" | while read -r t _; do printf '%s ' "$(fourcc "$t")"; done)
        say "video ok: $mt, $bytes bytes, boxes ${types% }, mvhd $ms ms, iris $dur s: ${p/#"$DIR"/<dir>}"
    done < <(jq -r "${path}[] | [.path, .media_type, (.bytes|tostring), (.duration_seconds // \"\"|tostring)] | @tsv" "$json")
    [ "$n" -gt 0 ] || die "no video artifacts in $json"
}

# verify_images NAME STEP TITLE — check the saved images of a successful paid step:
# ledger `ok` and mark it verified, or ledger `verify-failed` and stop (a rerun then
# repeats the checks, never the paid request).
verify_images() {
    local name=$1 step=$2 title=$3
    if ! (check_images "$RAW/$name.json" .result.artifacts "$(expect_of "$name")"); then
        ledger "$step" verify-failed 0 0 "$title: the paid request succeeded, but its output failed the checks"
        die "step $step: the output failed verification; the paid request is not repeated (run --step $step again to re-check, or delete $RAW/$name.json to pay for a new request)"
    fi
    now >"$RAW/$name.verified"
    ledger "$step" ok 0 0 "$title: output verified"
}

# verify_video NAME STEP — check a downloaded video; mark NAME verified, or ledger
# `verify-failed` and stop.
verify_video() {
    local name=$1 step=$2
    if ! (check_video "$RAW/$name.json" .result.job.artifacts); then
        ledger "$step" verify-failed 0 0 "the video of $name failed the checks"
        die "step $step: the video failed verification"
    fi
    now >"$RAW/$name.verified"
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

# The first artifact path of a successful, verified step (for reuse as an edit input).
artifact_of() {
    done_ok "$1" || die "step '$1' has not succeeded and been verified yet; run it first"
    jq -r '.result.artifacts[0].path' "$RAW/$1.json"
}

# paid_image_step NAME TITLE KEYVAR ARGS... — the shared flow of steps 1–3.
paid_image_step() {
    local name=$1 title=$2 keyvar=$3
    shift 3
    local step=${name#step}
    header "$step" "$title"
    if [ "$PLAN" = 1 ]; then
        estimate "$@"
        return 0
    fi
    if done_ok "$name"; then
        say "ESTIMATED COST: \$0 (already sent and verified: evidence/$name.json; a paid request is never repeated; delete $RAW/$name.json to pay for a new one)"
        return 0
    fi
    if envelope_ok "$name"; then
        say "ESTIMATED COST: \$0 (the paid request succeeded earlier but its output is not verified; checking it again without sending anything)"
        verify_images "$name" "$step" "$title"
        return 0
    fi
    [ ! -e "$RAW/$name.sending" ] ||
        die "an earlier run of step $step stopped while its paid request was in flight, so it may have been charged without a ledger line; check the provider's usage page, then delete $RAW/$name.sending to send it again"
    [ ! -e "$RAW/$name.leaked" ] ||
        die "an earlier run of step $step found a credential value in Iris's output (see the ledger); report the Iris bug, then delete $RAW/$name.leaked to send the paid request again"
    require_key "$keyvar"
    estimate "$@"
    check_budget "$step"
    paid_request "$name" "$step" "$@"
    if [ "$LAST_EXIT" != 0 ]; then
        ledger "$step" failed 0 "$LAST_EXIT" "$title: $(jq -r '.error.code // "error"' "$RAW/$name.json" 2>/dev/null || echo error)"
        die "step $step failed; see $EVID/$name.json"
    fi
    verify_images "$name" "$step" "$title"
}

# ----- steps -----------------------------------------------------------------------------

step1() {
    paid_image_step step1 "OpenAI image generation" OPENAI_API_KEY \
        image generate "$PROMPT_IMAGE" -m "$OPENAI_MODEL" \
        --size 1024x1024 --quality low -d "$WORK/step1"
}

step2() {
    paid_image_step step2 "Gemini image generation" GEMINI_API_KEY \
        image generate "$PROMPT_IMAGE" -m "$GEMINI_IMAGE_MODEL" \
        --resolution 512 --aspect-ratio 1:1 -d "$WORK/step2"
}

step3a() {
    local input
    if [ "$PLAN" = 1 ] && ! done_ok step1; then input=$(placeholder_png); else input=$(artifact_of step1); fi
    paid_image_step step3a "OpenAI edit reusing the step 1 image" OPENAI_API_KEY \
        image edit "$PROMPT_EDIT" -m "$OPENAI_MODEL" -i "$input" \
        --size 1024x1024 --quality low -d "$WORK/step3a"
}

step3b() {
    local input
    if [ "$PLAN" = 1 ] && ! done_ok step2; then input=$(placeholder_png); else input=$(artifact_of step2); fi
    paid_image_step step3b "Gemini edit reusing the step 2 image" GEMINI_API_KEY \
        image edit "$PROMPT_EDIT" -m "$GEMINI_IMAGE_MODEL" -i "$input" \
        --resolution 512 --aspect-ratio 1:1 -d "$WORK/step3b"
}

veo_args() {
    printf '%s\n' video generate "$PROMPT_VIDEO" -m "$VEO_MODEL" --duration "$VEO_SECONDS" --resolution 720p \
        --aspect-ratio 16:9 -d "$WORK/video"
}

job_id() {
    [ -s "$WORK/veo-job-id" ] || die "no Veo job id in $DIR yet (run step 4 first)"
    cat "$WORK/veo-job-id"
}

# Steps 5-7 need step 4's job. In a full run, they are skipped without one (step 4
# was skipped); a single step without one is an error.
need_job() {
    [ ! -s "$WORK/veo-job-id" ] || return 0
    if [ "$STEP" = all ]; then
        say "skipped: $DIR has no Veo job (step 4 did not submit one)"
        return 1
    fi
    die "no Veo job id in $DIR yet (run step 4 first)"
}

step4() {
    local args=() line jobs status
    while IFS= read -r line; do args+=("$line"); done < <(veo_args)
    header 4 "Veo submission without waiting (--detach)"
    if [ "$PLAN" = 1 ]; then
        estimate "${args[@]}"
        say "submitting also needs IRIS_LIVE_VEO_CONFIRM=$VEO_CONFIRM_VALUE"
        if [ -e "$VEO_GLOBAL" ]; then
            say "note: a Veo job was already submitted from this machine ($VEO_GLOBAL); step 4 will refuse"
        fi
        return 0
    fi
    if [ -e "$WORK/veo-submitted" ]; then
        if [ -s "$WORK/veo-job-id" ]; then
            say "ESTIMATED COST: \$0 (the Veo job $(job_id) was already submitted from $DIR; the live-test budget allows one Veo submission, so nothing is sent)"
            return 0
        fi
        die "a Veo submission was already attempted from $DIR ($WORK/veo-submitted) without a recorded job id; \
check usage in Google AI Studio before anything else; this script will not submit again"
    fi
    if [ "${IRIS_LIVE_VEO_CONFIRM:-}" != "$VEO_CONFIRM_VALUE" ]; then
        if [ "$STEP" = all ]; then
            say "skipped: the one Veo submission also needs IRIS_LIVE_VEO_CONFIRM=$VEO_CONFIRM_VALUE"
            return 0
        fi
        die "step 4 submits a paid Veo job, and the live-test budget allows ONE per budget. Set IRIS_LIVE_VEO_CONFIRM=$VEO_CONFIRM_VALUE \
only if no Veo job was submitted yet, by this script or by hand"
    fi
    if [ -e "$VEO_GLOBAL" ]; then
        die "a Veo job was already submitted from this machine ($(head -n 1 "$VEO_GLOBAL")); not submitting another. \
Delete $VEO_GLOBAL only when a new budget allows another submission"
    fi
    require_key GEMINI_API_KEY
    jobs=$(iris jobs list --json | jq '.result.jobs | length')
    [ "$jobs" = 0 ] || die "the state directory already holds $jobs job(s); refusing a second Veo submission"
    estimate "${args[@]}"
    check_budget 4
    # Both markers come BEFORE the request and are created atomically (noclobber):
    # even if this process dies mid-submit, or another run races this one, no second
    # paid job is submitted from this machine.
    (set -o noclobber && printf 'at=%s dir=%s\n' "$(now)" "$DIR" >"$VEO_GLOBAL") 2>/dev/null ||
        die "a Veo job was already submitted from this machine ($VEO_GLOBAL); not submitting another"
    (set -o noclobber && now >"$WORK/veo-submitted") 2>/dev/null ||
        die "another run is submitting from $DIR; not submitting again"
    paid_request step4 4 "${args[@]}" --detach
    case $LAST_EXIT in
        0)
            jq -r '.result.job.job_id' "$RAW/step4.json" >"$WORK/veo-job-id"
            printf 'job=%s\n' "$(job_id)" >>"$VEO_GLOBAL"
            say "job $(job_id) is $(jq -r '.result.job.status' "$RAW/step4.json"); remote operation recorded"
            ledger 4 ok 0 0 "Veo submission accepted (detached), job $(job_id)"
            ;;
        5)
            ledger 4 uncertain 0 5 "submission_uncertain: may have been accepted and billed"
            die "the outcome of the Veo submission is UNCERTAIN; check usage in Google AI Studio; do NOT resubmit"
            ;;
        *)
            status=$(jq -r '.error.job_status // "none"' "$RAW/step4.json" 2>/dev/null || echo unknown)
            ledger 4 failed 0 "$LAST_EXIT" "Veo submission rejected (job status $status)"
            if [ "$LAST_EXIT" = 2 ] || [ "$LAST_EXIT" = 3 ] || [ "$status" = failed ]; then
                # A definite rejection: nothing was accepted, so a later retry is allowed.
                rm -f "$WORK/veo-submitted" "$VEO_GLOBAL"
            fi
            die "step 4 failed; see $EVID/step4.json"
            ;;
    esac
}

step5() {
    header 5 "resume the job from a separate invocation (jobs status)"
    free_step "one status read"
    [ "$PLAN" = 0 ] || return 0
    need_job || return 0
    require_key GEMINI_API_KEY
    local id status
    id=$(job_id)
    run_json step5 5 jobs status "$id"
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
    free_step "status polls and one download; nothing is resubmitted"
    [ "$PLAN" = 0 ] || return 0
    need_job || return 0
    require_key GEMINI_API_KEY
    local id
    id=$(job_id)
    if done_ok step6; then
        say "already downloaded and verified (evidence/step6.json)"
        return 0
    fi
    if envelope_ok step6; then
        say "downloaded earlier but not verified; checking it again without any request"
        LAST_EXIT=0
    else
        run_json step6 6 jobs wait "$id" --timeout "$WAIT_LIMIT"
    fi
    case $LAST_EXIT in
        0)
            verify_video step6 6
            ledger 6 ok 0 0 "jobs wait downloaded the video and it passed the checks; no resubmission"
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
    free_step "no network while the local file is intact"
    [ "$PLAN" = 0 ] || return 0
    need_job || return 0
    require_key GEMINI_API_KEY
    local id first repeat copy jobs
    id=$(job_id)
    done_ok step6 || die "step 6 has not downloaded and verified the video yet"
    first=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step6.json")
    run_json step7 7 jobs download "$id"
    [ "$LAST_EXIT" = 0 ] || { ledger 7 failed 0 "$LAST_EXIT" "repeat jobs download"; die "step 7 failed; see $EVID/step7.json"; }
    jq -e '[.warnings[].code] | index("already_downloaded")' "$RAW/step7.json" >/dev/null ||
        { ledger 7 failed 0 0 "the repeat download did not report already_downloaded"; die "the repeat download did not report already_downloaded"; }
    repeat=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step7.json")
    run_json step7-copy 7 jobs download "$id" -d "$WORK/video-copy"
    [ "$LAST_EXIT" = 0 ] || { ledger 7 failed 0 "$LAST_EXIT" "copy jobs download"; die "step 7 failed; see $EVID/step7-copy.json"; }
    verify_video step7-copy 7
    copy=$(jq -r '.result.job.artifacts[0].sha256' "$RAW/step7-copy.json")
    if [ "$first" != "$repeat" ] || [ "$first" != "$copy" ]; then
        ledger 7 failed 0 0 "sha256 differs between retrievals"
        die "sha256 differs between retrievals"
    fi
    jobs=$(iris jobs list --json | jq '.result.jobs | length')
    [ "$jobs" = 1 ] || { ledger 7 failed 0 0 "$jobs job records"; die "expected exactly one job record, found $jobs"; }
    ledger 7 ok 0 0 "repeat download reported already_downloaded; copy matched sha256; one job"
}

step8() {
    header 8 "JSON mode and media checks over the saved outputs (no network)"
    free_step "reads the saved outputs"
    [ "$PLAN" = 0 ] || return 0
    local f ok=0 total=0
    for f in "$RAW"/*.json; do
        [ -e "$f" ] || continue
        total=$((total + 1))
        if ! jq -e '.schema_version == 1 and (has("ok") and has("result") and has("error") and has("warnings"))' "$f" >/dev/null 2>&1; then
            ledger 8 failed 0 1 "${f##*/} is not a v1 Iris envelope"
            die "$f is not a v1 Iris envelope"
        fi
        if jq -e '.ok == true' "$f" >/dev/null; then ok=$((ok + 1)); fi
    done
    if [ "$ok" -lt 1 ]; then
        ledger 8 failed 0 1 "no successful JSON-mode command saved yet"
        die "no successful JSON-mode command saved yet"
    fi
    if ! (
        for f in step1 step2 step3a step3b; do
            if envelope_ok "$f"; then check_images "$RAW/$f.json" .result.artifacts "$(expect_of "$f")"; fi
        done
        if envelope_ok step6; then check_video "$RAW/step6.json" .result.job.artifacts; fi
    ); then
        ledger 8 failed 0 1 "a saved output failed the checks"
        die "step 8 failed: a saved output failed the checks"
    fi
    ledger 8 ok 0 0 "$ok of $total saved JSON envelopes are successes (schema_version 1); saved media re-checked"
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

if [ "$PLAN" = 0 ]; then
    say "estimated spend recorded in $DIR so far: \$$(spent_total) of the \$$BUDGET_USD budget (IRIS_LIVE_BUDGET_USD)"
fi
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
    say "done; estimated spend recorded in $DIR: \$$(spent_total); ledger: $EVID/ledger.txt"
fi
