# Installing Iris

## Status: no release has been published yet

As of this writing, `doodla/iris` has **no published release**, so the installer paths below
describe the intended, tested behavior — verified with 96 offline test cases against local
fixtures (`sh tests/installer/run.sh`) and by `shellcheck -s sh install.sh`, not against a real
GitHub release. **What works today** is building from a checkout:

```console
$ git clone https://github.com/doodla/iris && cd iris
$ cargo install --locked --path .
```

This needs a Rust toolchain (`rustup` is the easiest way to get one); the minimum supported Rust
version is 1.89. Verified for this documentation: `cargo build --locked --release` succeeds
against the committed `Cargo.lock` and produces a working `iris --version`.

## Once a release exists: the one-command installer

```console
$ curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
```

`install.sh` is a small, readable POSIX `sh` script (no bash-isms; `shellcheck -s sh` clean).
What it does, in order:

1. Detects your platform: Linux x86_64, macOS x86_64, or macOS arm64 (Apple silicon is preferred
   even when the shell is running under Rosetta). Anything else — Windows, Linux arm64, a BSD, a
   32-bit system — fails immediately with a message naming what it detected; Iris does not
   support Windows in v1 (no builds, no CI, no installer path).
2. Requires `curl` (or `wget`), `tar`, and `sha256sum` (or `shasum -a 256`); missing tools are
   named individually rather than one generic "requirements not met" message.
3. Resolves `latest` by following `https://github.com/doodla/iris/releases/latest` to its
   redirect target — no GitHub API call, so it works without a token and isn't rate-limited.
4. Downloads the release archive and its `SHA256SUMS` file into a private `mktemp -d` directory
   that is removed on exit, Ctrl-C, or `TERM` (never left behind, even on failure).
5. Verifies the archive's SHA-256 against its line in `SHA256SUMS`; a missing line or a mismatch
   aborts with nothing installed.
6. Lists the archive before extracting and rejects anything unexpected: absolute paths, `..`
   entries, symlinks, files outside the single top-level `iris-vX.Y.Z-<target>/` directory, or a
   missing `iris` executable.
7. Extracts it and runs the extracted `iris --version` to confirm it actually executes on your
   machine before installing anything.
8. Copies `iris` into the target directory under a temporary name, then renames it over any
   existing `iris` — an atomic replace on the same filesystem. The directory is created if
   needed. **`sudo` is never used.**
9. Prints the installed version and, if the install directory isn't already on your `PATH`, the
   exact line to add to your shell's startup file.

It **never reads from stdin**, so piping it into `sh` is safe even with input already flowing
through that pipe; on any failure, nothing is installed and an existing `iris` is left exactly as
it was.

### Options

```console
$ install.sh --help
Install iris from a GitHub release.

Usage: install.sh [--version VERSION] [--dir DIR]
  curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh -s -- [OPTIONS]

Options (a flag wins over its environment variable):
  --version VERSION  vX.Y.Z, X.Y.Z or latest (default: latest)   env IRIS_VERSION
  --dir DIR          install directory (default: ~/.local/bin)    env IRIS_INSTALL_DIR
  -h, --help         print this help and exit

The archive is verified against the release SHA256SUMS before anything is
installed. sudo is never used; upgrade by running the installer again.
```

### Passing options through the pipe

A `curl | sh` pipeline needs `-s --` before the installer's own flags, so the shell knows they
belong to the *script*, not to `sh` itself:

```console
$ curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh -s -- --dir "$HOME/bin"
```

### Pinned installation

For reproducibility, pin **both** the installer script's ref (the branch/tag the `curl` URL
fetches) and the binary version (`--version`) to the same release tag — pinning only one leaves
the other floating:

```console
$ curl -fsSL https://raw.githubusercontent.com/doodla/iris/v0.1.0/install.sh | sh -s -- --version v0.1.0
```

### Download-and-inspect, instead of piping into a shell

If you'd rather read the script before running it:

```console
$ curl -fsSLO https://raw.githubusercontent.com/doodla/iris/main/install.sh
$ less install.sh
$ sh install.sh --version v0.1.0
```

**What the checksum proves, and what it doesn't:** `SHA256SUMS` is published in the same GitHub
release as the archive it checksums. Verifying against it confirms the archive you downloaded
matches what the release actually contains — it catches a corrupted or truncated download. It is
**not independent authenticity**: both files come from the same origin, so this does not protect
against a compromised release itself. Use GitHub's own release provenance if you need that
guarantee.

### Upgrade

Re-run the installer (with or without `--version`); the new binary replaces the old one by an
atomic rename, and a failure partway through leaves the previous `iris` untouched.

### Uninstall

```console
$ rm ~/.local/bin/iris        # or wherever --dir / IRIS_INSTALL_DIR pointed
```

Removing the executable is a **separate action** from deleting your configuration and job
history:

```console
$ iris config path
config file: ~/.config/iris/config.toml
state dir:   ~/.local/state/iris
jobs dir:    ~/.local/state/iris/jobs
$ rm ~/.config/iris/config.toml
$ rm -rf ~/.local/state/iris
```

`iris jobs delete --all` (see [jobs.md](jobs.md#local-deletion-vs-remote-state)) removes only
*local job records* — it is not a substitute for deleting the state directory, and neither of
these ever cancels or deletes anything on a provider.

## Testing the installer without a real release

Iris's own test suite (`tests/installer/run.sh`, `make-fixtures.sh`, `server.py`) builds fake
release archives and `SHA256SUMS` files, serves them from `127.0.0.1` only, and drives
`install.sh` against them with `env -i` (a clean environment: no ambient proxies or credentials),
using `uname`/`sysctl` shims to simulate Linux and macOS platform detection. Run it yourself:

```console
$ sh tests/installer/run.sh
```

Real output from this repository, captured for this documentation (96 cases, all offline, no
network beyond `127.0.0.1`):

```
ok   wget: server error 500
ok   wget: connection dropped without a response (retries are bounded)
ok   wget: archive download cut off halfway: old iris kept
ok   wget: connection refused
ok   wget: checksum mismatch, old iris kept
ok   shasum instead of sha256sum
ok   shasum: checksum mismatch, old iris kept
ok   piped: cat install.sh | sh -s -- --version v0.1.0 --dir DIR
ok   piped with defaults: cat install.sh | sh
ok   stdin with data is never read (the fake iris fails if it can read stdin)
ok   PATH hint for bash on Linux
ok   PATH hint for bash on macOS
ok   upgrade replaces the old iris by rename, not in place
ok   upgrade from v0.1.0 to latest in two runs
ok   chmod fails after staging: staged file removed, old iris kept
ok   mv fails after staging: staged file removed, old iris kept
...
96 passed, 0 failed
```

This is what "the installer is tested offline" actually means for this project: real checksum
mismatches, real truncated downloads, real permission failures — all against a local HTTP server,
never a live GitHub release.
