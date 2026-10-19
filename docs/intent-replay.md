# Intent replay

When a pull request cannot be merged by tiers 1–3, or breaks CI once merged onto the new head
(a semantic conflict), Sheltie can have Claude Code re-derive it from the author's recorded
intent, on your runner with your API key.

- **4a** resolves the conflicted hunks.
- **4b** re-implements the change on the new head.

A failed 4a is retried as 4b. After `max_attempts` failed attempts, the pull request returns
to its author.

## Recording the intent

The pull request description carries the intent as a fenced `sheltie-intent` block:

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
  `_ : . - /`.

Unknown fields are rejected. Sheltie reads the description on every pull request event, so
editing it updates the intent.

## Turning it on

In `.sheltie/merge.toml` on the default branch (every key is optional except `enabled`;
defaults shown):

```toml
[replay]
enabled = true
test_command = "cargo test --workspace -- {tests}"  # {tests} becomes the quoted test names
model = "sonnet"
max_turns = 30              # per run
max_budget_usd = 3.0        # per run, as estimated by Claude Code
daily_budget_usd = 20.0     # per repository, over the last 24 hours
similarity_threshold = 0.6  # below this, the result needs a human
max_attempts = 2            # per pull request head
```

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
    uses: softberries/sheltie/.github/workflows/replay.yml@v0.5.0
    with:
      sheltie-version: v0.5.0
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
  like any merge, lands it, and labels the pull request `sheltie: replayed`.
- **Similarity is below the threshold:** the result is kept on `sheltie/replay/<pr>-<head>`,
  the pull request is labelled `sheltie: needs re-approval`, and it returns to its author.

## Security

- Your API key stays in your repository's secrets; Sheltie never sees or stores it.
- The agent runs with `--bare` (no repository hooks or MCP servers) and no prompts. It can
  only use file tools and Bash for the test command; web tools are disabled. Turns and spend
  are capped.
- The agent and the acceptance tests run the pull request's code. They do so in a job with a
  read-only token and no git credentials, and the tests do not get the API key. A separate
  job that runs no pull request code pushes the result.
- Pull requests from forks are not replayed.
- Settings come only from the default branch.
