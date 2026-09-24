#!/bin/sh
# check-tag-version.sh — verify a release tag matches Cargo.toml's package version.
#
# Usage:
#   scripts/check-tag-version.sh <tag>     # e.g. v1.2.3 — fails unless tag == "v" + package.version
#   scripts/check-tag-version.sh           # dry run: no tag to check against (e.g. workflow_dispatch
#                                           # without a pushed tag); just resolves and prints the
#                                           # version that a real release tag would need to match.
#
# On success, prints the resolved package version (without the "v" prefix) to
# stdout. Run from the repository root (needs Cargo.toml in the cwd).
#
# Exit codes: 0 success, 1 usage/parse error, 2 tag/version mismatch.

set -eu

usage() {
    echo "usage: $0 [<tag>]" >&2
    exit 1
}

if [ "$#" -gt 1 ]; then
    usage
fi
tag="${1:-}"

manifest="Cargo.toml"
if [ ! -f "$manifest" ]; then
    echo "check-tag-version: $manifest not found in $(pwd)" >&2
    exit 1
fi

# Extract package.version from Cargo.toml without a TOML parser: find the
# [package] table, then the first `version = "..."` line inside it (stopping
# at the next `[...]` table header). This intentionally does not use `cargo
# pkgid`/`cargo metadata`, so it works even when Cargo.lock is stale or a
# toolchain isn't set up yet (this script is meant to run as the very first
# step of the release workflow).
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
    echo "check-tag-version: could not find package.version in $manifest" >&2
    exit 1
fi

# A shell case glob here (`[0-9]*.[0-9]*.[0-9]*`) would accept far more than
# semver: in a glob, `*` matches any string, so e.g. `1.2.3$(echo pwned)`
# matches it too. release.yml publishes this value as the verify job's output
# and passes it to later steps through `env:` variables, where it becomes part
# of archive names, the tag comparison, and the release title. Require
# strictly `X.Y.Z` with an optional `-prerelease` suffix instead, anchored at
# both ends, so nothing else can reach a file name, URL, or command line.
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    echo "check-tag-version: package.version '$version' does not look like semver" >&2
    exit 1
fi

if [ -z "$tag" ]; then
    echo "check-tag-version: dry run, no tag given; Cargo.toml version is $version (expected tag: v$version)" >&2
    echo "$version"
    exit 0
fi

expected="v$version"
if [ "$tag" != "$expected" ]; then
    echo "check-tag-version: tag '$tag' does not match Cargo.toml version '$version' (expected '$expected')" >&2
    exit 2
fi

echo "check-tag-version: tag '$tag' matches Cargo.toml version" >&2
echo "$version"
