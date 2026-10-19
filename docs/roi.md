# ROI report

Before installing anything, find out what Sheltie would have saved on your repository.
`sheltie-cargo roi` replays the last 90 days of your default branch through Sheltie's
scheduler and compares it with a one-at-a-time merge queue. It runs locally in your checkout
and sends nothing anywhere.

```sh
git clone https://github.com/acme/widgets && cd widgets
sheltie-cargo roi --ci-minutes 12
```

```text
Sheltie ROI report · acme/widgets
412 landings on origin/main, 2026-07-20 to 2026-10-17

  9 d 2 h less waiting and 361 fewer CI runs than a one-at-a-time merge queue

                            One at a time        Sheltie
  Waiting, total                 11 d 6 h       2 d 4 h
  Waiting, median                  48 min           0 s
  Waiting, 90th percentile     3 h 10 min        24 min
  Extra CI runs                       412            51
…
```

(Illustrative output.) `sheltie-cargo` comes with every
[release](https://github.com/softberries/sheltie/releases), or build it with
`cargo install --git https://github.com/softberries/sheltie sheltie-cargo`.

## Options

| Option | Default | Meaning |
| --- | --- | --- |
| `--ci-minutes` | required | Median CI time on the branch, in minutes |
| `--days` | `90` | Days of history |
| `--branch` | the remote's default branch | Branch to replay |
| `--exact` | off | Compute each landing's footprint with the full adapter (slower) |
| `--format` | `text` | `text`, `markdown` or `json` |
| `--name` | `owner/name` of `origin` | Name in the heading |
| `--ignore` | | Extra glob for files that never affect the build (repeatable) |

To get the median CI time of a GitHub Actions workflow named `CI` on `main`:

```sh
gh run list --workflow CI --branch main --event push --status success --limit 50 \
  --json startedAt,updatedAt \
  --jq '[.[] | ((.updatedAt|fromdate) - (.startedAt|fromdate)) / 60] | sort | .[length/2|floor] | ceil'
```

## How it works

**The history.** Every first-parent commit on the branch in the period is a landing: a
merged pull request (merge or squash commit) or a direct push. Its change is the diff against
its first parent. It counts as ready when it landed, which is the earliest it could have been.

**What each change was tested on.** A change's own CI ran on the branch as it was when the
pull request was last based. Sheltie rebuilds a change when something that landed after that
touches what it reads, so the replay needs that base:

- for a merge commit, the merge base of its second parent (the pull request head) with the
  branch;
- for a squash commit whose subject ends in `(#123)`, the merge base of `refs/pull/123/head`,
  if those refs have been fetched:

  ```sh
  git fetch origin '+refs/pull/*/head:refs/pull/*/head'
  ```

- otherwise (direct pushes, rebase merges, unfetched heads) it is unknown, and the change
  counts as tested on the branch as it was when it landed. That leaves out rebuilds Sheltie
  would have needed, so the report says how many changes it applies to.

The merge base is the oldest the tested branch can have been; CI that ran later on a merge
ref saw a newer branch, so with a known base the replay errs towards more rebuilds.

**Footprints.** By default each landing's footprint is predicted from its changed files
against the build graph of the branch tip, the way Sheltie predicts footprints for
[agents](agents.md). Prediction is pessimistic: every change is assumed to alter the
interfaces of what it writes, so more changes conflict than really would. Files no target
owns count as their own targets. With `--exact`, `sheltie-cargo` computes every landing's
footprint as CI would; that runs `cargo metadata` per landing and takes a few seconds each.

**The two queues.** Both take the same changes at the same times, with the same CI time.

- *One at a time*: each change lands one CI run after the later of its ready time and the
  previous landing.
- *Sheltie*: on every arrival and every finished build, the real scheduler plans the waiting
  changes. Changes that land now cost nothing extra: their own CI already passed and nothing
  that landed since touches them. A change that needs a rebuild, the merge engine or a
  combined build waits one CI run, together with its group. A change that conflicts with a
  group being built waits for that group.

The report shows the waiting time (total, median and 90th percentile), the CI runs each
queue adds on top of the pull requests' own CI, what Sheltie would have done with the
changes, and the [parallelism](dashboard.md#parallelism) estimate from the same footprints.

## What it does not claim

- Every CI run is assumed to pass, in both queues; red and flaky runs are not simulated.
- Changes without a known base are assumed to have been tested on the branch as it was when
  they landed. This matters: on six projects with merge queues, assuming that for every
  change cut Sheltie's rebuilds from 36–348 to almost none (see
  [Sheltie on real repositories](real-world.md)). Fetch pull request heads for squash-merged
  repositories.
- Merge-engine resolutions are assumed to succeed. In practice some would go back to their
  authors.
- The comparison is with a one-at-a-time merge queue. If you merge by hand without a queue
  today, waiting time is not what you save; the [shadow mode](self-hosting.md) report on your
  real pull requests is the better measure.
- At least 10 landings with a footprint are needed.
