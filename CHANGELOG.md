# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Iris intends to follow
[Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Added

- **Linux arm64 releases.** Each release includes `aarch64-unknown-linux-musl`, a static binary
  for arm64 Linux, such as ARM servers and the Linux containers that Docker runs on Apple silicon,
  and the installer installs it there. See
  [Install Iris](https://github.com/doodla/iris/blob/main/docs/guides/install.md).

### Changed

- **The installer falls back to git tags.** When GitHub's `releases/latest` page can't be read and
  `git` is installed, `install.sh` installs the highest `vX.Y.Z` tag that has a published release,
  and warns that it did. See [Install Iris](https://github.com/doodla/iris/blob/main/docs/guides/install.md).

### Fixed

- **A byte order mark at the start of a prompt file isn't sent.** Some editors, Notepad among
  them, start a UTF-8 file with one. Iris sent it to the provider as part of the prompt, and a
  file that held only one passed as a non-empty prompt. Iris now removes it, from `--prompt-file`
  and `--prompt-stdin` alike.
- **Usage errors in human mode redact API keys.** A usage error quotes the argument that it's
  about, so a key typed as an argument value, such as `--limit` followed by a pasted key, was
  printed on stderr. JSON mode already redacted it. Both modes now show `[REDACTED]`.
- **A wait limit too large to use is refused before anything is sent.** An enormous wait limit or
  poll interval, such as `--timeout 18446744073709551615` or a `wait_timeout` of the largest TOML
  integer, was accepted, and `video generate` then stopped with `internal_error` after its paid
  job had been submitted. Every duration setting is now at most a year, and a longer one fails
  with `invalid_argument` or `config_invalid` before anything is sent. See
  [Configuration reference](https://github.com/doodla/iris/blob/main/docs/reference/configuration.md#settings).
- **Redaction replaces a credential whole when it contains the other one.** If one API key was a
  prefix or substring of the other, output could show the longer key's remainder next to
  `[REDACTED]`. Iris now replaces longer credentials first.

## [0.1.0] - 2026-09-25

The first release of Iris, a command-line tool that generates images and videos with OpenAI and
Google models, for people and for agents.

### Added

- **Image generation and editing** with OpenAI's GPT Image 2.5 Sunburst, GPT Image 2.5 Flare, and
  GPT Image 2, and Google's Nano Banana 2, Nano Banana 2 Lite, and Nano Banana Pro. Edits can use
  several reference images and, with OpenAI, a mask. See
  [Generate and edit images](https://github.com/doodla/iris/blob/main/docs/guides/images.md).
- **Video generation** with Google's Veo 3.1, Veo 3.1 Fast, and Veo 3.1 Lite, in preview, with first
  frames, last frames, and reference images where the model supports them. Iris tracks each video
  as a job that any later process can follow, wait for, and download. See
  [Generate videos](https://github.com/doodla/iris/blob/main/docs/guides/videos.md).
- **Explicit models.** Every generation command names its model, with `-m` or in the config file.
  `iris models list` compares what each model is for and what the same output costs, and
  `iris models show` lists a model's options, prices, and access requirements. See
  [Choose a model and control costs](https://github.com/doodla/iris/blob/main/docs/guides/models-and-costs.md).
- **Cost control.** `--dry-run` makes every local check and estimates the cost without sending
  anything, and `--max-cost` refuses a request whose estimate is above a cap. Every cost figure is
  labeled as an estimate.
- **Paid requests sent once.** Iris retries a paid request only when it provably wasn't processed,
  and reports an uncertain outcome with exit code 5 instead of sending it again. It keeps every paid
  output, even one that it can't save where you asked. See
  [How Iris handles paid requests](https://github.com/doodla/iris/blob/main/docs/concepts/paid-requests.md).
- **Durable video jobs.** Iris writes each job record before it submits the job, atomically and
  under locks, so a job survives Ctrl+C, wait limits, and crashes. Downloads never generate a video
  again, are safe to repeat, and are validated. A label (`--label`) lets a rerun after a crash find
  the job instead of paying twice. See
  [How video jobs work](https://github.com/doodla/iris/blob/main/docs/concepts/video-jobs.md).
- **A machine-readable contract.** With `--json`, every command prints one envelope that follows a
  published JSON Schema (`iris schema`), with stable error codes, warning codes, and exit codes. See
  the [JSON output reference](https://github.com/doodla/iris/blob/main/docs/reference/json-output.md)
  and the [Errors reference](https://github.com/doodla/iris/blob/main/docs/reference/errors.md).
- **Setup and diagnostics:** `iris doctor`, `iris config show`, `iris config path`,
  `iris providers list`, shell completions for bash, zsh, fish, and elvish, and `iris version`.
- **Security.** Iris reads API keys only from `OPENAI_API_KEY` and `GEMINI_API_KEY`, never prints,
  logs, or stores them, and sends them only to the configured API origin, including across
  redirects. It redacts signed URLs in all output. See the
  [configuration reference](https://github.com/doodla/iris/blob/main/docs/reference/configuration.md#api-keys).
- **Distribution:** a checksum-verifying installer for Linux x86_64 and macOS, reproducible release
  archives with third-party license notices, and documentation organized as guides, concepts, and
  reference. See [Install Iris](https://github.com/doodla/iris/blob/main/docs/guides/install.md).

### Known limitations

- Iris can't recover an image request whose connection was lost after the provider accepted it:
  unlike a video, it has no job to resume.
- Veo audio can't be turned off, Veo keeps outputs for about 2 days, and every Veo model is a
  preview.
- Veo 3.1 Lite takes no negative prompt, and no Veo model takes one together with reference images.
  Iris refuses both before sending.
- Iris can't cancel a job at the provider. `iris jobs delete` removes only the local record.
- Windows isn't supported.
- The macOS archives aren't signed or notarized. See
  [Install a release archive by hand](https://github.com/doodla/iris/blob/main/docs/guides/install.md#install-a-release-archive-by-hand).

[Unreleased]: https://github.com/doodla/iris/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/doodla/iris/releases/tag/v0.1.0
