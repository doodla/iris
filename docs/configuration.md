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

A missing *default* file is fine (built-in defaults apply). A file named explicitly by `--config`
or `IRIS_CONFIG` that does not exist is `config_invalid` — you asked for it, so Iris tells you it
isn't there rather than silently falling back. `iris config path` prints the real, absolute
locations (output on Linux with `HOME=/home/you` and no `XDG_*` or `IRIS_*` variables set):

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
provider = "openai"                   # default provider for image commands

[video]
wait_timeout = "10m"
poll_interval = "10s"

[jobs]
store_prompts = false                 # see docs/jobs.md — off by default

[providers.openai]
base_url = "https://api.openai.com/v1"
image_model = "gpt-image-2.5-sunburst"   # must be a known model id/alias, or config_invalid
request_timeout = "300s"

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com"   # origin; Iris appends /v1 or /v1beta
image_model = "gemini-3.1-flash-image"
video_model = "veo-3.1-fast-generate-preview"
request_timeout = "300s"
```

Each `[providers.<id>]` table takes the same four keys; there is one per provider (`openai`,
`gemini`). **Unknown keys are rejected**, not ignored — a typo is caught immediately rather than
silently doing nothing (a relative `--config` path resolves against the current directory,
`/home/you` here):

```console
$ iris --config bad.toml config show
error[config_invalid]: config file /home/you/bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `image_model`, `video_model`, `request_timeout`
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

An unknown model in `image_model`/`video_model` is also caught at config-load time, before any
command tries to use it:

```console
$ iris --config bad3.toml config show
error[config_invalid]: config file /home/you/bad3.toml: `providers.openai.image_model`: unknown model 'not-a-real-model' (run `iris models list`)
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

`iris doctor` still runs the checks that do not need a valid configuration when the config file
itself is invalid, so an invalid file doesn't hide unrelated problems like a missing credential.
The directory, base URL, and job checks need the resolved settings, so they are skipped (real,
complete output):

```console
$ iris --config bad.toml doctor
[error]   config: config file /home/you/bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `image_model`, `video_model`, `request_timeout` (fix the config file, or point --config / IRIS_CONFIG at another file)
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
| image provider | `IRIS_IMAGE_PROVIDER` | `--provider` | `openai` |
| video wait timeout | `IRIS_WAIT_TIMEOUT` | `--timeout` | `10m` |
| video poll interval | `IRIS_POLL_INTERVAL` | `--poll-interval` | `10s` |
| store prompts in job records | `IRIS_STORE_PROMPTS` | — | `false` |
| OpenAI base URL | `IRIS_OPENAI_BASE_URL` | — | `https://api.openai.com/v1` |
| Gemini base URL | `IRIS_GEMINI_BASE_URL` | — | `https://generativelanguage.googleapis.com` |
| OpenAI default image model | — (config file only) | `--model` | catalog default |
| Gemini default image model | — (config file only) | `--model` | catalog default |
| Gemini default video model | — (config file only) | `--model` | catalog default |
| config file path | `IRIS_CONFIG` | `--config` | platform default |
| log filter | `IRIS_LOG` | `-v` (repeatable) | `warn` |

Provider/model resolution for generation, in order: provider = `--provider` > the provider implied
by `--model` (from the catalog) > `IRIS_IMAGE_PROVIDER` > config `image.provider` > `openai`.
Video has no provider setting: without `--provider` or `--model` it uses the provider whose catalog
declares a default video model, which today is `gemini`, the only video provider. Model =
`--model` > config `providers.<provider>.<kind>_model` > the catalog's default for that
(provider, operation).
Passing `--provider` together with a `--model` that belongs to a *different* provider is
`invalid_argument`, caught before anything is sent.

An environment variable's value is validated exactly like a config-file value — a bad one is
`config_invalid` naming the variable, not silently ignored.

Real `config show` output, with `HOME=/home/you`, both keys set, and
`IRIS_STATE_DIR=/home/you/iris-state`, `IRIS_OPENAI_BASE_URL=http://127.0.0.1:8080/v1`, and
`IRIS_GEMINI_BASE_URL=http://127.0.0.1:8080` (the table goes to stdout; the two
`non_default_base_url` warnings it also prints go to stderr and are shown in
[Base URL overrides](#base-url-overrides)):

```console
$ iris config show
config file: /home/you/.config/iris/config.toml (not found; defaults apply)
SETTING                           VALUE                               SOURCE
config_file                       /home/you/.config/iris/config.toml  default
output_dir                        /home/you                           default
state_dir                         /home/you/iris-state                env IRIS_STATE_DIR
image.provider                    openai                              default
video.wait_timeout                10m                                 default
video.poll_interval               10s                                 default
jobs.store_prompts                false                               default
providers.openai.base_url         http://127.0.0.1:8080/v1            env IRIS_OPENAI_BASE_URL
providers.openai.image_model      gpt-image-2.5-sunburst              default
providers.openai.request_timeout  5m                                  default
providers.gemini.base_url         http://127.0.0.1:8080/              env IRIS_GEMINI_BASE_URL
providers.gemini.image_model      gemini-3.1-flash-image              default
providers.gemini.video_model      veo-3.1-fast-generate-preview       default
providers.gemini.request_timeout  5m                                  default
log                               warn                                default
credentials (presence only):
  OPENAI_API_KEY: set
  GEMINI_API_KEY: set
```

## Base URL overrides

`providers.openai.base_url` / `providers.gemini.base_url` (and their `IRIS_*_BASE_URL`
environment variables) exist for testing and for routing through a proxy — this is how Iris's own
offline tests and this documentation's mock-server transcripts run without touching a real
provider. **Your credential is sent to whatever base URL is configured**, so `config show` and
`doctor` both flag a non-default base URL with a `non_default_base_url` warning, naming exactly
where the key would go:

```console
$ iris doctor
warning[non_default_base_url]: providers.openai.base_url is http://127.0.0.1:8080/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
warning[non_default_base_url]: providers.gemini.base_url is http://127.0.0.1:8080/ (from IRIS_GEMINI_BASE_URL); GEMINI_API_KEY is sent to that host over unencrypted HTTP
[ok]      config: no config file at /home/you/.config/iris/config.toml; built-in defaults apply
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
