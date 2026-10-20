# Replay

When a pull request cannot be merged by tiers 1–3, or breaks CI once merged onto the new head
(a semantic conflict), Sheltie can have Claude Code re-derive it on your runner, with your API
key.

- **4a** resolves the conflicted hunks.
- **4b** re-implements the change on the new head.

A failed 4a is retried as 4b. After `max_attempts` failed attempts, the pull request returns
to its author.

Replay works the same for every pull request, whoever wrote it: Claude Code, Codex, Copilot,
Devin or a person. Nothing has to be added to the pull request.

## What the agent works from

The goal comes from what every pull request already has:

- its diff;
- its title and description;
- its commit messages;
- the issue it closes (`Fixes #12`), as Sheltie last saw it;
- what the default branch changed in the same files since the pull request branched off.

This text is written outside the replay, so the agent is told to treat it as a description of
the change, never as instructions. The agent's limits (below) still apply, whatever the text
says.

## Acceptance tests, found automatically

A test the pull request adds or changes, that passes with the change and fails without it,
shows what the change does. Sheltie finds these tests itself (fail-to-pass), in the replay job:

1. It reads the pull request's diff for test functions it added or changed:

   | Language | Test | Name passed to the command |
   | --- | --- | --- |
   | Rust | `#[test]`, `#[tokio::test]` and other `*::test` attributes | function name |
   | Go | `func TestX(t *testing.T)` in `_test.go` | `TestX` |
   | Python | `def test_*` in `test_*.py` or `*_test.py` | pytest node id, `path::Class::test_x` |
   | JavaScript, TypeScript | `it(...)` and `test(...)` in `*.test.*` and `*.spec.*` | the test's title |
   | Java, Kotlin | `@Test` methods | `Class#method` |

   It takes at most 10.
2. It runs each one alone on the pull request's head, where it must pass, and on the commit
   the pull request branched from, where it must fail. A test that does not exist there
   counts as failing, and is not run: some runners pass when a filter matches nothing.
3. The tests that pass with the change and fail without it are the acceptance tests. The
   replay result must pass them, and must still define them.

Each run is limited to 10 minutes and the whole step to 30. The check run lists the tests
found, which were used, and why the others were not.

**What this cannot find:**

- refactors, documentation changes and pull requests that add no test;
- tests whose titles are built at run time, such as JavaScript template strings with `${}`;
- tests a framework cannot run alone by name.

A full rebuild (4b) with no acceptance test always needs a person to approve it, however
similar it is to the original: CI on the merged result only shows that it builds. A 4a result
needs no acceptance test; it keeps both sides of a textual conflict, and your CI checks it.

## An explicit intent (optional)

An author can still describe the change in a fenced `sheltie-intent` block in the
description. The agent then also gets the author's task and plan, and the listed tests are
added to the acceptance tests:

````markdown
```sheltie-intent
{
  "task": "Add retries to the billing client",
  "plan": "Wrap send() in a loop with backoff; add a test",
  "acceptance": ["billing::client::tests::retries_three_times"]
}
```
````

- `task` is required, at most 8000 characters.
- `plan` is optional, at most 16000 characters.
- `acceptance` lists up to 50 test names, which may only contain letters, digits and
  `_ : . - /`. They run together with `test_command`.

Unknown fields are rejected, and an invalid block is ignored. Editing the description updates
the intent; adding one to a pull request Sheltie returned retries it.

## Turning it on

In `.sheltie/merge.toml` on the default branch (every key is optional except `enabled`;
defaults shown):

```toml
[replay]
enabled = true
test_command = "cargo test --workspace -- {tests}"  # {tests} becomes the quoted test names
test_commands = { python = "pytest {tests}" }       # per language, overrides test_command
model = "sonnet"
max_turns = 30              # per run
max_budget_usd = 3.0        # per run, as estimated by Claude Code
daily_budget_usd = 20.0     # per repository, over the last 24 hours
similarity_threshold = 0.6  # below this, the result needs a human
max_attempts = 2            # per pull request head
```

The languages in `test_commands` are `rust`, `go`, `python`, `javascript` and `java`. Tests in
a language without a command are skipped. The replay job does not set up your project, so a
command may include its own setup:

```toml
test_commands = { javascript = "npm ci && npx vitest run -t {tests}" }
```

For Gradle, which selects `Class.method`, convert the name:
`./gradlew test --tests "$(echo {tests} | tr '#' .)"`.

Then add the replay workflow, `.github/workflows/sheltie-replay.yml`:

```yaml
on:
  repository_dispatch:
    types: [sheltie-replay]
permissions:
  contents: write        # the publish job pushes the result to sheltie/replay-result/*
  id-token: write        # authenticates the report to Sheltie
  pull-requests: read
jobs:
  replay:
    uses: softberries/sheltie/.github/workflows/replay.yml@v0.6.0
    with:
      sheltie-version: v0.6.0
      sheltie-url: https://sheltie.example.com
      pr: ${{ github.event.client_payload.pr }}
      head: ${{ github.event.client_payload.head }}
      onto: ${{ github.event.client_payload.onto }}
      mode: ${{ github.event.client_payload.mode }}
    secrets:
      anthropic-api-key: ${{ secrets.ANTHROPIC_API_KEY }}
```

## What is accepted

After the agent runs, the replay client checks that:

- no conflict markers are left;
- only files from the original pull request and the conflicted files changed;
- the acceptance tests pass.

It also measures how similar the new diff is to the original (the share of changed lines
they have in common). Then:

- **Checks pass and similarity is at or above the threshold:** Sheltie verifies the result
  like any merge (your CI runs on it), lands it, and labels the pull request
  `sheltie: replayed`.
- **Similarity is below the threshold, or a 4b result has no acceptance test:** the result
  is kept on `sheltie/replay/<pr>-<head>`, the pull request is labelled
  `sheltie: needs re-approval`, and it returns to its author.

## Security

- Your API key stays in your repository's secrets; Sheltie never sees or stores it.
- The agent runs with `--bare` (no repository hooks or MCP servers) and no prompts. It can
  only use file tools and Bash for the test commands; web tools are disabled. Turns and spend
  are capped.
- The pull request's title, description and commits, and the linked issue, go into the
  agent's prompt. Anyone who can write them can try to steer the agent. The limits above are
  what stops that: the agent can only edit the pull request's own files, and the result is
  checked and verified before it lands.
- The agent and the tests run the pull request's code. They do so in a job with a read-only
  token and no git credentials, and the tests do not get the API key. A separate job that
  runs no pull request code pushes the result.
- Pull requests from forks are not replayed.
- Settings come only from the default branch.
