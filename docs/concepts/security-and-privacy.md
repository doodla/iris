# Security and privacy

This page explains how Iris handles your API keys, your prompts, and the data that it keeps on
disk. To report a vulnerability, see the
[security policy](https://github.com/doodla/iris/blob/main/SECURITY.md).

## API keys

Iris reads API keys only from two environment variables: `OPENAI_API_KEY` and `GEMINI_API_KEY`.

- Iris doesn't read keys from the config file or from command-line flags. If a config file
  contains a key that looks like a credential, such as `api_key`, `token`, or `secret`, Iris
  rejects the file.
- Iris ignores `GOOGLE_API_KEY`, and `iris doctor` warns you if it's set. Google's SDKs prefer that
  variable over `GEMINI_API_KEY`, so honoring it would let a leftover variable choose your key.
- Iris never prints, logs, or stores a key. `iris doctor`, `iris config show`, and
  `iris providers list` report only whether each key is set.
- Iris sends a key only to its provider's configured base URL: the same scheme, host, and port. Iris
  follows redirects itself, and it drops the key on any redirect to another host.
- Iris sends the Gemini key in the `x-goog-api-key` header, never in a URL, because URLs end up in
  logs and proxies.

You can point a provider at another base URL, such as a proxy. The base URL must use `https`,
unless it's on your own machine (`localhost`, `127.0.0.0/8`, or `[::1]`). Every command that sends
a key to a base URL other than the default reports a `non_default_base_url` warning. For the rules,
see [Base URL overrides](../reference/configuration.md#base-url-overrides).

## Prompts and input files

- Iris doesn't log prompts or the content of input files. With `-v`, it logs request metadata only:
  the method, the redacted URL, the status, the provider's request ID, and the elapsed time.
- A video job record stores the prompt's SHA-256 hash and length, not its text, unless you set
  `jobs.store_prompts` to `true`. Job views never show the text. Records store how many input images
  a request had, but not their paths or content. See
  [What a job record contains](video-jobs.md#what-a-job-record-contains).
- A job's label is stored and shown as you wrote it, so don't put anything secret in a label.
- Iris asks Google not to store Gemini image requests (`store: false`), which takes precedence over
  a project-level logging setting.

## What Iris prints

- Iris removes user information from every URL that it prints or logs, and replaces query values
  with `REDACTED`, except for a few harmless parameters such as `alt`. A signed download URL never
  appears in output.
- Iris removes key values from error messages, and it shortens a provider's message to 500
  characters.
- Provider error messages appear only in `details.provider_message`, for diagnosis. Iris reports
  errors with its own stable codes. See the [Errors reference](../reference/errors.md).

## Files on disk

Iris creates its state directory with mode `0700` and job records with mode `0600`, so only your
user can read them. The state directory also holds the `unsaved` folder, where Iris keeps paid
outputs that it couldn't save where you asked. See
[Paid outputs are kept](paid-requests.md#paid-outputs-are-kept).

## Limits on responses

Iris reads each response only up to a limit, so a misbehaving server or proxy at a configured base
URL can't make it use unbounded memory:

| Response | Limit |
|---|---|
| Error responses | 1 MiB |
| Job status checks, model metadata, and video submissions | 16 MiB |
| Image responses, which carry the images inline | 512 MiB |
| Video downloads, streamed to disk | 4 GiB |

The largest valid image response, ten uncompressed 4K PNG images from OpenAI, is about 422 MiB.

A longer response to a free request, such as a status check, fails with `provider_bad_response`. A
longer response to a paid request has an uncertain outcome (`submission_uncertain`), because the
provider processed the request, and Iris doesn't send it again. A download that grows past 4 GiB is
stopped, and its partial file is deleted.

## Related

- [How Iris handles paid requests](paid-requests.md)
- [Install Iris](../guides/install.md): how the installer verifies what it installs.
- [Security policy](https://github.com/doodla/iris/blob/main/SECURITY.md): report a vulnerability.
