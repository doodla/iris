# Changelog

All notable changes to this project are documented in this file. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and Iris intends to follow
[Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

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
- **Careful paid requests.** Iris retries a paid request only when it provably wasn't processed,
  reports an uncertain outcome with exit code 5 instead of sending it again, and keeps every paid
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
  redirects. It redacts signed URLs in all output. See
  [Security and privacy](https://github.com/doodla/iris/blob/main/docs/concepts/security-and-privacy.md).
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
