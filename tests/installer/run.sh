#!/bin/sh
# tests/installer/run.sh - offline tests for install.sh (POSIX sh).
#
# Usage: sh tests/installer/run.sh
#   INSTALLER_SHELL='bash --posix'  shell that runs install.sh (default: sh)
#   KEEP_TEST_DIR=1                 keep the work directory for debugging
#
# Builds fake releases (make-fixtures.sh), serves them on 127.0.0.1 only
# (server.py, needs python3), and runs install.sh against them through
# IRIS_INSTALL_BASE_URL. Each run gets a clean environment (env -i, so no
# proxies or credentials), its own HOME and TMPDIR, and a PATH made of
# uname/sysctl shims (to simulate Linux and macOS) plus only the tools the
# installer is allowed to use. macOS is simulated here; real macOS runs
# happen only in hosted CI.

set -u

HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
INSTALLER=$(CDPATH='' cd -- "$HERE/../.." && pwd)/install.sh
LINUX=x86_64-unknown-linux-musl
TOP=iris-v0.1.0-$LINUX
NAME=$TOP.tar.gz
NONET=500/good # base URL for runs that must fail before using the network

die() {
  printf 'run.sh: %s\n' "$*" >&2
  exit 1
}

command -v python3 >/dev/null 2>&1 || die "python3 is required for the local HTTP server"
[ -f "$INSTALLER" ] || die "install.sh not found at $INSTALLER"

W=$(mktemp -d "${TMPDIR:-/tmp}/iris-installer-tests.XXXXXX") || die "mktemp failed"
SERVER_PID=
finish() {
  if [ -n "$SERVER_PID" ]; then kill "$SERVER_PID" 2>/dev/null; fi
  if [ "${KEEP_TEST_DIR:-}" = 1 ]; then echo "kept $W"; else rm -rf "$W"; fi
}
trap finish EXIT
trap 'exit 1' HUP INT PIPE TERM

# --- setup ---------------------------------------------------------------------

# The installer runs through a wrapper so INSTALLER_SHELL may carry options.
make_installer_shell() {
  ishell=${INSTALLER_SHELL:-sh}
  ishell_cmd=${ishell%% *}
  ishell_args=${ishell#"$ishell_cmd"}
  ishell_path=$(command -v "$ishell_cmd") || die "shell not found: $ishell_cmd"
  printf '#!/bin/sh\nexec %s%s "$@"\n' "$ishell_path" "$ishell_args" >"$W/installer-shell"
  chmod 755 "$W/installer-shell"
  SH=$W/installer-shell
}

make_shims() {
  mkdir -p "$W/shims"
  cat >"$W/shims/uname" <<'EOF'
#!/bin/sh
# uname shim for installer tests: reports FAKE_UNAME_S and FAKE_UNAME_M.
case ${1:-} in
  -s) echo "$FAKE_UNAME_S" ;;
  -m) echo "$FAKE_UNAME_M" ;;
  *) echo "uname shim: unsupported arguments: $*" >&2; exit 2 ;;
esac
EOF
  cat >"$W/shims/sysctl" <<'EOF'
#!/bin/sh
# sysctl shim for installer tests: hw.optional.arm64 is FAKE_SYSCTL_ARM64 if set.
if [ "$*" = "-n hw.optional.arm64" ] && [ -n "${FAKE_SYSCTL_ARM64:-}" ]; then
  echo "$FAKE_SYSCTL_ARM64"
  exit 0
fi
echo "sysctl: unknown oid '${2:-}'" >&2
exit 1
EOF
  chmod 755 "$W/shims/uname" "$W/shims/sysctl"
  # Without sysctl on PATH, as when /usr/sbin is missing from it.
  mkdir -p "$W/shims-nosysctl"
  cp "$W/shims/uname" "$W/shims-nosysctl/uname"
}

# toolbox NAME TOOL...: a directory with links to just these host tools
# (tools this host lacks are skipped).
toolbox() {
  tb=$W/tools/$1
  shift
  mkdir -p "$tb"
  for tool in "$@"; do
    tool_path=$(command -v "$tool" 2>/dev/null) || continue
    case $tool_path in /*) ln -s "$tool_path" "$tb/$tool" ;; esac
  done
}

make_toolboxes() {
  if command -v sha256sum >/dev/null 2>&1; then HASH=sha256sum; else HASH=shasum; fi
  toolbox default curl "$HASH" tar gzip mktemp mkdir cp chmod mv rm
  toolbox wget wget "$HASH" tar gzip mktemp mkdir cp chmod mv rm
  toolbox shasum curl shasum tar gzip mktemp mkdir cp chmod mv rm
  toolbox notar curl "$HASH" gzip mktemp mkdir cp chmod mv rm
  toolbox nohash curl tar gzip mktemp mkdir cp chmod mv rm
  toolbox nofetch "$HASH" tar gzip mktemp mkdir cp chmod mv rm
}

start_server() {
  sh "$HERE/make-fixtures.sh" "$W/www" || die "could not build fixtures"
  python3 "$HERE/server.py" "$W/www" "$W/port" 2>"$W/server.log" &
  SERVER_PID=$!
  tries=0
  while [ ! -s "$W/port" ]; do
    tries=$((tries + 1))
    [ "$tries" -le 100 ] || die "server did not start; see $W/server.log"
    sleep 0.1
  done
  SERVER=http://127.0.0.1:$(cat "$W/port")
}

# --- test case helpers -----------------------------------------------------------

PASSED=0
FAILED=0
CASE_NO=0

# begin NAME: start a case with a fresh HOME, TMPDIR and default settings,
# which the case may change before calling run.
begin() {
  CASE=$1
  CASE_NO=$((CASE_NO + 1))
  C=$W/case$CASE_NO
  mkdir -p "$C/home" "$C/tmp"
  : >"$C/failures"
  : >"$C/out"
  : >"$C/err"
  OS=Linux
  ARCH=x86_64
  ARM64=
  TOOLS=$W/tools/default
  SHIMS=$W/shims
  FRONT_PATH=
  USER_SHELL=/bin/bash
  ENV_VERSION=
  ENV_DIR=
  STDIN=/dev/null
  CWD=$C
  STATUS=
  BIN=$C/home/.local/bin
}

# in_env CMD...: exec CMD in the case's clean environment (call in a subshell).
in_env() {
  cd "$CWD" || exit 99
  exec env -i HOME="$C/home" TMPDIR="$C/tmp" SHELL="$USER_SHELL" \
    PATH="${FRONT_PATH:+$FRONT_PATH:}$SHIMS:$TOOLS" \
    FAKE_UNAME_S="$OS" FAKE_UNAME_M="$ARCH" FAKE_SYSCTL_ARM64="$ARM64" \
    IRIS_INSTALL_BASE_URL="$BASE" IRIS_VERSION="$ENV_VERSION" IRIS_INSTALL_DIR="$ENV_DIR" \
    "$@"
}

# set_base REPO: REPO is "<mode>/<repo>" on the test server, or a full URL.
set_base() {
  case $1 in
    http://*) BASE=$1 ;;
    *) BASE=$SERVER/$1 ;;
  esac
}

# run REPO [ARG...]: sh install.sh ARG... against REPO.
run() {
  set_base "$1"
  shift
  (in_env "$SH" "$INSTALLER" "$@") <"$STDIN" >"$C/out" 2>"$C/err"
  STATUS=$?
}

# run_piped REPO [ARG...]: cat install.sh | sh -s -- ARG... against REPO.
run_piped() {
  set_base "$1"
  shift
  # shellcheck disable=SC2002 # piping the script into sh is what is tested
  cat "$INSTALLER" | (in_env "$SH" -s -- "$@") >"$C/out" 2>"$C/err"
  STATUS=$?
}

fail() { printf '%s\n' "$*" >>"$C/failures"; }
expect_status() { [ "$STATUS" = "$1" ] || fail "exit status $STATUS, expected $1"; }
expect_out() { grep -F -e "$1" "$C/out" >/dev/null || fail "stdout lacks: $1"; }
expect_err() { grep -F -e "$1" "$C/err" >/dev/null || fail "stderr lacks: $1"; }
expect_no_out() { if grep -F -e "$1" "$C/out" >/dev/null; then fail "stdout has: $1"; fi; }
expect_no_err() { if grep -F -e "$1" "$C/err" >/dev/null; then fail "stderr has: $1"; fi; }
expect_missing() { if [ -e "$1" ] || [ -L "$1" ]; then fail "should not exist: $1"; fi; }

# expect_installed FILE VERSION TARGET: FILE is the fake iris for that release,
# and the installer reported it.
expect_installed() {
  if [ ! -f "$1" ] || [ ! -x "$1" ]; then
    fail "no executable at $1"
    return
  fi
  got=$("$1" --version </dev/null 2>&1)
  [ "$got" = "iris $2 ($3)" ] || fail "$1 --version says '$got', expected 'iris $2 ($3)'"
  expect_out "installed iris $2 ($3) to $1"
  expect_no_staging "$(dirname "$1")"
}

expect_no_staging() {
  for staged in "$1"/.iris.*; do
    if [ -e "$staged" ]; then fail "staging file left behind: $staged"; fi
  done
}

# ls -di is the portable way to read an inode number (no stat(1) in POSIX).
# shellcheck disable=SC2012
inode() { ls -di "$1" | { read -r number _ && printf '%s\n' "$number"; }; }

# seed_old DIR: put an "old" iris into DIR and remember its bytes and inode.
seed_old() {
  mkdir -p "$1"
  printf '#!/bin/sh\necho "iris 0.0.1 (old)"\n' >"$1/iris"
  chmod 755 "$1/iris"
  cp "$1/iris" "$C/old.copy"
  OLD_INODE=$(inode "$1/iris")
}

# expect_old_kept DIR: the iris in DIR is still the seeded file, untouched.
expect_old_kept() {
  cmp -s "$1/iris" "$C/old.copy" || fail "existing $1/iris was modified"
  [ "$(inode "$1/iris")" = "$OLD_INODE" ] || fail "existing $1/iris was replaced"
  expect_no_staging "$1"
}

# end: every case also checks that the installer left nothing in TMPDIR.
end() {
  leftovers=$(ls -A "$C/tmp")
  if [ -n "$leftovers" ]; then fail "left in TMPDIR: $leftovers"; fi
  if [ -s "$C/failures" ]; then
    FAILED=$((FAILED + 1))
    printf 'FAIL %s\n' "$CASE"
    sed 's/^/       - /' "$C/failures"
    printf '       stdout:\n'
    sed 's/^/       | /' "$C/out"
    printf '       stderr:\n'
    sed 's/^/       | /' "$C/err"
  else
    PASSED=$((PASSED + 1))
    printf 'ok   %s\n' "$CASE"
    # Show the key installer message as evidence.
    if [ "$STATUS" = 0 ]; then
      grep -e '^iris-install: installed ' -e 'warning:' "$C/out" "$C/err" | sed 's/^[^:]*:/       > /'
    else
      grep -e '^iris-install: error: ' "$C/err" | sed 's/^/       > /'
    fi
  fi
}

# --- cases -----------------------------------------------------------------------

platform_cases() {
  begin "Linux x86_64: latest release into ~/.local/bin, created without sudo"
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  expect_out "installing iris v0.2.0 ($LINUX) into $BIN"
  end

  begin "macOS x86_64 (Intel, no hw.optional.arm64)"
  OS=Darwin ARCH=x86_64
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 x86_64-apple-darwin
  end

  begin "macOS x86_64 with hw.optional.arm64=0"
  OS=Darwin ARCH=x86_64 ARM64=0
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 x86_64-apple-darwin
  end

  begin "macOS arm64"
  OS=Darwin ARCH=arm64
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 aarch64-apple-darwin
  end

  begin "macOS under Rosetta (uname x86_64, hw.optional.arm64=1) prefers arm64"
  OS=Darwin ARCH=x86_64 ARM64=1
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 aarch64-apple-darwin
  end

  # The system sysctl answers here, so the expected target depends on the host:
  # arm64 on Apple silicon, x86_64 elsewhere (e.g. Linux, where the key is absent).
  if [ "$(/usr/sbin/sysctl -n hw.optional.arm64 2>/dev/null)" = 1 ]; then
    host_arm64=yes want=aarch64-apple-darwin
  else
    host_arm64=no want=x86_64-apple-darwin
  fi
  begin "macOS x86_64 without sysctl on PATH asks /usr/sbin/sysctl (host arm64: $host_arm64)"
  OS=Darwin ARCH=x86_64 SHIMS=$W/shims-nosysctl
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$want"
  end

  for platform in "Linux aarch64" "Linux i686" "Linux armv7l" "MINGW64_NT-10.0-19045 x86_64" \
    "MSYS_NT-10.0-19045 x86_64" "CYGWIN_NT-10.0 x86_64" "FreeBSD amd64" "Darwin i386"; do
    begin "unsupported platform: $platform"
    OS=${platform% *} ARCH=${platform##* }
    run "$NONET"
    expect_status 1
    expect_err "unsupported platform: $platform"
    expect_missing "$C/home/.local"
    end
  done
}

version_cases() {
  begin "--version v0.1.0"
  run ok/good --version v0.1.0
  expect_status 0
  expect_installed "$BIN/iris" 0.1.0 "$LINUX"
  end

  begin "--version 0.1.0 (without v)"
  run ok/good --version 0.1.0
  expect_status 0
  expect_installed "$BIN/iris" 0.1.0 "$LINUX"
  end

  begin "--version=0.1.0"
  run ok/good --version=0.1.0
  expect_status 0
  expect_installed "$BIN/iris" 0.1.0 "$LINUX"
  end

  begin "--version latest"
  run ok/good --version latest
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  end

  begin "pre-release: --version 0.3.0-rc.1"
  run ok/good --version 0.3.0-rc.1
  expect_status 0
  expect_installed "$BIN/iris" 0.3.0-rc.1 "$LINUX"
  end

  begin "IRIS_VERSION=v0.1.0"
  ENV_VERSION=v0.1.0
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.1.0 "$LINUX"
  end

  begin "IRIS_VERSION=0.1.0 is overridden by --version latest"
  ENV_VERSION=0.1.0
  run ok/good --version latest
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  end

  begin "invalid version is rejected before any download"
  run "$NONET" --version 1.0
  expect_status 1
  expect_err "version '1.0' is not a release version"
  end

  begin "version with path characters is rejected"
  run "$NONET" --version 'v1.2.3/../../x'
  expect_status 1
  expect_err "contains unexpected characters"
  end

  begin "latest with no published release (404)"
  run ok/nolatest
  expect_status 1
  expect_err "could not find the latest release at $SERVER/ok/nolatest/latest (HTTP 404)"
  expect_missing "$C/home/.local"
  end

  begin "latest redirecting to a tag that is not a version"
  run ok/badlatest
  expect_status 1
  expect_err "latest release tag 'nightly' is not a release version"
  end

  begin "missing release (404)"
  run ok/good --version v9.9.9
  expect_status 1
  expect_err "not found (HTTP 404): $SERVER/ok/good/download/v9.9.9/SHA256SUMS"
  expect_missing "$C/home/.local"
  end
}

network_cases() {
  begin "server error 500 while resolving latest"
  run 500/good
  expect_status 1
  expect_err "could not find the latest release"
  expect_err "(HTTP 500)"
  end

  begin "server error 500 while downloading"
  run 500/good --version v0.1.0
  expect_status 1
  expect_err "download failed (HTTP 500): $SERVER/500/good/download/v0.1.0/SHA256SUMS"
  end

  begin "connection dropped without a response"
  run drop/good --version v0.1.0
  expect_status 1
  expect_err "network error: could not download $SERVER/drop/good/download/v0.1.0/SHA256SUMS"
  end

  begin "connection refused"
  run http://127.0.0.1:1/releases --version v0.1.0
  expect_status 1
  expect_err "network error: could not download http://127.0.0.1:1/releases/download/v0.1.0/SHA256SUMS"
  end

  begin "archive download cut off halfway: old iris kept"
  seed_old "$BIN"
  run truncate/good --version v0.1.0
  expect_status 1
  expect_err "network error: could not download $SERVER/truncate/good/download/v0.1.0/$NAME"
  expect_old_kept "$BIN"
  end

  begin "TERM during a download: exits non-zero, temporary files removed"
  rm -f "$W/www/slow.started"
  set_base slow/good
  (in_env "$SH" "$INSTALLER" --version v0.1.0) </dev/null >"$C/out" 2>"$C/err" &
  pid=$!
  tries=0
  while [ ! -e "$W/www/slow.started" ] && [ "$tries" -lt 100 ]; do
    tries=$((tries + 1))
    sleep 0.1
  done
  [ -e "$W/www/slow.started" ] || fail "the slow download never started"
  [ -n "$(ls -A "$C/tmp")" ] || fail "no temporary directory while downloading"
  kill -TERM "$pid"
  wait "$pid"
  STATUS=$?
  [ "$STATUS" != 0 ] || fail "exit status 0 after TERM"
  expect_missing "$C/home/.local"
  end
}

dir_cases() {
  begin "--dir DIR (nested, created)"
  run ok/good --dir "$C/a/b/bin"
  expect_status 0
  expect_installed "$C/a/b/bin/iris" 0.2.0 "$LINUX"
  expect_missing "$C/home/.local"
  end

  begin "IRIS_INSTALL_DIR"
  ENV_DIR=$C/envdir
  run ok/good
  expect_status 0
  expect_installed "$C/envdir/iris" 0.2.0 "$LINUX"
  end

  begin "--dir=DIR wins over IRIS_INSTALL_DIR"
  ENV_DIR=$C/envdir
  run ok/good --dir="$C/flagdir"
  expect_status 0
  expect_installed "$C/flagdir/iris" 0.2.0 "$LINUX"
  expect_missing "$C/envdir"
  end

  begin "relative --dir is resolved against the current directory"
  mkdir -p "$C/work"
  CWD=$C/work
  run ok/good --dir rel/bin
  expect_status 0
  expect_installed "$C/work/rel/bin/iris" 0.2.0 "$LINUX"
  end

  begin "--dir that cannot be created"
  printf 'not a directory\n' >"$C/file"
  run ok/good --dir "$C/file/bin"
  expect_status 1
  expect_err "could not create $C/file/bin"
  expect_err "never uses sudo"
  end

  begin "destination iris is a directory"
  mkdir -p "$BIN/iris"
  run ok/good
  expect_status 1
  expect_err "$BIN/iris is a directory"
  [ -d "$BIN/iris" ] || fail "$BIN/iris is no longer a directory"
  expect_no_staging "$BIN"
  end
}

option_cases() {
  begin "--help prints usage and exits 0 without downloading"
  run "$NONET" --help
  expect_status 0
  expect_out "Usage: install.sh [--version VERSION] [--dir DIR]"
  expect_out "IRIS_INSTALL_DIR"
  expect_missing "$C/home/.local"
  end

  begin "unknown option"
  run "$NONET" --prefix /usr
  expect_status 1
  expect_err "unknown option: --prefix"
  end

  begin "--dir without a value"
  run "$NONET" --dir
  expect_status 1
  expect_err "--dir needs a value"
  end

  begin "--version= with an empty value"
  run "$NONET" --version=
  expect_status 1
  expect_err "--version needs a value"
  end
}

# bad_release REPO STDERR: installing REPO's broken v0.1.0 over an old iris
# fails with STDERR and leaves the old iris byte-for-byte intact.
bad_release() {
  begin "rejects $1: nothing installed, old iris kept"
  seed_old "$BIN"
  run "ok/$1" --version v0.1.0
  expect_status 1
  expect_err "$2"
  expect_old_kept "$BIN"
  end
}

verification_cases() {
  bad_release badsum "checksum mismatch for $NAME"
  bad_release nosumline "SHA256SUMS for v0.1.0 has no line for $NAME"
  bad_release nosums "not found (HTTP 404): $SERVER/ok/nosums/download/v0.1.0/SHA256SUMS"
  bad_release malformedsum "malformed checksum for $NAME"
  bad_release dupsum "lists $NAME more than once"
  bad_release noasset "not found (HTTP 404): $SERVER/ok/noasset/download/v0.1.0/$NAME"
  bad_release notgzip "could not list $NAME"
  bad_release dotdot "unexpected entry in $NAME: '../evil'"
  bad_release dotdotinside "unexpected entry in $NAME: '$TOP/../evil'"
  bad_release absolute "unexpected entry in $NAME: '/"
  bad_release symlink "unexpected link or special file in $NAME"
  bad_release hardlink "unexpected link or special file in $NAME"
  bad_release noiris "$NAME does not contain $TOP/iris"
  bad_release extratop "unexpected entry in $NAME: 'other/'"
  bad_release extrafile "unexpected entry in $NAME: '$TOP/extra'"
  bad_release wrongtop "unexpected entry in $NAME: 'iris-v0.1.0/'"
  bad_release notexec "the downloaded iris does not run on this machine"
  bad_release failing "the downloaded iris does not run on this machine"
  bad_release silent "the downloaded iris printed no version"
}

tool_cases() {
  begin "no tar: clear message, nothing downloaded"
  TOOLS=$W/tools/notar
  run "$NONET"
  expect_status 1
  expect_err "missing required tool: tar"
  end

  begin "no sha256sum or shasum: clear message"
  TOOLS=$W/tools/nohash
  run "$NONET"
  expect_status 1
  expect_err "missing required tool: sha256sum or shasum"
  end

  begin "no curl or wget: clear message"
  TOOLS=$W/tools/nofetch
  run "$NONET"
  expect_status 1
  expect_err "missing required tool: curl or wget"
  end

  if [ -e "$W/tools/wget/wget" ]; then
    begin "wget instead of curl: latest via redirect"
    TOOLS=$W/tools/wget
    run ok/good
    expect_status 0
    expect_installed "$BIN/iris" 0.2.0 "$LINUX"
    end

    begin "wget: missing release (404)"
    TOOLS=$W/tools/wget
    run ok/good --version v9.9.9
    expect_status 1
    expect_err "not found (HTTP 404): $SERVER/ok/good/download/v9.9.9/SHA256SUMS"
    end

    begin "wget: server error 500"
    TOOLS=$W/tools/wget
    run 500/good --version v0.1.0
    expect_status 1
    expect_err "download failed (HTTP 500)"
    end

    begin "wget: connection dropped without a response (retries are bounded)"
    TOOLS=$W/tools/wget
    started=$(date +%s)
    run drop/good --version v0.1.0
    took=$(($(date +%s) - started))
    # Unbounded, GNU wget retries 20 times with backoff: about 145 s of silence.
    [ "$took" -le 30 ] || fail "took $took s; wget retries should be bounded"
    expect_status 1
    expect_err "network error: could not download $SERVER/drop/good/download/v0.1.0/SHA256SUMS"
    end

    begin "wget: archive download cut off halfway: old iris kept"
    TOOLS=$W/tools/wget
    seed_old "$BIN"
    run truncate/good --version v0.1.0
    expect_status 1
    expect_err "network error: could not download $SERVER/truncate/good/download/v0.1.0/$NAME"
    expect_old_kept "$BIN"
    end

    begin "wget: connection refused"
    TOOLS=$W/tools/wget
    run http://127.0.0.1:1/releases --version v0.1.0
    expect_status 1
    expect_err "network error: could not download http://127.0.0.1:1/releases/download/v0.1.0/SHA256SUMS"
    end

    begin "wget: checksum mismatch, old iris kept"
    TOOLS=$W/tools/wget
    seed_old "$BIN"
    run ok/badsum --version v0.1.0
    expect_status 1
    expect_err "checksum mismatch for $NAME"
    expect_old_kept "$BIN"
    end
  else
    echo "skip wget cases: wget is not installed on this host"
  fi

  if [ -e "$W/tools/shasum/shasum" ]; then
    begin "shasum instead of sha256sum"
    TOOLS=$W/tools/shasum
    run ok/good
    expect_status 0
    expect_installed "$BIN/iris" 0.2.0 "$LINUX"
    end

    begin "shasum: checksum mismatch, old iris kept"
    TOOLS=$W/tools/shasum
    seed_old "$BIN"
    run ok/badsum --version v0.1.0
    expect_status 1
    expect_err "checksum mismatch for $NAME"
    expect_old_kept "$BIN"
    end
  else
    echo "skip shasum cases: shasum is not installed on this host"
  fi
}

stdin_cases() {
  begin "piped: cat install.sh | sh -s -- --version v0.1.0 --dir DIR"
  run_piped ok/good --version v0.1.0 --dir "$C/piped"
  expect_status 0
  expect_installed "$C/piped/iris" 0.1.0 "$LINUX"
  end

  begin "piped with defaults: cat install.sh | sh"
  run_piped ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  end

  begin "stdin with data is never read (the fake iris fails if it can read stdin)"
  printf 'y\ny\n' >"$C/stdin"
  STDIN=$C/stdin
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  end
}

# The expected hints contain a literal $HOME and $PATH.
# shellcheck disable=SC2016
path_cases() {
  begin "PATH hint for bash on Linux"
  run ok/good
  expect_status 0
  expect_out "$BIN is not on your PATH. Add this line to ~/.bashrc"
  expect_out 'export PATH="$HOME/.local/bin:$PATH"'
  end

  begin "PATH hint for bash on macOS"
  OS=Darwin ARCH=arm64
  run ok/good
  expect_status 0
  expect_out "Add this line to ~/.bash_profile"
  end

  begin "PATH hint for zsh"
  USER_SHELL=/bin/zsh
  run ok/good
  expect_status 0
  expect_out "Add this line to ~/.zshrc"
  expect_out 'export PATH="$HOME/.local/bin:$PATH"'
  end

  begin "PATH hint for fish"
  USER_SHELL=/usr/bin/fish
  run ok/good
  expect_status 0
  expect_out "Add this line to ~/.config/fish/config.fish"
  expect_out 'fish_add_path "$HOME/.local/bin"'
  end

  begin "PATH hint for another shell"
  USER_SHELL=/bin/dash
  run ok/good
  expect_status 0
  expect_out "Add this line to ~/.profile"
  end

  begin "PATH hint for a directory outside HOME"
  run ok/good --dir "$C/opt/bin"
  expect_status 0
  expect_out "export PATH=\"$C/opt/bin:\$PATH\""
  end

  begin "no PATH hint when the directory is on PATH"
  FRONT_PATH=$BIN
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  expect_no_out "not on your PATH"
  expect_no_err "comes earlier"
  end

  begin "no PATH hint or warning when PATH lists the directory with a trailing slash"
  FRONT_PATH=$BIN/
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  expect_no_out "not on your PATH"
  expect_no_err "comes earlier"
  end

  begin "warning when another iris comes earlier on PATH"
  mkdir -p "$C/other"
  printf '#!/bin/sh\necho other\n' >"$C/other/iris"
  chmod 755 "$C/other/iris"
  FRONT_PATH=$C/other:$BIN
  run ok/good
  expect_status 0
  expect_err "$C/other/iris comes earlier on your PATH"
  end
}

# A directory name with a character that is special inside double quotes must
# not produce a paste-ready line (it could run a command from ~/.bashrc).
# shellcheck disable=SC2016 # the names are meant literally
unsafe_dir_cases() {
  for unsafe in 'q"uote' 'dollar$(id)' 'back`id`tick' 'back\slash'; do
    begin "no paste-ready PATH line for a directory named $unsafe"
    run ok/good --dir "$C/$unsafe"
    expect_status 0
    expect_installed "$C/$unsafe/iris" 0.2.0 "$LINUX"
    expect_out "$C/$unsafe is not on your PATH"
    expect_out "no line is shown"
    expect_no_out "export PATH="
    end
  done

  begin "a directory name with ':' cannot go on PATH: say so"
  run ok/good --dir "$C/a:b"
  expect_status 0
  expect_installed "$C/a:b/iris" 0.2.0 "$LINUX"
  expect_err "$C/a:b contains ':', so it cannot be added to PATH"
  expect_no_out "export PATH="
  end
}

upgrade_cases() {
  begin "upgrade replaces the old iris by rename, not in place"
  seed_old "$BIN"
  ln "$BIN/iris" "$C/old.link"
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  cmp -s "$C/old.link" "$C/old.copy" || fail "the old file was overwritten in place"
  [ "$(inode "$BIN/iris")" != "$OLD_INODE" ] || fail "iris still has the old inode"
  end

  begin "upgrade from v0.1.0 to latest in two runs"
  run ok/good --version v0.1.0
  expect_status 0
  expect_installed "$BIN/iris" 0.1.0 "$LINUX"
  run ok/good
  expect_status 0
  expect_installed "$BIN/iris" 0.2.0 "$LINUX"
  end
}

# --- main ------------------------------------------------------------------------

make_installer_shell
make_shims
make_toolboxes
start_server

printf 'installer: %s\ninstaller shell: %s\nserver: %s\n' \
  "$INSTALLER" "${INSTALLER_SHELL:-sh} ($(command -v "${ishell_cmd}"))" "$SERVER"
printf 'host: %s; %s; %s\n\n' "$(uname -sm)" "$(curl --version | sed -n 1p)" "$(tar --version | sed -n 1p)"

platform_cases
version_cases
network_cases
dir_cases
option_cases
verification_cases
tool_cases
stdin_cases
path_cases
unsafe_dir_cases
upgrade_cases

printf '\n%s passed, %s failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
