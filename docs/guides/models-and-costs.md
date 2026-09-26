# Choose a model and control costs

Iris never chooses a model for you, because models differ several-fold in price and in what they
accept. This guide shows how to compare models, choose one for a command or as a default, check
your access to it, and estimate and cap what a request costs.

## Compare models

To list the models that Iris knows, run `iris models list`. The output is similar to the following:

```text
MODEL                          PROVIDER  LIFECYCLE  OPERATIONS                  ALIASES
gpt-image-2.5-sunburst         openai    ga         image.generate, image.edit  gpt-image-2.5-sunburst-2026-09-08
  OpenAI's most capable image model, for workflows where editing precision matters most
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.01317 (medium), ~$0.05268 (high),
  ~$0.09366 (xhigh), ~$0.21072 (max)
gpt-image-2.5-flare            openai    ga         image.generate, image.edit  gpt-image-2.5-flare-2026-09-08
  OpenAI's fastest image model, for fast, high-quality everyday generation, at the same token rates
  as Sunburst
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.01317 (medium), ~$0.05268 (high),
  ~$0.09366 (xhigh), ~$0.21072 (max)
gpt-image-2                    openai    ga         image.generate, image.edit  gpt-image-2-2026-04-21
  The earlier GPT Image model; OpenAI says to use a 2.5 model for new integrations. Quality up to
  high, and at medium and high about 4x the 2.5 models' output tokens (OpenAI's calculator,
  indicative for 2.5)
  paid; one 1024x1024 image: ~$0.00588 (quality=low), ~$0.05268 (medium), ~$0.21072 (high)
gemini-3.1-flash-image         gemini    ga         image.generate, image.edit  nano-banana-2
  Google's most versatile image model, balancing speed with 4K output, world knowledge and text
  rendering; good with multiple reference images
  paid; one 1024x1024 image: ~$0.067
gemini-3.1-flash-lite-image    gemini    ga         image.generate, image.edit  nano-banana-2-lite
  Google's fastest and cheapest image model: 1K only, and not optimized for multiple reference
  images or multi-turn editing
  paid; one 1024x1024 image: ~$0.0336
gemini-3-pro-image             gemini    ga         image.generate, image.edit  nano-banana-pro
  Google's premium image model for the most complex visual tasks and professional assets; the
  highest per-image price at each resolution
  paid; one 1024x1024 image: ~$0.134
veo-3.1-fast-generate-preview  gemini    preview    video.generate              veo-fast
  Veo 3.1 optimized for speed: every Veo option Iris offers, 4k and reference images included, at a
  lower per-second price than Veo 3.1 Standard
  paid; one 8-second 720p video: ~$0.80
veo-3.1-generate-preview       gemini    preview    video.generate              veo
  Veo 3.1 Standard, which Google calls best for professional-grade 4K output and complex camera
  movements; every Veo option Iris offers, at the highest per-second price
  paid; one 8-second 720p video: ~$3.20
veo-3.1-lite-generate-preview  gemini    preview    video.generate              veo-lite
  The lowest-priced Veo model: up to 1080p, with no 4k, no reference images, and no negative prompt
  paid; one 8-second 720p video: ~$0.40
```

Under each model, Iris shows what the model is for, from the provider's documentation, and what the
same output costs with it. To narrow the list, add `--operation`, such as
`--operation video.generate`, or `--provider`.

### Compare costs

Iris prices every model on the same output, so that the costs compare: one 1024x1024 image for the
image models, and one 8-second 720p video for the video models. For the OpenAI models, the quality
sets the price, so Iris shows the price at every quality.

These are estimates from the published prices, for that one output. Other sizes, qualities, and
durations cost more or less. The OpenAI models follow OpenAI's price calculator, by which a larger
image that isn't square can cost less than a smaller square one. For the exact estimate of a
request, use a [dry run](#estimate-a-cost-before-you-run).

## Inspect a model

To see everything about one model, run `iris models show MODEL`, with a model ID or alias:

```sh
iris models show veo-lite
```

The output lists the model's operations, inputs, every option with its flag and default, the rules
that relate options, its published prices, and its access requirements. For example, part of the
output for `veo-lite` is:

```text
  options:
    --count: 1..=1 (default 1) [video.generate]
      Number of videos. Veo generates one video per request; the value is validated locally and never sent
    --duration: 4|6|8 (default 8) [video.generate]
      Video length in seconds (always sent). 1080p, 4k, and reference images require 8. Audio is always generated and cannot be disabled on the Gemini API
...
  constraints:
    - resolution 1080p or 4k requires duration 8 (the default) [high_resolution_requires_duration_8]
    - a last frame requires a first frame (--image) [last_frame_requires_first_frame]
...
  pricing (published prices; Iris shows estimates only):
    $0.05 per second — 720p video with audio; only charged if generated (as of 2026-09-24, https://ai.google.dev/gemini-api/docs/pricing)
    $0.08 per second — 1080p video with audio; only charged if generated (as of 2026-09-24, https://ai.google.dev/gemini-api/docs/pricing)
```

## Choose the model for a command

Pass the model's ID or alias with `-m` or `--model`:

```sh
iris image generate -m nano-banana-2 "a watercolor fox"
```

The model determines the provider, so the generation commands have no `--provider` flag.

A generation command without a model fails before it sends anything, and suggests how to choose
one:

```text
error[model_required]: image.generate needs a model: pass -m/--model, or set model in the [image] table of the config file
  hint: run `iris models list --operation image.generate` and pass -m <MODEL>, or set model under [image] in /home/you/.config/iris/config.toml
```

With `--json`, the error also lists every model that you could use, with its summary and cost. See
[`model_required`](../reference/errors.md#model_required).

If you mistype a model, Iris suggests the models that you might mean:

```text
error[unknown_model]: unknown model 'gpt-image-2.5'
  hint: did you mean gpt-image-2.5-sunburst or gpt-image-2.5-flare? otherwise run `iris models list --operation image.generate` and pass -m <MODEL>
```

If you name a model that the provider retired, Iris says why and what to use instead:

```text
error[unknown_model]: unknown model 'dall-e-3'
  hint: OpenAI removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12 and recommends a GPT Image 2.5 model for new integrations; use gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2
```

## Set a default model

To avoid passing `-m` every time, name a model in the
[config file](../reference/configuration.md#config-file):

```toml
[image]
model = "nano-banana-2"

[video]
model = "veo-lite"
```

`-m` still overrides the config file. Iris reports which one chose the model: human output adds
`(config image.model)` after the model's name, and JSON output has `model_source: "config"`. For
example, a dry run with the config file above shows:

```text
Dry run: nothing was sent and nothing was charged.
  operation:  image.generate
  provider:   gemini
  model:      gemini-3.1-flash-image (config image.model)
  ...
```

## Use a model that Iris doesn't know yet

When a provider releases a model before Iris adds it to its catalog, you can use it with
`--capabilities-from`, which names a known model with the same capabilities:

```sh
iris image generate -m gpt-image-2.6-preview --capabilities-from gpt-image-2.5-sunburst "a fox" \
  --size 1024x1024 --quality low
```

In this example, `gpt-image-2.6-preview` stands for a new model ID. Iris sends the ID as you typed
it, checks the request as if it were the known model, and reports an
`unverified_model_capabilities` warning. For Gemini and Veo, the ID can contain only letters,
digits, `.`, `_`, and `-`. Iris doesn't estimate the cost of such a model, because the known model's
prices might not apply. Without an estimate, you can't combine it with `--max-cost`.

## Set model options

Iris has typed flags for common options, such as `--size`, `--quality`, `--aspect-ratio`, and
`--duration`. Options without a typed flag are available with `-O NAME=VALUE`, which you can repeat.
To list every option of a model, and whether it has a typed flag, run `iris models show MODEL`.

If you type an `-O` option as a flag, the error ends with a hint that shows the right form:

```text
hint: did you mean -O background=VALUE? background is a model option without a flag of its own; run the command with --help for usage
```

Iris checks every option before it sends anything:

- A model that doesn't accept the option fails with `unsupported_option`, which names the models
  that do.
- A value must match one of the listed values exactly, including case. For example,
  `--resolution 4k` fails with a hint: `did you mean 4K?`.
- Rules that relate options are enforced too. For example, Veo requires an 8-second duration for
  1080p video.

## Estimate a cost before you run

To check a request and estimate its cost without sending it, add `--dry-run`:

```sh
iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low --dry-run
```

The output is similar to the following:

```text
Dry run: nothing was sent and nothing was charged.
  operation:  image.generate
  provider:   openai
  model:      gpt-image-2.5-sunburst
  async job:  no
  billing:    paid (requests are billed to the provider account at its published prices; no free tier)
  options:    background=auto compression=100 count=1 format=png moderation=auto quality=low size=1024x1024
  output:     /home/you/iris-<ulid>.png
  credential: OPENAI_API_KEY is set
  cost:       ~$0.00588 USD (estimate: 1 image × 196 output tokens × $30.00/1M (gpt-image-2.5-sunburst, low, 1024x1024); OpenAI calculator formula (indicative for GPT Image 2.5); prompt and input-image tokens not included)
```

A dry run makes every local check that the real run makes: the model, the options, the input files,
and the output paths. It doesn't need an API key. The `cost` line shows what the estimate is based
on and what it leaves out, such as prompt tokens.

If the model chooses the size or quality, as OpenAI's models do with the default `auto`, Iris can't
estimate the cost. It shows a warning that names the options to set:

```text
warning[cost_estimate_unavailable]: no cost estimate: quality and size are auto, so the model chooses them and the cost is unknown before the call; pass --quality (low, medium, high, xhigh, or max) and --size WIDTHxHEIGHT (such as 1024x1024) for an estimate
```

## Cap what a command spends

To refuse any request whose estimate is above an amount, add `--max-cost` with the amount in US
dollars:

```sh
iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low --max-cost 0.005
```

The output is similar to the following:

```text
error[cost_limit_exceeded]: the request is estimated at $0.00588 USD, above --max-cost $0.005
  hint: choose cheaper options or a cheaper model (`iris models list` compares the models' costs on the same output), or raise --max-cost
```

The command fails with exit code 2 before it sends anything, in a dry run too. A request without an
estimate also fails, because Iris can't check it. A request estimated at exactly the cap is sent.

> [!NOTE]
> `--max-cost` compares the estimate, which can leave out prompt, input-image, and thinking tokens.
> Your bill can be higher than the cap by what the estimate leaves out.

`--max-cost` applies only to the command that you pass it to: no config key or environment variable
sets it.

## Check your access

> [!IMPORTANT]
> Each provider bills its API usage separately, to your own API account. A ChatGPT Plus or Pro
> subscription, the Gemini app, a Google AI plan, or a Google Flow subscription doesn't include API
> access.

The models also have these requirements:

- The Gemini image models and Veo have no free tier. Your key's project needs a paid-tier billing
  plan, and on Prepay, a positive credit balance. Google Developer Program Cloud credits can pay for
  Gemini API usage.
- Google says the Gemini API rejects standard API keys starting in September 2026, without naming a
  day, and it already rejects unrestricted standard keys. Use an auth API key.
- The GPT Image models need a paid OpenAI usage tier, and OpenAI might require API Organization
  Verification for them.

`iris models show MODEL` lists a model's documented access requirements. To check that your key
can see a model, add `--check-access`. To check every model of each provider whose key is set, run
`iris doctor --check-access`. Each check is one free metadata request. It confirms only that the
model is visible to your key, not your billing tier, prepaid credit, or organization verification,
so a paid request can still fail.

## What's next

- [Generate and edit images](images.md)
- [Generate videos](videos.md)
- [How Iris handles paid requests](../concepts/paid-requests.md)
