# Generate and edit images

This guide shows how to generate an image from a prompt, edit images, and choose where Iris saves
them. Image commands are synchronous: Iris saves the image before the command returns.

## Before you begin

- [Install Iris](install.md) and set the API key for your provider.
- Choose a model. Each command names its model with `-m`, or uses the model in your config file.
  To compare models, see [Choose a model and control costs](models-and-costs.md).

## Generate an image

To generate an image, pass a model, a prompt, and an output path:

```sh
iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low -o bike.png
```

The output is similar to the following:

```text
Requesting 1 image from openai (gpt-image-2.5-sunburst); this is a paid request
Saved /home/you/bike.png
Estimated cost: ~$0.00613 USD (estimate from reported usage (gpt-image-2.5-sunburst): 50 text input tokens × $5.00/1M + 0 image input tokens × $8.00/1M + 196 output tokens × $30.00/1M; cached-input discounts not reported)
```

Iris prints the path of each saved image, and an estimate of the cost from the usage that the
provider reported. To check a request and its cost before you pay for it, add `--dry-run`. See
[Estimate a cost before you run](models-and-costs.md#estimate-a-cost-before-you-run).

For the Gemini models, Iris asks Google not to store your requests (`store: false`), even if logging
is turned on for your project.

## Provide the prompt

Give each command exactly one prompt source:

| Source | How |
|---|---|
| An argument | `iris image generate -m nano-banana-2 "a watercolor fox"` |
| A file | `-f PATH` or `--prompt-file PATH`. The file must be UTF-8. Iris trims trailing whitespace. |
| Standard input | `--prompt-stdin`, when standard input isn't a terminal |

For example, to read the prompt from standard input:

```sh
echo "a lighthouse at night" | iris image generate -m nano-banana-2 --prompt-stdin -o lighthouse.png
```

If you give two sources, the command fails with `usage_error`.

## Set image options

Iris has typed flags for the common image options: `--count` (`-n`), `--size`, `--aspect-ratio`,
`--resolution`, `--quality`, and `--format`. Each model accepts only some of them. For example, the
OpenAI models take `--size` and `--quality`, and the Gemini models take `--aspect-ratio` and
`--resolution`:

```sh
iris image generate -m nano-banana-2 --aspect-ratio 16:9 --resolution 2K "a watercolor fox" -o fox.jpg
```

An option that has no typed flag is available with `-O NAME=VALUE`, such as
`-O background=transparent` for the OpenAI models. To list every option of a model, run
`iris models show MODEL`. For more about options, see
[Set model options](models-and-costs.md#set-model-options).

If the model doesn't accept an option, the command fails with `unsupported_option` before it sends
anything, and names the models that do.

## Edit images

To edit an image, pass it with `-i` and describe the change:

```sh
iris image edit -m nano-banana-2 -i photo.png "make it autumn" -o fall.jpg
```

To compose a new image from several images, repeat `-i`:

```sh
iris image edit -m nano-banana-2 -i a.png -i b.png "combine these into one poster"
```

The OpenAI models can also take a mask. The transparent areas of the mask mark where the model
should edit. The mask must be a PNG image with an alpha channel, the same size as the first input
image:

```sh
iris image edit -m gpt-image-2.5-sunburst -i room.png --mask window-mask.png "add a large window" \
  --size 1024x1024 --quality low -o room-window.png
```

Iris checks the input images before it sends anything: their number, type, and size, and the mask's
format. The OpenAI models take up to 16 input images, and the Gemini models up to 14. For a model's
exact limits, run `iris models show MODEL`.

## Choose where images are saved

Without `-o` or `-d`, Iris saves each image in the output directory, with a generated name such as
`iris-01m3ec3f7m8pwemswax64ke6h8.png`. The output directory is `IRIS_OUTPUT_DIR`, `output_dir` in
the config file, or the current directory.

| Flag | Result |
|---|---|
| `-o PATH`, `--output PATH` | Saves to exactly that file. With several images, Iris numbers them from 1: `-n 3 -o p.png` saves `p-1.png`, `p-2.png`, and `p-3.png`. |
| `-d DIR`, `--out-dir DIR` | Saves in that directory, with generated names. Iris creates the directory if it's missing. |
| `--overwrite` | Replaces an existing file at the path. Without it, the command fails with `output_exists`. |

Iris writes images only to files, and it prints each saved path. It refuses `-o -` and output paths
such as `/dev/stdout` or `/dev/null`. With `--json`, the paths are in `result.artifacts[].path`.

> [!NOTE]
> If Iris can't save a paid image where you asked, for example because the disk is full, it saves
> the image in the state directory instead and tells you where. See
> [Paid outputs are kept](../concepts/paid-requests.md#paid-outputs-are-kept).

### Choose the file type

For the OpenAI models, the extension of `-o` chooses the image format: `-o fox.jpg` requests a JPEG.
You can also set the format with `--format`.

The Gemini models don't take a format: the provider chooses the image type. The extension then only
names the file. If the provider returns another type, Iris keeps the name and changes the extension
to match:

```sh
iris image generate -m nano-banana-2 "a watercolor fox" -o fox.jpg
```

The output is similar to the following:

```text
Requesting 1 image from gemini (gemini-3.1-flash-image); this is a paid request
warning[output_extension_may_change]: gemini-3.1-flash-image takes no output format: the provider chooses the image type (image/jpeg, image/png), so /home/you/fox.jpg may be saved as /home/you/fox.png instead (reported with output_extension_adjusted); without --overwrite, a file already there stops the run (output_exists)
warning[output_extension_adjusted]: the provider returned image/png; saving as /home/you/fox.png instead of /home/you/fox.jpg
Saved /home/you/fox.png
Estimated cost: ~$0.067206 USD (12 input tokens × $0.50/1M + 1120 image output tokens × $60.00/1M + 0 text and thinking tokens × $3.00/1M (gemini-3.1-flash-image, from reported usage; image tokens not itemized, all output priced as image))
```

When the provider chooses the type, Iris checks every name that the image might be saved under
before it sends the request. If you run the same command again while `fox.png` exists, it fails
before it pays for another image:

```text
error[output_exists]: output file /home/you/fox.png already exists, and the provider chooses the image type: /home/you/fox.jpg is saved there if it comes back as image/png
```

`--overwrite` replaces only the path that you named. If the image comes back under another
extension and a file with that name exists, Iris saves the image as `STEM.N.EXT` instead, with an
`output_renamed` warning.

## What's next

- [Generate videos](videos.md)
- [Choose a model and control costs](models-and-costs.md)
- [Use Iris in scripts and agents](agents.md)
- Every image flag: [`iris image generate`](../reference/cli.md#iris-image-generate) and
  [`iris image edit`](../reference/cli.md#iris-image-edit) in the CLI reference.
