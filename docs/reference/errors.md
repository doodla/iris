# Errors reference

When a command fails, Iris exits with a nonzero exit code and reports an error with a stable code.
In human mode, Iris prints the error on stderr:

```text
error[cost_limit_exceeded]: the request is estimated at $0.00588 USD, above --max-cost $0.005
  hint: choose cheaper options or a cheaper model (`iris models list` compares the models' costs on the same output), or raise --max-cost
```

With `--json`, the envelope's `error` field holds the [error object](#error-object). This page lists
the exit codes, the fields of the error object, and every error and warning code.

Iris's codes are its own. Iris maps every provider error to one of them, and keeps the provider's
own code only for diagnosis. The codes are part of the JSON output's versioned contract; see
[Versioning](json-output.md#versioning).

## Exit codes

| Exit code | Meaning | What to do |
|---|---|---|
| 0 | Success. | |
| 1 | A runtime or provider failure. | Check the error code. Some failures are retryable. |
| 2 | The request is invalid, or conflicts with local state, as given. | Fix the request before you run it again. |
| 3 | A credentials, access, or quota problem. | Fix the key, the account's access, or its quota. |
| 4 | Not finished yet. The video job continues remotely. | Wait, and run `iris jobs wait` again later. |
| 5 | Uncertain outcome. A paid request might have been processed and billed. | Check your usage in the provider's console before you run the command again. Iris never resends it. |
| 130 | Interrupted by Ctrl+C, SIGTERM, or SIGHUP. | Resume a video job with `iris jobs wait`. |

Exit code 2 covers both local validation, where nothing was sent, and a provider's rejection of a
malformed request, such as an OpenAI HTTP 400. Either way, fix the request. To tell them apart,
check `error.provider_status`: it's `null` when nothing was sent.

Two commands exit with 0 even when they report a problem, because they ran successfully:

- `iris doctor`: read `result.healthy`. See [doctor](json-output.md#doctor).
- `iris jobs status`: read `result.job.status`, which can be `failed`.

## Error object

The following error is for a model that Iris doesn't know. `candidates` is shortened:

```json
{
  "code": "unknown_model", "category": "validation",
  "message": "unknown model 'does-not-exist'",
  "hint": "run `iris models list --operation image.generate` and pass -m <MODEL>; to use a model Iris does not know yet, add --capabilities-from <KNOWN_MODEL> to declare which known model's capabilities it has",
  "retryable": false, "retry_after_seconds": null,
  "provider": null, "provider_status": null, "provider_code": null, "provider_request_id": null,
  "job_id": null, "job_status": null, "remote_operation_id": null,
  "details": { "candidates": [ { "model": "gpt-image-2.5-sunburst", "provider": "openai", "...": "..." } ],
               "suggestions": [] }
}
```

| Field | Type | Description |
|---|---|---|
| `code` | string | The error code. See [Error codes](#error-codes). |
| `category` | string | The code's category. |
| `message` | string | What went wrong. |
| `hint` | string or null | What to do next. |
| `retryable` | boolean or null | Whether sending the same request again might succeed. See [Retryable errors](#retryable-errors). |
| `retry_after_seconds` | integer or null | How long the provider asked to wait before a retry, in whole seconds. |
| `provider` | string or null | The provider that the error concerns. |
| `provider_status` | integer or null | The provider's HTTP status, or `null` if nothing was sent or no response arrived. |
| `provider_code` | string or null | The provider's own error code, for diagnosis. |
| `provider_request_id` | string or null | The provider's ID for the request, for its support team. |
| `job_id`, `job_status`, `remote_operation_id` | string or null | The job that the error concerns, if any. |
| `details` | object | Fields that depend on the code. See each code below. |

Some fields have specific rules:

- `provider` is the provider that responded. For an error while following or downloading a job, or
  an error about another job, such as `label_in_use`, it's that job's provider, even if the error
  itself is local. A refused `jobs delete` identifies the job only by `job_id` and `job_status`.
- `retry_after_seconds` comes from the provider's `Retry-After` header, `retry-after-ms` header, or
  Google's `RetryInfo`. It's at least 1, and it's `null` whenever `retryable` is `false`, even if
  the provider asked for a delay.
- `provider_code` and `details.provider_message` come from the provider and can change without
  notice. Don't branch on them.
- Iris removes key values from every string in an error, redacts URLs, and shortens a provider's
  message to 500 characters.

## Error codes

| Code | Category | Exit code | Retryable by default | Meaning |
|---|---|---|---|---|
| `usage_error` | usage | 2 | false | The command line doesn't parse. |
| `model_required` | usage | 2 | false | No model was given with `-m` or in the config file. |
| `invalid_argument` | validation | 2 | false | A value, an output path, or a deletion isn't allowed, or the provider rejected the request as malformed. |
| `unsupported_operation` | validation | 2 | false | The model can't do what the command asks. |
| `unsupported_option` | validation | 2 | false | The model doesn't accept an option or an input. |
| `unknown_model` | validation | 2 | false | Iris doesn't know the model. |
| `unknown_provider` | validation | 2 | false | A `--provider` value isn't a provider that Iris supports. |
| `input_file_invalid` | validation | 2 | false | An input image or prompt file doesn't exist, can't be read, or isn't accepted. |
| `cost_limit_exceeded` | validation | 2 | false | The estimated cost is above `--max-cost`, or there's no estimate. |
| `config_invalid` | config | 2 | false | The config file or an environment variable is invalid. |
| `output_exists` | conflict | 2 | false | A file already exists where Iris would save an output. |
| `label_in_use` | conflict | 2 | false | Another job record has the label. |
| `job_not_found` | not_found | 2 | false | No job record in the state directory has the ID. |
| `missing_credentials` | auth | 3 | false | The provider's API key isn't set. |
| `authentication_failed` | auth | 3 | false | The provider rejected the API key. |
| `permission_denied` | access | 3 | false | The key's account can't use the model or the resource. |
| `quota_exceeded` | quota | 3 | false | The account reached a quota or billing limit. |
| `rate_limited` | rate_limit | 1 | true | The provider asked Iris to wait longer than Iris waits on its own. |
| `content_blocked` | content | 1 | false | The provider blocked the prompt or the output. |
| `provider_error` | provider | 1 | unspecified (true for a 5xx) | The provider reported an error. |
| `provider_bad_response` | provider | 1 | unspecified | The provider's response was malformed, too large, or had no usable output. |
| `remote_job_failed` | provider | 1 | false | The video job failed at the provider. |
| `network_error` | network | 1 | true | Iris couldn't reach the provider, or the connection failed. |
| `request_timeout` | timeout | 1 | true | A request that isn't a paid submission timed out. |
| `download_failed` | artifact | 1 | true | A video download failed. |
| `artifact_expired` | artifact | 1 | false | The provider no longer has the output. |
| `invalid_media` | artifact | 1 | unspecified (true for a job output download) | Content that should be media isn't complete, valid media of its declared type. |
| `state_invalid` | io | 1 | false | A job record can't be read, or a newer version of Iris wrote it. |
| `io_error` | io | 1 | unspecified | A local file operation failed, for example because the disk is full. |
| `internal_error` | internal | 1 | unspecified | A bug in Iris, or a code from a newer version of Iris. |
| `wait_timeout` | pending | 4 | true | The wait limit passed. The video job continues remotely. |
| `job_not_ready` | pending | 4 | true | The video job is still running, so its outputs aren't ready. |
| `submission_uncertain` | uncertain | 5 | false | A paid request might or might not have been processed. |
| `interrupted` | interrupted | 130 | true (false while a paid request was in flight) | The command was interrupted. |

The following sections describe the codes whose `details` or behavior need more than one line.

### `usage_error`

`details.usage` holds the parser's full message, and `details.suggestions` lists what to type
instead, if anything is close. If you type a model option as a flag, such as `--background`, the
hint suggests its `-O` form and names the models that accept the option.

```json
{"code":"usage_error","category":"usage","message":"unexpected argument '--aspect_ratio' found",
 "hint":"did you mean --aspect-ratio? run the command with --help for usage",
 "details":{"suggestions":["--aspect-ratio"],
            "usage":"error: unexpected argument '--aspect_ratio' found\n\n  tip: a similar argument exists: '--aspect-ratio'\n\n..."},
 "...":"..."}
```

### `model_required`

A generation command got no `-m` and found no model for its operation in the config file. Iris sends
nothing and writes no job record, in a dry run too.

| Detail | Description |
|---|---|
| `operation` | The command's operation, such as `image.generate`. |
| `config_key` | The config key that could name a model: `image.model` or `video.model`. |
| `config_file` | The config file that Iris read, or would read. |
| `candidates` | The models that support the operation, in catalog order, each with its `model`, `provider`, `display_name`, `summary`, `aliases`, and [`standard_cost`](json-output.md#standard-cost). |

### `invalid_argument`

| Detail | When |
|---|---|
| `option`, `allowed` | A value isn't one of an option's listed values. `allowed` lists them, typed like the option, such as `[4, 6, 8]` for Veo's `duration`. Values match exactly: for a value that differs only in case, `suggestions` names the listed one, such as `4K` for `4k`. |
| `constraint` | The request breaks a rule that relates several options or inputs. The ID is one of the model's `constraints` in `iris models show`. |
| `flag` | A flag's value is malformed, such as a `--max-cost` that isn't a positive amount, or a `--label` that breaks the naming rules. |
| `path` | An output location can't be used: a file is in the way of a directory, permission is denied, the file system is read-only, or the path names standard output, a standard stream, or a device. Iris writes media only to files. |
| `option: "format"`, `path` | The extension of `-o` contradicts the requested format, or names a type that the model can't produce. |
| `deleted`, `refused` | `iris jobs delete` refused to delete some jobs, so it deleted nothing. `deleted` is empty and `refused` lists the IDs. |
| `outputs_not_downloaded`, `remote_expires_at` | `iris jobs delete` refused a succeeded job whose outputs, by index, aren't downloaded while the provider keeps them. |

A provider's rejection of a malformed request, such as an OpenAI HTTP 400, is also
`invalid_argument`, with the provider's `provider_status`.

### `unsupported_operation`

The model doesn't support the command's operation, such as `iris image generate -m veo-lite`.
`details.operation` names the operation and `details.candidates` lists the models that support it.
If the config file chose the model, `details.config_key` names the key.

### `unsupported_option`

The model doesn't accept a typed flag, an `-O` option, or an input such as `--mask`.
`details.option` names it: an option's name, or `mask`, `first_frame`, `last_frame`, or
`reference`. `details.supported_by` lists the catalog models that accept it for the operation.
`details.model_source` says whether `-m` or the config file chose the model.

```json
{"code":"unsupported_option","message":"model 'gpt-image-2.5-sunburst' does not support --aspect-ratio for image.generate",
 "hint":"--aspect-ratio is supported by: gemini-3.1-flash-image, gemini-3.1-flash-lite-image, gemini-3-pro-image; pass -m <MODEL>; options supported by this model for image.generate: --count, --size, --quality, --format, -O compression, -O background, -O moderation",
 "details":{"model_source":"flag","option":"aspect_ratio","supported_by":["gemini-3.1-flash-image","gemini-3.1-flash-lite-image","gemini-3-pro-image"]},
 "...":"..."}
```

### `unknown_model`

The name isn't a catalog model ID or alias. `details.candidates` lists the models that the command
can use. The hint and `details.suggestions` depend on the name:

- **A near miss**, such as `gpt-image-2.5` or `Nano-Banana-2`: `suggestions` lists the catalog
  models that the name nearly matches, and the hint asks "did you mean …?". If those models are
  for another operation, the hint says what they're for instead.
- **A name that Iris declines**, such as `dall-e-3`, `gpt-image-1`, `imagen-4`, `veo-3`, or
  `nano-banana`: the provider retired, deprecated, or limited the model, or offers it only on
  another platform. The hint says why, with the provider's date, and `suggestions` lists the models
  to use instead.
- **Any other name**: the hint suggests `--capabilities-from`, to use a model that Iris doesn't know
  yet.

For how Iris matches near misses and which names it declines, see
[Decisions](../contributing/decisions.md#built-in-models).

```json
"hint":"OpenAI removed DALL·E (dall-e-2, dall-e-3) from the API on 2026-05-12 and recommends a GPT Image 2.5 model for new integrations; use gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2",
"details":{"candidates":["..."],"suggestions":["gpt-image-2.5-sunburst","gpt-image-2.5-flare","gpt-image-2"]}
```

### `cost_limit_exceeded`

The request's estimate before sending is above `--max-cost`, or the request has no estimate. Iris
sends nothing and writes no job record, in a dry run too. A request estimated at exactly the cap is
sent.

| Detail | Description |
|---|---|
| `max_cost` | The cap, in US dollars. |
| `cost_estimate` | The request's [cost estimate](json-output.md#cost-estimate), or `null`. |
| `cost_estimate_unavailable` | Why there's no estimate, or `null`. It names the options that would give one, if any. |

The cap compares an estimate that can leave out prompt, input-image, and thinking tokens, so the
bill can still be higher than the cap. See
[Cost estimates](../concepts/paid-requests.md#cost-estimates).

### `config_invalid`

The message names the config key or environment variable and what's wrong with it. For the
validation rules, see [Configuration reference](configuration.md#validation).

### `output_exists`

A file exists at an output path, and you didn't pass `--overwrite`. `details.path` names it. When
the provider chooses the image type, Iris also checks the paths with the other extensions that the
model can return. So a rerun doesn't pay for an image that it would save next to an existing one.

### `label_in_use`

Another job record in the state directory has the label, in any status. Iris sends nothing and
writes no job record, in a dry run too. `job_id`, `job_status`, `provider`, and
`remote_operation_id` describe that job, and `details` holds its `label`, `model`, and `created_at`.
The hint depends on the job's status. See [Label a job](../guides/videos.md#label-a-job).

While a job record can't be read, a labeled submission fails with `state_invalid` instead:
`details.unreadable` lists the records and `details.label` the label.

### `missing_credentials`

Iris checks the API key after every other local check and before any network call, so a problem
with the request itself is reported first. `video generate`, `jobs wait`, and `jobs download` check
it before they create any output directory. A dry run never requires a key.

### `permission_denied`

The provider refused access to the model or resource. `iris jobs wait` also reports it when a status
check gets HTTP 404 during the retention period, with `provider_status: 404`. See
[Retention and expiry](../concepts/video-jobs.md#retention-and-expiry).

### `provider_bad_response`

| Detail | When |
|---|---|
| `limit_bytes`, `declared_bytes` | A response to a free request was longer than Iris reads. `declared_bytes` is present when the response declared a longer `Content-Length`. |
| `charge_possible`, `usage`, `cost_estimate`, `fallback_paths` | A paid image response had no usable image. See [Paid image failures](#paid-image-failures). |

### `download_failed`

When Iris refuses to fetch an output's URI with the current configuration, `details.uri` holds the
redacted URI. See [Downloads](../concepts/video-jobs.md#downloads).

### `job_not_ready`

`iris jobs download` found the job still `submitting` or `running`. If the status check that it
makes first failed for a temporary reason, `details.status_checked` is `false`, and a
`status_refresh_failed` warning says why.

### `submission_uncertain`

A paid request was sent, but Iris can't tell whether the provider processed it. `retryable` is
`false`, and `details.charge_possible` is `true`. For which cases count, see
[When the outcome is uncertain](../concepts/paid-requests.md#when-the-outcome-is-uncertain).

For an image request, `job_id` is `null`, and:

- `details.transport` is `timeout` for a timeout, or `other` for a failed connection or a response
  that couldn't be read in full. A dropped connection is never reported as a timeout.
- `provider_status` is the response's HTTP status, if a response started before the failure.
- An OpenAI error includes `details.client_request_id`, the `X-Client-Request-Id` that Iris sent.
  OpenAI's support team can use it to look up the request.

For a video submission, `job_id` names the job, whose status is `submission_unknown`:

```json
{"command":"video.generate","error":{"category":"uncertain","code":"submission_uncertain",
 "message":"the Gemini API answered the video request with HTTP 500, which does not prove the job was not created",
 "hint":"the provider may have accepted this paid request; check usage/billing in Google AI Studio before resubmitting; Iris will not resubmit automatically",
 "job_id":"job_01m3a61aafgj1zg9ry7xgv8rvc","job_status":"submission_unknown",
 "provider":"gemini","provider_code":"INTERNAL","provider_status":500,"provider_request_id":null,
 "remote_operation_id":null,"retry_after_seconds":null,
 "details":{"charge_possible":true,"provider_message":"internal"},
 "retryable":false},
 "ok":false,"result":null,"schema_version":1,
 "warnings":[{"code":"preview_model","message":"veo-3.1-lite-generate-preview is a preview model; its behavior, limits, and availability may change"}]}
```

The `preview_model` warning stays in the envelope: Iris keeps warnings from before a failed request.

### `interrupted`

The command stopped because of Ctrl+C, SIGTERM, or SIGHUP, and exited with code 130. Iris reports
one envelope, however the process was interrupted.

- By default, `retryable` is `true`: running the command again is harmless.
- While a paid request is in flight, `retryable` is `false` and `details.charge_possible` is `true`,
  because running the command again could pay twice.
- If the interrupt arrives before Iris starts sending a video request, Iris sends nothing, writes no
  job, and reports `retryable: true`.

See [Submitting a job](../concepts/video-jobs.md#submitting-a-job).

## Paid image failures

When a paid image request fails after the provider responded, the error can include these details:

| Detail | Description |
|---|---|
| `charge_possible` | `true` if the provider might have processed and billed the request. |
| `charged` | `true` if the provider completed the request and bills it, such as a Gemini response with text only or a blocked image. |
| `usage` | The usage that the provider reported, as a [Usage](json-output.md#usage) object. |
| `cost_estimate` | The cost estimated from that usage, if Iris can estimate it. |
| `saved` | Every image that Iris saved, wherever it saved them. |
| `index` | The index of the first image that Iris couldn't save as an image. |
| `fallback_paths` | Every file that Iris kept in the state directory's `unsaved` folder. |
| `fallback_error` | Why Iris couldn't keep a file even there. |

An error with `charge_possible: true` never has `retryable: true`. `charged: true` is a known
outcome, so it doesn't make an error non-retryable by itself, but running the command again sends a
new request, which is billed again. See
[Paid outputs are kept](../concepts/paid-requests.md#paid-outputs-are-kept).

## Retryable errors

`retryable` says whether sending the same request again might succeed:

- The table in [Error codes](#error-codes) gives each code's default. A specific error can differ:
  for example, `provider_error` is `true` for a 5xx that Iris classified as temporary.
- `retryable` is never an instruction to resend a paid request automatically. Iris itself never
  resends a paid request whose outcome is unknown. See
  [How Iris handles paid requests](../concepts/paid-requests.md).

### Errors of ended jobs

A job that ended without success (`failed`, `expired`, or `submission_unknown`) keeps its error in
`job.error`. `iris jobs wait` and `iris jobs download` report that error for such a job. It always
has `retryable: false`, because repeating those commands can't change the job.

If the error was recorded with a different `retryable` value, for example a `rate_limited`
submission, `details.submission_retryable` keeps the recorded value, and
`details.submission_retry_after_seconds` keeps a recorded delay. The hint says first that trying
again means a new, billed submission, then gives the hint that was recorded with the error. The
error that `iris video generate` itself reported keeps its original value and hint.

## Codes from newer versions

Iris reads error codes that it doesn't know without failing, so an older Iris can read a job record
that a newer one wrote. A job view shows such a code as `internal_error`, with the category
`internal`, and puts the recorded code in `details.recorded_code`. The job record keeps the original
code. When you write a client, treat an unknown error code by its category and exit code, and an
unknown warning code as informational text.

## Warning codes

A warning never fails a command. It tells you something about a result, or about a failed command.
In human mode, Iris prints warnings on stderr as `warning[CODE]: MESSAGE`. With `--json`, they're in
the envelope's `warnings` array, even when the command fails. The message describes the specific
case.

| Code | Meaning |
|---|---|
| `unverified_model_capabilities` | The model was used with `--capabilities-from`, so Iris assumes it has the named model's capabilities. If the ID nearly matches catalog models, the message also asks "did you mean -m …?", but Iris sends the ID as you typed it. |
| `output_extension_adjusted` | Iris added an extension to an `-o` path that had none, or changed it to match the type that the provider returned. |
| `output_renamed` | A different file appeared at the target in the meantime, so Iris saved the output as `STEM.N.EXT` instead of overwriting it. |
| `output_format_mismatch` | A valid image came back as a different type than requested or labeled. Iris saved it under its real type. |
| `cost_estimate_unavailable` | Iris couldn't estimate the request's cost. The message says why, and which options to set for an estimate. |
| `job_record_unreadable` | Iris couldn't read a local job record, and skipped it. |
| `provider_text_output` | The model also returned text. It's in the result's `text` field. |
| `already_downloaded` | An identical file is already at the target, so Iris wrote nothing. |
| `retention_limited` | The provider keeps the job's outputs only for a limited time. Download them before `remote_expires_at`. |
| `preview_model` | The model is a preview model. Its behavior, limits, and availability can change. |
| `non_default_base_url` | A provider's base URL isn't the default, and Iris sends that provider's API key there. |
| `content_filtered` | The provider filtered some of the job's outputs for safety. |
| `unexpected_output_count` | The provider returned a different number of items than requested. Iris kept every usable image. |
| `status_refresh_failed` | Iris couldn't check the job's remote status, so it shows the last known status. |
| `output_item_unusable` | A returned item isn't a usable image, so Iris skipped it. Iris kept every usable image. |
| `output_saved_elsewhere` | Iris saved a paid output in the state directory instead of where you asked: an image that it couldn't write there, which is also listed in `artifacts`, or the content of an item that isn't a usable image, kept as a `.bin` file. |
| `output_extension_may_change` | The model takes no `format` option, so the provider chooses the image type, and the `-o` extension may change. The message names the other paths, where an existing file stops the run without `--overwrite`. Iris reports it before sending, in a dry run too. |

The warning messages that describe returned items number them by their position in the provider's
response, from 0. That position can differ from an artifact's `index` when Iris skipped an earlier
item. `unexpected_output_count` counts every item, usable or not.

For example, one OpenAI request with an `-o` path that had no extension, and a `--format` that the
response didn't match, reported three warnings:

```json
"warnings": [
  {"code":"output_extension_adjusted","message":"/home/you/circle has no extension; saving as /home/you/circle.jpg"},
  {"code":"output_format_mismatch","message":"OpenAI returned response item 0 as image/png, but the request asked for output_format jpeg and the response declared output_format jpeg; it is kept as image/png because the request completed and may have been billed"},
  {"code":"output_extension_adjusted","message":"the provider returned image/png; saving as /home/you/circle.png instead of /home/you/circle.jpg"}
]
```

A later version of Iris can add warning codes. Treat a code that you don't know as informational
text, never as an error.
