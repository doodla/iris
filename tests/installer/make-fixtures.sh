#!/bin/sh
# make-fixtures.sh ROOT - build fake iris releases for tests/installer/run.sh.
#
# Each directory ROOT/<repo> looks like a GitHub releases URL as served by
# server.py: an optional LATEST file naming the latest tag, and assets under
# download/<tag>/. The "iris" executables are small shell scripts that print
# "iris X.Y.Z (<target>)" for --version, so they run on any test machine.
#
#   good         v0.1.0, v0.2.0 (latest) for every target, plus v0.3.0-rc.1 for Linux
#   nolatest     v0.1.0 but no latest release
#   badlatest    latest points at a tag that is not a version
#   nodocs       v0.1.0 for Linux, without docs/ (it is optional)
#   nesteddocs   v0.1.0 for Linux, with a subdirectory under docs/
#   nonotices    v0.1.0 for Linux, without THIRD-PARTY-LICENSES (the layout
#                of archives made before it was added)
#   <bad case>   v0.1.0 for the Linux target only, broken as the name says; the
#                SHA256SUMS line matches unless the checksum itself is the defect
set -eu

ROOT=$1
WORK=$ROOT/.work
LINUX=x86_64-unknown-linux-musl
TARGETS="$LINUX x86_64-apple-darwin aarch64-apple-darwin"

mkdir -p "$ROOT" "$WORK"

# sha256_line FILE: print "<hash>  <name>" for FILE (sha256sum format).
sha256_line() {
  (
    cd "$(dirname "$1")"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$(basename "$1")"
    else
      shasum -a 256 "$(basename "$1")"
    fi
  )
}

# fake_iris FILE VERSION TARGET [ok|fail|silent]: write a fake iris executable.
fake_iris() {
  case ${4:-ok} in
    ok) body="echo \"iris $2 ($3)\"" ;;
    fail) body='echo "iris: cannot run on this machine" >&2; exit 1' ;;
    silent) body=':' ;;
  esac
  cat >"$1" <<EOF
#!/bin/sh
# Fake iris $2 for $3, built by tests/installer/make-fixtures.sh.
if [ "\${1:-}" = --version ]; then
  # The installer must never let the binary read its stdin (the piped script).
  if IFS= read -r line || [ -n "\${line:-}" ]; then
    echo "fake iris: read unexpected stdin: \$line" >&2
    exit 3
  fi
  $body
  exit 0
fi
echo "fake iris $2: \$*"
EOF
  chmod 755 "$1"
}

# stage DIR TAG TARGET [MODE]: fill DIR with the files of a release archive.
stage() {
  mkdir -p "$1/docs"
  fake_iris "$1/iris" "${2#v}" "$3" "${4:-ok}"
  printf 'MIT License (fixture)\n' >"$1/LICENSE"
  printf 'THIRD-PARTY SOFTWARE NOTICES AND LICENSES (fixture)\n' >"$1/THIRD-PARTY-LICENSES"
  printf '# iris (fixture)\n' >"$1/README.md"
  printf '# Changelog (fixture)\n' >"$1/CHANGELOG.md"
  printf '# Installing iris (fixture)\n' >"$1/docs/install.md"
  printf '# JSON contract (fixture)\n' >"$1/docs/json-contract.md"
}

# release_dir REPO TAG: print (and create) the asset directory of a release.
release_dir() {
  mkdir -p "$ROOT/$1/download/$2"
  printf '%s\n' "$ROOT/$1/download/$2"
}

# good_archive REPO TAG TARGET: build a correct archive for one target.
good_archive() {
  top=iris-$2-$3
  src=$WORK/$1-$2-$3
  stage "$src/$top" "$2" "$3"
  out=$(release_dir "$1" "$2")/$top.tar.gz
  (cd "$src" && tar -czf "$out" "$top")
}

# write_sums REPO TAG: write SHA256SUMS for every archive of a release.
write_sums() {
  dir=$(release_dir "$1" "$2")
  : >"$dir/SHA256SUMS.tmp"
  for f in "$dir"/*.tar.gz; do
    if [ -f "$f" ]; then sha256_line "$f" >>"$dir/SHA256SUMS.tmp"; fi
  done
  mv "$dir/SHA256SUMS.tmp" "$dir/SHA256SUMS"
}

# --- good releases -----------------------------------------------------------
for tag in v0.1.0 v0.2.0; do
  for target in $TARGETS; do good_archive good "$tag" "$target"; done
  write_sums good "$tag"
done
good_archive good v0.3.0-rc.1 "$LINUX"
write_sums good v0.3.0-rc.1
echo v0.2.0 >"$ROOT/good/LATEST"

good_archive nolatest v0.1.0 "$LINUX"
write_sums nolatest v0.1.0

good_archive badlatest v0.1.0 "$LINUX"
write_sums badlatest v0.1.0
echo nightly >"$ROOT/badlatest/LATEST"

# Archives without docs/, and with a subdirectory in docs/.
stage "$WORK/nodocs/iris-v0.1.0-$LINUX" v0.1.0 "$LINUX"
rm -r "$WORK/nodocs/iris-v0.1.0-$LINUX/docs"
(cd "$WORK/nodocs" && tar -czf "$(release_dir nodocs v0.1.0)/iris-v0.1.0-$LINUX.tar.gz" "iris-v0.1.0-$LINUX")
write_sums nodocs v0.1.0

stage "$WORK/nonotices/iris-v0.1.0-$LINUX" v0.1.0 "$LINUX"
rm "$WORK/nonotices/iris-v0.1.0-$LINUX/THIRD-PARTY-LICENSES"
(cd "$WORK/nonotices" && tar -czf "$(release_dir nonotices v0.1.0)/iris-v0.1.0-$LINUX.tar.gz" "iris-v0.1.0-$LINUX")
write_sums nonotices v0.1.0

stage "$WORK/nesteddocs/iris-v0.1.0-$LINUX" v0.1.0 "$LINUX"
mkdir "$WORK/nesteddocs/iris-v0.1.0-$LINUX/docs/assets"
printf '<svg/>\n' >"$WORK/nesteddocs/iris-v0.1.0-$LINUX/docs/assets/diagram.svg"
(cd "$WORK/nesteddocs" && tar -czf "$(release_dir nesteddocs v0.1.0)/iris-v0.1.0-$LINUX.tar.gz" "iris-v0.1.0-$LINUX")
write_sums nesteddocs v0.1.0

# --- checksum defects ----------------------------------------------------------
TOP=iris-v0.1.0-$LINUX
NAME=$TOP.tar.gz

good_archive badsum v0.1.0 "$LINUX"
printf '%s  %s\n' "$(printf '%064d' 0 | tr 0 a)" "$NAME" >"$(release_dir badsum v0.1.0)/SHA256SUMS"

good_archive nosumline v0.1.0 "$LINUX"
good_archive nosumline v0.1.0 x86_64-apple-darwin
write_sums nosumline v0.1.0
dir=$(release_dir nosumline v0.1.0)
grep -v "$LINUX" "$dir/SHA256SUMS" >"$dir/SHA256SUMS.tmp"
mv "$dir/SHA256SUMS.tmp" "$dir/SHA256SUMS"

# Lines whose names only contain the archive name, each with its real hash:
# only an exact name match may count.
good_archive decoysums v0.1.0 "$LINUX"
dir=$(release_dir decoysums v0.1.0)
sum=$(sha256_line "$dir/$NAME")
sum=${sum%% *}
for decoy in "$NAME.sig" "x$NAME" "sub/$NAME" "$NAME.bak"; do
  printf '%s  %s\n' "$sum" "$decoy"
done >"$dir/SHA256SUMS"

good_archive nosums v0.1.0 "$LINUX"

good_archive malformedsum v0.1.0 "$LINUX"
printf 'deadbeef  %s\n' "$NAME" >"$(release_dir malformedsum v0.1.0)/SHA256SUMS"

good_archive dupsum v0.1.0 "$LINUX"
write_sums dupsum v0.1.0
dir=$(release_dir dupsum v0.1.0)
sha256_line "$dir/$NAME" >>"$dir/SHA256SUMS"

good_archive noasset v0.1.0 "$LINUX"
write_sums noasset v0.1.0
rm "$(release_dir noasset v0.1.0)/$NAME"

# --- archive defects (SHA256SUMS is correct for each) ---------------------------
# bad_archive REPO DIR PATH...: from DIR, pack PATH... as the v0.1.0 Linux
# archive of REPO (-P stores absolute and ".." names as given), with a
# matching SHA256SUMS.
bad_archive() {
  repo=$1
  from=$2
  shift 2
  out=$(release_dir "$repo" v0.1.0)/$NAME
  (cd "$from" && tar -czPf "$out" "$@")
  write_sums "$repo" v0.1.0
}

printf '<html>Not Found</html>\n' >"$(release_dir notgzip v0.1.0)/$NAME"
write_sums notgzip v0.1.0

stage "$WORK/dotdot/in/$TOP" v0.1.0 "$LINUX"
printf 'evil\n' >"$WORK/dotdot/evil"
bad_archive dotdot "$WORK/dotdot/in" "$TOP" ../evil

stage "$WORK/dotdotinside/$TOP" v0.1.0 "$LINUX"
printf 'evil\n' >"$WORK/dotdotinside/evil"
bad_archive dotdotinside "$WORK/dotdotinside" "$TOP" "$TOP/../evil"

stage "$WORK/absolute/$TOP" v0.1.0 "$LINUX"
printf 'evil\n' >"$WORK/absolute/evil"
bad_archive absolute "$WORK/absolute" "$TOP" "$WORK/absolute/evil"

# ".." inside docs/ that climbs out of the top-level directory.
stage "$WORK/docsdotdot/$TOP" v0.1.0 "$LINUX"
printf 'evil\n' >"$WORK/docsdotdot/evil"
bad_archive docsdotdot "$WORK/docsdotdot" "$TOP" "$TOP/docs/../../evil"

stage "$WORK/symlink/$TOP" v0.1.0 "$LINUX"
rm "$WORK/symlink/$TOP/LICENSE"
ln -s /etc/passwd "$WORK/symlink/$TOP/LICENSE"
bad_archive symlink "$WORK/symlink" "$TOP"

stage "$WORK/docslink/$TOP" v0.1.0 "$LINUX"
ln -s /etc/passwd "$WORK/docslink/$TOP/docs/passwd.md"
bad_archive docslink "$WORK/docslink" "$TOP"

stage "$WORK/hardlink/$TOP" v0.1.0 "$LINUX"
rm "$WORK/hardlink/$TOP/README.md"
ln "$WORK/hardlink/$TOP/iris" "$WORK/hardlink/$TOP/README.md"
bad_archive hardlink "$WORK/hardlink" "$TOP"

# A FIFO under an allowed name: only the entry type gives it away.
stage "$WORK/fifo/$TOP" v0.1.0 "$LINUX"
rm "$WORK/fifo/$TOP/LICENSE"
mkfifo "$WORK/fifo/$TOP/LICENSE"
bad_archive fifo "$WORK/fifo" "$TOP"

stage "$WORK/noiris/$TOP" v0.1.0 "$LINUX"
rm "$WORK/noiris/$TOP/iris"
bad_archive noiris "$WORK/noiris" "$TOP"

stage "$WORK/extratop/$TOP" v0.1.0 "$LINUX"
mkdir -p "$WORK/extratop/other"
printf 'x\n' >"$WORK/extratop/other/file"
bad_archive extratop "$WORK/extratop" "$TOP" other

# THIRD-PARTY-LICENSES must be a file, not a directory of files.
stage "$WORK/noticesdir/$TOP" v0.1.0 "$LINUX"
rm "$WORK/noticesdir/$TOP/THIRD-PARTY-LICENSES"
mkdir "$WORK/noticesdir/$TOP/THIRD-PARTY-LICENSES"
printf 'x\n' >"$WORK/noticesdir/$TOP/THIRD-PARTY-LICENSES/MIT"
bad_archive noticesdir "$WORK/noticesdir" "$TOP"

stage "$WORK/extrafile/$TOP" v0.1.0 "$LINUX"
printf 'x\n' >"$WORK/extrafile/$TOP/extra"
bad_archive extrafile "$WORK/extrafile" "$TOP"

stage "$WORK/wrongtop/iris-v0.1.0" v0.1.0 "$LINUX"
bad_archive wrongtop "$WORK/wrongtop" iris-v0.1.0

stage "$WORK/notexec/$TOP" v0.1.0 "$LINUX"
chmod 644 "$WORK/notexec/$TOP/iris"
bad_archive notexec "$WORK/notexec" "$TOP"

stage "$WORK/failing/$TOP" v0.1.0 "$LINUX" fail
bad_archive failing "$WORK/failing" "$TOP"

stage "$WORK/silent/$TOP" v0.1.0 "$LINUX" silent
bad_archive silent "$WORK/silent" "$TOP"

rm -rf "$WORK"
