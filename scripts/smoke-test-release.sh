#!/bin/sh
# scripts/smoke-test-release.sh — check a packaged release archive before it is published.
#
# Usage:
#   scripts/smoke-test-release.sh <version> <target-triple> [dist-dir]
#
# Takes <dist-dir>/iris-v<version>-<target-triple>.tar.gz (default dist-dir:
# "dist", relative to the repo root), as made by scripts/package-release.sh,
# and checks on this machine that:
#   1. the packaged binary runs: `iris --version` prints exactly
#      "iris <version> (<target-triple>)", `iris --json version` and
#      `iris --json models list` each print one JSON document, and
#      `iris schema` succeeds. If EXPECTED_GIT_COMMIT is set, version.git_commit
#      must equal it (the release workflow passes the commit it named to the
#      build through IRIS_GIT_COMMIT);
#   2. install.sh installs the archive: the archive and a SHA256SUMS file are
#      served from 127.0.0.1 by tests/installer/server.py, the installer runs
#      in a clean environment (env -i: no proxies or credentials) into a
#      temporary directory, the installed iris reports the same version, and
#      nothing besides iris was installed.
#
# Needs python3 (for the JSON checks and the local server) and the tools
# install.sh needs. No provider is contacted and no credentials are used.
# Exits non-zero, with a message on stderr, at the first failed check.

set -eu

usage() {
    echo "usage: $0 <version> <target-triple> [dist-dir]" >&2
    exit 1
}

die() {
    echo "smoke-test-release: $*" >&2
    exit 1
}

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    usage
fi
version="$1"
target="$2"
dist_dir="${3:-dist}"
case $version in
    '' | *[!0-9A-Za-z.-]*) die "version '$version' has unexpected characters" ;;
esac
case $target in
    '' | *[!0-9A-Za-z_.-]*) die "target '$target' has unexpected characters" ;;
esac

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

archive_file="iris-v${version}-${target}.tar.gz"
archive="$dist_dir/$archive_file"
[ -f "$archive" ] || die "archive not found: $archive (run scripts/package-release.sh $target first)"
command -v python3 >/dev/null 2>&1 || die "python3 is required"

work=$(mktemp -d "${TMPDIR:-/tmp}/iris-smoke.XXXXXX")
server_pid=""
cleanup() {
    status=$?
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$work"
    exit "$status"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

mkdir "$work/home" "$work/tmp"
expected_version="iris $version ($target)"

# Runs the iris at $1 with the remaining arguments in a clean environment: no
# credentials, no configuration from this machine's HOME.
run_iris() {
    bin=$1
    shift
    env -i PATH="$PATH" HOME="$work/home" TMPDIR="$work/tmp" "$bin" "$@" </dev/null
}

# --- 1. the packaged binary ---------------------------------------------------------
mkdir "$work/extract"
tar -xzf "$archive" -C "$work/extract"
bin="$work/extract/iris-v${version}-${target}/iris"
if [ ! -f "$bin" ] || [ ! -x "$bin" ]; then
    die "extracted binary missing or not executable: $bin"
fi

got=$(run_iris "$bin" --version) || die "packaged iris --version failed"
[ "$got" = "$expected_version" ] || die "packaged iris --version printed '$got', expected '$expected_version'"
echo "packaged binary: $got"

run_iris "$bin" --json version >"$work/version.json" || die "packaged iris --json version failed"
EXPECTED_GIT_COMMIT="${EXPECTED_GIT_COMMIT:-}" python3 -c '
import json, os, sys
with open(sys.argv[1]) as f:
    doc = json.load(f)
commit = doc["result"]["git_commit"]
expected = os.environ["EXPECTED_GIT_COMMIT"]
print("packaged binary: git_commit %s" % json.dumps(commit))
if expected and commit != expected.lower():
    sys.exit("smoke-test-release: git_commit is %r, expected %r" % (commit, expected))
' "$work/version.json"

run_iris "$bin" --json models list >"$work/models.json" || die "packaged iris --json models list failed"
python3 -c 'import json, sys; json.load(open(sys.argv[1]))' "$work/models.json" ||
    die "iris --json models list did not print a single JSON document"
run_iris "$bin" schema >/dev/null || die "packaged iris schema failed"

# --- 2. install.sh with this archive ---------------------------------------------------
# server.py serves <root>/<repo>/download/<tag>/<asset> at /ok/<repo>/download/...,
# the layout of a GitHub releases URL.
tag="v$version"
assets="$work/www/release/download/$tag"
mkdir -p "$assets"
cp "$archive" "$assets/$archive_file"
(
    cd "$assets"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$archive_file"
    else
        shasum -a 256 "$archive_file"
    fi
) >"$assets/SHA256SUMS"

python3 tests/installer/server.py "$work/www" "$work/port" 2>"$work/server.log" &
server_pid=$!
tries=0
while [ ! -s "$work/port" ]; do
    tries=$((tries + 1))
    if [ "$tries" -gt 100 ]; then
        cat "$work/server.log" >&2 || true
        die "the local release server did not start"
    fi
    sleep 0.1
done
base_url="http://127.0.0.1:$(cat "$work/port")/ok/release"

install_dir="$work/bin"
env -i PATH="$PATH" HOME="$work/home" TMPDIR="$work/tmp" IRIS_INSTALL_BASE_URL="$base_url" \
    sh install.sh --version "$tag" --dir "$install_dir" </dev/null ||
    die "install.sh failed to install $archive_file"

got=$(run_iris "$install_dir/iris" --version) || die "installed iris --version failed"
[ "$got" = "$expected_version" ] || die "installed iris --version printed '$got', expected '$expected_version'"
for other in "$install_dir"/* "$install_dir"/.[!.]* "$install_dir"/..?*; do
    if [ "$other" != "$install_dir/iris" ] && { [ -e "$other" ] || [ -L "$other" ]; }; then
        die "install.sh installed something besides iris: $other"
    fi
done
echo "installed binary: $got"
echo "smoke-test-release: $archive_file passed"
