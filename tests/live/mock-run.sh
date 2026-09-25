#!/bin/sh
# tests/live/mock-run.sh - free, offline test of scripts/live-verify.sh.
#
# Usage: sh tests/live/mock-run.sh [IRIS_BINARY]
#   IRIS_BINARY      the iris to drive (default: ${CARGO_TARGET_DIR:-target}/debug/iris;
#                    build it first with `cargo build --locked`)
#   KEEP_TEST_DIR=1  keep the work directory for debugging
#
# Needs bash, jq and python3. No provider is contacted, no key is used, and
# nothing is charged. Every run of live-verify.sh is under env -i with its own
# HOME, so no real key, proxy setting or configuration of this machine reaches
# it, and it checks:
#   1. --plan (live mode, no keys) prints Iris's estimates for the paid steps
#      and sends nothing: every proxy variable points at the local mock, which
#      would record any request that tried to leave.
#   2. A full run in the script's mock mode (IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=
#      mock-only, fake keys, both base URLs on the mock at 127.0.0.1) passes all
#      eight steps, every ledger line says mode=MOCK, and the mock received
#      exactly one OpenAI generation and one edit, two Gemini generateContent
#      calls, one Veo submission, one operation poll and one video download,
#      each with its own provider's credential header and nothing else.
#   3. The same run again passes without a single request: no paid step is
#      sent twice, no job is resubmitted, and the video is not downloaded again.
# It prints one line per check and exits non-zero if any check fails.

set -u

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH='' cd -- "$HERE/../.." && pwd)
SCRIPT=$ROOT/scripts/live-verify.sh

die() {
  printf 'mock-run: %s\n' "$*" >&2
  exit 1
}

BIN=${1:-${CARGO_TARGET_DIR:-$ROOT/target}/debug/iris}
case $BIN in /*) ;; *) BIN=$(pwd)/$BIN ;; esac
[ -x "$BIN" ] || die "no iris binary at $BIN (run cargo build --locked, or pass the binary's path)"
for tool in bash jq python3; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is required"
done

W=$(mktemp -d "${TMPDIR:-/tmp}/iris-live-mock.XXXXXX") || die "mktemp failed"
SERVER_PID=
finish() {
  if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null; fi
  if [ "${KEEP_TEST_DIR:-}" = 1 ]; then echo "kept $W"; else rm -rf "$W"; fi
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

mkdir -p "$W/mock" "$W/home" "$W/tmp"
python3 "$HERE/mock_providers.py" "$W/mock" 2>"$W/mock.log" &
SERVER_PID=$!
tries=0
while [ ! -s "$W/mock/port" ]; do
  tries=$((tries + 1))
  [ "$tries" -le 100 ] || die "the mock did not start; see $W/mock.log"
  sleep 0.1
done
MOCK=http://127.0.0.1:$(cat "$W/mock/port")
REQUESTS=$W/mock/requests.jsonl

FAILED=0
check() {
  if [ "$1" = ok ]; then
    printf 'ok   %s\n' "$2"
  else
    FAILED=$((FAILED + 1))
    printf 'FAIL %s\n' "$2"
  fi
}
# count ROUTE [AUTH]: requests the mock recorded for ROUTE (with AUTH), or all
# requests for "all".
count() {
  if [ "$1" = all ]; then
    jq -s 'length' "$REQUESTS"
  else
    jq -s --arg r "$1" --arg a "${2:-}" '[.[] | select(.route == $r and ($a == "" or .auth == $a))] | length' "$REQUESTS"
  fi
}
expect_count() {
  got=$(count "$1" "${3:-}")
  if [ "$got" = "$2" ]; then check ok "$4 ($got)"; else check fail "$4: $got, expected $2"; fi
}
show() {
  sed 's/^/       | /' "$1"
}

# --- 1. --plan: estimates only, nothing sent ---------------------------------------
# Every proxy variable points at the mock, so a request that tried to leave
# this machine would be recorded there instead.
env -i PATH="$PATH" HOME="$W/home" TMPDIR="$W/tmp" \
  HTTP_PROXY="$MOCK" HTTPS_PROXY="$MOCK" ALL_PROXY="$MOCK" \
  http_proxy="$MOCK" https_proxy="$MOCK" all_proxy="$MOCK" \
  bash "$SCRIPT" --plan --bin "$BIN" --dir "$W/plan" </dev/null >"$W/plan.out" 2>&1
status=$?
if [ "$status" = 0 ]; then check ok "--plan exits 0"; else check fail "--plan exited $status"; show "$W/plan.out"; fi
# A paid step's line is "ESTIMATED COST: $<amount>; <basis>"; a free one's
# says "$0 (free: ...)".
estimates=$(grep -c 'ESTIMATED COST: \$[0-9.]*; ' "$W/plan.out")
if [ "$estimates" = 5 ]; then
  check ok "--plan prints Iris's estimate for each paid step (1, 2, 3a, 3b, 4)"
else
  check fail "--plan printed $estimates paid estimates, expected 5"
  show "$W/plan.out"
fi
if grep -q 'plan only: nothing was sent' "$W/plan.out"; then
  check ok "--plan says nothing was sent"
else
  check fail "--plan does not say nothing was sent"
fi
expect_count all 0 "" "--plan sent no request"

# --- 2. a full mock-mode run ----------------------------------------------------------
# run_mock LOG: all steps against the mock, with fake keys only.
run_mock() {
  env -i PATH="$PATH" HOME="$W/home" TMPDIR="$W/tmp" \
    IRIS_LIVE_ALLOW_BASE_URL_OVERRIDE=mock-only \
    IRIS_OPENAI_BASE_URL="$MOCK/v1" IRIS_GEMINI_BASE_URL="$MOCK" \
    OPENAI_API_KEY=fake-openai-key-for-the-mock GEMINI_API_KEY=fake-gemini-key-for-the-mock \
    IRIS_LIVE_CONFIRM=yes-i-accept-charges IRIS_LIVE_VEO_CONFIRM=submit-one-veo-job \
    IRIS_LIVE_WAIT=2m \
    bash "$SCRIPT" --bin "$BIN" --dir "$W/run" </dev/null >"$1" 2>&1
}

run_mock "$W/run1.out"
status=$?
if [ "$status" = 0 ]; then check ok "mock run exits 0"; else check fail "mock run exited $status"; show "$W/run1.out"; fi
LEDGER=$W/run/evidence/ledger.txt
for step in 1 2 3a 3b 4 5 6 7 8; do
  if grep -q "step=$step status=ok .* mode=MOCK" "$LEDGER" 2>/dev/null; then
    check ok "step $step passed (ledger: status=ok, mode=MOCK)"
  else
    check fail "no 'step=$step status=ok ... mode=MOCK' line in the ledger"
  fi
done
if grep -q 'mode=LIVE' "$LEDGER" 2>/dev/null; then check fail "a ledger line says mode=LIVE"; else check ok "no ledger line says mode=LIVE"; fi
expect_count openai.generations 1 bearer "one OpenAI generation, with a bearer token only"
expect_count openai.edits 1 bearer "one OpenAI edit, with a bearer token only"
expect_count gemini.generateContent 2 x-goog-api-key "two Gemini generateContent calls, with x-goog-api-key only"
expect_count veo.submit 1 x-goog-api-key "one Veo submission"
expect_count veo.poll 1 x-goog-api-key "one Veo operation poll"
expect_count veo.download 1 x-goog-api-key "one video download"
expect_count unknown 0 "" "no request the mock does not know"
expect_count all 7 "" "nothing else was requested"

# --- 3. the same run again: nothing is sent ---------------------------------------------
run_mock "$W/run2.out"
status=$?
if [ "$status" = 0 ]; then check ok "second mock run exits 0"; else check fail "second mock run exited $status"; show "$W/run2.out"; fi
expect_count all 7 "" "the second run sent no request"

if [ "$FAILED" -ne 0 ] && [ -s "$W/mock.log" ]; then
  printf '       mock server log:\n'
  show "$W/mock.log"
fi
printf '\n%s failed\n' "$FAILED"
[ "$FAILED" -eq 0 ]
