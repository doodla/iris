# JSON output reference

When you add `--json` to a command, Iris prints one JSON document that describes the result or the
error. This page describes that document: the envelope, the result of each command, and the objects
that results share.

For error, exit, and warning codes, see the [Errors reference](errors.md). For how to use JSON mode
in a script, see [Use Iris in scripts and agents](../guides/agents.md).

## Output rules

With `--json`:

- Every command prints exactly one JSON document on stdout, followed by a newline. Nothing else is
  written to stdout.
- Progress lines and diagnostics go to stderr, as plain text.
- Warnings appear only in the envelope's `warnings` array, not on stderr.
- Iris never prompts for input.

In every document:

- Every documented key is present. A value that can be missing is `null`; its key isn't omitted.
- Keys are sorted alphabetically. Don't rely on their order.
- Timestamps are RFC 3339, in UTC, such as `2026-09-24T16:13:32Z`. Paths are absolute. Sizes are
  integers, in bytes.

Iris prints each document on one line. The examples on this page are formatted for reading, and
some are shortened with `"...": "..."`.

## JSON Schema

Iris publishes a JSON Schema (draft 2020-12) of its output. Iris generates the schema from the same
types that it serializes, and a test checks the committed copy, so the schema can't drift from the
output. To save the schema to a file, run:

```sh
iris schema > iris-output.v1.schema.json
```

`iris --json schema` prints the same schema inside an envelope, as `result.schema`. The schema is
also in the repository as
[`schema/iris-output.v1.schema.json`](https://github.com/doodla/iris/blob/main/schema/iris-output.v1.schema.json).
Its `$id` is `https://raw.githubusercontent.com/doodla/iris/main/schema/iris-output.v1.schema.json`,
and it describes `schema_version` 1.

The schema encodes the rules of the output, not only its shapes:

- Every key that's always present is `required`. A value that can be missing allows `null`.
- `ok: true` requires a result and a `null` error, and `ok: false` requires the reverse.
- A successful envelope's `result` has the result type of its `command`.
- An error's `category` is the one that its `code` belongs to, for every code that the schema lists.
- Error codes, commands, warning codes, provider IDs, and `billing` values are open sets. The schema
  lists the known values and accepts any other value of the same form: a snake_case word, or, for a
  command, snake_case words joined by dots. An envelope from a later version of Iris with the same
  `schema_version` still validates. Every other enumeration, such as `category` or a status, is
  closed.

## Versioning

`schema_version` is the major version of the JSON output as a whole: the envelope, every result
type, the error object, and the code tables. Iris 0.1.0 prints version 1.

- Additive changes keep the version: a new field, a new error or warning code, or a new command.
- Renaming or removing a field or a code, or changing its meaning, increments the version.

To read output from a version that you don't know:

- Check `schema_version`. If you don't recognize it, read the fields that you understand and don't
  assume anything about the others.
- Handle an error code that you don't know by its `category` and exit code.
- Treat a warning code that you don't know as informational text.

## Envelope

Every document is an envelope:

```json
{
  "schema_version": 1,
  "ok": true,
  "command": "image.generate",
  "result": { "...": "..." },
  "error": null,
  "warnings": [ { "code": "cost_estimate_unavailable", "message": "..." } ]
}
```

| Field | Type | Description |
|---|---|---|
| `schema_version` | integer | The version of the JSON output. See [Versioning](#versioning). |
| `ok` | boolean | `true` if the command succeeded. |
| `command` | string or null | The command, such as `image.generate`. `null` if Iris couldn't recognize a command, and for `--help`. |
| `result` | object or null | The command's result, if `ok` is `true`. See [Results](#results). |
| `error` | object or null | The error, if `ok` is `false`. See [Error object](errors.md#error-object). |
| `warnings` | array | Warnings, each with a `code` and a `message`. The array can be non-empty when the command fails. See [Warning codes](errors.md#warning-codes). |

`command` is one of `image.generate`, `image.edit`, `video.generate`, `jobs.list`, `jobs.status`,
`jobs.wait`, `jobs.download`, `jobs.delete`, `models.list`, `models.show`, `providers.list`,
`config.show`, `config.path`, `doctor`, `schema`, `completions`, or `version`. A later version can
add commands.

Some command lines don't run a command:

- If Iris can't parse the command line, it prints an envelope with a `usage_error`, and exits with
  code 2. `command` names the command if Iris recognized one, as in `iris image generate --bogus`.
  This applies whenever `--json` appears anywhere in the arguments.
- `--help` with `--json` prints `ok: true`, `command: null`, and the help text as `result.help`.
- `--version` with `--json` prints the same result as `iris version`, with `command: "version"`.

### Suggested commands

The generation and `jobs` commands suggest commands to run next: in `next_steps`, and in the hints
and warnings of their errors. Run a suggested command in the same environment, with the same state
directory, base URL variables, and keys. If you chose the config file with `--config` or
`IRIS_CONFIG`, the suggested commands include it, such as
`iris --config /home/you/iris.toml jobs wait JOB_ID`, quoted for the shell when needed. Iris adds
nothing else to them.

## Results

### image.generate and image.edit

```json
{
  "provider": "openai", "model": "gpt-image-2.5-sunburst", "model_source": "flag",
  "operation": "image.generate", "status": "succeeded",
  "created_at": "2026-09-26T08:15:05Z", "completed_at": "2026-09-26T08:15:05Z",
  "provider_request_id": null,
  "artifacts": [ { "index": 0, "path": "/home/you/bike.png", "media_type": "image/png",
                   "bytes": 4555, "sha256": "83b0d385...", "width": 1024, "height": 1024,
                   "duration_seconds": null } ],
  "text": null,
  "usage": { "input_tokens": 50, "output_tokens": 196, "total_tokens": 246,
             "provider_usage": { "...": "..." } },
  "cost_estimate": { "estimated": true, "currency": "USD", "amount": 0.00613, "...": "..." }
}
```

| Field | Type | Description |
|---|---|---|
| `provider` | string | The provider of the model. |
| `model` | string | The model ID that Iris sent. |
| `model_source` | string | `flag` if `-m` named the model, or `config` if the config file did. Iris never chooses a model itself. |
| `operation` | string | `image.generate` or `image.edit`. |
| `status` | string | `succeeded`. |
| `created_at`, `completed_at` | string | When the request started and finished. |
| `provider_request_id` | string or null | The provider's ID for the request, if it sent one. |
| `artifacts` | array of [Artifact](#artifact) | The saved images. |
| `text` | string or null | Text that the model returned with the images. A `provider_text_output` warning reports it. |
| `usage` | [Usage](#usage) or null | The usage that the provider reported. |
| `cost_estimate` | [Cost estimate](#cost-estimate) or null | The estimated cost, from the reported usage if there is any. |

### video.generate, jobs.status, jobs.wait, and jobs.download

```json
{ "job": { "...": "..." }, "next_steps": ["iris jobs wait job_01m3ec3srq9zc1vk50301pyxne"] }
```

| Field | Type | Description |
|---|---|---|
| `job` | [Job](#job) | The job. |
| `next_steps` | array of strings | Commands to run next, as given. See [Suggested commands](#suggested-commands). A labeled job that's still active includes `iris jobs list --label LABEL`. |

### jobs.list

`{ "jobs": [Job] }`, newest first. Iris skips a record that it can't read, with a
`job_record_unreadable` warning, instead of failing the whole list. `--label` lists at most one job.
`--status`, `--provider`, and `--limit` filter the list.

### jobs.delete

```json
{
  "deleted": ["job_01m3ec3srq9zc1vk50301pyxne"],
  "remote_effect": "none",
  "note": "Local records only; remote jobs and downloaded files are untouched."
}
```

| Field | Type | Description |
|---|---|---|
| `deleted` | array of strings | The IDs of the deleted job records. |
| `remote_effect` | string | Always `none`: deleting a record doesn't cancel or delete anything at the provider. |
| `note` | string | A sentence that says so. With `--all --force`, it also names the unreadable records that were deleted. |

If Iris refuses a deletion, it deletes nothing and reports `invalid_argument`. See
[`invalid_argument`](errors.md#invalid_argument).

### models.list

`{ "models": [Model] }`, one per catalog model, with these fields:

| Field | Type | Description |
|---|---|---|
| `id` | string | The model ID that Iris sends to the provider. |
| `provider` | string | The model's provider. |
| `display_name` | string | The model's name, such as `GPT Image 2.5 Sunburst`. |
| `summary` | string | What the model is for, and its trade-off, from the provider's documentation. |
| `aliases` | array of strings | Other names that `-m` accepts, such as dated snapshots and nicknames. |
| `lifecycle` | string | `ga`, `preview`, or `deprecated`, as the provider documents it. |
| `billing` | string | `paid` if requests are billed at the provider's published prices. Every model in Iris 0.1.0 is `paid`. The set is open: read a value that you don't know as "requests may cost money". |
| `operations` | array of strings | What the model can do: `image.generate`, `image.edit`, or `video.generate`. |
| `standard_cost` | [Standard cost](#standard-cost) or null | What the same standard output costs with this model. `null` if Iris can't estimate the model's cost before a request. |

### models.show

`{ "model": Model }`, with the fields of [models.list](#modelslist) and the model's full
capabilities. The following example is shortened to the first entry of `options`, `constraints`,
and `pricing`:

```json
{
  "id": "gpt-image-2.5-sunburst", "provider": "openai", "display_name": "GPT Image 2.5 Sunburst",
  "summary": "OpenAI's most capable image model, for workflows where editing precision matters most",
  "aliases": ["gpt-image-2.5-sunburst-2026-09-08"], "lifecycle": "ga", "billing": "paid",
  "operations": ["image.generate", "image.edit"],
  "inputs": { "max_input_images": 16, "input_media_types": ["image/png","image/jpeg","image/webp"],
              "max_input_bytes": 15700000, "mask": true,
              "mask_requirements": { "media_types": ["image/png"], "max_bytes": 4000000,
                                     "alpha_channel_required": true, "same_size_as_first_image": true },
              "first_frame": false, "last_frame": false, "max_reference_images": 0,
              "max_request_bytes": null },
  "options": [ { "name": "count", "type": "integer", "min": 1, "max": 10, "values": null,
                 "syntax": null, "max_chars": null, "default": 1, "flag": "--count",
                 "operations": ["image.generate","image.edit"],
                 "description": "Number of images to produce in one request (sent as `n`)." } ],
  "constraints": [ { "id": "compression_requires_jpeg_or_webp", "options": ["compression", "format"],
                     "inputs": [], "description": "compression applies only to jpeg or webp output (the default format is png)" } ],
  "outputs": { "media_types": ["image/png","image/jpeg","image/webp"], "max_count": 10 },
  "limits": { "max_prompt_chars": 32000 },
  "pricing": [ { "description": "Text input tokens (prompt)", "unit": "1M text input tokens",
                 "usd": 5.0, "source_url": "https://developers.openai.com/api/docs/pricing",
                 "as_of": "2026-09-24" } ],
  "standard_cost": { "...": "..." },
  "access": { "credential_env": "OPENAI_API_KEY", "credential_present": true,
              "requirements": ["API Organization Verification may be required for GPT Image models",
                               "Paid usage tier required (no free-tier limits listed)"],
              "account_access": "not_checked", "checked_at": null },
  "capabilities_source": "catalog", "catalog_as_of": "2026-09-24",
  "docs_url": "https://developers.openai.com/api/docs/guides/image-generation"
}
```

| Field | Description |
|---|---|
| `inputs` | Accepted input images: their number, media types, and size in bytes; whether the model takes a mask (`mask_requirements` is `null` if not); first and last frames; reference images; and `max_request_bytes`, the documented limit on a whole request that carries its inputs inline, or `null`. Iris checks an upper bound of the encoded request size against it before sending. |
| `options` | Every option the model accepts. See [Options](#options). |
| `constraints` | Rules that relate several options or inputs, each with an `id`, the `options` and `inputs` that it involves, and a `description`. Iris enforces every listed rule: a request that breaks one fails with `invalid_argument`, with the rule's ID in `details.constraint`. |
| `outputs` | The media types that the model returns, and the most outputs per request. |
| `limits` | Other limits, such as `max_prompt_chars`. |
| `pricing` | The published prices, each with a `description`, a `unit`, the price in `usd`, a `source_url`, and the date checked (`as_of`). |
| `access` | The API key variable and whether it's set, the documented access `requirements`, and `account_access`. |
| `capabilities_source`, `catalog_as_of` | Where the capabilities come from, and the date that the catalog was checked against the provider's documentation. |
| `docs_url` | The provider's documentation for the model. |

`access.account_access` is `not_checked`, unless you pass `--check-access`. Then Iris makes one
free metadata request and reports `available`, `unavailable`, or `unknown`, and sets `checked_at`.
`available` means only that the model is visible to your key. It doesn't check billing tier,
prepaid credit, or organization verification, so a paid request can still fail with
`permission_denied` or `quota_exceeded`.

#### Options

Each entry of `options` describes one option:

| Field | Description |
|---|---|
| `name` | The option's name, as used with `-O NAME=VALUE`. |
| `flag` | The option's typed flag, such as `--count`, or `null` if it's available only with `-O`. |
| `type` | `enum` (strings from `values`), `integer` (every whole number from `min` to `max`, or only the integers in `values`, such as Veo's `duration`: `[4, 6, 8]`), `boolean`, or `string`. A `string` option either matches a pattern that `syntax` describes, or is free text of at most `max_chars` characters. |
| `default` | The value that applies when you omit the option, typed like its values, or `null` if the provider documents none. |
| `operations` | The operations that accept the option. |
| `description` | What the option does. |

### providers.list

`{ "providers": [Provider] }`:

| Field | Type | Description |
|---|---|---|
| `id` | string | The provider ID, such as `openai` or `gemini`. |
| `display_name` | string | The provider's name. |
| `credential_env` | string | The environment variable that holds the provider's API key. |
| `credential_present` | boolean | Whether that variable is set. Iris never reports its value. |
| `operations` | array of strings | The operations that the provider's models support. |
| `base_url` | string | Where Iris sends the provider's requests, and its API key. |
| `docs_url` | string | The provider's documentation. |

### config.show

```json
{
  "config_file": "/home/you/.config/iris/config.toml",
  "config_file_exists": false,
  "settings": [ { "key": "output_dir", "value": "/home/you", "source": "default", "env_var": "IRIS_OUTPUT_DIR" } ],
  "credentials": [ { "env": "OPENAI_API_KEY", "present": true } ]
}
```

Each setting has its `key`, its `value`, its `source` (`flag`, `env`, `file`, or `default`), and
the environment variable that can set it (`env_var`), or `null`. `credentials` lists each API key
variable and whether it's set, never its value.

### config.path

`{ "config_file", "state_dir", "jobs_dir" }`: absolute paths. The config file doesn't have to exist.

### doctor

`{ "healthy": true, "checks": [ { "id", "status", "message" } ] }`

`status` is `ok`, `warning`, or `error`. `healthy` is `false` only if a check has the status
`error`. `iris doctor` exits with code 0 whenever its checks ran, so read `healthy` instead of the
exit code. See [Exit codes](errors.md#exit-codes).

Each missing API key is a `warning`. If no key is set at all, a `credentials` check is also present,
with the status `error`.

Check IDs are unique within a result:

| ID | Checks |
|---|---|
| `config` | The config file. |
| `credentials.PROVIDER` | Whether the provider's API key is set. |
| `credentials` | Present only if no provider key is set. |
| `credentials.google_api_key` | Warns that `GOOGLE_API_KEY` is set and ignored. |
| `state_dir`, `output_dir` | Whether the directories are usable. |
| `base_url.PROVIDER` | Whether the provider's base URL is overridden. |
| `jobs` | Whether the local job records are readable. |
| `access.PROVIDER.MODEL` | With `--check-access`: whether the model is visible to your key. One per catalog model of each provider whose key is set, in catalog order. |
| `access.PROVIDER` | With `--check-access`: the provider's models weren't checked, for example because its key isn't set. |
| `access` | With `--check-access`: the configuration is invalid, so no model was checked. |

An `ok` access check means that the model is visible to your key. It doesn't mean that your
billing tier, prepaid credit, or organization verification allow a paid request.

### schema

`{ "schema": { "...": "the JSON Schema" } }`. See [JSON Schema](#json-schema).

### completions

`{ "shell": "bash", "script": "..." }`

### version

```json
{ "name": "iris", "version": "0.1.0", "schema_version": 1, "target": "x86_64-unknown-linux-gnu", "git_commit": null }
```

`git_commit` is the commit that the binary was built from, or `null`. Release archives report the
tagged commit. See [Verify what you installed](../guides/install.md#verify-what-you-installed).

### Dry-run plan

A generation command with `--dry-run` prints a plan instead of a result, and sends nothing. The
envelope's `command` is still the generation command, such as `video.generate`.

A dry run makes every local check that the real run makes, in the same order, and stops where the
real run would stop. That includes the model and options, the input files and the rules that relate
them, and whether the output paths can be written. The API key isn't required. A dry run creates no
directories and leaves no files behind: it checks that the nearest existing directory is writable
with a file that it removes right away.

```json
{
  "dry_run": true, "provider": "gemini", "model": "veo-3.1-fast-generate-preview",
  "model_source": "config", "operation": "video.generate", "async_job": true, "detach": false,
  "label": null,
  "wait": { "timeout": { "seconds": 1200.0, "source": "file", "flag": "--timeout",
                         "env_var": "IRIS_WAIT_TIMEOUT", "key": "video.wait_timeout" },
            "poll_interval": { "seconds": 10.0, "source": "default", "flag": "--poll-interval",
                               "env_var": "IRIS_POLL_INTERVAL", "key": "video.poll_interval" } },
  "billing": "paid",
  "options": { "aspect_ratio": "16:9", "count": 1, "duration": 8, "resolution": "720p" },
  "inputs": [ { "role": "first_frame", "path": "/home/you/fox.png", "media_type": "image/png", "bytes": 75 } ],
  "outputs": [ "/home/you/<job_id>.mp4" ],
  "credential_present": true,
  "cost_estimate": { "estimated": true, "currency": "USD", "amount": 0.8, "...": "..." },
  "max_cost": 1.0,
  "prompt_fingerprint": { "sha256": "c039da7d...", "chars": 31 }
}
```

| Field | Description |
|---|---|
| `dry_run` | Always `true`. |
| `provider`, `model`, `model_source`, `operation` | As in the real result. |
| `async_job` | `true` for a video job, `false` for an image request. |
| `detach` | `true` if you passed `--detach`, so the real run would return right after submitting. |
| `label` | The label that the real run would record, or `null`. A plan with a label exists only if no job record has it. |
| `wait` | How the real run would wait for its job: the wait limit (`timeout`) and the time between status checks (`poll_interval`), each in `seconds`, with its `source` (`flag`, `env`, `file`, or `default`) and the `flag`, `env_var`, and config `key` that can set it. `null` with `--detach` and for image commands. |
| `billing` | The model's billing value. For a model used with `--capabilities-from`, the known model's value. |
| `options` | Every option that the real run would send, with defaults filled in. |
| `inputs` | The input files, each with its `role`, `path`, `media_type`, and size in `bytes`. |
| `outputs` | The paths that the real run would write. See the note after this table. |
| `credential_present` | Whether the provider's API key is set. |
| `cost_estimate` | The estimate before the request, or `null` with a `cost_estimate_unavailable` warning. A model used with `--capabilities-from` never has one. |
| `max_cost` | The cap from `--max-cost`, in US dollars, or `null`. A plan with a cap exists only if the estimate is at most the cap. |
| `prompt_fingerprint` | The fingerprint of the prompt that the real run would send. See [Job](#job). |

Planned paths are absolute and normalized, without `.` or `..`, and without resolving symbolic
links. A name that the real run generates appears as a pattern: `iris-<ulid>.EXT` for an image and
`<job_id>.EXT` for a video, with `-N` before the extension when there are several outputs. An `-o`
name with several outputs becomes `STEM-N.EXT`. `N` counts from 1, while `artifacts[].index` counts
from 0.

## Shared objects

### Job

A job's view. Views never include the remote download URIs that the job record keeps.

| Field | Type | Description |
|---|---|---|
| `job_id` | string | The job's ID: `job_` followed by 26 lowercase letters or digits. |
| `label` | string or null | The label from `--label`. No two local job records have the same label. Iris stores and shows it as written. |
| `provider`, `model`, `operation` | string | The provider, the model ID, and `video.generate`. |
| `model_source` | string or null | `flag` or `config`. `null` in a record that doesn't say. |
| `status` | string | `submitting`, `running`, `succeeded`, `failed`, `expired`, or `submission_unknown`. See [Job states](../concepts/video-jobs.md#job-states). |
| `created_at`, `submitted_at`, `updated_at`, `completed_at`, `last_checked_at` | string or null | Timestamps. `completed_at` is when Iris first saw the job finish. |
| `remote_operation_id` | string or null | The provider's operation name. |
| `remote_expires_at` | string or null | The earliest time that the provider may delete the outputs. See [Retention and expiry](../concepts/video-jobs.md#retention-and-expiry). |
| `request` | object | The resolved options that Iris sent, and the number of input images of each role (`input_counts`). Never paths or file content. |
| `output_plan` | object | Where `jobs wait` and `jobs download` save the outputs when you don't pass `-o` or `-d`: the `-o` path (`path`) or the output directory (`dir`) in effect at submission, and whether `--overwrite` was given. |
| `outputs` | array | One entry per output: its `index`, `media_type`, `download_state` (`pending`, `downloaded`, `failed`, or `expired`), `artifact` once downloaded, and `last_error`. |
| `artifacts` | array of [Artifact](#artifact) | The downloaded outputs, the same objects as `outputs[].artifact`. |
| `error` | object or null | The error of a job that ended without success. See [Errors of ended jobs](errors.md#errors-of-ended-jobs). |
| `usage` | [Usage](#usage) or null | Usage that the provider reported. |
| `cost_estimate` | [Cost estimate](#cost-estimate) or null | The estimated cost. |
| `prompt_fingerprint` | object | The prompt's `sha256` and length in characters (`chars`). Never the text. |

`prompt_fingerprint.sha256` is the lowercase hexadecimal SHA-256 of the prompt as sent, encoded as
UTF-8. A prompt from `--prompt-file` or `--prompt-stdin` is hashed without its trailing whitespace.
`chars` counts Unicode scalar values. To find a job whose command was killed before it printed the
job ID, see [Recover a job after a crash](../guides/videos.md#recover-a-job-after-a-crash).

### Artifact

A saved file.

| Field | Type | Description |
|---|---|---|
| `index` | integer | The output's position, from 0. |
| `path` | string | The absolute path of the file. |
| `media_type` | string | The file's type, judged by its content, such as `image/png`. |
| `bytes` | integer | The file size. |
| `sha256` | string | The file's SHA-256 hash, in hexadecimal. |
| `width`, `height` | integer or null | Image dimensions, in pixels. |
| `duration_seconds` | number or null | Video length. |

### Usage

Token usage that the provider reported.

| Field | Type | Description |
|---|---|---|
| `input_tokens`, `output_tokens`, `total_tokens` | integer or null | Token counts. |
| `provider_usage` | object or null | The provider's own usage object, without anything sensitive. |

### Cost estimate

An estimated cost. Iris never reports an exact cost.

| Field | Type | Description |
|---|---|---|
| `estimated` | boolean | Always `true`. |
| `currency` | string | `USD`. |
| `amount` | number | The estimated amount. |
| `basis` | string | The calculation, and what it leaves out, in words. |
| `source_url` | string | The price list that the estimate uses. |
| `as_of` | string | The date that the prices were checked. |

When Iris can't estimate a request, such as an OpenAI request with the quality or size set to
`auto`, the field is `null`, and a `cost_estimate_unavailable` warning says which options to set
for an estimate. See [Cost estimates](../concepts/paid-requests.md#cost-estimates).

### Standard cost

What one standard output costs with a model, so that you can compare models on the same output:
one 1024x1024 image for image models, or one 8-second 720p video for video models.

```json
"standard_cost": {
  "output": "one 1024x1024 image",
  "estimates": [
    { "options": { "quality": "low", "size": "1024x1024" },
      "cost_estimate": { "estimated": true, "currency": "USD", "amount": 0.00588,
                         "basis": "estimate: 1 image × 196 output tokens × $30.00/1M (gpt-image-2.5-sunburst, low, 1024x1024); OpenAI calculator formula (indicative for GPT Image 2.5); prompt and input-image tokens not included",
                         "source_url": "https://developers.openai.com/api/docs/pricing", "as_of": "2026-09-24" } },
    { "options": { "quality": "medium", "size": "1024x1024" },
      "cost_estimate": { "amount": 0.01317, "...": "..." } }
  ]
}
```

`output` describes the standard output. `estimates` has one entry for each request that produces
it. The OpenAI models have one entry per quality that has an estimate, lowest first, because the
quality sets their price. Every other model has one entry: `aspect_ratio` `1:1` at `resolution`
`1K` for the Gemini image models, and `duration` `8` at `resolution` `720p` for Veo.

Each entry's `options` are the values to pass, with a typed flag or `-O`. Other options keep their
defaults. Its `cost_estimate` is the estimate that a dry run of the same request reports. For
OpenAI prices at other sizes, see
[Choose a model and control costs](../guides/models-and-costs.md#compare-costs).
