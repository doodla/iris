#!/bin/sh
# scripts/check.sh - run the checks that CI runs, locally, and summarize them.
#
# Usage: sh scripts/check.sh [--fix] [--msrv]
#   --fix    first format the code and regenerate the generated files (the JSON
#            Schema and the CLI reference), then check. Review `git diff`
#            afterwards: a changed schema is a change to the JSON contract.
#   --msrv   also check and test with the minimum Rust version (rust-version
#            in Cargo.toml), when rustup has that toolchain.
#
# The checks, in order: cargo fmt, Clippy with warnings denied, the test suite,
# cargo deny, ShellCheck, the installer's offline tests, and the offline test of
# scripts/live-verify.sh. These are the checks of .github/workflows/ci.yml,
# except the release dry run and the package check. A check whose tool isn't
# installed is skipped, and the summary says how to get it. Every check runs,
# even after one fails, and the script ends with one line per check. It exits 1
# if any check failed, and 0 otherwise.
#
# Like the test suite, it needs no API key and costs nothing: it unsets
# OPENAI_API_KEY and GEMINI_API_KEY before it starts.

set -u

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT" || exit 1
# Every check runs in the checkout the script belongs to; a copy elsewhere
# would check whatever is around it.
if ! grep -q '^name = "iris"$' Cargo.toml 2>/dev/null; then
    echo "check.sh: $ROOT is not an Iris checkout; run the scripts/check.sh of the checkout to check" >&2
    exit 2
fi

usage="usage: sh scripts/check.sh [--fix] [--msrv]"
fix=0
msrv=0
for arg in "$@"; do
    case $arg in
        --fix) fix=1 ;;
        --msrv) msrv=1 ;;
        -h | --help)
            echo "$usage"
            exit 0
            ;;
        *)
            echo "check.sh: unknown argument '$arg' ($usage)" >&2
            exit 2
            ;;
    esac
done

# Never let a real key reach a check.
unset OPENAI_API_KEY GEMINI_API_KEY

summary=""
failed=0
skipped=0

record() {
    summary="$summary  $1
"
}

# run NAME COMMAND [ARG...]: run one check and record whether it passed.
run() {
    name=$1
    shift
    printf '\n== %s: %s\n' "$name" "$*"
    if "$@"; then
        record "ok       $name"
    else
        record "FAILED   $name"
        failed=$((failed + 1))
    fi
}

# skip NAME WHY: record a check that couldn't run.
skip() {
    printf '\n== %s: skipped (%s)\n' "$1" "$2"
    record "skipped  $1 ($2)"
    skipped=$((skipped + 1))
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# Write the schema to a temporary file first, so a failed build never
# truncates the committed one.
regenerate_schema() {
    tmp=$(mktemp) || return 1
    if cargo run -q --locked -- schema >"$tmp"; then
        mv "$tmp" schema/iris-output.v1.schema.json
    else
        rm -f "$tmp"
        return 1
    fi
}

if [ "$fix" -eq 1 ]; then
    run "format" cargo fmt --all
    run "regenerate the JSON Schema" regenerate_schema
    run "regenerate the CLI reference" env IRIS_UPDATE_DOCS=1 cargo test -q --locked --test cli_reference
fi

run "fmt" cargo fmt --all --check
run "clippy" cargo clippy --all-targets --locked -- -D warnings
run "test" cargo test --locked --no-fail-fast

if cargo deny --version >/dev/null 2>&1; then
    run "deny" cargo deny check
else
    skip "deny" "install it with: cargo install --locked cargo-deny"
fi

if have shellcheck; then
    run "shellcheck" sh -c 'shellcheck -s sh install.sh && shellcheck scripts/*.sh tests/installer/*.sh tests/live/*.sh'
else
    skip "shellcheck" "install ShellCheck: https://github.com/koalaman/shellcheck#installing"
fi

run "installer" sh tests/installer/run.sh

if have bash && have jq && have python3; then
    iris=${CARGO_TARGET_DIR:-target}/debug/iris
    run "live-verify.sh (offline mock)" sh -c "cargo build -q --locked && sh tests/live/mock-run.sh '$iris'"
else
    skip "live-verify.sh (offline mock)" "it needs bash, jq, and python3"
fi

if [ "$msrv" -eq 1 ]; then
    version=$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)
    if rustup run "$version" cargo -V >/dev/null 2>&1; then
        run "msrv ($version)" sh -c "cargo +$version check --locked --all-targets && cargo +$version test --locked --no-fail-fast"
    else
        skip "msrv ($version)" "install the toolchain with: rustup toolchain install $version"
    fi
fi

printf '\ncheck.sh summary:\n%s' "$summary"
if [ "$failed" -gt 0 ]; then
    echo "check.sh: $failed check(s) failed; their output is under the == line with their name"
    exit 1
fi
if [ "$fix" -eq 1 ]; then
    echo "check.sh: review the changes with git diff, the regenerated files above all"
fi
if [ "$skipped" -gt 0 ]; then
    echo "check.sh: no check failed; $skipped skipped, which CI runs"
else
    echo "check.sh: every check passed"
fi
