# Install Iris

This guide shows how to install Iris, set your API keys, and check your setup. It also covers
pinned installs, building from source, upgrades, and uninstalling.

## Supported platforms

| Platform | Release target | Minimum version |
|---|---|---|
| Linux x86_64 | `x86_64-unknown-linux-musl` | Linux kernel 3.2. The binary is statically linked, so it doesn't depend on the system's C library. |
| macOS on Intel | `x86_64-apple-darwin` | macOS 10.12 Sierra |
| macOS on Apple silicon | `aarch64-apple-darwin` | macOS 11.0 Big Sur |

Iris doesn't support other systems, such as Windows, Linux on arm64, BSDs, or 32-bit systems. The
installer stops on them with a message that names what it detected.

## Install with the installer

Run the installer:

```sh
curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
```

The installer installs `iris` to `~/.local/bin` without `sudo`. It prints the installed version and,
if that directory isn't on your `PATH`, the line to add to your shell's startup file.

The installer is a short POSIX `sh` script. It does the following:

1. Detects your platform. On a Mac with Apple silicon, it installs the Apple silicon build even
   when the shell runs under Rosetta.
2. Checks for `curl` or `wget`, `tar`, and `sha256sum` or `shasum`, and names any that are missing.
3. Finds the latest release by following GitHub's `releases/latest` redirect, so it needs no API
   token and isn't rate-limited.
4. Downloads the release archive and its `SHA256SUMS` file into a private temporary directory,
   which it removes on exit, even after a failure or Ctrl+C.
5. Verifies the archive's SHA-256 checksum. A missing or wrong checksum stops the install.
6. Lists the archive before extracting it, and stops on anything unexpected, such as an absolute
   path, a `..` entry, or a link.
7. Runs the extracted `iris --version` to check that it works on your machine.
8. Copies `iris` to the install directory under a temporary name, then renames it over any
   existing `iris`, so the replacement is atomic.

The installer never reads from standard input, so it's safe to pipe into `sh`. If any step fails,
nothing is installed, and an existing `iris` stays as it was.

If your system has BusyBox `wget` without certificate checks, the installer stops before it uses
anything that it downloaded, because the download could be tampered with. Install `curl` instead.

### Installer options

```text
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

To pass options through the pipe, add `-s --` before them, so that `sh` passes them to the script:

```sh
curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh -s -- --dir "$HOME/bin"
```

### Pin a version

To install the same version every time, pin both the installer's URL and the version to the same
release tag. Pinning only one of them leaves the other one unpinned.

```sh
curl -fsSL https://raw.githubusercontent.com/doodla/iris/v0.1.0/install.sh | sh -s -- --version v0.1.0
```

### Read the installer before you run it

To inspect the script first, download it, read it, and then run it:

```sh
curl -fsSLO https://raw.githubusercontent.com/doodla/iris/v0.1.0/install.sh
less install.sh
sh install.sh --version v0.1.0
```

> [!NOTE]
> The checksum proves that you downloaded the archive that the release contains. It doesn't prove
> who made the release, because the archive and `SHA256SUMS` come from the same place. Iris
> releases don't include signatures or provenance attestations. If you need stronger assurance,
> verify the tag and the release on GitHub.

## Install a release archive by hand

Each release archive contains one directory, `iris-vX.Y.Z-TARGET/`, with the `iris` binary, its
license, `THIRD-PARTY-LICENSES`, `README.md`, `CHANGELOG.md`, and this documentation. To install
it, copy `iris` to a directory on your `PATH`.

On macOS, Gatekeeper refuses to run a binary from an archive that you downloaded with a web
browser, because Iris releases aren't signed or notarized by Apple. Download the archive with
`curl -fLO` instead, or remove the quarantine mark from the unpacked binary:

```sh
xattr -d com.apple.quarantine ./iris
```

The installer isn't affected, because `curl` and `wget` don't mark downloads.

## Build from source

To build Iris from source, you need Rust 1.89 or later. [rustup](https://rustup.rs) is the easiest
way to get it.

```sh
git clone https://github.com/doodla/iris && cd iris
cargo install --locked --path .
```

`--locked` builds with the dependency versions in the committed `Cargo.lock`. Cargo installs
`iris` in its `bin` directory, `~/.cargo/bin` by default.

## Set your API keys

Iris reads API keys only from environment variables. Set the key for each provider that you use:

```sh
export OPENAI_API_KEY="OPENAI_KEY"
export GEMINI_API_KEY="GEMINI_KEY"
```

Replace the following:

- `OPENAI_KEY`: an API key from the OpenAI API platform. You need it for the GPT Image models.
- `GEMINI_KEY`: an API key from Google AI Studio. You need it for the Nano Banana and Veo models.

> [!IMPORTANT]
> Each provider bills your API account for every request. A ChatGPT, Gemini app, or Google AI
> subscription doesn't include API access. For the access and billing that each model requires,
> see [Choose a model and control costs](models-and-costs.md#check-your-access).

## Check your setup

Run `iris doctor`:

```sh
iris doctor
```

The output is similar to the following:

```text
[ok]      config: no config file at /home/you/.config/iris/config.toml (it is optional)
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
[ok]      state_dir: state directory /home/you/.local/state/iris does not exist yet; it will be created on first use
[ok]      output_dir: output directory /home/you is writable
[ok]      base_url.openai: openai API base URL is the default (https://api.openai.com/v1)
[ok]      base_url.gemini: gemini API base URL is the default (https://generativelanguage.googleapis.com/)
[ok]      jobs: 0 local job record(s) readable
Healthy.
```

`iris doctor` reports only whether each key is set, never its value. To also check that your keys
can see each model, add `--check-access`. It makes one free metadata request per model.

> [!NOTE]
> Iris uses your system's trusted certificates for HTTPS. On a minimal container image, install
> `ca-certificates`, or your distribution's equivalent, or every request fails certificate checks.

## Verify what you installed

To see the version, the build target, and the commit that the binary was built from, run:

```sh
iris --json version
```

For a Linux release archive, the output is similar to the following:

```text
{"command":"version","error":null,"ok":true,"result":{"git_commit":"<40 hex digits>","name":"iris","schema_version":1,"target":"x86_64-unknown-linux-musl","version":"0.1.0"},"schema_version":1,"warnings":[]}
```

A release archive reports the commit that its tag points to. To check it, compare `git_commit` with
the output of `git rev-parse vX.Y.Z^{commit}` in a clone of the repository.

A binary that you build yourself reports `null`, unless you name the commit when you build it:

```sh
IRIS_GIT_COMMIT=$(git rev-parse HEAD) cargo install --locked --path .
```

## Install shell completions

Iris prints completion scripts for bash, zsh, fish, and elvish. For example:

```sh
iris completions bash > ~/.local/share/bash-completion/completions/iris
iris completions zsh > "${fpath[1]}/_iris"
iris completions fish > ~/.config/fish/completions/iris.fish
```

## Upgrade

Run the installer again, with or without `--version`. The new binary replaces the old one with an
atomic rename, so a failed upgrade leaves the previous `iris` as it was.

## Uninstall Iris

1. Remove the binary. If you installed it with `--dir`, remove it from that directory instead.

   ```sh
   rm ~/.local/bin/iris
   ```

2. Optional: to delete your configuration and job history, print their paths:

   ```sh
   iris config path
   ```

   Then delete the config file and the state directory that it names. The state directory's
   `unsaved` folder can hold paid outputs that Iris couldn't save where you asked, so move them out
   first.

> [!WARNING]
> On macOS, the config file is inside the state directory, `~/Library/Application Support/iris`.
> Deleting the state directory also deletes your config file. To delete only the job history,
> delete its `jobs` folder.

`iris jobs delete --all` deletes only local job records, and neither it nor uninstalling cancels or
deletes anything at a provider. See
[Deleting job records](../concepts/video-jobs.md#deleting-job-records).

## What's next

- [Generate and edit images](images.md)
- [Generate videos](videos.md)
- [Choose a model and control costs](models-and-costs.md)
- [Use Iris in scripts and agents](agents.md)
