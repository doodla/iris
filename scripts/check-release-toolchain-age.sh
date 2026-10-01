#!/bin/sh
# scripts/check-release-toolchain-age.sh — fail when the pinned release
# toolchain is more than a given number of minor versions behind the newest
# stable Rust (docs/contributing/releasing.md#update-the-pinned-tools).
#
# Usage:
#   scripts/check-release-toolchain-age.sh <pinned> <stable> <max-behind>
#       e.g. scripts/check-release-toolchain-age.sh 1.94.1 1.98.1 3
#
# <pinned> is RELEASE_RUST_TOOLCHAIN from .github/workflows/release.yml,
# <stable> the newest stable Rust version (the second word of `rustc -V` on
# the stable toolchain), and <max-behind> how many minor versions <pinned> may
# trail it. Rust ships a minor version every six weeks, and Dependabot never
# updates the pin, so the weekly Release toolchain workflow runs this check.
#
# Exit codes: 0 within the limit, 1 usage error, 2 too far behind.

set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <pinned> <stable> <max-behind>" >&2
    exit 1
fi
pinned="$1"
stable="$2"
max_behind="$3"

for version in "$pinned" "$stable"; do
    if ! printf '%s\n' "$version" | grep -Eq '^1\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$'; then
        echo "check-release-toolchain-age: '$version' is not a Rust version 1.Y.Z" >&2
        exit 1
    fi
done
case $max_behind in
    '' | *[!0-9]*)
        echo "check-release-toolchain-age: <max-behind> must be a whole number, got '$max_behind'" >&2
        exit 1
        ;;
esac

pinned_minor=$(printf '%s\n' "$pinned" | cut -d. -f2)
stable_minor=$(printf '%s\n' "$stable" | cut -d. -f2)
behind=$((stable_minor - pinned_minor))

if [ "$behind" -gt "$max_behind" ]; then
    echo "check-release-toolchain-age: the release toolchain $pinned is $behind minor versions behind stable \
$stable, more than $max_behind; move releases to a newer Rust version \
(docs/contributing/releasing.md#change-the-release-toolchain)" >&2
    exit 2
fi
echo "check-release-toolchain-age: the release toolchain $pinned is within $max_behind minor versions of stable $stable"
