# Configuration

## Credentials

Iris reads provider credentials **only** from environment variables — never from the config
file, never from a command-line flag, and never with a fallback name:

| provider | environment variable |
|---|---|
| OpenAI | `OPENAI_API_KEY` |
| Google Gemini (images and Veo) | `GEMINI_API_KEY` (not `GOOGLE_API_KEY` — Iris explicitly ignores that name; `doctor` warns when `GOOGLE_API_KEY` is set, so a common mistake doesn't fail silently) |

Credentials are held in a `Secret` type that never prints or serializes its contents — Iris
reports only *presence*, never a value. Real `doctor` output, abridged to the credential rows
(`...` marks the omitted lines; `doctor` also reports `config`, `state_dir`, `output_dir`,
`base_url.openai`, `base_url.gemini`, and `jobs` — see [Setup](../README.md#setup) in the README
for the full block):

```console
$ iris doctor
...
[ok]      credentials.openai: OPENAI_API_KEY is set
[warning] credentials.gemini: GEMINI_API_KEY is not set; gemini commands will fail with missing_credentials
...
```

A key that is not set is a warning while another provider's key is set. With no key set at all,
`doctor` adds an error, and `healthy` is false:

```console
$ iris doctor
...
[error]   credentials: no provider API key is set (OPENAI_API_KEY, GEMINI_API_KEY): every generation command would fail with missing_credentials
...
Problems found (see [error] lines).
```

A missing key for the provider a command needs is `missing_credentials` (exit 3), checked after
every other local validation and before any network call — so a bad prompt or an unsupported
option is still reported as such even with no key set. `video generate`, `jobs wait`, and
`jobs download` also check it before creating any output directory (only a fetch from the
provider's own origin needs it: copying an already downloaded output does not), so a run that
stops for a missing key leaves nothing behind. `--dry-run` never requires a credential;
it reports whether one is present without requiring it.

## Config file

TOML. Location, in order: `--config <PATH>` > `IRIS_CONFIG` > the platform default:

| platform | default location |
|---|---|
| Linux | `$XDG_CONFIG_HOME/iris/config.toml`, else `~/.config/iris/config.toml` |
| macOS | `~/Library/Application Support/iris/config.toml` |

A missing *default* file is fine: settings then come from flags, the environment, and the built-in
defaults, and no model is configured. A file named explicitly by `--config` or `IRIS_CONFIG` that
does not exist is `config_invalid` — you asked for it, so Iris tells you it isn't there rather
than silently falling back. `iris config path` prints the real, absolute locations (output on
Linux with `HOME=/home/you` and no `XDG_*` or `IRIS_*` variables set):

```console
$ iris config path
config file: /home/you/.config/iris/config.toml
state dir:   /home/you/.local/state/iris
jobs dir:    /home/you/.local/state/iris/jobs
```

Full key set:

```toml
output_dir = "~/Pictures/iris"        # ~ expanded
state_dir = "/custom/state"           # optional

[image]
model = "gpt-image-2.5-sunburst"      # image generate/edit without -m; a catalog image model

[video]
model = "veo-3.1-lite-generate-preview"   # video generate without -m; a catalog video model
wait_timeout = "10m"
poll_interval = "10s"

[jobs]
store_prompts = false                 # see docs/jobs.md — off by default

[providers.openai]
base_url = "https://api.openai.com/v1"
request_timeout = "300s"

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com"   # origin; Iris appends /v1 or /v1beta
request_timeout = "300s"
submit_timeout = "60s"
```

`[image] model` and `[video] model` are the only settings that name a model; see
[Choosing the model](#choosing-the-model). There is one `[providers.<id>]` table per provider
(`openai`, `gemini`), with the same keys; `submit_timeout` applies only to a provider with video
models (`gemini`), and setting it for another is `config_invalid`. See [Timeouts](#timeouts) for
what `request_timeout` and `submit_timeout` cover. **Unknown keys are rejected**, not ignored — a
typo is caught immediately rather than silently doing nothing (a relative `--config` path
resolves against the current directory, `/home/you` here):

```console
$ iris --config bad.toml config show
error[config_invalid]: config file /home/you/bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `request_timeout`, `submit_timeout`
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
$ echo $?
2
```

A table for a provider Iris does not have is rejected the same way, whatever it contains
(`providers.<id>`: unknown key; expected one of `openai`, `gemini`).

**Any key that looks like a credential — `api_key`, anything ending in `_key`, `key`, `token`,
`secret`, or `password`, case-insensitive, at any depth — is rejected too**, with a message
pointing at the environment variables instead, so a well-meaning `api_key = "sk-..."` in a config
file (which would otherwise get committed to a repo) is caught rather than silently accepted:

```console
$ iris --config bad2.toml config show
error[config_invalid]: config file /home/you/bad2.toml: `providers.openai.api_key`: credentials are read only from OPENAI_API_KEY / GEMINI_API_KEY, never from the config file
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

A model in `image.model` or `video.model` that the catalog does not know, or that is of the other
kind, is also caught at config-load time, before any command tries to use it:

```console
$ iris --config bad3.toml config show
error[config_invalid]: config file /home/you/bad3.toml: `image.model`: unknown model 'not-a-real-model'
  hint: set image.model to a model listed by `iris models list --operation image.generate` or `iris models list --operation image.edit`
$ iris --config bad4.toml config show
error[config_invalid]: config file /home/you/bad4.toml: `video.model`: model 'gemini-3.1-flash-image' does not support video.generate (supports: image.generate, image.edit)
  hint: set video.model to a model listed by `iris models list --operation video.generate`
```

`iris models list` and `iris models show` (without `--check-access`) never read the config file,
so while the file is invalid you can still list the models these hints name and inspect each one's
options and prices. A name Iris declines, such as `nano-banana` or `dall-e-3`, gets the same
hint as with `-m` (see [decisions.md](decisions.md#built-in-models)).

`iris doctor` still runs the checks that do not need a valid configuration when the config file
itself is invalid, so an invalid file doesn't hide unrelated problems like a missing credential.
The directory, base URL, and job checks need the resolved settings, so they are skipped (real,
complete output):

```console
$ iris --config bad.toml doctor
[error]   config: config file /home/you/bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `request_timeout`, `submit_timeout` (fix the config file, or point --config / IRIS_CONFIG at another file)
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
Problems found (see [error] lines).
$ echo $?
0
```

## Precedence: flag > environment variable > config file > default

| setting | env var | flag | default |
|---|---|---|---|
| output directory | `IRIS_OUTPUT_DIR` | `-d`/`--out-dir` | current directory |
| state directory | `IRIS_STATE_DIR` | — | platform default |
| image model (`image generate`, `image edit`) | — (config file `image.model` only) | `-m`/`--model` | none: `model_required` |
| video model (`video generate`) | — (config file `video.model` only) | `-m`/`--model` | none: `model_required` |
| video wait timeout | `IRIS_WAIT_TIMEOUT` | `--timeout` | `10m` |
| video poll interval | `IRIS_POLL_INTERVAL` | `--poll-interval` | `10s` |
| store prompts in job records | `IRIS_STORE_PROMPTS` | — | `false` |
| OpenAI base URL | `IRIS_OPENAI_BASE_URL` | — | `https://api.openai.com/v1` |
| Gemini base URL | `IRIS_GEMINI_BASE_URL` | — | `https://generativelanguage.googleapis.com` |
| config file path | `IRIS_CONFIG` | `--config` | platform default |
| log filter | `IRIS_LOG` | `-v` (repeatable) | `warn` |

An environment variable's value is validated exactly like a config-file value — a bad one is
`config_invalid` naming the variable, not silently ignored. That includes paths: `IRIS_CONFIG`,
`IRIS_OUTPUT_DIR`, and `IRIS_STATE_DIR` must be absolute or start with `~/`, like `output_dir` and
`state_dir` in the config file, because a relative one would follow each command's working
directory (a state directory that moves loses its jobs). Relative paths given as flags
(`--config`, `-d`/`--out-dir`, `-o`, input files) resolve against the current directory.

`--max-cost` and `--label` have no environment variable or config key: a spending cap or a label
applies only to the command that names it. `--max-cost` caps the request's pre-call estimate, never
the bill (see [`cost_limit_exceeded`](json-contract.md#error-object)); `--label` is described in
[jobs.md](jobs.md#labels-find-a-job-and-never-submit-it-twice).

If the current directory does not exist (it was deleted under a running shell), commands that
do not need it still work: `version`, `schema`, `completions`, `--help`, `config path`/`show`,
`providers list`, `models`, `jobs list`/`status`, and `doctor` (`config show` then reports the
default output directory as `.`, and `doctor` flags it as unusable). A command that does need it —
one given a relative path, or a generation command whose outputs would go to the default output
directory — fails with `io_error` before anything is sent; an absolute `-o`, `-d`, or
`IRIS_OUTPUT_DIR` avoids that.

Real `config show` output, with `HOME=/home/you`, both keys set, and
`IRIS_STATE_DIR=/home/you/iris-state`, `IRIS_OPENAI_BASE_URL=http://127.0.0.1:8080/v1`, and
`IRIS_GEMINI_BASE_URL=http://127.0.0.1:8080` (the table goes to stdout; the two
`non_default_base_url` warnings it also prints go to stderr and are shown in
[Base URL overrides](#base-url-overrides)):

```console
$ iris config show
config file: /home/you/.config/iris/config.toml (not found)
SETTING                           VALUE                               SOURCE
config_file                       /home/you/.config/iris/config.toml  default
output_dir                        /home/you                           default
state_dir                         /home/you/iris-state                env IRIS_STATE_DIR
image.model                       (none)                              default
video.model                       (none)                              default
video.wait_timeout                10m                                 default
video.poll_interval               10s                                 default
jobs.store_prompts                false                               default
providers.openai.base_url         http://127.0.0.1:8080/v1            env IRIS_OPENAI_BASE_URL
providers.openai.request_timeout  5m                                  default
providers.gemini.base_url         http://127.0.0.1:8080/              env IRIS_GEMINI_BASE_URL
providers.gemini.request_timeout  5m                                  default
providers.gemini.submit_timeout   1m                                  default
log                               warn                                default
credentials (presence only):
  OPENAI_API_KEY: set
  GEMINI_API_KEY: set
```

## Choosing the model

Iris never chooses a model for you. A generation command uses, in order:

1. `-m`/`--model`: a catalog id or alias (`iris models list` shows them), or an id the catalog does
   not know together with `--capabilities-from <KNOWN_MODEL>`;
2. the config file: `[image] model` for `image generate` and `image edit`, `[video] model` for
   `video generate`;
3. nothing: the command fails with `model_required` (exit 2) before anything is sent, and a
   `--dry-run` fails the same way.

The provider is the model's provider: the generation commands have no `--provider` flag (`models
list` and `jobs list` have one, as a filter). No environment variable names a model.

```console
$ iris image generate "a fox"
error[model_required]: image.generate needs a model: pass -m/--model, or set model in the [image] table of the config file
  hint: run `iris models list --operation image.generate` and pass -m <MODEL>, or set model under [image] in /home/you/.config/iris/config.toml
$ echo $?
2
```

With `--json`, the error's `details` hold the `operation`, the `config_key` (`image.model` or
`video.model`), the resolved `config_file`, and the `candidates`: one `{model, provider,
display_name, summary, aliases, standard_cost}` object per catalog model that supports the
operation, in catalog order, with what the model is for and what the same output costs with it
(one 1024x1024 image, or one 8-second 720p video; see
[json-contract.md](json-contract.md#error-object)). An
`-m` naming no catalog model is `unknown_model` (exit 2) with the same `candidates`; a near miss of
catalog models (`gpt-image-2.5`, `Nano-Banana-2`) has them in `suggestions` and a hint asking "did
you mean …?", and a name Iris declines (a model its provider deprecated, shut down, limited, or
serves only elsewhere, such as `dall-e-3` or `veo-3`) has a hint that says why and what to use
instead (see [decisions.md](decisions.md#built-in-models)).

A configured model must be a catalog id or alias of its table's kind (`image.model` an image
model, `video.model` a video model); anything else is `config_invalid` naming the key when the
config loads (see [Config file](#config-file)). It is stored as the id `-m` would send: the
canonical id for a nickname such as `nano-banana-2`, a dated snapshot as written; `config show`
shows it. `--capabilities-from` has no config equivalent. If `image.model` does not support the
operation being run, that command fails with `unsupported_operation`, naming the key in its
message and in `details.config_key`, with the models that do support it in `details.candidates`.

`-m` always wins over the config file. Every result says which of the two chose the model:
`model_source` is `flag` or `config` in the dry-run plan, the image result, and the job (see
[json-contract.md](json-contract.md)). Human output names the key when the config file chose it
(output with `[image] model = "nano-banana-2"` and no key set):

```console
$ iris image generate "a fox" --dry-run
Dry run: nothing was sent and nothing was charged.
  operation:  image.generate
  provider:   gemini
  model:      gemini-3.1-flash-image (config image.model)
  async job:  no
  billing:    paid (requests are billed to the provider account at its published prices; no free tier)
  options:    count=1 resolution=1K thinking_level=minimal
  output:     /home/you/iris-<ulid>.jpg
  credential: GEMINI_API_KEY is NOT set (required for the real run)
  cost:       ~$0.067 USD (1 image × $0.067 (gemini-3.1-flash-image, 1K); input and thinking tokens not included)
```

The progress line of a real run names it the same way: `Requesting 1 image from gemini
(gemini-3.1-flash-image, config image.model); this is a paid request`. So does an error that names
the model, after its name, and its `details.model_source` says `config` (`flag` for `-m`): with that
file, `iris image generate "a fox" --quality low` fails with `model 'gemini-3.1-flash-image' (config
image.model) does not support --quality for image.generate`.

## Timeouts

Each request Iris sends has its own time limit, per attempt (a retried request gets a fresh one).
None of them is the caller's wait limit: that is `video.wait_timeout` (`--timeout`), and when it
passes, the remote job continues (see [jobs.md](jobs.md)). A dry run of `video generate` shows
the wait limit and poll interval it would use, with the source of each.

| time limit | covers | default | setting |
|---|---|---|---|
| connect | establishing the connection of every request | 15 s | — |
| `request_timeout` | one paid image request (`image generate`, `image edit`), from connecting until its answer, which carries the images, is read | 5 m | `providers.<id>.request_timeout` |
| `submit_timeout` | one Veo job submission (`video generate`), until the answer naming the job arrives | 1 m | `providers.gemini.submit_timeout` |
| status check | one status request (`jobs status`, `jobs wait`, `jobs download`, `video generate` while waiting) and one model-metadata read (`--check-access`) | 30 s | — |
| download idle | the longest pause between two pieces of a download (a download as a whole has no time limit) | 1 m | — |

**Upload allowance.** A request with a body — an image edit with its input images, a Veo job
with reference or frame images — also gets the time needed to upload that body at 256 KiB/s
(about 2 Mbit/s), at most 10 minutes more: a 23 MB Veo request gets `submit_timeout` + about
90 s. A small JSON request gets nothing noticeable. A paid request cut off mid-upload has an
unknown outcome (`submission_uncertain`, and for a video job `submission_unknown`), so if your
uplink is slower than that, raise `request_timeout` or `submit_timeout` rather than retrying.

A job record still `submitting` is only declared abandoned once no live submitter could still be
waiting for its answer: three attempts, each allowed the connect time limit plus `submit_timeout`
plus the largest upload allowance, plus up to a minute of waiting between attempts, plus a minute
of grace — about 37 minutes with the defaults (see [jobs.md](jobs.md)). The submitting process
records its own budget in the job record, so a later process with shorter timeouts still waits at
least that long.

## Base URL overrides

`providers.openai.base_url` / `providers.gemini.base_url` (and their `IRIS_*_BASE_URL`
environment variables) exist for testing and for routing through a proxy — this is how Iris's own
offline tests and this documentation's mock-server transcripts run without touching a real
provider. **Your credential is sent to whatever base URL is configured**, so:

- A base URL must use `https`. Plain `http` is accepted only for a loopback host — `localhost`, an
  address in `127.0.0.0/8`, or `[::1]` (a mock server or proxy on your own machine) — because
  anywhere else the key would cross a network unencrypted. Any other `http://` base URL is
  `config_invalid` naming the variable or config key, before any command runs. Downloads follow
  the same rule: every hop must be `https`, except between loopback hosts when the base URL itself
  is a loopback `http` one. Requests to a loopback host never go through a system proxy
  (`HTTPS_PROXY`, `HTTP_PROXY`, `ALL_PROXY`), whatever `NO_PROXY` says, so a loopback `http` key
  never reaches a proxy in clear text; requests to any other host honor the system proxy settings.
- Every command that sends a provider's key to a non-default base URL — `image generate`/`edit`,
  `video generate`, `jobs status`/`wait`/`download` when they reach the provider, and
  `models show --check-access` — reports a `non_default_base_url` warning, once per provider,
  naming exactly where the key goes (a command that sends no key, like `--dry-run` or
  `jobs status --no-refresh`, does not). `config show` and `doctor` flag every overridden base URL:

```console
$ iris doctor
warning[non_default_base_url]: providers.openai.base_url is http://127.0.0.1:8080/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
warning[non_default_base_url]: providers.gemini.base_url is http://127.0.0.1:8080/ (from IRIS_GEMINI_BASE_URL); GEMINI_API_KEY is sent to that host over unencrypted HTTP
[ok]      config: no config file at /home/you/.config/iris/config.toml (it is optional)
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
[ok]      state_dir: state directory /home/you/iris-state does not exist yet; it will be created on first use
[ok]      output_dir: output directory /home/you is writable
[warning] base_url.openai: providers.openai.base_url is http://127.0.0.1:8080/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
[warning] base_url.gemini: providers.gemini.base_url is http://127.0.0.1:8080/ (from IRIS_GEMINI_BASE_URL); GEMINI_API_KEY is sent to that host over unencrypted HTTP
[ok]      jobs: 0 local job record(s) readable
Healthy.
```

**Veo downloads through a proxy.** A finished Veo job names its video by a Files API download URL
(`https://generativelanguage.googleapis.com/v1beta/files/<id>:download?alt=media`). Iris
downloads a Veo output only from such a URL under the configured Gemini base URL — the same
origin, below the base URL's path prefix — so the key is never sent anywhere else. A proxy base
URL must therefore rewrite those URLs in the operation answer to its own origin and prefix (for a
base URL of `https://proxy.example/gemini`:
`https://proxy.example/gemini/v1beta/files/<id>:download?alt=media`). A pass-through proxy that
leaves Google's URLs as they are still lets you submit and follow jobs, and they still succeed,
but each download is refused with `download_failed` (not retryable as is; `details.uri` holds the
redacted URL). Nothing is lost: point the base URL back at
`https://generativelanguage.googleapis.com` (or fix the proxy) and run `iris jobs download <id>`
while the provider still keeps the output (about 2 days) — every download checks against the base
URL configured at that moment.

## Security rules

- Generated media, local job/state data, a private config file, secrets, and local
  scratch/temporary work files are all git-ignored in this repository (see `.gitignore`).
- Prompts and input contents are never logged by default; `-v`/`--verbose` logs request
  *metadata* only (method, redacted URL, status, provider request id, elapsed time) — never
  prompt text, never a credential.
- Every URL Iris prints or logs is redacted first (`redact_url`): userinfo is stripped and every
  query value is replaced with `REDACTED` except a small allowlist (e.g. `alt`), so a signed
  download URL never leaks in output.
- API answers are read into memory only up to a limit, so a misbehaving server or proxy at a
  configured base URL cannot make Iris buffer gigabytes: 16 MiB for JSON answers (status checks,
  model metadata, video job submissions) and 512 MiB for image answers, which carry the images
  inline (the largest legitimate one, ten uncompressed 4K PNGs from OpenAI, is about 422 MiB). Error
  answers are read up to 1 MiB. A longer status or metadata answer is `provider_bad_response`; a
  longer answer to a paid request is `submission_uncertain` (the provider processed the request,
  but its answer was lost), and it is never resent. Downloads stream to disk instead and are
  capped at 4 GiB (see [jobs.md](jobs.md#downloads)).
- Test fixtures in this repository contain no real keys; tests set fake ones (e.g.
  `test-openai-key-000`) through the process environment, never through argv.
