---
name: Bug report
about: Iris did something that doesn't match its documented behavior
title: ""
labels: bug
---

<!--
If this might be a security issue, such as an API key or a signed URL that appears where it
shouldn't, don't file a public issue. Follow SECURITY.md instead.
-->

## What happened

<!-- What you ran, what you expected, and what happened instead. -->

## Version

```console
$ iris --json version
<paste the output here>
```

## Command and error

<!--
The exact command and, if it failed, its JSON output. Paste text, not a screenshot. Redact anything
in the prompt or file paths that you'd rather not share. Iris never prints API keys, but check the
output before you paste it, as you would any log.
-->

```console
$ iris ... --json
<paste the full JSON output here>
```

If you didn't use `--json`, paste the plain output and the exit code (`echo $?`).

## Environment

- OS and architecture:
- Installed with: (the installer, `cargo install`, or a build from source)
- Providers involved: (OpenAI, Gemini, or both)

## Anything else

<!-- Your config file, a job ID, or other context. Iris rejects config keys that look like
credentials, but redact anything you like. -->
