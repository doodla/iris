# Documentation style guide

This guide describes how to write and organize the Iris documentation: the README, the pages under
`docs/`, `CONTRIBUTING.md`, and `SECURITY.md`.

Iris follows the [Google developer documentation style guide](https://developers.google.com/style).
This page adds the house rules and the decisions that the Google guide leaves open. Where this page
is silent, follow the Google guide.

## Write each page for one reader

Every page has one type, and each type lives in its own directory:

| Type | Answers | Directory |
|---|---|---|
| Guide | How do I do this task? | `docs/guides/` |
| Concept | How does this work, and why? | `docs/concepts/` |
| Reference | What exactly does this command, field, or code do? | `docs/reference/` |
| Contributing | How do I change Iris? | `docs/contributing/` |

The README is the landing page. It says what Iris is, how to install it, and how to get a first
result, and it links to everything else.

- Start every page with one or two sentences that say what it covers.
- Keep implementation details, such as file locks, module names, and retry classes, out of guides.
  Put them in a concept page, or in the contributing docs.
- End a guide with a "What's next" section of links.

## Give every fact one home

Document each behavior in exactly one place. Other pages link to that place, with at most one
sentence of summary.

| Fact | Home |
|---|---|
| Commands, arguments, flags, and their defaults | [CLI reference](../reference/cli.md), generated from `--help` |
| The JSON envelope and result fields | [JSON output reference](../reference/json-output.md) |
| Exit, error, and warning codes | [Errors reference](../reference/errors.md) |
| Settings, environment variables, API key handling, logging, and time limits | [Configuration reference](../reference/configuration.md) |
| What Iris guarantees about paid requests | [How Iris handles paid requests](../concepts/paid-requests.md) |
| Job states, job records, downloads, retention, and deletion | [How video jobs work](../concepts/video-jobs.md) |
| Why Iris made a choice, with sources | [Decisions](decisions.md) |

When you change a behavior, update its home first. Then search the docs for the old behavior, and
replace any restatement with a link.

Change the README only when the installation, the quickstart, or a headline capability changes.

## Voice and tone

- Address the reader as "you". Use present tense and active voice. Use contractions, such as
  "don't" and "can't".
- Say what Iris does and what the reader can do. State a limitation once, where the reader needs
  it, often in a note.
- Don't defend a design in a guide. Explain the reason in a concept page, or link to
  [Decisions](decisions.md).
- Don't describe Iris with adjectives such as "polished", "honest", "simple", or "powerful". Show
  what it does instead.
- Save "never" for guarantees, and state each guarantee in its home. Elsewhere, "doesn't" is
  usually enough.

## Sentences and paragraphs

- Put one idea in each sentence. Aim for 25 words or fewer.
- Keep paragraphs to about five lines. Turn a series into a list, and a comparison into a table.
- Put a condition before the instruction: "To resume a job, run …", "If the wait times out, …".
- Use parentheses and semicolons sparingly, and don't nest parentheses. A second sentence is usually
  clearer.
- Don't use bold for emphasis. If the reader must not miss something, use a note. You can use bold
  for the run-in heading of a list item.

## Word list

| Use | Instead of |
|---|---|
| response, respond | answer, for what a provider returns |
| job ID, model ID | job id, model id |
| job record | record, when the context doesn't make it clear |
| state directory | state dir |
| dry run (noun), `--dry-run` (flag) | dry-run as a noun |
| for example, such as | e.g. |
| that is | i.e. |
| and so on, or a complete list | etc. |
| Ctrl+C | Ctrl-C or ^C, except in captured output |
| provider | vendor |

Write product names as their owners do: OpenAI, Google, the Gemini API, Veo, GPT Image, and Nano
Banana. Write Iris commands in code format: `iris jobs wait`.

## Headings

- Use sentence case.
- Start a guide's headings with a verb, such as "Resume a video job". Use a noun phrase for concept
  and reference headings, such as "Job states".
- Don't put flags, paths, or type signatures in headings. A reference entry whose name is code, such
  as an error code, can use it as its heading.

## Procedures

- Number the steps, put one action in each step, and start each step with a verb.
- Introduce the procedure with a sentence, such as "To resume a job, do the following:".
- If a step prints something that the reader needs, show it.

## Commands and output

- Put commands in `sh` code blocks without a prompt, so that readers can copy them.
- Show output in a separate `text` block after the command, introduced by "The output is similar
  to the following:".
- Format JSON output for reading, and say once on the page that Iris prints it on one line. Shorten
  it with `"...": "..."`.
- Break a command that's longer than about 100 characters with a backslash at the end of the line.
- Use `UPPER_SNAKE_CASE` placeholders, such as `JOB_ID`, and list them after the code block under
  "Replace the following:". Use the same name for the same thing on every page: `JOB_ID`, `MODEL`,
  `PROMPT`, `LABEL`, `PATH`, `DIR`, and `USD`.
- Capture every output from the current binary. For commands that call a provider, run the binary
  against the mock server in `tests/live/mock_providers.py`, and don't show the
  `non_default_base_url` warnings that the mock setup causes. Don't mention the mock server on a
  user page: "similar to the following" covers the differences.

## Notes and warnings

Use a GitHub alert for information that the reader must not miss:

- `> [!NOTE]` for a limitation or a behavior that isn't obvious.
- `> [!IMPORTANT]` for billing and access requirements.
- `> [!WARNING]` for an action that can lose data or cost money twice.

Use at most three alerts on a page, and keep each to one or two sentences.

## Links

- Use the page title or a description as link text, not a file name or "here".
- Link within the repository with relative paths.
- The release archive ships only `README.md`, `CHANGELOG.md`, `LICENSE`, and `docs/`. From those
  files, link to anything else in the repository, such as `CONTRIBUTING.md` or `scripts/`, with an
  absolute `https://github.com/doodla/iris/blob/main/...` URL.
- Link to the provider's official documentation, not to a third-party page.

## Formatting

- Wrap prose at 100 characters. Don't wrap tables, URLs, or code.
- Use a table for reference information with more than two attributes.
- Write dates as `YYYY-MM-DD`. Don't write "currently", "today", "new", or "soon": name the version
  or the date instead.
- Refer to a release by its version number, such as Iris 0.1.0.

## Checks

`cargo test` checks parts of this guide:

- `tests/docs.rs` checks that every relative link and anchor resolves, and that the files in the
  release archive link outside it only with absolute URLs. It also checks that prose, outside code,
  has no Latin abbreviations, no spaced em dashes, and no "answer" for a provider's response. This
  page, the generated CLI reference, and the agent instruction files are exempt from the prose
  checks.
- `tests/cli_reference.rs` checks that the [CLI reference](../reference/cli.md) matches the help.
  After you change help text, regenerate the page with
  `IRIS_UPDATE_DOCS=1 cargo test --test cli_reference`.
- `tests/schema_contract.rs` checks that the code tables in the
  [Errors reference](../reference/errors.md) list exactly the codes that Iris defines.
