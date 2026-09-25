#!/bin/sh
# scripts/release-notes.sh — print one version's CHANGELOG.md section, the body
# of its GitHub release.
#
# Usage:
#   scripts/release-notes.sh <version> [changelog]
#
# <version> is X.Y.Z (optionally with a -prerelease suffix), without the "v".
# [changelog] defaults to CHANGELOG.md at the repo root.
#
# Prints the lines under the version's Keep a Changelog heading, "## [X.Y.Z]"
# (usually followed by " - YYYY-MM-DD"), up to the next "## " heading or the
# link reference definitions at the end of the file, without leading or
# trailing blank lines. Fails with a message on stderr, printing nothing, when
# the heading is missing or its section is empty, so that a release is never
# published without its notes.

set -eu

usage() {
    echo "usage: $0 <version> [changelog]" >&2
    exit 1
}

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    usage
fi
version="$1"
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
changelog="${2:-$repo_root/CHANGELOG.md}"

# Only characters a version can hold, so the value is safe as an awk string.
if ! printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'; then
    echo "release-notes: '$version' is not a version (expected X.Y.Z, without the v)" >&2
    exit 1
fi
if [ ! -f "$changelog" ]; then
    echo "release-notes: $changelog not found" >&2
    exit 1
fi

notes=$(awk -v heading="## [$version]" '
    # The first heading for this version, alone or followed by " - date".
    !found && (($0 == heading) || (index($0, heading " ") == 1)) { found = 1; inside = 1; next }
    inside && (/^## / || /^\[[^]]*\]:[[:space:]]/) { inside = 0 }
    inside {
        # Hold blank lines back until more text follows, which drops the
        # trailing ones; leading ones are dropped until the first text.
        if ($0 ~ /^[[:space:]]*$/) { if (seen) held = held $0 "\n"; next }
        printf "%s%s\n", held, $0
        held = ""
        seen = 1
    }
    END { if (!found) exit 3 }
' "$changelog") || {
    echo "release-notes: $changelog has no \"## [$version]\" heading; add the release's section (CONTRIBUTING.md, Releasing) before tagging v$version" >&2
    exit 1
}
if [ -z "$notes" ]; then
    echo "release-notes: the \"## [$version]\" section of $changelog is empty" >&2
    exit 1
fi
printf '%s\n' "$notes"
