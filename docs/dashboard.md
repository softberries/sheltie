# Dashboard

The dashboard at `/dashboard` shows, for every repository where the App is installed and you
have access on GitHub:

- **Time saved** over the last 30 days (below).
- **Activity**: landings per day, and what is ready, in flight, landed and returned now.
- **Parallelism**: how many agents the repository can take at once before their changes
  start colliding (below).
- **Agents**: a scorecard per agent that opens pull requests (below).
- **Agent spend**: what replays cost, in estimated dollars and tokens (below).
- **Queue**: open pull requests, their state, footprint and what Sheltie will do next.
- **Recently landed**: each landing's commit, how it was proven safe, and which tier resolved a
  conflict.
- **Returned to their authors**, with the reason, and **agents at work** (claimed issues).

![The dashboard for a repository (example data)](images/dashboard.png)

Sign in with GitHub. Sheltie reads which repositories you can access when you sign in, so after
installing the App on another repository, sign out and in again. The dashboard is read-only.
Setting it up is part of [Self-hosting](self-hosting.md#dashboard).

## Time saved

Sheltie saves two different kinds of time, and the dashboard shows them separately instead of
adding them up.

**Waiting time.** How much sooner pull requests reached the default branch than they would
have in a one-at-a-time merge queue, the usual alternative. Sheltie simulates that queue
from the real times your pull requests became ready (approved, checks green): each one lands
one CI run after the later of its ready time and the previous landing. The CI time is the
median measured duration of CI on your default branch (from the first check run's start to
the last one's end, Sheltie's own check excluded). Sheltie's own landing times are measured.
The headline is the difference summed over the period; the panel also compares the median
time from ready to landed. If Sheltie was slower, the panel says so.

**Author time.** Conflicts Sheltie resolved without the author: merge-engine resolutions
(structural merges, regenerated files, declared rules) and changes re-derived by an agent.
Each counts at an assumed number of minutes an author would have spent instead,
15 per conflict and 30 per re-derived change by default. Set your own in
[`[estimates]`](merge-rules.md#estimates) in `.sheltie/merge.toml`. The dashboard always
shows the assumption next to the number.

Also shown: changes landed through Sheltie, and bad commits it found and reverted.

What it does not claim:

- The comparison is with a one-at-a-time merge queue, not with merging by hand without a queue.
- Waiting time is wall-clock time until a change is on the default branch, not people's working
  hours.
- With fewer than 10 landings in the period, or no CI time measured yet, there is no waiting
  estimate.
- In shadow mode nothing lands through Sheltie, so the panel shows the *predicted* saving: the
  time merges waited after Sheltie would have landed them.

## Parallelism

How many agents can work on the repository at the same time while most of their changes
still land on their own, without a combined build or the merge engine.

Sheltie estimates it from the footprints of the pull requests merged into the default branch
in the last 90 days (at most the 2,000 most recent). It treats them as the kind of work an
agent picks up next. With `N` agents each holding one open pull request, a change lands on
its own when it is independent of the other `N - 1`. A change that conflicts with a share
`c` of the history stays independent with probability `(1 - c)^(N - 1)`. The panel shows the
average over the history for 1, 2, 4, … 64 agents, and the headline is the largest number of
agents for which at least 80% of changes land on their own.

Two changes conflict when one writes a target the other writes or affects, the same rule the
scheduler uses. A conflict is not a failure: Sheltie builds the changes together or resolves
them in [tiers](introduction.md). It costs a CI run or a merge, though, and conflicts are
where waiting time comes back.

The panel also names the targets involved in the most conflicting pairs. Those are the places
where splitting a crate or a central file would raise the number.

What it does not claim:

- It assumes agents pick work independently of each other. Work that clusters (a refactoring
  and its follow-ups) collides more than the estimate says.
- Pull requests whose footprint was never uploaded are left out. With fewer than 20
  footprints, there is no estimate.

## Agents

One row per agent, for the pull requests opened in the last 30 days:

- **PRs** opened, and **merged** with the merge rate: merged against closed without merging.
  Open pull requests count in neither.
- **Returned**: Sheltie sent the pull request back to its author at least once (a conflict it
  could not resolve, or CI failing on the combined state), out of all pull requests.
- **Reverted**: a sweep found it broke the default branch and Sheltie reverted it, out of the
  merged ones. Reverts done by hand are not counted.
- **Replayed**: merged after Sheltie [re-derived it](intent-replay.md), out
  of the merged ones.
- **Review rounds**: reviews requesting changes, per merged pull request.
- **Time to merge**: median time from Sheltie first seeing the pull request to its merge.
- **Replay cost**: what Sheltie's replays of the agent's pull requests cost, per merged pull
  request. It is not what the agent itself cost: Sheltie does not see that.

Which agent opened a pull request comes from [`[[agents]]`](merge-rules.md#agents) rules in
`.sheltie/merge.toml`: by author, branch prefix or label. Without a rule, bot accounts count
as themselves and people are grouped as "People", so the panel never ranks colleagues.
Pull requests opened before Sheltie recorded authors show as "Not recorded".

The same table is available as text, Markdown or JSON:

```sh
sheltie-server scorecard --repo acme/widgets --days 30
```

## Agent spend

Only [replay](intent-replay.md) uses a model. Structural merges, regenerated files,
declared rules, footprints and scheduling cost nothing. The panel shows, for the last 30 days:

- **Estimated spend**, per day and in total, with the number of replays, how many passed
  the checks, and the model most replays used.
- **The last 24 hours against `daily_budget_usd`**, when replay is enabled. That is the
  window the budget limits. The bar turns red from 80%.
- **Tokens**: input, output, and prompt-cache reads and writes, summed over the whole agent
  run, including subagents. Replays run by a replay workflow older than this feature report
  no tokens. The panel then says how many of the replays the counts cover.
- **Per replay** (average cost) and **per hour saved**: the spend divided by the author time
  saved in the same period (see [Time saved](#time-saved)).

The overview adds each repository's spend to its card and the total to the summary.

The amounts are the agent's own estimates (Claude Code's `total_cost_usd`), not your bill.
Replays run on your CI runners with your API key, so check your provider's console for actual
charges.

