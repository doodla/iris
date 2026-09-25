# Security policy

## Reporting a vulnerability

Please **do not** open a public GitHub issue for a security vulnerability. Instead:

1. Go to the [Security tab](https://github.com/doodla/iris/security) of `doodla/iris`.
2. **If private vulnerability reporting is enabled for this repository** (a **"Report a
   vulnerability"** button on that tab), use it to open a private advisory. This starts a
   confidential conversation with the maintainers, visible only to you and them, before anything
   is public.

Drafting a security advisory directly is a maintainer-only action, so if you don't see that button
— private reporting isn't enabled, or you don't have access to it — open a regular public issue
asking for another way to reach the maintainers **without describing the vulnerability itself** (no
technical details, no proof of concept); a maintainer will follow up with a private channel.

Please include what you'd include in any good report: the affected version or commit, a minimal
reproduction, and the impact as you understand it. You do not need to propose a fix.

## Supported versions

Iris is pre-1.0 (`0.x`). Security fixes target the latest released version; there is no formal
long-term-support branch yet.

## Scope

Iris is a local command-line tool. Things that are particularly worth a private report:

- Credentials (`OPENAI_API_KEY`, `GEMINI_API_KEY`) leaking into logs, error messages, persisted
  job records, or any other output.
- A request being sent to a host other than the configured provider API origin while still
  carrying a credential (including across a redirect).
- Path traversal or unsafe file handling in output/download filenames.
- A way to make the installer (`install.sh`) install or execute something other than a verified
  release archive.

Things that are **not** a vulnerability report, and can be filed as regular issues instead: a
provider changing its own API/pricing/behavior out from under Iris's catalog, or Iris correctly
reporting an error a misconfigured account produced.

## How Iris handles secrets, for context

This is background for reporters, not a guarantee that supersedes the actual code — see
[docs/configuration.md](docs/configuration.md#security-rules) and
[AGENTS.md](AGENTS.md#money-credentials-and-safety) for the durable rules this project holds
itself to:

- Credentials are read only from `OPENAI_API_KEY` and `GEMINI_API_KEY`, held in a type that
  never prints or serializes its contents, and are never written to the config file, a log line,
  an error message, a persisted job record, or a command-line argument.
- Every error message, log line, and persisted `last_error` is passed through redaction before it
  can reach output, and every printed URL has its userinfo and query values redacted, except the
  values of a small allowlist of non-secret query parameters such as `alt`.
- A provider's credential header is attached only to requests whose scheme, host, and port match
  that provider's *configured* base URL — including across redirects, which Iris follows itself
  precisely so it can enforce this, rather than letting the HTTP client follow them silently.
- Generated media, local job/state data, a private config file, secrets, and local
  scratch/temporary work files are git-ignored (see `.gitignore`) so they can't end up committed
  by accident.
