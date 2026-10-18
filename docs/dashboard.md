# Dashboard

The dashboard at `/dashboard` shows, for every repository where the App is installed and you
have access on GitHub:

- **Time saved** over the last 30 days (below).
- **Activity**: landings per day, and what is ready, in flight, landed and returned now.
- **Agent spend**: what intent replays cost, in estimated dollars and tokens (below).
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
(structural merges, regenerated files, declared rules) and changes re-derived from their
intent. Each counts at an assumed number of minutes an author would have spent instead,
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

## Agent spend

Only [intent replay](intent-replay.md) uses a model. Structural merges, regenerated files,
declared rules, footprints and scheduling cost nothing. The panel shows, for the last 30 days:

- **Estimated spend**, per day and in total, with the number of replays and how many passed
  the checks.
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

