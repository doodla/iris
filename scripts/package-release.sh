#!/bin/sh
# scripts/package-release.sh — build the release archive (docs/install.md) for one target.
#
# Usage:
#   scripts/package-release.sh <target-triple> [output-dir]
#
# Reads the already-built binary from
# "${CARGO_TARGET_DIR:-target}/<target-triple>/release/iris" — build it first,
# e.g.:
#   cargo build --release --locked --target x86_64-unknown-linux-musl
#
# and packages it into <output-dir>/iris-vX.Y.Z-<target-triple>.tar.gz (default
# output-dir: "dist", relative to the repo root): a single top-level directory
# containing the binary, LICENSE, README.md, CHANGELOG.md and the docs/
# directory that README.md links to. Nothing else: no links or special files,
# no absolute or ".." entries (the installer checks this too; see
# docs/install.md).
#
# The archive is reproducible: the same inputs give the same bytes on the same
# kind of host. Entry order, owner, group, permissions and timestamps are fixed
# (every mtime is SOURCE_DATE_EPOCH, by default the commit time of HEAD), and
# the gzip header carries no file name or timestamp. GNU tar is used when it is
# installed (as tar, or as gtar on macOS); otherwise BSD tar, with the same
# normalization done through its options and by touching the staged files.
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
if [ ! -d docs ]; then
    missing="$missing docs/"
fi
if [ -n "$missing" ]; then
    echo "package-release: required file(s) missing from repo root:$missing" >&2
    exit 1
fi

src_date="${SOURCE_DATE_EPOCH:-$(git -C "$repo_root" log -1 --format=%ct 2>/dev/null || date +%s)}"
case $src_date in
    '' | *[!0-9]*)
        echo "package-release: SOURCE_DATE_EPOCH must be a whole number of seconds, got '$src_date'" >&2
        exit 1
        ;;
esac

archive_name="iris-v${version}-${target}"

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/iris-package.XXXXXX")
cleanup() {
    rm -rf "$work_dir"
}
trap cleanup EXIT INT TERM

stage_dir="$work_dir/$archive_name"
mkdir "$stage_dir"
cp "$bin_path" "$stage_dir/iris"
cp LICENSE README.md CHANGELOG.md "$stage_dir/"
# -P copies a link as a link, so the check below refuses it instead of
# packaging whatever it points to.
cp -RP docs "$stage_dir/docs"

# Only regular files and directories are published. Staging copied nothing
# else, so anything else here is a link or special file in the source tree.
odd=$(find "$stage_dir" ! -type f ! -type d)
if [ -n "$odd" ]; then
    echo "package-release: refusing to package links or special files:" >&2
    echo "$odd" >&2
    exit 1
fi

# Permissions must not depend on the umask or on modes in the checkout.
find "$stage_dir" -type d -exec chmod 755 {} +
find "$stage_dir" -type f -exec chmod 644 {} +
chmod 755 "$stage_dir/iris"

# Every entry in one fixed, byte-wise sorted order, archived without recursion
# so that the order never depends on the file system.
(cd "$work_dir" && find "$archive_name" -print | LC_ALL=C sort) >"$work_dir/entries"

gnu_tar=""
for candidate in tar gtar; do
    if "$candidate" --version 2>/dev/null | grep -q 'GNU tar'; then
        gnu_tar=$candidate
        break
    fi
done

if [ -n "$gnu_tar" ]; then
    (cd "$work_dir" && "$gnu_tar" --create --format=gnu --no-recursion \
        --owner=0 --group=0 --numeric-owner --mtime="@${src_date}" \
        --file "$work_dir/archive.tar" --files-from "$work_dir/entries")
else
    # BSD tar has no --mtime, so every staged entry gets the same timestamp
    # first (`date -r SECONDS` is the BSD spelling, `date -d @SECONDS` the GNU
    # one). The ustar format records nothing beyond what is fixed here (no
    # extended attributes), and COPYFILE_DISABLE stops macOS from adding
    # AppleDouble "._" entries.
    stamp=$(TZ=UTC0 date -r "$src_date" '+%Y%m%d%H%M.%S' 2>/dev/null ||
        TZ=UTC0 date -d "@$src_date" '+%Y%m%d%H%M.%S')
    find "$stage_dir" -exec env TZ=UTC0 touch -t "$stamp" {} +
    (cd "$work_dir" && COPYFILE_DISABLE=1 tar --create --format=ustar --no-recursion \
        --uid 0 --gid 0 --uname '' --gname '' \
        --file "$work_dir/archive.tar" -T "$work_dir/entries")
fi
# -n: no file name or timestamp in the gzip header.
gzip -n -c "$work_dir/archive.tar" >"$work_dir/archive.tar.gz"

# Self-check before anything reaches output-dir: the archive must list exactly
# the intended entries under one top-level directory, nothing more. Directory
# names are compared without the trailing slash that tar may print.
listing=$(tar --list --file "$work_dir/archive.tar.gz" | sed 's:/*$::' | LC_ALL=C sort)
expected=$({
    printf '%s\n' "$archive_name" "$archive_name/iris" "$archive_name/LICENSE" \
        "$archive_name/README.md" "$archive_name/CHANGELOG.md"
    find docs -print | sed "s|^|$archive_name/|"
} | LC_ALL=C sort)
if [ "$listing" != "$expected" ]; then
    echo "package-release: unexpected archive contents for ${archive_name}.tar.gz" >&2
    echo "--- got ---" >&2
    echo "$listing" >&2
    echo "--- expected ---" >&2
    echo "$expected" >&2
    exit 1
fi

mkdir -p "$out_dir"
# Resolve out_dir to an absolute path, so a relative output-dir is taken
# relative to the repo root, where this script runs.
out_dir=$(CDPATH='' cd -- "$out_dir" && pwd)
archive_path="$out_dir/${archive_name}.tar.gz"
mv -f "$work_dir/archive.tar.gz" "$archive_path"

echo "$archive_path"
