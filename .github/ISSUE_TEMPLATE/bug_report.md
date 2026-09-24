---
name: Bug report
about: Something Iris did (or didn't do) that doesn't match its documented behavior
title: ""
labels: bug
---

<!--
Before filing: if this might be a credential/security issue (a key or signed URL appearing
somewhere it shouldn't), please use private reporting instead — see SECURITY.md — not a public
issue.
-->

## What happened

<!-- What you ran, what you expected, and what happened instead. -->

## Version

```console
$ iris --json version
<paste the output here>
```

## Command and JSON error

The exact command (redact anything sensitive in the prompt or file paths if you'd rather not
share it) and, if it failed, the JSON error object — never a screenshot, and please double-check
it before pasting that no credential value is present (Iris never prints one, but paste with the
same care you'd give any log):

```console
$ iris ... --json
<paste the full JSON output here>
```

If `--json` wasn't used, the plain output and exit code (`echo $?`) are still useful.

## Environment

- OS / architecture:
- Installed via: (installer / `cargo install` / built from source)
- Provider(s) involved: (OpenAI / Gemini / Veo)

## Anything else

<!-- Config file contents (with any credential-like key already rejected by Iris itself, so
there shouldn't be one — but redact freely), relevant job id, or other context. -->
