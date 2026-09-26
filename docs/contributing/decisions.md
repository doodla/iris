# Decisions

This log records the consequential choices behind Iris: what was decided, why, and the official
sources the decision rests on. Every source below was checked on **2026-09-24** against the pages
as they read that day; provider APIs change, so re-check a source before relying on a decision
that depends on it. [architecture.md](architecture.md) describes how the code is organized;
this document explains why it behaves the way it does.

Each entry has the decision, the reasoning, and its sources. Where official documents disagree,
the entry says so and names the reading Iris chose.

## Contents

- [Provider APIs](#provider-apis)
- [Models](#models)
- [Paid requests: retries and uncertain outcomes](#paid-requests-retries-and-uncertain-outcomes)
- [Jobs: persistence, locking, and atomic writes](#jobs-persistence-locking-and-atomic-writes)
- [Downloads and trust](#downloads-and-trust)
- [Retention and expiry](#retention-and-expiry)
- [Access, credentials, and cost estimates](#access-credentials-and-cost-estimates)
- [Hand-rolled code, and why](#hand-rolled-code-and-why)
- [Dependencies and toolchain](#dependencies-and-toolchain)
- [Release targets and CI](#release-targets-and-ci)

## Provider APIs

### Thin REST clients, not SDK crates

**Decision.** Each provider adapter is a small REST client on `reqwest` inside
`src/providers/<provider>/`; Iris uses no provider SDK crate.

**Why.** Neither provider publishes an official Rust SDK: OpenAI lists only a community library
for Rust, and Google's official Gemini SDKs cover Python, JavaScript/TypeScript, Go, Java, and C#.
The community crates evaluated did not fit Iris's contract. `async-openai` retries POST requests
on 429/5xx by default (turning that off needs an optional middleware feature) and does not expose
response headers such as `x-request-id` or `Retry-After`; the most-used Gemini crate had no Veo
(`predictLongRunning`) support and closed enums that reject finish reasons the API returns. Iris
needs only a handful of endpoints (two OpenAI, one Gemini image endpoint, Veo submit and poll,
file download), so a thin client keeps retries, headers, redaction, and error mapping under Iris's
control. Provider wire types stay inside the adapter, so switching an adapter to an SDK later
would be a local change.

**Sources.** [OpenAI libraries](https://developers.openai.com/api/docs/libraries) ·
[Gemini API libraries](https://ai.google.dev/gemini-api/docs/libraries) ·
[async-openai](https://github.com/64bit/async-openai) ·
[crates.io](https://crates.io/) (crate metadata for the Gemini crates compared)

### OpenAI: the Images API, with JSON edit bodies

**Decision.** Image generation uses `POST /v1/images/generations`; editing uses
`POST /v1/images/edits` with an `application/json` body in which every input image and the
optional mask is a `data:<type>;base64,…` URL. The base URL is `https://api.openai.com/v1`. Every
request sends `model` explicitly, authenticates with `Authorization: Bearer <key>`, and carries a
fresh `X-Client-Request-Id`; OpenAI's `x-request-id` is captured on success and on errors. Not
used: the Responses API, the Batch API, multipart edits, and `file_id` inputs.

**Why.** The Images API is the documented image endpoint and is fully synchronous: images return
inline as base64, with no job id, no retrieval endpoint, and no stored application state, so Iris
offers no detach or recovery for it. The edits endpoint accepts both multipart and JSON; the JSON
form documents `moderation`, has explicit schema bounds, and shares request handling with
generation. Its per-image limit is the data-URL length cap (20,971,520 characters, about 15.7 MB
of image), which Iris checks locally before sending; multipart would allow larger files but
leaves `moderation` undocumented. `file_id` inputs would need Files API uploads, which are kept
until deleted. `model` is always sent because the documented default for generation is a model
that has been removed. `X-Client-Request-Id` lets OpenAI support look up a request whose answer
was lost; it is not a deduplication mechanism, and OpenAI does not say whether the Images endpoints
record it.

**Sources.** [Image generation guide](https://developers.openai.com/api/docs/guides/image-generation) ·
[Images API reference](https://developers.openai.com/api/reference/resources/images) ·
[OpenAPI specification](https://github.com/openai/openai-openapi) ·
[API overview: authentication and request ids](https://developers.openai.com/api/reference/overview) ·
[Changelog (JSON edits)](https://developers.openai.com/api/docs/changelog) ·
[Your data](https://developers.openai.com/api/docs/guides/your-data)

### Gemini images: `generateContent` on `v1`, with `store: false`

**Decision.** Image generation and editing use
`POST https://generativelanguage.googleapis.com/v1/models/{model}:generateContent` with
`generationConfig.responseModalities: ["IMAGE"]`, `imageConfig` (`aspectRatio`, `imageSize`) and
`thinkingConfig` only for options the user set, and `store: false` on every request. Input
images are sent inline. Not used: the Interactions API and `generationConfig.responseFormat`.

**Why.** Google recommends the Interactions API for new projects but states that
`generateContent` "remains fully supported". `generateContent` supports all three Nano Banana
models Iris ships, whereas the Interactions model table omits the Flash Lite image model; the
Interactions API also stores requests by default (55 days on the paid tier), and its `v1beta`
schema had a breaking change in 2026. `generateContent` errors use the standard `google.rpc.Status`
shape. `v1` has every field Iris needs, and Google states that all models are supported on both
`v1` and `v1beta`. `store` defaults to off for `generateContent`, but Iris sends `store: false`
explicitly because the explicit value takes precedence over a project-level logging setting.
The REST samples put aspect ratio and size under `responseFormat.image`, while the discovery
document types those fields as enums and the official Python SDK marks that field unsupported on
the Gemini API; `imageConfig` is consistent with the schema and the SDKs, so Iris uses it. Google
documents two different inline request limits (20 MB and 100 MB); Iris enforces the stricter
20 MB before sending.

**Sources.** [Image generation guide](https://ai.google.dev/gemini-api/docs/image-generation) ·
[generateContent image examples](https://ai.google.dev/gemini-api/docs/generate-content/image-generation) ·
[Interactions API overview](https://ai.google.dev/gemini-api/docs/interactions-overview) ·
[Migrating to Interactions](https://ai.google.dev/gemini-api/docs/migrate-to-interactions) ·
[Interactions breaking changes (May 2026)](https://ai.google.dev/gemini-api/docs/interactions-breaking-changes-may-2026) ·
[API versions](https://ai.google.dev/gemini-api/docs/api-versions) ·
[Logs and datasets (`store`)](https://ai.google.dev/gemini-api/docs/logs-datasets) ·
[v1 discovery document, revision 20260923](https://generativelanguage.googleapis.com/$discovery/rest?version=v1) ·
[Image understanding (20 MB inline)](https://ai.google.dev/gemini-api/docs/image-understanding) ·
[File input methods (100 MB)](https://ai.google.dev/gemini-api/docs/file-input-methods) ·
[python-genai SDK](https://github.com/googleapis/python-genai)

### Gemini: the key goes in a header, and the base URL is the origin

**Decision.** The Gemini key is sent only in the `x-goog-api-key` header, never as a `?key=`
query parameter. The configured Gemini base URL is the origin
(`https://generativelanguage.googleapis.com`); the adapter appends `/v1` for images and
image-model metadata and `/v1beta` for Veo, operations, and files. The OpenAI base URL keeps its
`/v1` path.

**Why.** The header is the documented authentication method; the API also accepts a `key` query
parameter, but URLs end up in logs, proxies, and error messages, so Iris never puts a key in one.
Veo's `predictLongRunning` and the operations methods exist only in the `v1beta` discovery
document, so one base URL has to serve two API versions.

**Sources.** [API keys](https://ai.google.dev/gemini-api/docs/api-key) ·
[v1beta discovery document, revision 20260923](https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta) ·
[API versions](https://ai.google.dev/gemini-api/docs/api-versions)

### Veo: `predictLongRunning`, in the official SDKs' wire form

**Decision.** A video job is submitted with
`POST /v1beta/models/{model}:predictLongRunning` and followed with `GET /v1beta/{operation name}`.
The request body follows the official SDKs where the guide's REST samples differ: images as
`bytesBase64Encoded` plus `mimeType`, `durationSeconds` as a JSON integer, reference images with
`referenceType: "ASSET"`, and no sample count (the only value is 1). `durationSeconds`,
`resolution`, and `aspectRatio` are always sent, with Iris's defaults (8 seconds, 720p, 16:9)
when the user sets none. Audio is not an option.

**Why.** The operation name returned by the submit call can be polled from any later process, so
Veo is a real provider-native asynchronous job and gets a durable job record, `--detach`, and
`jobs status|wait|download`. The guide's REST samples conflict with the official Python and
JavaScript SDKs on several wire details (inline image encoding, duration as a string, lowercase
`asset`), and some printed samples are not valid JSON; the SDKs are what Google's own clients
send, so Iris follows them (a live reference-image request on Veo 3.1 Fast confirmed the
`ASSET` casing on 2026-09-25). Sending duration, resolution, and aspect ratio
explicitly bounds the cost of every job instead of depending on server defaults. Audio is always
on for Veo 3.1 on the Gemini API and the SDKs reject `generateAudio` there, so Iris offers no
audio flag. The discovery document has no cancel or delete method for these operations, so Iris
offers no remote cancellation. It declares a `files.delete` method, but whether it applies to
generated Veo outputs is not documented, so Iris neither offers remote deletion nor says the
provider lacks it.

Live requests on 2026-09-25 also showed where the API refuses `negativePrompt`, which the SDKs map
but the guide's parameter table omits: Veo 3.1 Lite refuses it outright ("isn't supported by this
model"), and Veo 3.1 Fast refuses it next to a reference image ("not supported in your use case")
while accepting it for text-to-video. Iris declares no negative prompt for Lite and refuses a
negative prompt with reference images on Fast and Standard before sending; the Standard
combination was not tried, and the provider documents no support for it.

**Sources.** [Veo guide](https://ai.google.dev/gemini-api/docs/veo) ·
[Video generation overview](https://ai.google.dev/gemini-api/docs/video) ·
[v1beta discovery document, revision 20260923](https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta) ·
[Models API reference](https://ai.google.dev/api/models) ·
[Files API reference](https://ai.google.dev/api/files) ·
[python-genai `models.py`](https://github.com/googleapis/python-genai/blob/7672ff6a08b92d45f6718e1195845b818705bad6/google/genai/models.py) ·
[js-genai converters](https://github.com/googleapis/js-genai/blob/4d7e80b03dda3b9104649cb69bd8471f52f0c207/src/converters/_models_converters.ts)

## Models

### No default model

**Decision.** Iris never chooses a model for the caller. A generation command uses `-m`/`--model`,
or the model the config file names for its operation (`[image] model`, `[video] model`), and
reports which of the two it used (`model_source`: `flag` or `config`). With neither, it fails with
`model_required` before anything is sent, and the error lists the catalog models that support the
operation. There is no default model, no default provider, and no environment variable
that names a model; the generation commands take no `--provider`, because the provider is the
model's.

**Why.** Iris is primarily for agents, and an agent needs to know what it buys. A default hides
the model and its price: Google's models differ several-fold in price (Veo 3.1 Lite costs $0.05
a second at 720p and Veo 3.1 $0.40; Nano Banana 2 Lite costs $0.0336 for a 1K image and Nano
Banana Pro $0.134), and a command that names no model spends money on a model the caller never
chose. A default also makes general-looking flags depend on an unseen model: whether
`--quality`, `--aspect-ratio`, or `--mask` is accepted, and what it costs, would follow a choice
the command line does not show. And a default silently changes what scripts buy when the catalog
changes: the catalog follows models that providers retire on published dates, so a script that
relied on the default would start buying another model at another price without any change to
its command line. A model named once in the config file keeps command lines short where that is
wanted, and every result still says where its model came from.

**Sources.** [Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing) ·
[OpenAI deprecations](https://developers.openai.com/api/docs/deprecations) ·
[Gemini deprecations](https://ai.google.dev/gemini-api/docs/deprecations)

### Built-in models

**Decision.** The catalog registers, per provider:

| provider | registered | not registered |
|---|---|---|
| OpenAI | `gpt-image-2.5-sunburst`, `gpt-image-2.5-flare`, `gpt-image-2` (dated snapshots as aliases) | `gpt-image-1`, `gpt-image-1.5`, `gpt-image-1-mini`, `chatgpt-image-latest`, `dall-e-2`, `dall-e-3` |
| Gemini images | `gemini-3.1-flash-image` (Nano Banana 2), `gemini-3.1-flash-lite-image` (Nano Banana 2 Lite), `gemini-3-pro-image` (Nano Banana Pro) | `gemini-2.5-flash-image`, every Imagen model, the `*-preview` image ids |
| Veo | `veo-3.1-fast-generate-preview`, `veo-3.1-generate-preview`, `veo-3.1-lite-generate-preview` | `veo-2.0-*`, `veo-3.0-*`, the Vertex AI / Gemini Enterprise `veo-3.1-*-001` ids, Gemini Omni |

**Why.** OpenAI recommends `gpt-image-2.5-sunburst` for API use; Flare is the documented faster
model at the same token rates. `gpt-image-1` shuts down on 2026-10-23, `gpt-image-1.5`,
`gpt-image-1-mini`, and `chatgpt-image-latest` on 2026-12-01, and DALL·E was removed on
2026-05-12. Google calls
`gemini-3.1-flash-image` its go-to image model. `gemini-2.5-flash-image` has been limited to
projects that already used it since 2026-09-18 and is listed with an earliest shutdown date of
2026-10-02 (another Google page says the 2.5 models are not deprecated, so Iris names no
shutdown date); Imagen 4 shut down on 2026-08-17 (the earlier Imagen models before it) and the
Gemini 3 preview image ids on 2026-06-25 (`gemini-2.5-flash-image-preview` on 2026-01-15). On the
Gemini API only the three Veo 3.1 preview models remain; Veo 2.0 and 3.0 shut down on
2026-06-30, and the Veo 3.1 `-001` ids (GA, and Lite in preview) exist only on Google Cloud's
enterprise platform, which uses other authentication and endpoints. The three trade price against
options: Veo 3.1 and Veo 3.1 Fast are documented to support every video option Iris offers,
including 4k and reference images, with Fast at a quarter of Veo 3.1's per-second price at 720p and
less than it at every resolution, while Veo 3.1 Lite costs least and offers neither 4k nor
reference images. Reference images on Fast, which the official cookbook lists only for Veo 3.1,
were confirmed by a live 4k request on 2026-09-25. Gemini Omni Flash is a video model on the
Interactions API and is out of scope for this version.

A model id Iris does not know is refused unless `--capabilities-from <known model>` says which
known model's capabilities it has; such a request is validated as that model but gets no cost
estimate, because the known model's prices need not apply.

Aliases are dated snapshots or unambiguous nicknames (`nano-banana-2`, `nano-banana-pro`,
`veo-fast`). The bare `nano-banana` is deliberately none: Google's "Nano Banana" is
`gemini-2.5-flash-image`, which Iris does not register, so accepting it for Nano Banana 2 would
silently run a differently branded model.

Every name in the "not registered" column, and that nickname, is a name Iris declines: each
provider's catalog module declares them with the provider's reason and date and the registered
models to use instead. An exact name also matches its dated snapshots (`<name>-YYYY-MM-DD`),
families match by prefix (`imagen-…`, `veo-2.0-…`, `veo-3.0-…`, `gemini-omni-…`, `dall-e-…`), and
so do the short names people type for them (`imagen-4`, `veo-3`, `veo-3-fast`, `dalle-3`, `omni`).
`--model dall-e-3` is `unknown_model` with a hint such as "OpenAI removed DALL·E (dall-e-2,
dall-e-3) from the API on 2026-05-12 and recommends a GPT Image 2.5 model for new integrations; use
gpt-image-2.5-sunburst, gpt-image-2.5-flare, or gpt-image-2", and the same name in the config file
is `config_invalid` with the same reason. The replacements named are those that support the
command's operation (for the config file, the key's): a command that none of them fits, such as
`video generate -m dall-e-3`, gets the reason and the command that lists the models it can use.
A declined name gets no `--capabilities-from` suggestion: it would send a model the provider
deprecated, shut down, limited, or serves only elsewhere. Neither does a near miss of registered
models (`gpt-image-2.5`, `veo-3.1-lite`, `Nano-Banana-2`, `GPT Image 2.5 Flare`), since it would
send a guessed id: its hint asks "did you mean …?" with the ids it nearly names
(`details.suggestions`). Near misses are found by four plain rules, tried in order and ignoring
case: an id or alias; a display name; the start of an id or alias; and the name's words, where
every word that some registered name has must be the suggested model's too (a word of four or
more letters may be nearly spelled, by Jaro-Winkler similarity). The last rule is there because
the registered names differ mostly in their tier and version words, and a suggestion that drops
one steers to another price: `veo3-fast` must never suggest Veo 3.1, which costs four times as
much, nor `gpt-image-2.5-flair` the older `gpt-image-2`. Every suggestion can be explained by the
rule that found it (see [json-output.md](../reference/json-output.md#error-object)).

**Sources.** [OpenAI model pages](https://developers.openai.com/api/docs/models/gpt-image-2.5-sunburst) ·
[OpenAI deprecations](https://developers.openai.com/api/docs/deprecations) ·
[OpenAI changelog](https://developers.openai.com/api/docs/changelog) ·
[Gemini image generation guide](https://ai.google.dev/gemini-api/docs/image-generation) ·
[Gemini models](https://ai.google.dev/gemini-api/docs/models) ·
[Gemini deprecations](https://ai.google.dev/gemini-api/docs/deprecations) ·
[Gemini changelog](https://ai.google.dev/gemini-api/docs/changelog) ·
[Veo guide](https://ai.google.dev/gemini-api/docs/veo) ·
[Veo 3.1 on the Gemini Enterprise Agent Platform](https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/veo/3-1-generate) ·
[Gemini Omni](https://ai.google.dev/gemini-api/docs/omni) ·
[Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing)

### What each model is for, and what the same output costs

**Decision.** Every catalog model has a one-line `summary` (what it is for and its trade-off) and
is priced on a standard output, the same for every model of an operation kind: one 1024x1024
image for the image operations, one 8-second 720p video for video (every Veo model supports it).
Each model declares the requests that give exactly that output: for the OpenAI models `--size
1024x1024` at every quality with an estimate (low, medium, and high, and xhigh and max on the 2.5
models), since at one size the quality sets their price; for the Gemini image models 1K at 1:1,
which the aspect-ratio and image-size table of Google's image generation guide gives as 1024x1024
for Nano Banana 2 and Nano Banana Pro (checked 2026-09-24; the table has no row for Nano Banana 2
Lite, whose list of aspect ratios links to it, so Lite's 1K is read from the same table), with the
aspect ratio explicit because an edit without one matches its input image; for Veo 8 seconds at
720p. `models list` and `models show`
report them as `standard_cost` (the output, and each request's options and estimate; human output
lists the estimates with the option values that tell them apart), and the `model_required` and
`unknown_model` candidates carry the summary and the same `standard_cost`. The summaries use
the providers' own positioning: OpenAI's guide chooses Sunburst for workflows where editing
precision matters most and Flare for fast, high-quality everyday generation (its most capable
and its fastest model); Google describes Nano Banana 2 as its most versatile image model, Nano
Banana 2 Lite as its fastest and cheapest, Nano Banana Pro as the premium choice for the most
complex visual tasks, Veo 3.1 Standard as best for professional-grade 4K output and complex
camera movements, and the Veo Fast versions as optimized for speed. For GPT Image 2, which its
page calls state-of-the-art while the guide says to use a 2.5 model for new integrations, the
summary gives that advice and the documented differences: its quality levels and, by OpenAI's
calculator (indicative for the 2.5 models), its output tokens. The catalog tests check the
summaries' relative price claims against the rate tables. The Gemini image and Veo guides were
checked for this on 2026-09-25, the other pages on 2026-09-24.

**Why.** An agent must name its model (see [No default model](#no-default-model)), so it needs to
know what each model is for and what it costs before choosing, without reading provider pages. A
comparison of costs needs the same output: the least each model can cost is the price of a
different output (a 3:1 image at low quality from OpenAI, a 512-pixel image from Nano Banana 2, a
4-second video from Veo), which says little about which model costs less for the same work. The
estimates come from each model's own estimator, so they cannot disagree with the estimate a dry
run of the same request gets, and the catalog tests check that every declared request is valid
for each of the model's operations, asks for one output, and gives the standard output by the
catalog's own declarations (the OpenAI size, Google's image-size table, the Veo duration and
resolution). At other sizes an OpenAI model's price follows OpenAI's calculator formula, which
scales each quality's base down by the aspect ratio: at low quality 1024x1024 needs 196 output
tokens and 1536x1024 158, so a larger non-square size can cost less than a smaller square one.
That is OpenAI's published formula, not an Iris quirk, and the `size` option's description says
so.

**Sources.** [OpenAI model pages](https://developers.openai.com/api/docs/models/gpt-image-2.5-sunburst) ·
[OpenAI image generation guide (calculator)](https://developers.openai.com/api/docs/guides/image-generation) ·
[Gemini image generation guide](https://ai.google.dev/gemini-api/docs/image-generation) ·
[its aspect-ratio and image-size table](https://ai.google.dev/gemini-api/docs/image-generation#aspect_ratios_and_image_size) ·
[Veo guide](https://ai.google.dev/gemini-api/docs/veo) ·
[Veo 3.1 model page](https://ai.google.dev/gemini-api/docs/models/veo-3.1-generate-preview) ·
[OpenAI pricing](https://developers.openai.com/api/docs/pricing) ·
[Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing)

## Paid requests: retries and uncertain outcomes

### Retry classes, and why vendor retry guidance is overridden

**Decision.** Every HTTP call runs under one of three retry classes:

- **Paid submission** (image generate and edit, Veo submit), at most 3 attempts. Retried only
  when the request provably was not processed: a connection that failed before the request was
  sent, an HTTP 429 rate limit (never a quota or billing code such as OpenAI's
  `insufficient_quota`, and never Gemini's 402), or a documented rejection that says the request
  was not processed (OpenAI's 503 `server_is_overloaded`). A timeout or reset after sending, and
  any other 5xx, is never retried.
- **Idempotent read** (Veo polls, model metadata), at most 5 attempts, retrying connection
  errors, timeouts, 408, 429, and 5xx.
- **Download**, at most 5 attempts, with the read rules; every attempt restarts into an emptied
  file.

Backoff is exponential (1 s doubling to at most 30 s) with full jitter. A provider-requested
delay (`Retry-After`, `retry-after-ms`, or Google's `RetryInfo`) is honored up to 60 seconds; a
longer one stops with `rate_limited` and the delay in `retry_after_seconds`. An error that
trying again cannot help (`retryable: false`, such as an uncertain paid submission or a used-up
daily quota) never carries the delay, whatever the answer asked for: a caller that waits it out
and sends the request again could pay twice, or only fail again. An
`x-should-retry: false` response header (which OpenAI sends) stops retries.

**Why.** Both providers' guidance says to retry 5xx and timeouts, and OpenAI's official Python
SDK re-sends a POST on connection errors, 408, 409, 429, and 5xx. Neither Images API nor Veo
offers an idempotency key or a way to find a request whose answer was lost, so resending an
ambiguous paid request can generate, and bill, the same output twice. Iris therefore retries
paid requests only where the provider has said the request was not processed, and leaves every
other decision to the caller. Rate-limit retries follow OpenAI's guidance to honor `Retry-After`
as a minimum and to stop, rather than retry sooner, when the requested delay is longer than the
client will wait.

**Sources.** [OpenAI rate limits and retries](https://developers.openai.com/api/docs/guides/rate-limits) ·
[OpenAI error codes](https://developers.openai.com/api/docs/guides/error-codes) ·
[OpenAPI specification](https://github.com/openai/openai-openapi) (no idempotency key on the Images endpoints) ·
[openai-python `_base_client.py`](https://github.com/openai/openai-python/blob/main/src/openai/_base_client.py) ·
[Gemini troubleshooting (retry guidance)](https://ai.google.dev/gemini-api/docs/troubleshooting) ·
[Gemini API errors](https://ai.google.dev/gemini-api/docs/generate-content/api-errors) ·
[Google AIP-194: automatic retry configuration](https://google.aip.dev/194)

### Reporting an outcome Iris cannot know

**Decision.** A paid request whose outcome is unknown is reported as `submission_uncertain`
(exit 5), never resubmitted:

- **Images, both providers:** a timeout or a lost connection after the request was sent, or an
  answer that could not be read in full (cut off, or longer than the 512 MiB Iris reads). The
  error has `retryable: false`, `details.charge_possible: true`, and `job_id: null`.
- **OpenAI images:** also an HTTP 408 or 5xx answer, except the documented overload rejection.
- **Gemini images:** an HTTP 4xx or 5xx answer is not uncertain; it keeps its ordinary code
  (a 5xx is `provider_error`, retryable) without `charge_possible`.
- **Veo:** a 408 or 5xx answer, a timeout or reset after sending, or a 2xx answer without a usable
  operation name. The job stays on disk as `submission_unknown`, and Iris never resubmits it.

Ctrl-C (or SIGTERM or SIGHUP) while a paid request is in flight reports `interrupted` with
`retryable: false` and `details.charge_possible: true`. During a Veo submission the first
interrupt is deferred until the answer arrives, so the operation name is recorded.

**Why.** Google's billing documentation says requests that fail with a 400 or 500 error are not
charged, so a Gemini error answer proves nothing was billed; OpenAI makes no such statement, and
neither provider documents whether a request that timed out on the client was processed. Veo
keeps the strict rule for every 5xx: such an answer does not prove that no operation was created,
and an operation that exists runs, and is billed if it succeeds, with nothing for Iris to follow
it by. Exit 5 and `charge_possible` tell a caller
to check the provider's usage page before trying again.

A definite rejection of a malformed request (an HTTP 400 that the provider documents as not
generating output) is `invalid_argument` with exit 2, like a local validation error: in both
cases the request has to be fixed before it is run again. `error.provider_status` is `null`
exactly when nothing was sent.

**Sources.** [Gemini billing (failed requests are not charged)](https://ai.google.dev/gemini-api/docs/billing) ·
[OpenAI error codes](https://developers.openai.com/api/docs/guides/error-codes) ·
[Veo guide](https://ai.google.dev/gemini-api/docs/veo) ·
[v1beta discovery document](https://generativelanguage.googleapis.com/$discovery/rest?version=v1beta) (no request id or idempotency field)

## Jobs: persistence, locking, and atomic writes

**Decision.** Only provider-native asynchronous operations (Veo) create job records; synchronous
image calls never do. A record is written before the paid request is sent and updated with the
operation name after it, in `<state_dir>/jobs/<job_id>.json` (directory mode `0700`, file mode
`0600`). Records are versioned (`schema_version: 1`): a reader refuses a record from a newer
major version, keeps fields it does not know when it rewrites a record, and reads error codes it
does not know as `internal_error` while keeping the original. Every write goes to a temporary file
in the same directory, is flushed with `sync_all`, renamed over the record, and followed by a
best-effort directory sync. A per-job lock file (`std::fs::File::lock`, which is `flock` on Linux
and macOS) serializes each read-modify-write, and a second per-job lock is held for a whole
download; listing takes no lock. A job may carry a caller's label (`--label`), unique among the
records of the state directory: a record with a label is created under a store-wide lock
(`labels.lock`), taken before the other records' labels are read and held until the new record is
written, so a label another record has is refused (`label_in_use`) and two processes creating one
label cannot both submit; while a record cannot be read, a labeled submission is refused, since that
record could have the label. A record still `submitting` after the whole paid-submission
timeout budget plus 60 seconds is reported as `submission_unknown`. Prompts are stored as a
SHA-256 hash and a character count unless `jobs.store_prompts` is enabled. `jobs delete` removes
only local records, refuses active jobs without `--force`, and never cancels anything remotely.

**Why.** Writing the record first means a crash inside the submission window still leaves
something to diagnose instead of an untracked paid job. Temporary file plus rename is the
standard way to make a file replacement atomic, so concurrent readers never see a partial record,
and `flock` locks work between processes and between threads of one process because they belong
to the open file description. The standard library has provided file locking since Rust 1.89, so
no locking crate is needed; `tempfile` supplies the exclusive temporary files. Keeping unknown
fields lets an older Iris rewrite a record written by a newer one without destroying data.
Prompts can be sensitive, and a hash is enough to match a job to a prompt you still have.
A submission cannot be made idempotent at the provider, since Veo offers no idempotency key (see
[Retry classes](#retry-classes-and-why-vendor-retry-guidance-is-overridden)), so the label makes it
idempotent locally: a script that reruns with the same label after a crash finds the job instead of
paying for a second one. The check has to share a lock with the write, since two processes that
each checked first and then wrote could both submit, and it cannot pass over a record it cannot
read.

**Sources.** [`std::fs::File::lock`](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock) ·
[`tempfile::NamedTempFile`](https://docs.rs/tempfile/3.27.0/tempfile/struct.NamedTempFile.html) ·
[Veo guide (operation names and polling)](https://ai.google.dev/gemini-api/docs/veo)

## Downloads and trust

**Decision.** Generation and download are separate outcomes. A finished Veo operation with output
URIs makes the job `succeeded` and records the URIs; whether Iris will fetch one is decided at
download time against the base URL configured then. A Veo output URI is fetched only if it is on
the configured Gemini base URL's origin, under its path, in the form `…/v1beta/files/<id>:download`
with a File id of 1 to 40 lowercase letters, digits, or dashes that neither starts nor ends with
a dash. A refused URI fails that output's download (`download_failed`), never the job. The HTTP
client follows no redirects on its own: downloads follow at most 5 hops themselves, send the
provider credential only to hops on the configured base URL's origin, and require `https` unless
the base URL itself is `http` (local mock servers). Bytes stream into a
`.<name>.iris-part-<random>` file in the target directory, created exclusively, then are
validated (magic bytes and structure, and an API error document served as 200 is rejected) and
renamed into place without replacing an existing file unless `--overwrite` is given. A download
is capped at 4 GiB. Repeating a download of an intact file does nothing
(`already_downloaded`). A paid image that cannot be saved where requested is written to
`<state_dir>/unsaved/` and reported, never discarded; so is returned content that is not a valid
image, kept as received (`.bin`).

**Why.** The Veo guide downloads outputs with the API key and `curl -L`, so a redirect is
expected and its target host is not documented. `reqwest`'s default redirect policy removes a
few well-known sensitive headers (such as `Authorization` and cookies) on a cross-origin hop but
keeps the rest, so it would forward
`x-goog-api-key` to whatever host a redirect names; following redirects manually is the only way
to keep the key on the provider's origin. The File id rule comes from the Files API reference
(the Python SDK's own parser would cut an id at its first dash). Making the trust check a download
decision means a job behind a proxy that does not rewrite URIs still succeeds and can be
downloaded later, after the base URL is corrected, while the provider still keeps the output.

**Sources.** [Veo guide (download with `curl -L`)](https://ai.google.dev/gemini-api/docs/veo) ·
[Files API reference (File id rule)](https://ai.google.dev/api/files) ·
[reqwest 0.13.5 redirect handling (source)](https://docs.rs/crate/reqwest/0.13.5/source/src/redirect.rs) ·
[reqwest changelog](https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md)

## Retention and expiry

**Decision.** Veo outputs are recorded as kept for 2 days: `remote_expires_at` is `submitted_at`
plus 2 days, the earliest time Google may delete them. Iris never refuses a download on that
estimate; it always asks the file host. A 410 answer is `artifact_expired`. A 403 or 404 is
`artifact_expired` only after `remote_expires_at`; before it, it is a retryable
`download_failed` and the output stays downloadable. A status check that answers 404 makes a job
`expired` only when the answer is Google's `NOT_FOUND` error and the retention period since
submission has passed; any other 404 leaves the job `running` and reports the error. Local job
records are never pruned automatically.

**Why.** Google documents that generated videos are stored for 2 days and then removed. It does
not document how long operations stay pollable or exactly when the 2 days start, so counting from
submission gives the earliest possible deletion time, and treating it as a lower bound avoids
refusing a download the provider would still serve. A 404 inside the retention period more likely
means a key from another project or a base URL pointing elsewhere than a deleted job, so it must
not turn a paid job into `expired`.

**Sources.** [Veo guide (2-day retention)](https://ai.google.dev/gemini-api/docs/veo) ·
[Gemini API errors (`NOT_FOUND`)](https://ai.google.dev/gemini-api/docs/generate-content/api-errors)

## Access, credentials, and cost estimates

**Decision.** Iris targets the developer APIs only. Credentials are read only from
`OPENAI_API_KEY` and `GEMINI_API_KEY`; `GOOGLE_API_KEY` is ignored (and `doctor` warns when it is
set). `--check-access` makes one free model-metadata read and reports that a model is visible to
the key, not that the account can use it. Cost figures are estimates from the providers'
published prices (before a call) or from the usage the provider reports (after one), labeled as
estimates with their source and date; when no point estimate is supportable (for example an
`auto` size or quality on OpenAI), Iris says so instead of guessing, and the model's estimator
names the options to pass for one. Every model declares its `billing`, reported by `models
list`, `models show` and dry-run plans: `paid` for every model today, since requests are billed
to the provider account at its published prices and neither provider has a free tier for these
models (not every request is billed: Google does not charge for a video it blocks). Human output
derives its "this is a paid request" wording from that value. `--max-cost <USD>` caps what one
generation command may spend: a request whose pre-call estimate is above the cap, or that has no
estimate, is refused before anything is sent (`cost_limit_exceeded`), in a dry run too. The cap
is given on the command line only, with no config key or environment variable.

**Why.** Consumer subscriptions do not grant API access: Google states that Google AI plan
benefits apply only in the AI Studio web interface and that API use is billed separately
(Developer Program Cloud credits can pay for API usage). Google's SDKs read `GOOGLE_API_KEY` in
preference to `GEMINI_API_KEY` when both are set, so honoring it would let a stray variable
silently pick the key. Gemini image and Veo models have no free tier, and Google says standard
API keys will be rejected from September 2026 (no exact day given), so the access notes
recommend an auth key. OpenAI may require API Organization Verification for GPT Image models;
neither that nor a billing tier is visible to a metadata read, which is why the check claims only
visibility. An agent needs whether a request costs money, and what to change to learn how much,
as data it can act on rather than prose: `billing` is a value (an open set, so a later free model
needs no special case), and the estimator that cannot price a request knows which of its options
made it unknowable. A cap lets an agent that pays stop a request before it is sent, and the
pre-call estimate is the only figure Iris has at that point, so the cap compares the estimate and
says so: estimates can leave out prompt, input-image, and thinking tokens (each basis says what),
so the bill can be higher than the cap, by what the basis leaves out, and it is never described
as a guarantee. The difference can be large: OpenAI does not document how the input images of a
GPT Image edit are counted, and an edit takes up to 16 of them. A request without an estimate
cannot be checked, so it is refused rather than let through; the refusal carries the reason there
is none, which says what to pass for an estimate when other options give one, and otherwise (a
model without an estimator, or one resolved with `--capabilities-from`) the hint says to run
without the cap. A cap in the
config file or the environment would apply to commands that never mention it, an unseen choice of
the kind Iris avoids for the model too (see [No default model](#no-default-model)).

**Sources.** [Google AI plans and the Gemini API](https://ai.google.dev/gemini-api/docs/google-ai-plans) ·
[Gemini API keys](https://ai.google.dev/gemini-api/docs/api-key) ·
[Gemini API pricing](https://ai.google.dev/gemini-api/docs/pricing) ·
[Gemini billing](https://ai.google.dev/gemini-api/docs/billing) ·
[OpenAI image generation guide (organization verification)](https://developers.openai.com/api/docs/guides/image-generation) ·
[OpenAI pricing](https://developers.openai.com/api/docs/pricing)

## Hand-rolled code, and why

Iris prefers maintained libraries and tools; these pieces are its own code, each for a stated
reason.

- **Retry classification.** The backoff schedule comes from the `backon` crate; the decision
  *whether* a failure may be retried is Iris's own, because no crate models paid versus
  idempotent requests. `reqwest-retry` treats every 5xx, 408, 429, timeout, and connection error
  as transient for any HTTP method and does not read `Retry-After`; `backoff` is unmaintained
  (RUSTSEC-2025-0012). Iris also applies full jitter itself, because `backon`'s jitter adds up to
  one extra delay on top of the schedule and would exceed the cap.
- **Release pipeline.** Releases are built by a GitHub Actions workflow plus a small packaging
  script (`scripts/package-release.sh`), on pinned, established actions, instead of `cargo-dist`.
  Iris needs a gate that runs every packaged binary and installs it with `install.sh` before
  anything is published. dist's documented pipeline has no step that executes the built binary,
  and adding one means custom jobs in a workflow file that dist generates and owns (it refuses to
  run when that file differs from what it would generate, unless that check is switched off).
  dist is also still before 1.0. The hand-rolled part is small: packaging into versioned
  `iris-vX.Y.Z-<target>.tar.gz` archives with one `SHA256SUMS` file, and a publish job that runs
  only after every check and smoke test passed on the tagged commit.
- **Installer.** `install.sh` is a short POSIX `sh` script, because the release archives are not
  dist's (so its generated installer does not apply) and the installer has specific duties:
  verify the archive against `SHA256SUMS` before installing, reject unexpected archive contents,
  run the binary before replacing anything, never read from stdin, never use `sudo`, and fail
  clearly on unsupported platforms. It is tested offline against locally served fixtures.
- **Media structure checks.** Images of the decodable types are fully decoded with the `image`
  crate and sniffed with `infer`; MP4 and HEIC/HEIF files are checked by a small ISO-BMFF box
  walker, and GIF by a block walker. Iris needs only a few facts (first box, `moov` present,
  duration, truncation), and the MP4 crates are unmaintained (`mp4`), MPL-licensed (`mp4parse`),
  newer than the minimum Rust version (`re_mp4`), or much heavier; the walker is about a hundred
  lines of bounds-checked parsing.
- **Lexical path normalization.** Planned output paths drop `.` and resolve `..` without touching
  the file system; the standard library's `Path::normalize_lexically` is not stable yet.

**Sources.** [backon](https://docs.rs/backon/1.6.0/backon/) ·
[reqwest-retry](https://docs.rs/reqwest-retry/0.9.1/reqwest_retry/) ·
[RUSTSEC-2025-0012](https://rustsec.org/advisories/RUSTSEC-2025-0012.html) ·
[dist: CI customization and custom jobs](https://axodotdev.github.io/cargo-dist/book/ci/customizing.html) ·
[dist: CI overview](https://axodotdev.github.io/cargo-dist/book/ci/index.html) ·
[dist: configuration reference (`allow-dirty`)](https://axodotdev.github.io/cargo-dist/book/reference/config.html) ·
[dist releases](https://github.com/axodotdev/cargo-dist/releases) ·
[image](https://crates.io/crates/image) · [infer](https://crates.io/crates/infer) ·
[mp4](https://crates.io/crates/mp4) · [mp4parse](https://crates.io/crates/mp4parse) ·
[re_mp4](https://crates.io/crates/re_mp4) · [mp4-atom](https://crates.io/crates/mp4-atom) ·
[`Path::normalize_lexically`](https://doc.rust-lang.org/std/path/struct.Path.html#method.normalize_lexically)

## Dependencies and toolchain

- **Minimum Rust version 1.89, edition 2024.** 1.89 is the first release with
  `std::fs::File::lock`, which replaces a locking crate; edition 2024 needs 1.85, and no
  dependency declares a newer minimum. A CI job checks the crate and its tests and runs the tests
  (`cargo check --all-targets`, `cargo test`) with the `rust-version` that `Cargo.toml` declares,
  so the claim stays true.
- **`dirs` rather than `directories`.** `directories`' GitHub repository is archived and has had
  no release since January 2025; `dirs` is maintained (7.0.0, September 2026) and gives exactly
  the paths Iris documents: XDG config and state directories on Linux (absolute
  `$XDG_*_HOME` only), and `~/Library/Application Support` on macOS, where it has no state
  directory, so Iris uses the data directory. Both pull in `option-ext`, which is MPL-2.0; the
  license check allows MPL-2.0 for that one crate only, since file-level copyleft on an
  unmodified dependency does not affect Iris's MIT license. Distributing the binary still
  obliges Iris to tell its recipients where `option-ext`'s source is (MPL-2.0 section 3.2(a)),
  which `THIRD-PARTY-LICENSES` does (next item).
- **Third-party notices in every release archive, generated by cargo-about.** The release binary
  statically links its dependencies, and their licenses (MIT, Apache-2.0, BSD-3-Clause, ISC,
  Unicode-3.0, MPL-2.0) require passing on their license texts and copyright notices, or where
  the source is, with the binary. `scripts/package-release.sh` runs `cargo about generate`
  for the archive's target, so the file covers exactly the normal dependencies linked into that
  binary, and writes `THIRD-PARTY-LICENSES`: each license's text with the crates using it, each
  crate's crates.io address (where its source is), and a note on the MPL-2.0 crates.
  `about.toml` accepts the same licenses as `deny.toml`, and ships the combined notice files of
  `aws-lc-sys` and `aws-lc-rs` whole (pinned by checksum), because cargo-about cannot match them
  to one license and would otherwise drop their copyright notices. Packaging requires one exact
  cargo-about version and fails on any cargo-about warning, so the notices are complete and the
  archives stay reproducible.
- **`reqwest` 0.13 with its default `rustls` TLS.** In 0.13, `rustls` with the aws-lc crypto
  provider and `rustls-platform-verifier` is the default, so certificates are verified against
  the operating system's trust store (on Linux, the system CA bundle). Minimal containers therefore
  need `ca-certificates`. The aws-lc provider needs a C compiler to build but no CMake, and it
  builds for the static musl target with the distribution's musl tools (CI builds that target on
  every change).
- **Everything else** was chosen for maintenance, license, and weight: `clap` and
  `clap_complete` for parsing and completions, `tokio`, `serde`/`schemars` (the published schema
  is generated from the serialized types), `jiff` for timestamps, `tempfile`, `sha2`, `ulid`,
  `toml`, `tracing`, `signal-hook` for SIGTERM/SIGHUP, `strsim` (which `clap` already links, for
  its own suggestions) for the words of a near-miss model name, and `wiremock`, `assert_cmd`, and
  `jsonschema` for tests. `cargo deny check` enforces MIT-compatible licenses and no known
  advisories.

**Sources.** [`std::fs::File::lock` (stable since 1.89.0)](https://doc.rust-lang.org/std/fs/struct.File.html#method.lock) ·
[Rust 2024 edition](https://doc.rust-lang.org/edition-guide/rust-2024/index.html) ·
[dirs](https://docs.rs/dirs/7.0.0/dirs/) · [dirs-rs](https://codeberg.org/dirs/dirs-rs) ·
[directories-rs (archived)](https://github.com/dirs-dev/directories-rs) ·
[reqwest changelog (0.13: rustls and the platform verifier by default)](https://github.com/seanmonstar/reqwest/blob/master/CHANGELOG.md) ·
[aws-lc-rs build requirements](https://aws.github.io/aws-lc-rs/requirements/index.html) ·
[cargo-deny](https://embarkstudios.github.io/cargo-deny/) ·
[cargo-about](https://embarkstudios.github.io/cargo-about/) ·
[MPL-2.0](https://www.mozilla.org/en-US/MPL/2.0/) ·
[option-ext](https://crates.io/crates/option-ext)

## Release targets and CI

**Decision.** Releases ship three archives: `x86_64-unknown-linux-musl` (a static binary) and
`x86_64-apple-darwin` and `aarch64-apple-darwin`, each built on a native runner. There is no
Linux arm64 or Windows build. CI runs formatting, Clippy with warnings denied, offline tests on
Linux and macOS, the minimum-Rust-version check, and `cargo deny` for advisories and licenses,
with actions pinned to commit SHAs; the advisory scan also runs weekly. CI on `main` uses current
stable Rust and the minimum version; releases are checked and built with one pinned Rust version,
which the release workflow logs (`rustc -Vv`) and which maintainers bump deliberately. A release's
notes are its version's `CHANGELOG.md` section, read with parse-changelog, and a tag without one,
or with an empty one, is not released.

**Why.** A `-gnu` binary built on a recent runner needs at least that runner's glibc and fails on
older distributions; the musl build is statically linked and runs on any x86_64 Linux kernel 3.2
or newer, regardless of the host's C library. Rust's platform table gives macOS 10.12 as the
minimum for `x86_64-apple-darwin` and macOS 11.0 for `aarch64-apple-darwin`. The macOS 13 runner
image has been retired, so the Intel build uses the current Intel runner label. `cargo-deny`
covers both advisories and licenses with one tool, and its action has current releases. With
`stable`, a Rust release landing between CI on `main` and the tag push would change the compiler,
and Clippy's lints, under an already-tested commit, and no record would say which compiler built
an archive; a pinned version makes a release repeatable and its log says what built it. GitHub's
generated notes list pull requests, not the user-facing changes the changelog records.
parse-changelog is the established tool for reading one version's section of such a file
(create-gh-release-action uses it). A simple line-based extractor stops early at a reference-link
definition or at a `## ` line inside a code block and returns the shortened section without an
error; parse-changelog handles both and rejects a version with two headings. Only the release's
verify job, which has a read-only token, runs it; it passes the text
to the publish job as an artifact. So the job holding the write token runs no extra tool and
publishes exactly the text that was checked.

**Sources.** [Rust platform support](https://doc.rust-lang.org/nightly/rustc/platform-support.html) ·
[GitHub-hosted runner images](https://github.com/actions/runner-images) ·
[macOS 13 runner retirement](https://github.blog/changelog/2025-09-19-github-actions-macos-13-runner-image-is-closing-down/) ·
[cargo-deny-action](https://github.com/EmbarkStudios/cargo-deny-action) ·
[parse-changelog](https://github.com/taiki-e/parse-changelog)
