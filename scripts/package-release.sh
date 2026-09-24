#!/bin/sh
# scripts/package-release.sh — build a C-07-shaped release archive for one target.
#
# Usage:
#   scripts/package-release.sh <target-triple> [output-dir]
#
# Reads the already-built binary from
# "${CARGO_TARGET_DIR:-target}/<target-triple>/release/iris" — build it first,
# e.g.:
#   cargo build --release --locked --target x86_64-unknown-linux-musl
#
# and packages it into <output-dir>/iris-vX.Y.Z-<target-triple>.tar.gz
# (default output-dir: "dist"), with the exact contract layout (see
# .iris-work/contracts/C-07-release-installer.md): a single top-level
# directory containing exactly the binary, LICENSE, README.md and
# CHANGELOG.md, no other paths, no symlinks, no absolute or ".." entries.
#
# Prints the produced archive's path to stdout on success. Run from anywhere;
# it resolves the repo root from its own location.

set -eu

usage() {
    echo "usage: $0 <target-triple> [output-dir]" >&2
    exit 1
}

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    usage
fi
target="$1"
out_dir="${2:-dist}"

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

manifest="Cargo.toml"
if [ ! -f "$manifest" ]; then
    echo "package-release: $manifest not found in $repo_root" >&2
    exit 1
fi

# Same dependency-free version extraction as check-tag-version.sh (see that
# script for why this avoids `cargo pkgid`/`cargo metadata`).
version=$(awk '
    /^\[package\]/ { in_pkg = 1; next }
    /^\[/ { in_pkg = 0 }
    in_pkg && /^[[:space:]]*version[[:space:]]*=/ {
        line = $0
        sub(/^[^"]*"/, "", line)
        sub(/".*$/, "", line)
        print line
        exit
    }
' "$manifest")
if [ -z "$version" ]; then
    echo "package-release: could not find package.version in $manifest" >&2
    exit 1
fi

bin_dir="${CARGO_TARGET_DIR:-target}/$target/release"
bin_path="$bin_dir/iris"
if [ ! -f "$bin_path" ]; then
    echo "package-release: built binary not found: $bin_path" >&2
    echo "package-release: build it first, e.g.: cargo build --release --locked --target $target" >&2
    exit 1
fi
if [ ! -x "$bin_path" ]; then
    echo "package-release: built binary is not executable: $bin_path" >&2
    exit 1
fi

missing=""
for f in LICENSE README.md CHANGELOG.md; do
    if [ ! -f "$f" ]; then
        missing="$missing $f"
    fi
done
if [ -n "$missing" ]; then
    echo "package-release: required file(s) missing from repo root:$missing" >&2
    exit 1
fi

archive_name="iris-v${version}-${target}"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/iris-package.XXXXXX")
cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT INT TERM

stage_dir="$work_dir/$archive_name"
mkdir -p "$stage_dir"
cp "$bin_path" "$stage_dir/iris"
chmod 755 "$stage_dir/iris"
cp LICENSE "$stage_dir/LICENSE"
cp README.md "$stage_dir/README.md"
cp CHANGELOG.md "$stage_dir/CHANGELOG.md"
chmod 644 "$stage_dir/LICENSE" "$stage_dir/README.md" "$stage_dir/CHANGELOG.md"

# Refuse to publish a symlink under the staged tree — the contract requires
# none, and staging is entirely files this script just copied, so any
# symlink here means a source file itself was a symlink.
if find "$stage_dir" -type l | grep -q .; then
    echo "package-release: refusing to package a symlink under $stage_dir" >&2
    exit 1
fi

mkdir -p "$out_dir"
# Resolve out_dir to an absolute path before leaving $repo_root's context in
# the tar invocation, so a relative --output-dir given by the caller still
# lands where they expect.
out_dir=$(CDPATH='' cd -- "$out_dir" && pwd)
archive_path="$out_dir/${archive_name}.tar.gz"

if tar --version 2>/dev/null | grep -q GNU; then
    # GNU tar: pin owner/group/mtime for a reproducible archive.
    src_date="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct 2>/dev/null || date +%s)}"
    tar --create --gzip --file "$archive_path" \
        --sort=name --owner=0 --group=0 --numeric-owner \
        --mtime="@${src_date}" \
        -C "$work_dir" "$archive_name"
else
    # BSD/macOS tar has no --sort/--mtime/--owner; the archive is still
    # correct, just not necessarily byte-for-byte reproducible.
    COPYFILE_DISABLE=1 tar --create --gzip --file "$archive_path" -C "$work_dir" "$archive_name"
fi

# Self-check: the archive must list exactly the four intended entries under
# one top-level directory, nothing more.
listing=$(tar --list --file "$archive_path" | LC_ALL=C sort)
expected=$(printf '%s\n' \
    "$archive_name/" \
    "$archive_name/CHANGELOG.md" \
    "$archive_name/LICENSE" \
    "$archive_name/README.md" \
    "$archive_name/iris" | LC_ALL=C sort)
if [ "$listing" != "$expected" ]; then
    echo "package-release: unexpected archive contents in $archive_path" >&2
    echo "--- got ---" >&2
    echo "$listing" >&2
    echo "--- expected ---" >&2
    echo "$expected" >&2
    exit 1
fi

echo "$archive_path"
