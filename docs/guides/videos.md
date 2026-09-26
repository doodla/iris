# Generate videos

This guide shows how to generate a video with Veo, follow the job, and download the video, even
from another process or after a crash. For how jobs work behind these commands, see
[How video jobs work](../concepts/video-jobs.md).

## Before you begin

- [Install Iris](install.md) and set `GEMINI_API_KEY`. Veo has no free tier: your key's project
  needs a paid-tier billing plan. See [Check your access](models-and-costs.md#check-your-access).
- The Veo models are previews. Google can change their behavior, limits, and availability.

## Generate a video and wait for it

To generate a video, pass a model and a prompt:

```sh
iris video generate -m veo-lite "waves crashing at dusk" --duration 4 -o waves.mp4
```

Iris submits the job, checks it every 10 seconds, and saves the video when it's ready. The output is
similar to the following:

```text
Submitting job job_01m3edfzf05b5enc57akzrhadk to gemini (veo-3.1-lite-generate-preview); this is a paid request
Job job_01m3edfzf05b5enc57akzrhadk accepted by gemini
Job job_01m3edfzf05b5enc57akzrhadk is running (40% done)
Job job_01m3edfzf05b5enc57akzrhadk succeeded
Downloading output 0 of job job_01m3edfzf05b5enc57akzrhadk
warning[preview_model]: veo-3.1-lite-generate-preview is a preview model; its behavior, limits, and availability may change
Saved /home/you/waves.mp4
```

The command waits for up to 10 minutes. To change that, pass `--timeout`, such as `--timeout 30m`.
If the time passes first, the job keeps running, and you can
[wait for it again](#wait-for-a-job-and-download-the-video).

The common video options have typed flags: `--duration`, `--resolution`, `--aspect-ratio`, and
`--negative-prompt`. To start from images, pass a first frame with `--image`, a last frame with
`--last-frame`, or reference images with `--ref`. Not every model accepts every option: run
`iris models show MODEL` to see what a model takes. Veo always generates audio, and you can't turn
it off.

## Submit a job and return

To submit a job without waiting for it, add `--detach`:

```sh
iris video generate -m veo-lite "a paper boat drifting on a pond" --duration 4 --detach
```

The output is similar to the following:

```text
Submitting job job_01m3ede0hbar1cwbyjgnyv7mm7 to gemini (veo-3.1-lite-generate-preview); this is a paid request
Job job_01m3ede0hbar1cwbyjgnyv7mm7 accepted by gemini
warning[preview_model]: veo-3.1-lite-generate-preview is a preview model; its behavior, limits, and availability may change
Submitted job job_01m3ede0hbar1cwbyjgnyv7mm7: running (gemini veo-3.1-lite-generate-preview)
Next: iris jobs status job_01m3ede0hbar1cwbyjgnyv7mm7
Next: iris jobs wait job_01m3ede0hbar1cwbyjgnyv7mm7
```

Iris records the job on your disk before it submits it. You can close the terminal: any later Iris
process that uses the same state directory can follow the job.

## Check a job's status

To check a job, run `iris jobs status`:

```sh
iris jobs status JOB_ID
```

Replace `JOB_ID` with the job's ID, such as `job_01m3ede0hbar1cwbyjgnyv7mm7`.

The output is similar to the following:

```text
Job job_01m3ede0hbar1cwbyjgnyv7mm7 is running (40% done)
job_01m3ede0hbar1cwbyjgnyv7mm7
  status:     running
  provider:   gemini
  model:      veo-3.1-lite-generate-preview
  prompt:     31 characters, sha256 c039da7d465dad0428a4cc4af986569874d79ecb6ff0233b454c7b154d587745
  created:    2026-09-26T08:31:13Z
  submitted:  2026-09-26T08:31:13Z
  checked:    2026-09-26T08:31:13Z
  remote op:  models/veo-3.1-lite-generate-preview/operations/mock-op-1
  save to:    directory /home/you
  cost:       ~$0.20 USD (4 s × $0.05/s (veo-3.1-lite-generate-preview, 720p, audio included); estimate; blocked videos are not charged)
```

Checking a status is free. To show only the local record, without asking the provider, add
`--no-refresh`.

## Wait for a job and download the video

To wait for a job and save its video, run `iris jobs wait`:

```sh
iris jobs wait JOB_ID
```

The output is similar to the following:

```text
Job job_01m3ede0hbar1cwbyjgnyv7mm7 is running (40% done)
Job job_01m3ede0hbar1cwbyjgnyv7mm7 succeeded
Downloading output 0 of job job_01m3ede0hbar1cwbyjgnyv7mm7
Saved /home/you/job_01m3ede0hbar1cwbyjgnyv7mm7.mp4
```

If the wait limit passes first, the command exits with code 4, and the job continues:

```text
error[wait_timeout]: job job_01m3ede0hbar1cwbyjgnyv7mm7 did not finish within 1ms; it continues remotely
  hint: resume with `iris jobs wait job_01m3ede0hbar1cwbyjgnyv7mm7` (or check with `iris jobs status job_01m3ede0hbar1cwbyjgnyv7mm7`)
  job: job_01m3ede0hbar1cwbyjgnyv7mm7 (status running)
  remote operation: models/veo-3.1-lite-generate-preview/operations/mock-op-1
```

Pressing Ctrl+C also stops only the wait. The command exits with code 130, and the job keeps
running. To resume, run `iris jobs wait JOB_ID` again.

> [!WARNING]
> Veo keeps generated videos for 2 days. Download your video before then. After that, the download
> fails with `artifact_expired`, and getting the video again means paying for a new job.

## Download the video again

`iris jobs download` saves the outputs of a finished job. It never generates the video again, so
you can run it as often as you like. If the saved file is intact, Iris doesn't even contact the
provider:

```sh
iris jobs download JOB_ID
```

The output is similar to the following:

```text
warning[already_downloaded]: an identical file is already at /home/you/job_01m3ede0hbar1cwbyjgnyv7mm7.mp4; nothing was written
Saved /home/you/job_01m3ede0hbar1cwbyjgnyv7mm7.mp4
```

To save another copy, pass `-d` or `-o`. Iris copies the saved file without a network request. To
fetch the video from the provider again and replace the saved file, add `--overwrite`.

If the job is still running, `iris jobs download` exits with code 4 (`job_not_ready`) instead of
waiting. Use `iris jobs wait` to wait.

## Choose where the video is saved

When it submits a job, `iris video generate` records where to save the video: its `-o` path, or
else the output directory in effect at that time, and whether you passed `--overwrite`. By default,
the video is saved as `JOB_ID.mp4` in the output directory.

`iris jobs wait` and `iris jobs download` save to their own `-o` or `-d`, if you pass one.
Otherwise, they save where the job recorded, whatever `IRIS_OUTPUT_DIR` or `output_dir` say at that
time. Every job view shows the recorded place as `save to`, or `output_plan` with `--json`.

## Label a job

A label names a job so that you can find it later, and so that running the same command twice
doesn't pay for two videos. To add a label, pass `--label`:

```sh
iris video generate -m veo-lite "a paper boat drifting on a pond" --duration 4 --label paper-boat-1 --detach
```

A label is 1 to 64 letters, digits, `.`, `_`, or `-`, and starts with a letter or digit. Labels are
case-sensitive. Iris stores the label as you wrote it, so don't put anything secret in it.

No two job records in a state directory have the same label. If you run the command again, it fails
before it sends anything, and names the job:

```text
error[label_in_use]: label 'paper-boat-1' is already used by job job_01m3ec3srq9zc1vk50301pyxne (running, created 2026-09-26T08:08:10Z)
  hint: the job is still running: follow it with `iris jobs status job_01m3ec3srq9zc1vk50301pyxne` or `iris jobs wait job_01m3ec3srq9zc1vk50301pyxne`; deleting its local record does not cancel the remote job, which keeps running and is billed; to submit another paid job, use another label
  job: job_01m3ec3srq9zc1vk50301pyxne (status running)
  remote operation: models/veo-3.1-lite-generate-preview/operations/mock-op-1
```

What to do next depends on the job's status, and the hint says it:

| The labeled job is | What to do |
|---|---|
| `submitting` or `running` | Follow it with `iris jobs status` or `iris jobs wait`. Deleting its record doesn't cancel it. To submit another paid job, use another label. |
| `succeeded` | Save its outputs with `iris jobs download`. To submit another job under the same label, delete its record first. |
| `failed` or `expired` | To try again, delete its record first, or use another label. The new submission is billed. |
| `submission_unknown` | The provider might have accepted and billed it. Check your usage in the provider's console before you submit again. |

To find a labeled job, run `iris jobs list --label LABEL`.

## Recover a job after a crash

If the process that submitted a job was killed, it might not have printed the job ID.

- **If you used a label**, run the same command again. It fails with `label_in_use` and names the
  job. You can also run `iris jobs list --label LABEL`.
- **If you didn't use a label**, list the jobs that are still being submitted:

  ```sh
  iris jobs list --status submitting --json
  ```

  After about 37 minutes, such a job is reported as `submission_unknown`, so list that status
  instead. Match the job's `model`, `created_at`, and `prompt_fingerprint` with your request. The
  fingerprint is the SHA-256 hash of the prompt, which you can compute:

  ```sh
  printf %s "a paper boat drifting on a pond" | sha256sum
  ```

  A dry run of the command also shows the fingerprint, as `result.prompt_fingerprint`, so a script
  can record it before it submits.

A job record that has no operation name can't be finished. Check the provider's console for the
request, then delete the record with `iris jobs delete JOB_ID --force`.

## List jobs

To list your jobs, newest first, run `iris jobs list`. The output is similar to the following:

```text
JOB ID                          LABEL         STATUS     PROVIDER  MODEL                          CREATED
job_01m3ec3srq9zc1vk50301pyxne  paper-boat-1  succeeded  gemini    veo-3.1-lite-generate-preview  2026-09-26T08:08:10Z
job_01m3ec3f7m8pwemswax64ke6h8  -             succeeded  gemini    veo-3.1-lite-generate-preview  2026-09-26T08:07:59Z
```

To filter the list, add `--status`, `--provider`, `--label`, or `--limit`.

## Delete job records

To delete a job's local record, run `iris jobs delete JOB_ID`. To delete every record, use `--all`.
Deleting a record doesn't cancel the job or delete videos that you saved.

Iris refuses to delete a record when that would lose track of paid work, such as a job that's still
running:

```text
error[invalid_argument]: job job_01m3ec3srq9zc1vk50301pyxne is still running; deleting its local record would make the job unrecoverable
  hint: wait for the job to finish (`iris jobs wait job_01m3ec3srq9zc1vk50301pyxne`), or pass --force to delete the local record anyway (the remote job is not cancelled)
  job: job_01m3ec3srq9zc1vk50301pyxne (status running)
```

To delete it anyway, add `--force`. For the rules, see
[Deleting job records](../concepts/video-jobs.md#deleting-job-records).

## Follow jobs from another machine or container

Job records live in your state directory, so only a process that uses the same state directory can
follow a job. In a container or CI job, keep the state directory on a persistent volume, or set
`IRIS_STATE_DIR` to the same path for every command. See
[Where job records live](../concepts/video-jobs.md#where-job-records-live).

## What's next

- [Use Iris in scripts and agents](agents.md): a video workflow that survives crashes.
- [How video jobs work](../concepts/video-jobs.md)
- Every flag: [`iris video generate`](../reference/cli.md#iris-video-generate) and the
  [`iris jobs`](../reference/cli.md#iris-jobs) commands in the CLI reference.
