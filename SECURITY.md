# Security policy

## Report a vulnerability

Don't open a public issue for a vulnerability. Report it privately through GitHub:

1. Go to the [Security tab](https://github.com/doodla/iris/security) of this repository.
2. Click **Report a vulnerability**, and describe the issue.

The report starts a private conversation with the maintainers, and nothing is public until a fix is
ready. Include the affected version or commit, a minimal reproduction, and the impact as you
understand it. You don't need to propose a fix.

If you don't see **Report a vulnerability**, open a public issue that asks for a private way to
reach the maintainers. Don't describe the vulnerability in it. A maintainer will follow up.

## Supported versions

Iris is before version 1.0. Security fixes go into the latest release, and there's no long-term
support branch.

## Scope

Iris is a local command-line tool. These problems are worth a private report:

- An API key (`OPENAI_API_KEY` or `GEMINI_API_KEY`) that appears in logs, error messages, job
  records, or any other output.
- A request that carries a credential to a host other than the provider's configured API origin,
  including through a redirect.
- Path traversal, or other unsafe file handling, in output or download file names.
- A way to make the installer, `install.sh`, install or run anything other than a verified release
  archive.

These aren't vulnerabilities, and you can report them as regular issues:

- A provider that changes its API, prices, or behavior so that Iris's catalog is out of date.
- An error that Iris reports correctly for a misconfigured account.

For how Iris handles API keys, see [API keys](docs/reference/configuration.md#api-keys). For what
Iris stores about a video job, see
[What a job record contains](docs/concepts/video-jobs.md#what-a-job-record-contains).
