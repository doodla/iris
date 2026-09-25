#!/bin/sh
# install.sh - install the iris command-line tool from a GitHub release.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
#   curl -fsSL https://raw.githubusercontent.com/doodla/iris/v0.1.0/install.sh | sh -s -- --version v0.1.0
#   sh install.sh [--version VERSION] [--dir DIR]
#
# Options (a flag wins over its environment variable):
#   --version VERSION  vX.Y.Z, X.Y.Z or latest (default: latest)   env IRIS_VERSION
#   --dir DIR          install directory (default: ~/.local/bin)    env IRIS_INSTALL_DIR
#   -h, --help         print this help and exit
#
# Test-only override, not for normal use:
#   IRIS_INSTALL_BASE_URL  replaces https://github.com/doodla/iris/releases, so
#                          <base>/latest and <base>/download/<tag>/<asset> are used.
#
# What it does, in order:
#   1. Detects the platform: Linux x86_64, macOS x86_64 or macOS arm64 (arm64 is
#      preferred on Apple silicon even under Rosetta). Anything else fails.
#   2. Needs curl (or wget), tar, and sha256sum (or shasum).
#   3. Resolves "latest" by following <base>/latest to .../tag/<tag> (no API).
#   4. Downloads SHA256SUMS and iris-<tag>-<target>.tar.gz into a private
#      temporary directory, removed on exit or interruption.
#   5. Verifies the archive's SHA-256 against its line in SHA256SUMS.
#   6. Accepts only the release layout: regular files iris, LICENSE,
#      THIRD-PARTY-LICENSES, README.md and CHANGELOG.md, and a docs/ directory
#      of regular files, in the single directory iris-<tag>-<target>/.
#      Absolute paths, "..", links, other entries, or a missing iris are
#      rejected.
#   7. Extracts it and checks that `iris --version` runs on this machine.
#   8. Copies iris, and only iris, into DIR under a temporary name, then
#      renames it over any existing iris (atomic on one filesystem). DIR is
#      created if needed; sudo is never used.
#   9. Prints the installed version, and the line to add to your shell
#      startup file if DIR is not on PATH.
# It never reads standard input, so piping it into sh is safe. On any failure
# nothing is installed, an existing iris is left untouched, and it exits 1 with
# a message on stderr.
#
# The checksum proves the archive matches the release's SHA256SUMS. Both come
# from the same release, so this guards against corrupted or truncated
# downloads, not against a compromised release.

set -u

# Linux release target: musl gives a static binary that runs on any x86_64
# Linux kernel; kept in one variable here in case that ever needs to change.
LINUX_X86_64_TARGET=x86_64-unknown-linux-musl
DEFAULT_BASE_URL=https://github.com/doodla/iris/releases
NL='
'

tmp_dir=
staged_file=

say() { printf 'iris-install: %s\n' "$*"; }
warn() { printf 'iris-install: warning: %s\n' "$*" >&2; }
die() {
  printf 'iris-install: error: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Install iris from a GitHub release.' \
    '' \
    'Usage: install.sh [--version VERSION] [--dir DIR]' \
    '  curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh -s -- [OPTIONS]' \
    '' \
    'Options (a flag wins over its environment variable):' \
    '  --version VERSION  vX.Y.Z, X.Y.Z or latest (default: latest)   env IRIS_VERSION' \
    '  --dir DIR          install directory (default: ~/.local/bin)    env IRIS_INSTALL_DIR' \
    '  -h, --help         print this help and exit' \
    '' \
    'The archive is verified against the release SHA256SUMS before anything is' \
    'installed. sudo is never used; upgrade by running the installer again.'
}

cleanup() {
  if [ -n "$staged_file" ]; then rm -f "$staged_file"; fi
  if [ -n "$tmp_dir" ]; then rm -rf "$tmp_dir"; fi
}

parse_args() {
  version=${IRIS_VERSION:-latest}
  install_dir=${IRIS_INSTALL_DIR:-}
  while [ $# -gt 0 ]; do
    case $1 in
      --version | --dir)
        [ $# -ge 2 ] || die "$1 needs a value (see --help)"
        set_option "$1" "$2"
        shift 2
        ;;
      --version=* | --dir=*)
        set_option "${1%%=*}" "${1#*=}"
        shift
        ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) die "unknown option: $1 (see --help)" ;;
    esac
  done
  if [ -z "$install_dir" ]; then
    if [ -z "${HOME:-}" ]; then die "HOME is not set; choose a directory with --dir"; fi
    install_dir=$HOME/.local/bin
  fi
  base_url=${IRIS_INSTALL_BASE_URL:-$DEFAULT_BASE_URL}
  base_url=${base_url%/}
}

set_option() {
  [ -n "$2" ] || die "$1 needs a value (see --help)"
  case $1 in
    --version) version=$2 ;;
    --dir) install_dir=$2 ;;
  esac
}

have() { command -v "$1" >/dev/null 2>&1; }

# Sets os and target, or fails naming what was detected.
detect_target() {
  os=$(uname -s)
  arch=$(uname -m)
  target=
  case $os in
    Linux)
      if [ "$arch" = x86_64 ]; then target=$LINUX_X86_64_TARGET; fi
      ;;
    Darwin)
      # Under Rosetta, uname -m says x86_64 on Apple silicon; use native arm64.
      # sysctl is in /usr/sbin, which a minimal PATH may leave out.
      sysctl_cmd=sysctl
      have sysctl || sysctl_cmd=/usr/sbin/sysctl
      if [ "$arch" = x86_64 ] && [ "$("$sysctl_cmd" -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
        arch=arm64
      fi
      case $arch in
        x86_64) target=x86_64-apple-darwin ;;
        arm64 | aarch64) target=aarch64-apple-darwin ;;
      esac
      ;;
  esac
  if [ -z "$target" ]; then
    die "unsupported platform: $os $arch (release builds exist for Linux x86_64, macOS x86_64 and macOS arm64; elsewhere, build from source: https://github.com/doodla/iris)"
  fi
}

# Sets fetcher and hasher, or fails naming the missing tool.
check_tools() {
  if have curl; then
    fetcher=curl
  elif have wget; then
    fetcher=wget
  else
    die "missing required tool: curl or wget"
  fi
  if have sha256sum; then
    hasher=sha256sum
  elif have shasum; then
    hasher=shasum
  else
    die "missing required tool: sha256sum or shasum"
  fi
  for tool in tar mktemp mkdir cp chmod mv rm; do
    have "$tool" || die "missing required tool: $tool"
  done
}

# Fails unless $1 looks like a release tag (vX.Y.Z, optionally -rc.N etc.).
check_tag() {
  case $1 in
    v[0-9]*.[0-9]*.[0-9]*) ;;
    *) die "$2 is not a release version (expected vX.Y.Z, X.Y.Z or latest)" ;;
  esac
  case $1 in
    *[!0-9A-Za-z.-]*) die "$2 contains unexpected characters" ;;
  esac
}

# Sets tag from $version, unless it is "latest" (resolved later).
normalize_version() {
  tag=
  case $version in
    latest) return 0 ;;
    v*) tag=$version ;;
    *) tag=v$version ;;
  esac
  check_tag "$tag" "version '$version'"
}

make_tmp_dir() {
  tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/iris-install.XXXXXXXX") ||
    die "could not create a temporary directory"
}

# http_get URL FILE: fetch URL (following redirects) into FILE and set
# http_status and final_url. Returns 1 if no complete HTTP response arrived:
# no response at all, or a transfer cut off after a successful status.
# A stalled connection fails after about a minute instead of hanging: curl
# gives up below 1 KB/s for 60 s, wget after 60 s without data, twice.
http_get() {
  if [ "$fetcher" = curl ]; then
    curl_out=$(curl --silent --show-error --location --proto "$protocols" \
      --connect-timeout 30 --speed-limit 1024 --speed-time 60 \
      --output "$2" --write-out '%{http_code} %{url_effective}' "$1") ||
      return 1
    http_status=${curl_out%% *}
    final_url=${curl_out#* }
  else
    wget --quiet --server-response --tries=2 --timeout=60 \
      --output-document="$2" "$1" 2>"$tmp_dir/headers"
    wget_status=$?
    http_status=
    final_url=$1
    while read -r field value _; do
      case $field in
        HTTP/*) http_status=$value ;;
        [Ll]ocation:) final_url=$value ;;
      esac
    done <"$tmp_dir/headers"
    # The last status line decides, not wget's exit code: GNU wget exits 8 on
    # an HTTP error status, but BusyBox wget exits 1, as it does on a network
    # failure. An error status is a response for the caller to report; any
    # other status counts only if wget succeeded, because a failure after it
    # means the body, or the redirect's target, never fully arrived.
    case $http_status in
      '') return 1 ;;
      [45][0-9][0-9]) ;;
      *) [ "$wget_status" -eq 0 ] || return 1 ;;
    esac
  fi
}

# download URL FILE WHAT: like http_get, but fails unless the result is HTTP 200.
download() {
  http_get "$1" "$2" || die "network error: could not download $1"
  case $http_status in
    200) ;;
    404) die "not found (HTTP 404): $1 (does release $tag exist and include $3?)" ;;
    *) die "download failed (HTTP $http_status): $1" ;;
  esac
}

# Sets tag by following <base>/latest, which redirects to .../tag/<tag>.
resolve_latest() {
  latest_url=$base_url/latest
  no_release_hint="if none is published yet, build from source or pass --version"
  http_get "$latest_url" /dev/null || die "network error: could not reach $latest_url"
  if [ "$http_status" != 200 ]; then
    die "could not find the latest release at $latest_url (HTTP $http_status); $no_release_hint"
  fi
  case $final_url in
    */tag/*) tag=${final_url##*/tag/} ;;
    *) die "could not tell the latest release from $final_url (expected .../tag/vX.Y.Z); $no_release_hint" ;;
  esac
  check_tag "$tag" "latest release tag '$tag'"
}

# Sets expected_sum from the archive's single, well-formed SHA256SUMS line.
find_expected_sum() {
  expected_sum=
  matches=0
  while read -r sum name || [ -n "$sum" ]; do
    if [ "${name#\*}" = "$archive_name" ]; then
      expected_sum=$sum
      matches=$((matches + 1))
    fi
  done <"$tmp_dir/SHA256SUMS"
  case $matches in
    0) die "SHA256SUMS for $tag has no line for $archive_name; refusing to install an unverified archive" ;;
    1) ;;
    *) die "SHA256SUMS for $tag lists $archive_name more than once" ;;
  esac
  case $expected_sum in
    *[!0-9a-f]*) die "malformed checksum for $archive_name in SHA256SUMS: $expected_sum" ;;
  esac
  if [ ${#expected_sum} -ne 64 ]; then
    die "malformed checksum for $archive_name in SHA256SUMS: $expected_sum"
  fi
}

verify_checksum() {
  if [ "$hasher" = sha256sum ]; then
    sum_out=$(sha256sum <"$archive")
  else
    sum_out=$(shasum -a 256 <"$archive")
  fi || die "could not compute the SHA-256 of $archive_name"
  actual_sum=${sum_out%% *}
  if [ "$actual_sum" != "$expected_sum" ]; then
    die "checksum mismatch for $archive_name: SHA256SUMS says $expected_sum, download is $actual_sum; nothing was installed"
  fi
  say "verified SHA-256 of $archive_name"
}

# Rejects any archive that is not exactly the documented release layout.
# docs/ is optional: only the executable is ever installed.
check_layout() {
  if ! tar -tzf "$archive" >"$tmp_dir/names" 2>/dev/null ||
    ! tar -tvzf "$archive" >"$tmp_dir/details" 2>/dev/null; then
    die "could not list $archive_name with tar (not a gzip tarball, or gzip missing?)"
  fi
  found_iris=no
  while IFS= read -r entry; do
    case $entry in
      "$top" | "$top/" | "$top/LICENSE" | "$top/THIRD-PARTY-LICENSES") ;;
      "$top/README.md" | "$top/CHANGELOG.md") ;;
      "$top/docs" | "$top/docs/") ;;
      "$top/docs/"*) check_docs_entry "$entry" ;;
      "$top/iris") found_iris=yes ;;
      *) unexpected_entry "$entry" ;;
    esac
  done <"$tmp_dir/names"
  [ "$found_iris" = yes ] || die "$archive_name does not contain $top/iris"
  # Only regular files and directories: no symbolic/hard links or devices.
  while IFS= read -r entry; do
    case $entry in
      *' -> '* | *' link to '* | [!d-]*) die "unexpected link or special file in $archive_name: $entry" ;;
    esac
  done <"$tmp_dir/details"
}

unexpected_entry() {
  die "unexpected entry in $archive_name: '$1' (only $top/ with iris, LICENSE, THIRD-PARTY-LICENSES, README.md, CHANGELOG.md and docs/ is allowed)"
}

# Documentation may sit at any depth under docs/, but never behind an empty,
# "." or ".." path component.
check_docs_entry() {
  docs_path=${1#"$top/docs/"}
  case /${docs_path%/}/ in
    *//* | */./* | */../*) unexpected_entry "$1" ;;
  esac
}

# Extracts the archive and sets new_bin and new_version from `iris --version`.
extract_and_check() {
  mkdir "$tmp_dir/x" || die "could not create $tmp_dir/x"
  tar -xzf "$archive" -C "$tmp_dir/x" || die "could not extract $archive_name"
  new_bin=$tmp_dir/x/$top/iris
  if [ ! -f "$new_bin" ] || [ -L "$new_bin" ]; then die "$top/iris is not a regular file"; fi
  new_version=$("$new_bin" --version) ||
    die "the downloaded iris does not run on this machine (\`iris --version\` failed); nothing was installed"
  new_version=${new_version%%"$NL"*}
  [ -n "$new_version" ] || die "the downloaded iris printed no version; nothing was installed"
}

# Copies new_bin next to the destination, then renames it into place.
install_binary() {
  mkdir -p -- "$install_dir" ||
    die "could not create $install_dir (choose a writable directory with --dir; this installer never uses sudo)"
  install_dir=$(CDPATH='' cd -- "$install_dir" && pwd) || die "could not enter $install_dir"
  dest=$install_dir/iris
  if [ -d "$dest" ]; then die "$dest is a directory; remove it or choose another --dir"; fi
  staged_file=$(mktemp "$install_dir/.iris.XXXXXXXX") ||
    die "could not write to $install_dir (choose a writable directory with --dir; this installer never uses sudo)"
  cp "$new_bin" "$staged_file" || die "could not write $staged_file"
  chmod 755 "$staged_file" || die "could not make $staged_file executable"
  mv -f "$staged_file" "$dest" || die "could not move the new iris into place at $dest"
  staged_file=
}

# Prints the line to add to the user's shell startup file if needed.
print_path_hint() {
  case $install_dir in
    *:*)
      warn "$install_dir contains ':', so it cannot be added to PATH; run $dest by its full path, or reinstall with another --dir"
      return 0
      ;;
  esac
  case ":${PATH:-}:" in
    *":$install_dir:"* | *":$install_dir/:"*)
      # command -v spells the path as PATH does, so allow for the trailing slash.
      found=$(command -v iris 2>/dev/null) || found=
      case $found in
        "" | "$dest" | "$install_dir//iris") ;;
        *) warn "$found comes earlier on your PATH, so 'iris' runs that copy instead" ;;
      esac
      return 0
      ;;
  esac
  shown_dir=$install_dir
  if [ -n "${HOME:-}" ] && [ "$HOME" != / ]; then
    case $install_dir in "$HOME"/*) shown_dir=\$HOME${install_dir#"$HOME"} ;; esac
  fi
  path_line="export PATH=\"$shown_dir:\$PATH\""
  case ${SHELL:-} in
    */zsh) rc_file=.zshrc ;;
    */bash) if [ "$os" = Darwin ]; then rc_file=.bash_profile; else rc_file=.bashrc; fi ;;
    */fish)
      rc_file=.config/fish/config.fish
      path_line="fish_add_path \"$shown_dir\""
      ;;
    *) rc_file=.profile ;;
  esac
  # The line puts the directory between double quotes, where " $ ` and \ are
  # special and a newline ends the line. Never print a line that would run
  # something else when pasted into a startup file.
  case ${shown_dir#\$HOME} in
    *\"* | *\$* | *\`* | *\\* | *"$NL"*)
      say "$install_dir is not on your PATH. Add it to PATH in ~/$rc_file, then open a new shell. (Its name needs shell escaping, so no line is shown.)"
      return 0
      ;;
  esac
  say "$install_dir is not on your PATH. Add this line to ~/$rc_file, then open a new shell:"
  printf '\n    %s\n\n' "$path_line"
}

main() {
  # When piped (curl ... | sh), stdin is this script, and there is nothing to
  # prompt for: make sure no command we run can read it or wait on a terminal.
  exec </dev/null
  trap cleanup EXIT
  trap 'exit 1' HUP INT TERM

  parse_args "$@"
  detect_target
  check_tools
  normalize_version
  protocols='=https'
  case $base_url in http://*) protocols='=http,https' ;; esac

  make_tmp_dir
  if [ -z "$tag" ]; then resolve_latest; fi
  say "installing iris $tag ($target) into $install_dir"

  archive_name=iris-$tag-$target.tar.gz
  archive=$tmp_dir/$archive_name
  top=iris-$tag-$target
  download "$base_url/download/$tag/SHA256SUMS" "$tmp_dir/SHA256SUMS" SHA256SUMS
  find_expected_sum
  download "$base_url/download/$tag/$archive_name" "$archive" "$archive_name"
  verify_checksum
  check_layout
  extract_and_check
  install_binary

  say "installed $new_version to $dest"
  print_path_hint
}

main "$@"
