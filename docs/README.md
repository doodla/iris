# Iris documentation

This index lists every page of the Iris documentation. To install Iris and generate your first
image, start with the [README](../README.md).

## Guides

Step-by-step instructions for common tasks.

| Page | What it covers |
|---|---|
| [Install Iris](guides/install.md) | The installer, pinned versions, building from source, API keys, checking your setup, upgrades, and uninstalling. |
| [Generate and edit images](guides/images.md) | Prompts, image options, edits with reference images and masks, and where images are saved. |
| [Generate videos](guides/videos.md) | Waiting and detaching, following and downloading jobs, labels, and recovering after a crash. |
| [Choose a model and control costs](guides/models-and-costs.md) | Comparing models, choosing a default, dry runs, `--max-cost`, and account access. |
| [Use Iris in scripts and agents](guides/agents.md) | JSON mode, exit codes, and a video workflow that doesn't pay twice. |

## Concepts

How Iris works, and why.

| Page | What it covers |
|---|---|
| [How Iris handles paid requests](concepts/paid-requests.md) | The guarantees, retries, uncertain outcomes, billed errors, and cost estimates. |
| [How video jobs work](concepts/video-jobs.md) | Job records, states, submission, labels, downloads, retention, and deletion. |

## Reference

Complete, exact descriptions.

| Page | What it covers |
|---|---|
| [CLI reference](reference/cli.md) | The help of every command, generated from the CLI itself. |
| [JSON output reference](reference/json-output.md) | The envelope, every result type, and the JSON Schema. |
| [Errors reference](reference/errors.md) | Exit codes, error codes and their details, and warning codes. |
| [Configuration reference](reference/configuration.md) | Every setting, API keys, the config file, paths, time limits, and base URL overrides. |

## Contributing

For people who change Iris. Start with the
[contributing guide](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md).

| Page | What it covers |
|---|---|
| [Architecture](contributing/architecture.md) | The module layers, the provider traits, and where each invariant is enforced. |
| [Decisions](contributing/decisions.md) | Why Iris uses the APIs, models, retry rules, and tools that it does, with sources. |
| [Add a provider](contributing/adding-a-provider.md) | A step-by-step guide and checklist, with a worked example. |
| [Live testing](contributing/live-testing.md) | The paid, opt-in verification against the real APIs, and its log. |
| [Releasing Iris](contributing/releasing.md) | Release archives, the installer tests, and cutting a release. |
| [Documentation style guide](contributing/style-guide.md) | How to write and organize these pages. |
