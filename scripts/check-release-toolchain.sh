#!/bin/sh
# scripts/check-release-toolchain.sh — log the Rust toolchain a release job
# runs with, and fail unless it is the pinned release toolchain.
#
# Usage:
#   scripts/check-release-toolchain.sh <rust-version>     e.g. 1.94.1
#
# The release workflow passes its RELEASE_RUST_TOOLCHAIN. The full `rustc -Vv`
# and `cargo -V` output goes to the job log, recording exactly which compiler
# built the archives.

set -eu

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <rust-version>" >&2
    exit 1
fi
want="$1"

rustc -Vv
cargo -V

got=$(rustc -V)
case $got in
    "rustc $want "*) ;;
    *)
        echo "check-release-toolchain: '$got' is not the pinned release toolchain $want" >&2
        exit 1
        ;;
esac
