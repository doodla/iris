# How Iris handles paid requests

Every image and video that Iris generates is billed to your provider API account. This page
explains how Iris sends paid requests, when it retries them, and what it does when it can't tell
whether a request was billed. The goal of these rules is that you never pay twice for the same
output, and never lose an output that you paid for.

## Guarantees

- **Local checks come first.** Iris validates the model, options, input files, output paths, and
  API key before it sends anything. A request that fails a local check costs nothing.
- **A paid request is sent once.** Iris retries it only when the request provably didn't reach the
  provider, or the provider rejected it before processing it.
- **An uncertain outcome is reported, not retried.** If Iris can't tell whether the provider
  accepted a paid request, it exits with code 5 and doesn't send the request again.
- **Paid outputs are kept.** If Iris can't save an output where you asked, it saves it in the state
  directory and tells you where.
- **Existing files are safe.** Iris doesn't replace a file unless you pass `--overwrite`.
- **Downloads don't regenerate.** Downloading a video, once or many times, never submits a job.
- **Stopping a wait doesn't stop a job.** Ctrl+C or a wait time limit ends the command, but the
  video job keeps running and you can resume it.
- **Costs are estimates.** Iris labels every cost figure as an estimate. Only your provider's
  invoice is exact.

## Check a request before you pay

Add `--dry-run` to any generation command. Iris runs every local check that the real command runs,
prints the plan with a cost estimate, and sends nothing. To refuse any request that's estimated
above a limit, add `--max-cost` with an amount in US dollars. For examples, see
[Choose a model and control costs](../guides/models-and-costs.md).

## When Iris retries

Iris sorts every HTTP call by what repeating it could cost:

| Call | Attempts | Iris retries after |
|---|---|---|
| Paid request: an image generation or edit, or a video submission | Up to 3 | A connection that failed before the request was sent; HTTP 429 for a rate limit; OpenAI's HTTP 503 `server_is_overloaded`, which OpenAI documents as not processed |
| Free request: a job status check or a model metadata read | Up to 5 | Connection errors, timeouts, and HTTP 408, 429, and 5xx |
| Download of a finished video | Up to 5 | The same errors as a free request; each attempt starts over with an empty file |

A quota or billing error isn't a rate limit, even when the provider sends it with HTTP 429, as
OpenAI does for `insufficient_quota`. Iris doesn't retry it.

Between attempts, Iris waits with exponential backoff from 1 second up to 30 seconds, with jitter.
When the provider asks for a delay, for example with a `Retry-After` header, Iris waits for up to 60
seconds. If the provider asks for a longer delay, Iris stops with `rate_limited` and reports the
delay in `retry_after_seconds`.

Both providers recommend retrying server errors and timeouts. For paid requests, Iris doesn't,
because neither provider offers a way to send a request again without risking a second charge. For
the sources, see
[Decisions](../contributing/decisions.md#retry-classes-and-why-vendor-retry-guidance-is-overridden).

## When the outcome is uncertain

A paid request has an uncertain outcome when Iris sent it but didn't receive a complete response.
For example, the connection dropped after the request was sent. The provider might have processed
and billed the request, or it might not have.

Iris reports an uncertain outcome as `submission_uncertain`, with exit code 5, `retryable: false`,
and `details.charge_possible: true`, and it doesn't retry the request. Before you run the command
again, check your usage in the provider's console.

| Request | The outcome is uncertain when |
|---|---|
| Any image request | The request timed out or the connection dropped after it was sent, or the response couldn't be read in full |
| OpenAI image request | Any of the above, or OpenAI returned HTTP 408 or 5xx other than the documented 503 `server_is_overloaded` |
| Video submission | The request timed out or the connection dropped after it was sent, Veo returned HTTP 408 or 5xx, or a successful response couldn't be read in full or had no operation name |

A Gemini image request that fails with an HTTP error isn't uncertain, because Google doesn't charge
for requests that fail with a 400 or 500 error. The error keeps its usual code, such as
`provider_error` for a 5xx, and you can run the command again.

Iris can't recover an image request after an uncertain outcome: the image APIs return the image in
the response, and they don't offer a way to look up a request later. A video submission is
different. Iris keeps its job record with the status `submission_unknown`, so you can find it
later. See [How video jobs work](video-jobs.md#job-states).

### Interrupting a paid request

If you press Ctrl+C, or the process receives SIGTERM or SIGHUP, while a paid request is in flight,
Iris exits with code 130 (`interrupted`) and reports `retryable: false` and
`details.charge_possible: true`. During a video submission, Iris waits for the provider's response
to the first interrupt so that it can record the job. See
[Submitting a job](video-jobs.md#submitting-a-job).

## Billed errors

Some requests fail after the provider has completed them. For example, a Gemini image request can
end with text only, or with an image that Google blocked. Google bills such a request by the tokens
that it used.

Iris marks these errors with `details.charged: true`, and includes the usage that the provider
reported (`details.usage`) and, when it can, a cost estimate (`details.cost_estimate`). Unlike
`charge_possible`, `charged` describes a known outcome. Running the command again sends a new
request, which is billed again.

OpenAI doesn't document whether it bills a response without a usable image, so Iris reports such an
error with `details.charge_possible: true` instead.

## Paid outputs are kept

Iris judges each returned image by its content, not by the type that the provider reports:

- An image of a different type than you asked for is saved under its real type, with an
  `output_format_mismatch` warning.
- An item that isn't a usable image is skipped, with an `output_item_unusable` warning. Iris keeps
  its content, as received, in a `.bin` file in the `unsaved` folder of the state directory.
- If Iris can't write a valid image where you asked, for example because the disk is full, it saves
  the image in the `unsaved` folder instead and reports the path with an `output_saved_elsewhere`
  warning.

A response fails only when it has no usable image at all, with `provider_bad_response` and
`details.charge_possible: true`. To find the `unsaved` folder, run `iris config path`: it's in the
state directory. Move its files elsewhere before you delete the state directory.

## Cost estimates

Iris estimates what a request costs twice:

- Before it sends the request, from the provider's published prices. `--dry-run` and `--max-cost`
  use this estimate.
- After the request, from the usage that the provider reports, if the provider reports any.

Every estimate has `estimated: true` and a `basis` that shows the calculation and what it leaves
out, such as prompt, input-image, or thinking tokens. Your bill can be higher than the estimate by
what the basis leaves out.

When Iris can't estimate a request before sending it, for example because the model chooses the
quality or size, it reports a `cost_estimate_unavailable` warning that names the options to set.
Google doesn't charge for a video that it blocks.

## What Iris can't do

- Recover an image request after an uncertain outcome.
- Cancel a video job after you submit it. Veo doesn't provide a way to cancel a job, and
  `iris jobs delete` removes only the local record.

## Related

- [Errors reference](../reference/errors.md): `submission_uncertain`, `interrupted`, and the other
  codes.
- [How video jobs work](video-jobs.md): job states, downloads, and retention.
- [Decisions](../contributing/decisions.md#paid-requests-retries-and-uncertain-outcomes): the
  provider documentation behind these rules.
