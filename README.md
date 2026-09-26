# Iris

Iris is a command-line tool that generates images and videos with OpenAI and Google models. You can
also use it to edit images. It gives you and your agents one interface across providers, with JSON
output, cost estimates before you spend, and video jobs that you can resume from any process.

```sh
iris image generate -m gpt-image-2.5-sunburst "a watercolor fox in a misty forest" -o fox.png
```

> [!IMPORTANT]
> Iris calls paid APIs with your own API keys, and each provider bills your API account. A ChatGPT,
> Gemini app, or Google AI subscription doesn't include API access.

## Why Iris

- **One CLI for OpenAI and Google.** Generate and edit images with GPT Image and Nano Banana, and
  generate videos with Veo, with the same commands and flags.
- **Built for agents.** With `--json`, every command prints one JSON document that follows a
  published schema, and every error has a stable code and exit code.
- **Costs before you pay.** A dry run checks a request and estimates its cost without sending it,
  and `--max-cost` refuses any request that's estimated above your limit.
- **Video jobs that survive.** Iris records each video job before it submits it, so you can resume
  the job from any later process, even after a crash.
- **Careful with paid requests.** Iris never resends a request that might have been billed, and it
  keeps every image that you paid for. See
  [How Iris handles paid requests](docs/concepts/paid-requests.md).

## Install

On Linux (x86_64) or macOS, run:

```sh
curl -fsSL https://raw.githubusercontent.com/doodla/iris/main/install.sh | sh
```

The installer verifies the release's checksum, and installs `iris` in `~/.local/bin` without
`sudo`. To pin a version, build from source, or uninstall, see
[Install Iris](docs/guides/install.md).

## Quickstart

1. Set the API key for each provider that you use:

   ```sh
   export OPENAI_API_KEY="OPENAI_KEY"
   export GEMINI_API_KEY="GEMINI_KEY"
   ```

2. Check your setup:

   ```sh
   iris doctor
   ```

3. Generate an image:

   ```sh
   iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low -o bike.png
   ```

   The output is similar to the following:

   ```text
   Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
   Saved /home/you/bike.png
   Estimated cost: ~$0.00613 USD (estimate from reported usage (gpt-image-2.5-sunburst): 50 text input tokens × $5.00/1M + 0 image input tokens × $8.00/1M + 196 output tokens × $30.00/1M; cached-input discounts not reported)
   ```

   To check a request and its cost before you pay for it, add `--dry-run`.

4. Generate a video. Iris submits the job, waits for it, and saves the video:

   ```sh
   iris video generate -m veo-lite "waves crashing at dusk" --duration 4 -o waves.mp4
   ```

   If the wait is interrupted, the job keeps running. To resume, run `iris jobs wait JOB_ID`.

## Use Iris from scripts and agents

Add `--json` to any command. Iris prints exactly one JSON document on stdout, and sends progress to
stderr. The exit code tells you what kind of failure happened, without parsing text.

For a video workflow that survives crashes without paying twice, see
[Use Iris in scripts and agents](docs/guides/agents.md).

## Models

Iris doesn't choose a model for you. It supports these models:

| Provider | Models | Operations |
|---|---|---|
| OpenAI | GPT Image 2.5 Sunburst, GPT Image 2.5 Flare, GPT Image 2 | Generate and edit images |
| Google | Nano Banana 2, Nano Banana 2 Lite, Nano Banana Pro | Generate and edit images |
| Google | Veo 3.1, Veo 3.1 Fast, and Veo 3.1 Lite, in preview | Generate videos |

To compare what each model is for and what it costs, run `iris models list`. Then pass the model
with `-m`, or set a default in the config file. See
[Choose a model and control costs](docs/guides/models-and-costs.md).

## Documentation

### Guides

- [Install Iris](docs/guides/install.md)
- [Generate and edit images](docs/guides/images.md)
- [Generate videos](docs/guides/videos.md)
- [Choose a model and control costs](docs/guides/models-and-costs.md)
- [Use Iris in scripts and agents](docs/guides/agents.md)

### Concepts

- [How Iris handles paid requests](docs/concepts/paid-requests.md)
- [How video jobs work](docs/concepts/video-jobs.md)
- [Security and privacy](docs/concepts/security-and-privacy.md)

### Reference

- [CLI reference](docs/reference/cli.md)
- [JSON output reference](docs/reference/json-output.md)
- [Errors reference](docs/reference/errors.md)
- [Configuration reference](docs/reference/configuration.md)

For every page, including the contributor docs, see the [documentation index](docs/README.md).

## Project status

Iris 0.1.0 is the first release. The command line, the JSON output, and the error codes are
versioned, and changes to them follow the
[versioning policy](docs/reference/json-output.md#versioning). Iris runs on Linux (x86_64) and
macOS. It doesn't support Windows. The Veo models are previews, so Google can change their behavior
and limits. For what changed in each release, see the [changelog](CHANGELOG.md).

## Contributing

Contributions are welcome. To build Iris, run the offline test suite, and propose a change, see the
[contributing guide](https://github.com/doodla/iris/blob/main/CONTRIBUTING.md). To report a
vulnerability, see the [security policy](https://github.com/doodla/iris/blob/main/SECURITY.md).

## License

Iris is licensed under the [MIT License](LICENSE).
