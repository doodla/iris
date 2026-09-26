# Use Iris in scripts and agents

This guide shows how to call Iris from a script or an AI agent. It covers JSON output, exit codes,
checking a request before you pay, and a video job that survives a crash without paying twice.

## Use JSON mode

Add `--json` to any command, before or after the command name:

```sh
iris --json version
```

The output is similar to the following:

```text
{"command":"version","error":null,"ok":true,"result":{"git_commit":null,"name":"iris","schema_version":1,"target":"x86_64-unknown-linux-gnu","version":"0.1.0"},"schema_version":1,"warnings":[]}
```

With `--json`, Iris prints exactly one JSON document on stdout, and sends progress lines to stderr.
It doesn't prompt for input. Every document is an envelope: check `ok`, then read `result`, or
`error.code` if the command failed. Warnings are in `warnings`, even when the command fails.

To validate the output, or to generate types from it, save the JSON Schema:

```sh
iris schema > iris-output.v1.schema.json
```

For every field, see the [JSON output reference](../reference/json-output.md).

## Branch on the exit code

The exit code tells you what kind of failure happened, without parsing text. For example, 4 means
that a video job is still running, and 5 means that a paid request might have been billed. For
every code and what to do about it, see [Exit codes](../reference/errors.md#exit-codes).

Keep these rules in mind:

- An error with exit code 2 and `provider_status: null` didn't reach the provider, so it cost
  nothing. With other exit codes, `null` can also mean that no response arrived.
- `retryable` says whether sending the same request again might succeed. After exit code 5, don't
  send the request again automatically. See
  [Handle an uncertain outcome](#handle-an-uncertain-outcome).
- `iris doctor` and `iris jobs status` exit with 0 when they run, even when they report a problem.
  Read `result.healthy` and `result.job.status` instead.

## Check a request before you pay

A dry run makes every local check that the real run makes, and estimates the cost, without sending
anything or needing an API key. To read the estimate from a script:

```sh
iris image generate -m gpt-image-2.5-sunburst "a red bicycle" --size 1024x1024 --quality low \
  --dry-run --json | jq '.result.cost_estimate.amount'
```

The output is `0.00588`. The plan also includes the options that the real run would send, the
output paths, and the prompt's fingerprint. `cost_estimate` is `null` when Iris can't estimate the
request, and a `cost_estimate_unavailable` warning says which options to set. See
[Dry-run plan](../reference/json-output.md#dry-run-plan).

To make a command refuse any request above an amount, add `--max-cost` with the amount in US
dollars. See [Cap what a command spends](models-and-costs.md#cap-what-a-command-spends).

## Generate a video without paying twice

A video job can outlive the process that submitted it. To follow a job safely from any process,
use this flow:

1. Submit the job with `--detach` and a label that names the intended video. If the same command
   runs again, for example after a crash, it fails with `label_in_use` and names the existing job,
   instead of paying for a second one.
2. Wait for the job with `iris jobs wait --timeout`. The command exits with 4 while the job is still
   running, and with 0 once the video is saved. You can also poll `iris jobs status`, which exits
   with 0 and reports the job's state in `result.job.status`.
3. Save the video. `iris jobs wait` downloads it, unless you pass `--no-download`. To download it
   again later, run `iris jobs download`, which never generates it again.

Each step is safe to repeat. The following POSIX shell script implements the flow:

```sh
label=paper-boat-1
out=$(iris --json video generate -m veo-lite "a paper boat drifting on a pond" \
  --duration 4 --label "$label" --detach)
status=$?
if [ "$status" -ne 0 ] && [ "$(printf '%s' "$out" | jq -r '.error.code')" != label_in_use ]; then
  printf '%s\n' "$out" >&2
  exit "$status"
fi
job_id=$(printf '%s' "$out" | jq -r '.result.job.job_id // .error.job_id')

while :; do
  iris --json jobs wait "$job_id" --timeout 10m > wait.json
  status=$?
  [ "$status" -eq 4 ] || break
done
exit "$status"
```

The script submits the job, or finds the job that an earlier run submitted under the same label.
Then it waits until the job ends. When the script exits with 0, `wait.json` holds the job, and
`result.job.artifacts[].path` names the saved video. Running the script again doesn't submit
another job: it finds the same job, and reports the video as `already_downloaded`.

If the labeled job failed, `iris jobs wait` reports the job's recorded error, and the script exits
with its code. Paying for a new job is then your decision: delete the old record, or choose another
label. For what to do in each state, see [Label a job](videos.md#label-a-job).

## Handle an uncertain outcome

Exit code 5 (`submission_uncertain`) means that Iris sent a paid request but can't tell whether the
provider processed it. Don't send the request again automatically, because you might pay twice.
Instead, check your usage in the provider's console.

- For an image request, there's nothing to follow, because the image APIs don't offer a way to
  look up a request later.
- For a video, Iris keeps the job with the status `submission_unknown`. If you used a label, running
  the command again is refused, so the flow above stays safe.

For the cases that count as uncertain, see
[When the outcome is uncertain](../concepts/paid-requests.md#when-the-outcome-is-uncertain).

## Keep one state directory

Only a process that uses the same state directory can follow a job. When your agent runs in a
container or CI, set `IRIS_STATE_DIR` to a path on a persistent volume, the same for every command.
See [Where job records live](../concepts/video-jobs.md#where-job-records-live).

## What's next

- [JSON output reference](../reference/json-output.md)
- [Errors reference](../reference/errors.md)
- [How Iris handles paid requests](../concepts/paid-requests.md)
