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
(`doctor` also reports `config`, `state_dir`, `output_dir`, `base_url.openai`, `base_url.gemini`,
and `jobs` — see [Setup](../README.md#setup) in the README for the full, real block):

```console
$ iris doctor
...
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
...
```

A missing key for the provider a command needs is `missing_credentials` (exit 3), checked after
every other local validation and before any network call — so a bad prompt or an unsupported
option is still reported as such even with no key set. `--dry-run` never requires a credential;
it reports whether one is present without requiring it.

## Config file

TOML. Location, in order: `--config <PATH>` > `IRIS_CONFIG` > the platform default:

| platform | default location |
|---|---|
| Linux | `$XDG_CONFIG_HOME/iris/config.toml`, else `~/.config/iris/config.toml` |
| macOS | `~/Library/Application Support/iris/config.toml` |

A missing *default* file is fine (built-in defaults apply). A file named explicitly by `--config`
or `IRIS_CONFIG` that does not exist is `config_invalid` — you asked for it, so Iris tells you it
isn't there rather than silently falling back.

```console
$ iris config path
config file: ~/.config/iris/config.toml
state dir:   ~/.local/state/iris
jobs dir:    ~/.local/state/iris/jobs
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

**Unknown keys are rejected**, not ignored — a typo is caught immediately rather than silently
doing nothing:

```console
$ iris --config ./bad.toml config show
error[config_invalid]: config file ./bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `image_model`, `video_model`, `request_timeout`
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
$ echo $?
2
```

**Any key that looks like a credential — `api_key`, anything ending in `_key`, `key`, `token`,
`secret`, or `password`, case-insensitive, at any depth — is rejected too**, with a message
pointing at the environment variables instead, so a well-meaning `api_key = "sk-..."` in a config
file (which would otherwise get committed to a repo) is caught rather than silently accepted:

```console
$ iris --config ./bad2.toml config show
error[config_invalid]: config file ./bad2.toml: `providers.openai.api_key`: credentials are read only from OPENAI_API_KEY / GEMINI_API_KEY, never from the config file
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

An unknown model in `image_model`/`video_model` is also caught at config-load time, before any
command tries to use it:

```console
$ iris --config ./bad3.toml config show
error[config_invalid]: config file ./bad3.toml: `providers.openai.image_model`: unknown model 'not-a-real-model' (run `iris models list`)
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

`iris doctor` always reports **every** check it can, even when the config file itself is invalid
— an invalid config file doesn't hide unrelated problems like a missing credential:

```console
$ iris --config ./bad.toml doctor
[error]   config: config file ./bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `image_model`, `video_model`, `request_timeout` (fix the config file, or point --config / IRIS_CONFIG at another file)
[ok]      credentials.openai: OPENAI_API_KEY is set
[ok]      credentials.gemini: GEMINI_API_KEY is set
...
Problems found (see [error] lines).
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
by `--model` (from the catalog) > `IRIS_IMAGE_PROVIDER` > config `image.provider` > `openai`
(video has only one provider, `gemini`, so there is nothing to resolve). Model = `--model` >
config `providers.<provider>.<kind>_model` > the catalog's default for that (provider, operation).
Passing `--provider` together with a `--model` that belongs to a *different* provider is
`invalid_argument`, caught before anything is sent.

An environment variable's value is validated exactly like a config-file value — a bad one is
`config_invalid` naming the variable, not silently ignored.

Real `config show`, run with `IRIS_STATE_DIR`, `IRIS_OPENAI_BASE_URL`, and `IRIS_GEMINI_BASE_URL`
set (edited for width; every row and the exact `source` values are real):

```console
$ iris config show
SETTING                           VALUE                       SOURCE
config_file                       ~/.config/iris/config.toml  default
output_dir                        /home/you                   default
state_dir                         /tmp/.../state               env IRIS_STATE_DIR
image.provider                    openai                      default
video.wait_timeout                10m                         default
video.poll_interval               10s                         default
jobs.store_prompts                false                       default
providers.openai.base_url         http://127.0.0.1:50045/v1   env IRIS_OPENAI_BASE_URL
providers.openai.image_model      gpt-image-2.5-sunburst      default
providers.openai.request_timeout  5m                          default
providers.gemini.base_url         http://127.0.0.1:50045/     env IRIS_GEMINI_BASE_URL
providers.gemini.image_model      gemini-3.1-flash-image      default
providers.gemini.video_model      veo-3.1-fast-generate-preview  default
providers.gemini.request_timeout  5m                          default
log                               warn                        default
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
warning[non_default_base_url]: providers.openai.base_url is http://127.0.0.1:50045/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
```

## Security rules

- Generated media, local job/state data, a private config file, secrets, and local
  scratch/temporary work files are all git-ignored in this repository (see `.gitignore`).
- Prompts and input contents are never logged by default; `-v`/`--verbose` logs request
  *metadata* only (method, redacted URL, status, provider request id, elapsed time) — never
  prompt text, never a credential.
- Every URL Iris prints or logs is redacted first (`redact_url`): userinfo is stripped and every
  query value is replaced with `REDACTED` except a small allowlist (e.g. `alt`), so a signed
  download URL never leaks in output.
- Test fixtures in this repository contain no real keys; tests set fake ones (e.g.
  `test-openai-key-000`) through the process environment, never through argv.
