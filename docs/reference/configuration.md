# Configuration reference

Iris works without a config file. This page lists every setting, how Iris resolves it, and how to
set it with a flag, an environment variable, or the config file. It also covers API keys, paths,
time limits, and base URL overrides.

## How Iris resolves a setting

Iris takes each setting from the first of these sources that sets it:

1. A command-line flag.
2. An environment variable.
3. The config file.
4. The built-in default.

To see every setting, its value, and its source, run `iris config show`. The output is similar to
the following:

```text
config file: /home/you/.config/iris/config.toml (loaded)
SETTING                           VALUE                                       SOURCE
config_file                       /home/you/.config/iris/config.toml          default
output_dir                        /home/you                                   default
state_dir                         /home/you/.local/state/iris                 default
image.model                       gemini-3.1-flash-image                      file
video.model                       (none)                                      default
video.wait_timeout                20m                                         file
video.poll_interval               10s                                         default
jobs.store_prompts                false                                       default
providers.openai.base_url         https://api.openai.com/v1                   default
providers.openai.request_timeout  5m                                          default
providers.gemini.base_url         https://generativelanguage.googleapis.com/  default
providers.gemini.request_timeout  5m                                          default
providers.gemini.submit_timeout   1m                                          default
log                               warn                                        default
credentials (presence only):
  OPENAI_API_KEY: set
  GEMINI_API_KEY: not set
```

## Settings

| Setting | Config key | Environment variable | Flag | Default |
|---|---|---|---|---|
| Output directory | `output_dir` | `IRIS_OUTPUT_DIR` | `-d`, `--out-dir` | The current directory |
| State directory | `state_dir` | `IRIS_STATE_DIR` | | See [Paths](#paths) |
| Image model | `image.model` | | `-m`, `--model` | None: a command without a model fails with `model_required` |
| Video model | `video.model` | | `-m`, `--model` | None: a command without a model fails with `model_required` |
| Video wait limit | `video.wait_timeout` | `IRIS_WAIT_TIMEOUT` | `--timeout` | `10m` |
| Video poll interval | `video.poll_interval` | `IRIS_POLL_INTERVAL` | `--poll-interval` | `10s` |
| Store prompt text in job records | `jobs.store_prompts` | `IRIS_STORE_PROMPTS` | | `false` |
| OpenAI base URL | `providers.openai.base_url` | `IRIS_OPENAI_BASE_URL` | | `https://api.openai.com/v1` |
| Gemini base URL | `providers.gemini.base_url` | `IRIS_GEMINI_BASE_URL` | | `https://generativelanguage.googleapis.com` |
| Request time limit | `providers.PROVIDER.request_timeout` | | | `300s` |
| Video submission time limit | `providers.gemini.submit_timeout` | | | `60s` |
| Config file | | `IRIS_CONFIG` | `--config` | See [Config file](#config-file) |
| Log filter | | `IRIS_LOG` | `-v`, `-vv` | `warn` |

Values have these formats:

- **Durations**: a number with a unit, such as `90s`, `10m`, or `1h`, or a number of seconds. The
  poll interval must be at least 2 seconds.
- **Booleans**: `true` or `false`. In environment variables, Iris also accepts `1` and `0`, `yes`
  and `no`, and `on` and `off`.
- **Paths**: in the config file and in environment variables, an absolute path or a path that starts
  with `~/`. A relative path would follow each command's working directory, and a state directory
  that moves loses its jobs. Paths that you pass as flags can be relative to the current directory.
- **Log filter**: `IRIS_LOG` takes
  [`tracing` filter directives](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html),
  such as `debug`. `-v` logs Iris's debug messages, and `-vv` its trace messages. For a request,
  Iris logs only metadata, such as its method, redacted URL, status, provider request ID, and
  elapsed time. Logs never contain prompts, the content of input files, keys, or signed URLs.

Iris validates an environment variable's value like a config file value. An invalid value fails
with `config_invalid`, which names the variable.

`--max-cost` and `--label` have no environment variable or config key. A spending cap or a label
applies only to the command that names it.

## API keys

Iris reads API keys only from these environment variables:

| Provider | Environment variable |
|---|---|
| OpenAI | `OPENAI_API_KEY` |
| Google Gemini, for images and Veo | `GEMINI_API_KEY` |

Iris doesn't read keys from flags, and it rejects a config file that contains one. See
[Validation](#validation). It also ignores `GOOGLE_API_KEY`, which Google's SDKs prefer when both
variables are set, so a leftover variable can't choose your key. `iris doctor` warns you if
`GOOGLE_API_KEY` is set.

Iris handles keys in the following ways:

- It never prints, logs, or stores a key. `iris config show`, `iris doctor`, and
  `iris providers list` report only whether each key is set.
- It sends a key only to its provider's base URL: the same scheme, host, and port. API requests
  don't follow redirects, and a download that's redirected anywhere else continues without the key.
- It sends the Gemini key in the `x-goog-api-key` header, never in a URL, because URLs end up in
  logs and error messages.

To send a key to another base URL, such as a proxy, see [Base URL overrides](#base-url-overrides).

A command whose provider's key isn't set fails with `missing_credentials` (exit code 3). Iris checks
the key after every other local check, and `--dry-run` doesn't require one. `iris doctor` reports a
missing key as a warning while another provider's key is set, and as an error when no key is set:

```text
[warning] credentials.gemini: GEMINI_API_KEY is not set; gemini commands will fail with missing_credentials
```

## Config file

The config file is a TOML file. Iris reads it from the first of these locations that's set:

1. The path from `--config PATH`.
2. The path in `IRIS_CONFIG`.
3. The platform default:

   | Platform | Default location |
   |---|---|
   | Linux | `$XDG_CONFIG_HOME/iris/config.toml`, or `~/.config/iris/config.toml` |
   | macOS | `~/Library/Application Support/iris/config.toml` |

If the file at the default location doesn't exist, Iris uses the other sources and no model is
configured. If a file that you named with `--config` or `IRIS_CONFIG` doesn't exist, Iris fails with
`config_invalid`. To see the paths on your machine, run `iris config path`.

The following file sets every key:

```toml
output_dir = "~/Pictures/iris"
state_dir = "/custom/state"

[image]
model = "gpt-image-2.5-sunburst"      # image generate and image edit without -m

[video]
model = "veo-3.1-lite-generate-preview"   # video generate without -m
wait_timeout = "10m"
poll_interval = "10s"

[jobs]
store_prompts = false

[providers.openai]
base_url = "https://api.openai.com/v1"
request_timeout = "300s"

[providers.gemini]
base_url = "https://generativelanguage.googleapis.com"   # the origin: Iris adds /v1 or /v1beta
request_timeout = "300s"
submit_timeout = "60s"
```

`[image] model` and `[video] model` are the only settings that name a model. Each must be a catalog
model ID or alias of its kind. Iris stores the ID that `-m` would send: the canonical ID for a
nickname such as `nano-banana-2`, or a dated snapshot as written. `iris config show` shows the
stored ID. `--capabilities-from` has no config equivalent. For how to choose a model, see
[Choose a model and control costs](../guides/models-and-costs.md).

There's one `[providers.PROVIDER]` table for each provider, `openai` and `gemini`, with the same
keys. `submit_timeout` applies only to a provider with video models, so setting it for `openai`
fails with `config_invalid`.

### Validation

Iris validates the config file when it loads it, before any command runs:

- **Unknown keys fail.** A typo fails instead of silently doing nothing. A table for a provider that
  Iris doesn't have fails the same way.
- **Keys that look like credentials fail.** This includes `api_key`, `key`, `token`, `secret`,
  `password`, and any key that ends in `_key`, in any case and at any depth. Iris reads keys only
  from the environment, and a key in a config file can end up committed to a repository.
- **Models are checked.** A model that Iris doesn't know, or of the wrong kind, fails.

For example, a typo in a key:

```sh
iris --config bad.toml config show
```

The output is similar to the following:

```text
error[config_invalid]: config file /home/you/bad.toml: `providers.openai.typo_field`: unknown key; expected one of `base_url`, `request_timeout`, `submit_timeout`
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

A credential in the config file:

```text
error[config_invalid]: config file /home/you/bad2.toml: `providers.openai.api_key`: credentials are read only from OPENAI_API_KEY / GEMINI_API_KEY, never from the config file
  hint: fix the config file, or point --config / IRIS_CONFIG at another file
```

A model of the wrong kind:

```text
error[config_invalid]: config file /home/you/bad4.toml: `video.model`: model 'gemini-3.1-flash-image' does not support video.generate (supports: image.generate, image.edit)
  hint: set video.model to a model listed by `iris models list --operation video.generate`
```

While the config file is invalid, some commands still work:

- `iris models list` and `iris models show`, without `--check-access`, don't read the config file,
  so you can look up the models that a hint names.
- `iris doctor` runs every check that doesn't need a valid configuration, so an invalid file doesn't
  hide other problems, such as a missing key. It skips the directory, base URL, and job checks.

## Paths

| Path | Linux | macOS |
|---|---|---|
| Config file | `$XDG_CONFIG_HOME/iris/config.toml`, or `~/.config/iris/config.toml` | `~/Library/Application Support/iris/config.toml` |
| State directory | `$XDG_STATE_HOME/iris`, or `~/.local/state/iris` | `~/Library/Application Support/iris` |
| Jobs directory | `STATE_DIR/jobs` | `STATE_DIR/jobs` |

On macOS, the config file is inside the state directory. Keep this in mind before you delete the
state directory. See [Uninstall Iris](../guides/install.md#uninstall-iris).

To print the absolute paths that Iris uses, run `iris config path`. The output is similar to the
following:

```text
config file: /home/you/.config/iris/config.toml
state dir:   /home/you/.local/state/iris
jobs dir:    /home/you/.local/state/iris/jobs
```

If the current directory no longer exists, for example because it was deleted under a running
shell:

- These commands still work: `version`, `schema`, `completions`, `--help`, `config path`,
  `config show`, `providers list`, `models`, `jobs list`, `jobs status`, and `doctor`.
- A command that needs the directory fails with `io_error` before it sends anything. A command
  needs it if you gave it a relative path, or if it saves to the default output directory. An
  absolute `-o`, `-d`, or `IRIS_OUTPUT_DIR` avoids this.

## Time limits

Each request that Iris sends has its own time limit, and each retry gets a fresh one. None of them
is the time that a command waits for a video job: that's the wait limit, `video.wait_timeout` or
`--timeout`. When the wait limit passes, the job continues remotely.

| Time limit | Covers | Default | Setting |
|---|---|---|---|
| Connect | Establishing the connection, for every request | 15 s | |
| Request | One paid image request, from connecting until its response, which carries the images, is read | 5 min | `providers.PROVIDER.request_timeout` |
| Submission | One video submission, until the response that names the job arrives | 1 min | `providers.gemini.submit_timeout` |
| Status check | One job status check, or one model metadata read with `--check-access` | 30 s | |
| Download idle | The longest pause between two pieces of a download. A download as a whole has no time limit. | 1 min | |

A request that carries data, such as an image edit with its input images or a video with reference
images, also gets the time to upload it at 256 KiB/s, up to 10 more minutes. For example, a 23 MB
video request gets `submit_timeout` plus about 90 seconds. A paid request that's cut off during the
upload has an uncertain outcome, so if your connection uploads more slowly than that, raise
`request_timeout` or `submit_timeout`.

A job record that stays `submitting` is reported as `submission_unknown` only after the whole
submission time budget has passed. The budget covers three attempts, each with the connect limit,
`submit_timeout`, and the largest upload allowance. It adds up to a minute between attempts, and
one more minute. With the defaults, that's about 37 minutes. See
[Job states](../concepts/video-jobs.md#job-states).

## Base URL overrides

You can send a provider's requests to another base URL, for example a proxy or a local mock server
for testing. Iris sends the provider's API key to that base URL, so it applies these rules:

- A base URL must use `https`. Plain `http` is allowed only for a host on your own machine:
  `localhost`, an address in `127.0.0.0/8`, or `[::1]`. Any other `http` URL fails with
  `config_invalid` before any command runs.
- Downloads follow the same rule on every redirect: each hop must use `https`, except between local
  hosts when the base URL itself is a local `http` URL.
- Requests to a local host never go through a system proxy (`HTTPS_PROXY`, `HTTP_PROXY`, or
  `ALL_PROXY`), whatever `NO_PROXY` says, so a key sent over `http` never reaches a proxy
  unencrypted. Requests to other hosts use the system proxy settings.
- Every command that sends a key to a base URL other than the default reports a
  `non_default_base_url` warning once per provider. `iris config show` and `iris doctor` flag every
  overridden base URL.

The Gemini base URL is an origin, such as `https://generativelanguage.googleapis.com`. Iris adds
`/v1` for image requests and `/v1beta` for Veo, operations, and files. The OpenAI base URL includes
its `/v1` path.

With overrides, `iris doctor` reports lines similar to the following:

```text
warning[non_default_base_url]: providers.openai.base_url is http://127.0.0.1:8080/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
...
[warning] base_url.openai: providers.openai.base_url is http://127.0.0.1:8080/v1 (from IRIS_OPENAI_BASE_URL); OPENAI_API_KEY is sent to that host over unencrypted HTTP
```

### Download Veo videos through a proxy

A finished Veo job names its video with a Files API download URL, such as
`https://generativelanguage.googleapis.com/v1beta/files/FILE_ID:download?alt=media`. Iris downloads
a video only from such a URL under the configured Gemini base URL: the same origin, below the base
URL's path. This keeps the key from going anywhere else.

So a proxy must rewrite those URLs in its responses to its own origin and path. For example, with a
base URL of `https://proxy.example/gemini`, the URL must become
`https://proxy.example/gemini/v1beta/files/FILE_ID:download?alt=media`.

With a proxy that passes Google's URLs through unchanged, you can still submit and follow jobs,
and they still succeed, but each download fails with `download_failed`. Nothing is lost: set the
base URL back to `https://generativelanguage.googleapis.com`, or fix the proxy, and run
`iris jobs download JOB_ID` while the provider still keeps the video, for about 2 days. Every
download checks the base URL that's configured at that moment.
