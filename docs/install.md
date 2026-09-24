# Installing Iris

## Supported platforms and runtime requirements

| platform | target triple | archive name | minimum OS/kernel |
|---|---|---|---|
| Linux x86_64 | `x86_64-unknown-linux-musl` | `iris-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` | any x86_64 Linux kernel ≥ 3.2, regardless of the host's glibc version or its absence — the binary is statically linked (musl libc, static-pie) |
| macOS x86_64 (Intel) | `x86_64-apple-darwin` | `iris-vX.Y.Z-x86_64-apple-darwin.tar.gz` | macOS 10.12+ (Sierra and later) |
| macOS arm64 (Apple silicon) | `aarch64-apple-darwin` | `iris-vX.Y.Z-aarch64-apple-darwin.tar.gz` | macOS 11.0+ (Big Sur and later — also the first macOS version Apple silicon Macs shipped with, so this is not a practical additional restriction) |

Anything else (Linux arm64, Windows/MSYS/Cygwin, a BSD, a 32-bit system) is not supported: the
installer fails immediately with a clear message naming what it detected, rather than silently
installing the wrong archive.

The musl target for Linux is the release target: musl gives a static binary that runs on any
x86_64 Linux kernel, regardless of the host's glibc version or its absence.

Each archive holds a single directory, `iris-vX.Y.Z-<target>/`, containing the `iris` executable,
`LICENSE`, `README.md`, `CHANGELOG.md`, and `docs/` (this documentation, which the README links
to). The installer installs only `iris`; the rest is there for a manual install and for reading
offline. Unpacking an archive by hand works too: copy the `iris` executable anywhere on your
`PATH`.

HTTPS requests (installer download, and every provider API call `iris` itself makes) use the
**system's CA trust store**, not a bundled one. On a minimal container or base image, install
`ca-certificates` (or your distribution's equivalent) first, or TLS verification will fail.

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
   entries, symlinks or other special files, anything outside the single top-level
   `iris-vX.Y.Z-<target>/` directory or besides the files listed above, or a missing `iris`
   executable.
7. Extracts it and runs the extracted `iris --version` to confirm it actually executes on your
   machine before installing anything.
8. Copies `iris`, and nothing else from the archive, into the target directory under a
   temporary name, then renames it over any existing `iris` — an atomic replace on the same
   filesystem. The directory is created if needed. **`sudo` is never used.**
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
$ curl -fsSLO https://raw.githubusercontent.com/doodla/iris/v0.1.0/install.sh
$ less install.sh
$ sh install.sh --version v0.1.0
```

(Pin the installer ref in the `curl` URL to the same tag you pass to `--version` — as in
[Pinned installation](#pinned-installation) above — or drop `--version` entirely and let it default
to `latest`; fetching from `main` while pinning `--version` to an older tag mixes an unpinned
script with a pinned binary.)

**What the checksum proves, and what it doesn't:** `SHA256SUMS` is published in the same GitHub
release as the archive it checksums. Verifying against it confirms the archive you downloaded
matches what the release actually contains — it catches a corrupted or truncated download. It is
**not independent authenticity**: both files come from the same origin, so this does not protect
against a compromised release itself. Iris's release workflow currently publishes no signatures or
provenance attestations for its archives — verify the tag and the release itself on GitHub
yourself if you need stronger assurance of authenticity than "the checksum matches what GitHub
currently serves."

### Upgrade

Re-run the installer (with or without `--version`); the new binary replaces the old one by an
atomic rename, and a failure partway through leaves the previous `iris` untouched.

### Uninstall

```console
$ rm ~/.local/bin/iris        # or wherever --dir / IRIS_INSTALL_DIR pointed
```

Removing the executable is a **separate action** from deleting your configuration and job
history. Run `iris config path` first to see the real, absolute paths on your machine — they
differ by platform:

```console
$ iris config path
config file: ~/.config/iris/config.toml
state dir:   ~/.local/state/iris
jobs dir:    ~/.local/state/iris/jobs
$ rm ~/.config/iris/config.toml
$ rm -rf ~/.local/state/iris
```

**On macOS, be careful: the config file and the state directory are the same directory**
(`~/Library/Application Support/iris`) — `config.toml` lives directly inside it, alongside the
`jobs/` subdirectory. Deleting the whole state directory on macOS also deletes your config file,
which is *not* a separate action there. To delete job history only on macOS, remove the `jobs/`
subdirectory, not the whole state directory, and delete `config.toml` on its own if you also want
that gone:

```console
$ iris config path
config file: ~/Library/Application Support/iris/config.toml
state dir:   ~/Library/Application Support/iris
jobs dir:    ~/Library/Application Support/iris/jobs
$ rm -rf ~/Library/Application\ Support/iris/jobs   # job history only
$ rm ~/Library/Application\ Support/iris/config.toml   # config, separately
```

`iris jobs delete --all` (see [jobs.md](jobs.md#local-deletion-vs-remote-state)) removes only
*local job records* — it is not a substitute for deleting the state directory, and neither of
these ever cancels or deletes anything on a provider.

## Verifying what you installed

`iris --json version` reports the version, the target triple, and `git_commit`, the commit the
binary was built from:

```console
$ iris --json version
{"command":"version","error":null,"ok":true,"result":{"git_commit":"<40 hex digits>","name":"iris","schema_version":1,"target":"x86_64-unknown-linux-musl","version":"0.1.0"},"schema_version":1,"warnings":[]}
```

`git_commit` is fixed at build time: it is the value of `IRIS_GIT_COMMIT`, or else of
`GITHUB_SHA` (which GitHub Actions sets to the commit it checked out), lowercased, when that value
is 7 to 40 hexadecimal digits; otherwise it is `null`. Release archives are built by the release
workflow from the tagged commit, so for them it is the commit the release tag points to — compare
it with `git rev-parse vX.Y.Z^{commit}` in a clone. A binary you build yourself reports `null`
unless you name the commit:

```console
$ IRIS_GIT_COMMIT=$(git rev-parse HEAD) cargo install --locked --path .
```

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
