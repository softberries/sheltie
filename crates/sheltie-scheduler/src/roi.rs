//! The ROI report: a repository's merge history replayed through the scheduler, against a
//! one-at-a-time merge queue.
//!
//! Each landing on the default branch is a change that became ready when it landed. Two
//! queues take the same arrivals:
//!
//! - **One at a time.** Each change waits for the previous one, then needs one CI run on top
//!   of it: it lands at `max(ready, previous landing) + CI time` (the model of
//!   [`serial_queue_waits`]).
//! - **Sheltie.** On every arrival and completion, [`plan_epoch`] plans the waiting changes.
//!   A change that lands now costs nothing more: its own CI already passed and nothing landed
//!   since touches it. Anything else (reverify, merge engine, a combined build) is one CI run
//!   for the group, after which the whole group lands. A change that conflicts with a group
//!   in flight waits for it.
//!
//! Each change is assumed to be based on the default branch as it was when it became ready
//! in the replay, and every CI run to pass. See `docs/roi.md`.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use sheltie_core::{Footprint, Relation, TargetId};

use crate::capacity::Capacity;
use crate::report::percentile;
use crate::savings::serial_queue_waits;
use crate::{Candidate, Decision, plan_epoch};

/// Fewer landings than this are too few to report on.
pub const MIN_CHANGES: usize = 10;

/// A change from the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoricalChange {
    /// When it became ready; for history read from git, when it landed.
    pub ready_at: DateTime<Utc>,
    /// Its footprint.
    pub footprint: Footprint,
}

/// How one queue did on the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QueueOutcome {
    /// CI runs the queue needed on top of each pull request's own CI.
    pub ci_runs: u64,
    /// Time from ready to landed, summed over the changes, in seconds.
    pub total_wait_seconds: i64,
    /// Median time from ready to landed, in seconds.
    pub median_wait_seconds: i64,
    /// 90th percentile of the time from ready to landed, in seconds.
    pub p90_wait_seconds: i64,
}

/// What happened to the changes in the Sheltie replay, by their final decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Decisions {
    /// Landed as soon as they were ready, with no extra CI run.
    pub landed_at_once: u64,
    /// Rebuilt on the new head because something landed that they read.
    pub reverified: u64,
    /// Built together with changes they semantically conflict with.
    pub built_together: u64,
    /// Sent to the merge engine: they write a target another change writes.
    pub merge_engine: u64,
}

/// Both queues on the same history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Replay {
    /// Changes replayed.
    pub changes: usize,
    /// CI time per run, in seconds.
    pub ci_seconds: i64,
    /// The one-at-a-time queue.
    pub serial: QueueOutcome,
    /// Sheltie.
    pub sheltie: QueueOutcome,
    /// Sheltie's decisions.
    pub decisions: Decisions,
}

impl Replay {
    /// Waiting time saved against the one-at-a-time queue, in seconds; negative if Sheltie
    /// was slower.
    pub fn wait_saved_seconds(&self) -> i64 {
        self.serial.total_wait_seconds - self.sheltie.total_wait_seconds
    }

    /// CI runs saved against the one-at-a-time queue.
    pub fn ci_runs_saved(&self) -> i64 {
        i64::try_from(self.serial.ci_runs).unwrap_or(i64::MAX)
            - i64::try_from(self.sheltie.ci_runs).unwrap_or(i64::MAX)
    }
}

fn outcome(mut waits: Vec<i64>, ci_runs: u64) -> QueueOutcome {
    waits.sort_unstable();
    QueueOutcome {
        ci_runs,
        total_wait_seconds: waits.iter().sum(),
        median_wait_seconds: percentile(&waits, 50).unwrap_or(0),
        p90_wait_seconds: percentile(&waits, 90).unwrap_or(0),
    }
}

/// A group of changes in one CI run.
struct Flight {
    done_at: DateTime<Utc>,
    members: Vec<usize>,
}

/// Replays `changes` through both queues with `ci_seconds` per CI run. Returns `None` with
/// fewer than [`MIN_CHANGES`] changes or a CI time that is not positive.
pub fn replay(changes: &[HistoricalChange], ci_seconds: i64) -> Option<Replay> {
    if changes.len() < MIN_CHANGES || ci_seconds <= 0 {
        return None;
    }
    let mut order: Vec<usize> = (0..changes.len()).collect();
    order.sort_by_key(|&i| (changes[i].ready_at, i));
    let ci = Duration::seconds(ci_seconds);

    // One at a time.
    let ready: Vec<DateTime<Utc>> = order.iter().map(|&i| changes[i].ready_at).collect();
    let serial = outcome(serial_queue_waits(&ready, ci_seconds), changes.len() as u64);

    // Sheltie.
    let n = changes.len();
    let mut landed_at: Vec<Option<DateTime<Utc>>> = vec![None; n];
    let mut final_decision: Vec<&'static str> = vec![""; n];
    let mut pending: Vec<usize> = Vec::new();
    let mut flights: Vec<Flight> = Vec::new();
    let mut next = 0;
    let mut ci_runs = 0u64;
    // Writes landed so far, in landing order: (when, change).
    let mut landed_log: Vec<(DateTime<Utc>, usize)> = Vec::new();

    loop {
        let next_arrival = order.get(next).map(|&i| changes[i].ready_at);
        let next_done = flights.iter().map(|f| f.done_at).min();
        let now = match (next_arrival, next_done) {
            (Some(a), Some(d)) => a.min(d),
            (Some(a), None) => a,
            (None, Some(d)) => d,
            (None, None) => break,
        };
        // Completions first: their writes are on the branch for this epoch.
        let (done, flying): (Vec<Flight>, Vec<Flight>) =
            flights.into_iter().partition(|f| f.done_at <= now);
        flights = flying;
        for f in done {
            for m in f.members {
                landed_at[m] = Some(f.done_at);
                landed_log.push((f.done_at, m));
            }
        }
        while let Some(&i) = order.get(next) {
            if changes[i].ready_at > now {
                break;
            }
            pending.push(i);
            next += 1;
        }

        // Changes that conflict with a group in flight wait for it to land.
        let in_flight: Vec<usize> = flights.iter().flat_map(|f| f.members.clone()).collect();
        let (waiting, plannable): (Vec<usize>, Vec<usize>) = pending.iter().partition(|&&p| {
            in_flight.iter().any(|&f| {
                changes[p].footprint.relation(&changes[f].footprint) != Relation::Independent
            })
        });
        pending = waiting;
        if plannable.is_empty() {
            continue;
        }
        let candidates: Vec<Candidate<usize>> = plannable
            .iter()
            .map(|&p| {
                let since = changes[p].ready_at;
                let landed_writes: BTreeSet<TargetId> = landed_log
                    .iter()
                    .filter(|(at, _)| *at > since)
                    .flat_map(|(_, m)| changes[*m].footprint.writes.iter().cloned())
                    .collect();
                Candidate {
                    key: p,
                    priority: 0,
                    waited_minutes: (now - since).num_minutes(),
                    footprint: changes[p].footprint.clone(),
                    landed_writes: Some(landed_writes),
                }
            })
            .collect();
        let plan = plan_epoch(&candidates);

        // Group the changes that share a CI run: chains and batches by their member list,
        // merge-engine changes with the ready changes they conflict with.
        let mut group_of: BTreeMap<usize, usize> = BTreeMap::new();
        let mut groups: Vec<BTreeSet<usize>> = Vec::new();
        let mut join = |members: &[usize], group_of: &mut BTreeMap<usize, usize>| {
            let existing: BTreeSet<usize> = members
                .iter()
                .filter_map(|m| group_of.get(m).copied())
                .collect();
            let target = match existing.iter().next() {
                Some(&g) => g,
                None => {
                    groups.push(BTreeSet::new());
                    groups.len() - 1
                }
            };
            for &g in existing.iter().skip(1) {
                let moved = std::mem::take(&mut groups[g]);
                for m in &moved {
                    group_of.insert(*m, target);
                }
                groups[target].extend(moved);
            }
            for &m in members {
                group_of.insert(m, target);
                groups[target].insert(m);
            }
        };
        for (&key, decision) in &plan {
            final_decision[key] = decision.label();
            match decision {
                Decision::LandNow { .. } => {
                    landed_at[key] = Some(now);
                    landed_log.push((now, key));
                }
                Decision::Reverify { .. } => join(&[key], &mut group_of),
                Decision::MergeEngine { with, .. } => {
                    let mut members = with.clone();
                    members.push(key);
                    join(&members, &mut group_of);
                }
                Decision::SpeculativeChain { chain, .. } => join(chain, &mut group_of),
                Decision::GroupBatch { group } => join(group, &mut group_of),
            }
        }
        for members in groups.into_iter().filter(|g| !g.is_empty()) {
            ci_runs += 1;
            flights.push(Flight {
                done_at: now + ci,
                members: members.into_iter().collect(),
            });
        }
    }

    let mut decisions = Decisions::default();
    for d in &final_decision {
        match *d {
            "land_now" => decisions.landed_at_once += 1,
            "reverify" => decisions.reverified += 1,
            "merge_engine" => decisions.merge_engine += 1,
            _ => decisions.built_together += 1,
        }
    }
    let waits: Vec<i64> = (0..n)
        .map(|i| landed_at[i].map_or(0, |at| (at - changes[i].ready_at).num_seconds().max(0)))
        .collect();
    Some(Replay {
        changes: n,
        ci_seconds,
        serial,
        sheltie: outcome(waits, ci_runs),
        decisions,
    })
}

/// The ROI report for one repository.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoiReport {
    /// Repository name or path, for the heading.
    pub repository: String,
    /// The branch replayed.
    pub branch: String,
    /// First and last landing replayed.
    pub from: DateTime<Utc>,
    /// Last landing replayed.
    pub to: DateTime<Utc>,
    /// How footprints were obtained, e.g. "predicted from changed files".
    pub footprints: String,
    /// Landings left out because no footprint could be computed.
    pub skipped: usize,
    /// The replay.
    pub replay: Replay,
    /// How many agents the repository could take at once, from the same footprints.
    pub capacity: Option<Capacity>,
}

/// `1 d 4 h`, `3 h 20 min`, `12 min`, `40 s`.
pub fn duration_words(seconds: i64) -> String {
    let s = seconds.unsigned_abs();
    let sign = if seconds < 0 { "−" } else { "" };
    let (d, h, m) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60);
    let body = if d > 0 {
        format!("{d} d {h} h")
    } else if h > 0 {
        format!("{h} h {m} min")
    } else if m > 0 {
        format!("{m} min")
    } else {
        format!("{s} s")
    };
    format!("{sign}{body}")
}

impl RoiReport {
    fn rows(&self) -> Vec<(&'static str, String, String)> {
        let r = &self.replay;
        let (s, q) = (&r.sheltie, &r.serial);
        vec![
            (
                "Waiting, total",
                duration_words(q.total_wait_seconds),
                duration_words(s.total_wait_seconds),
            ),
            (
                "Waiting, median",
                duration_words(q.median_wait_seconds),
                duration_words(s.median_wait_seconds),
            ),
            (
                "Waiting, 90th percentile",
                duration_words(q.p90_wait_seconds),
                duration_words(s.p90_wait_seconds),
            ),
            (
                "Extra CI runs",
                q.ci_runs.to_string(),
                s.ci_runs.to_string(),
            ),
        ]
    }

    fn headline(&self) -> String {
        let r = &self.replay;
        let saved = r.wait_saved_seconds();
        let runs = r.ci_runs_saved();
        format!(
            "{} {} waiting and {} {} CI run{} than a one-at-a-time merge queue",
            duration_words(saved.abs()),
            if saved >= 0 { "less" } else { "more" },
            runs.abs(),
            if runs >= 0 { "fewer" } else { "more" },
            if runs.abs() == 1 { "" } else { "s" },
        )
    }

    fn decision_lines(&self) -> Vec<(u64, &'static str)> {
        let d = &self.replay.decisions;
        vec![
            (d.landed_at_once, "land as soon as they are ready"),
            (d.reverified, "rebuild on the new head first"),
            (d.built_together, "build together with a conflicting change"),
            (
                d.merge_engine,
                "go to the merge engine (same targets written)",
            ),
        ]
    }

    fn capacity_line(&self) -> Option<String> {
        self.capacity.as_ref().map(|c| {
            format!(
                "{}{} agent{} at once keep{} {:.0}% of changes landing on their own",
                c.agents,
                if c.at_least { "+" } else { "" },
                if c.agents == 1 && !c.at_least {
                    ""
                } else {
                    "s"
                },
                if c.agents == 1 && !c.at_least {
                    "s"
                } else {
                    ""
                },
                c.threshold * 100.0
            )
        })
    }

    fn assumptions(&self) -> Vec<String> {
        vec![
            format!(
                "{} landings on `{}` from {} to {}, each ready when it landed.",
                self.replay.changes,
                self.branch,
                self.from.format("%Y-%m-%d"),
                self.to.format("%Y-%m-%d")
            ),
            format!(
                "CI takes {} per run, and every run passes.",
                duration_words(self.replay.ci_seconds)
            ),
            format!("Footprints {}.", self.footprints),
            "Waiting is wall-clock time from ready to landed; the pull requests' own CI is the same in both queues and not counted.".to_owned(),
        ]
    }

    /// Renders the report for a terminal; bold headings when `color`.
    pub fn to_text(&self, color: bool) -> String {
        let (bold, dim, reset) = if color {
            ("\x1b[1m", "\x1b[2m", "\x1b[0m")
        } else {
            ("", "", "")
        };
        let mut out = format!(
            "{bold}Sheltie ROI report · {}{reset}\n{dim}{} landings on {}, {} to {}{reset}\n\n  {bold}{}{reset}\n",
            self.repository,
            self.replay.changes,
            self.branch,
            self.from.format("%Y-%m-%d"),
            self.to.format("%Y-%m-%d"),
            self.headline()
        );
        let rows = self.rows();
        let w = rows
            .iter()
            .map(|(l, _, _)| l.chars().count())
            .max()
            .unwrap_or(0);
        let col = rows
            .iter()
            .map(|(_, a, _)| a.chars().count())
            .max()
            .unwrap_or(0)
            .max("One at a time".len());
        out.push_str(&format!(
            "\n  {}  {dim}{:>col$}  {:>13}{reset}\n",
            " ".repeat(w),
            "One at a time",
            "Sheltie"
        ));
        for (label, a, b) in rows {
            out.push_str(&format!(
                "  {label}{}  {a:>col$}  {bold}{b:>13}{reset}\n",
                " ".repeat(w - label.chars().count())
            ));
        }
        out.push_str(&format!(
            "\n{bold}With Sheltie, of {} changes{reset}\n",
            self.replay.changes
        ));
        for (n, what) in self.decision_lines() {
            out.push_str(&format!("  {n:>5}  {what}\n"));
        }
        if let Some(line) = self.capacity_line() {
            out.push_str(&format!("\n{bold}Parallelism{reset}\n  {line}\n"));
            if let Some(c) = &self.capacity {
                for h in &c.hot_targets {
                    out.push_str(&format!(
                        "  {:>5}  conflicting pair{} through {}\n",
                        h.pairs,
                        if h.pairs == 1 { "" } else { "s" },
                        h.target
                    ));
                }
            }
        }
        out.push_str(&format!("\n{bold}Assumptions{reset}\n"));
        for a in self.assumptions() {
            out.push_str(&format!("  {dim}·{reset} {}\n", a.replace('`', "")));
        }
        if self.skipped > 0 {
            out.push_str(&format!(
                "  {dim}·{reset} {} landing{} without a footprint left out.\n",
                self.skipped,
                if self.skipped == 1 { "" } else { "s" }
            ));
        }
        out
    }

    /// The report as Markdown.
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# Sheltie ROI report: {}\n\n**{}.**\n\n| | One at a time | Sheltie |\n| --- | ---: | ---: |\n",
            self.repository,
            self.headline()
        );
        for (label, a, b) in self.rows() {
            out.push_str(&format!("| {label} | {a} | **{b}** |\n"));
        }
        out.push_str(&format!(
            "\n## With Sheltie, of {} changes\n\n",
            self.replay.changes
        ));
        for (n, what) in self.decision_lines() {
            out.push_str(&format!("- {n} {what}\n"));
        }
        if let Some(line) = self.capacity_line() {
            out.push_str(&format!("\n## Parallelism\n\n{line}.\n"));
            if let Some(c) = &self.capacity
                && !c.hot_targets.is_empty()
            {
                out.push_str("\nMost conflicts go through:\n\n");
                for h in &c.hot_targets {
                    out.push_str(&format!("- `{}`: {} pairs\n", h.target, h.pairs));
                }
            }
        }
        out.push_str("\n## Assumptions\n\n");
        for a in self.assumptions() {
            out.push_str(&format!("- {a}\n"));
        }
        if self.skipped > 0 {
            out.push_str(&format!(
                "- {} landings without a footprint left out.\n",
                self.skipped
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use proptest::prelude::*;

    fn at(minutes: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    fn change(minute: i64, writes: &[&str], reads: &[&str]) -> HistoricalChange {
        let set = |v: &[&str]| v.iter().map(|s| TargetId::new(*s)).collect::<BTreeSet<_>>();
        let writes = set(writes);
        let mut reads = set(reads);
        reads.extend(writes.iter().cloned());
        HistoricalChange {
            ready_at: at(minute),
            footprint: Footprint {
                writes,
                reads,
                ..Footprint::default()
            },
        }
    }

    #[test]
    fn too_little_history_gives_no_report() {
        let few: Vec<_> = (0..MIN_CHANGES as i64 - 1)
            .map(|i| change(i, &["a"], &[]))
            .collect();
        assert_eq!(replay(&few, 600), None);
        let enough: Vec<_> = (0..MIN_CHANGES as i64)
            .map(|i| change(i, &["a"], &[]))
            .collect();
        assert_eq!(replay(&enough, 0), None);
    }

    #[test]
    fn independent_changes_land_at_once() {
        // Ten independent changes a minute apart; CI takes 10 minutes.
        let all: Vec<_> = (0..10)
            .map(|i| change(i, &[&format!("t{i}")], &[]))
            .collect();
        let r = replay(&all, 600).unwrap();
        assert_eq!(r.sheltie.ci_runs, 0);
        assert_eq!(r.sheltie.total_wait_seconds, 0);
        assert_eq!(r.decisions.landed_at_once, 10);
        // The serial queue lands one every 10 minutes: waits 10, 19, 28 … 91 minutes.
        assert_eq!(r.serial.ci_runs, 10);
        assert_eq!(
            r.serial.total_wait_seconds,
            (0..10).map(|i| (10 + 9 * i) * 60).sum::<i64>()
        );
        assert_eq!(r.ci_runs_saved(), 10);
        assert!(r.wait_saved_seconds() > 0);
    }

    #[test]
    fn a_writer_and_a_reader_ready_together_are_built_once() {
        let mut all: Vec<_> = (0..8)
            .map(|i| change(i * 100, &[&format!("t{i}")], &[]))
            .collect();
        // A writer of `api` and a reader of it become ready together: one combined build.
        all.push(change(1000, &["api"], &[]));
        all.push(change(1000, &["ui"], &["api"]));
        let r = replay(&all, 600).unwrap();
        assert_eq!(r.decisions.built_together, 2);
        assert_eq!(r.sheltie.ci_runs, 1);
        assert_eq!(r.sheltie.total_wait_seconds, 2 * 600);
    }

    #[test]
    fn same_target_goes_to_the_merge_engine_and_followers_wait() {
        let mut all: Vec<_> = (0..8)
            .map(|i| change(i * 100, &[&format!("t{i}")], &[]))
            .collect();
        all.push(change(1000, &["core"], &[]));
        all.push(change(1000, &["core"], &[]));
        // Ready while the merge is in flight and conflicting with it: waits, then is rebuilt.
        all.push(change(1005, &["core"], &[]));
        let r = replay(&all, 600).unwrap();
        assert_eq!(r.decisions.merge_engine, 3);
        // One run for the pair, one for the follower after the pair lands at 1010.
        assert_eq!(r.sheltie.ci_runs, 2);
        let follower_wait = (1020 - 1005) * 60;
        assert_eq!(r.sheltie.total_wait_seconds, 2 * 600 + follower_wait);
    }

    #[test]
    fn report_renders_headline_rows_and_assumptions() {
        let all: Vec<_> = (0..30)
            .map(|i| change(i, &[&format!("t{}", i % 25)], &[]))
            .collect();
        let r = replay(&all, 900).unwrap();
        let report = RoiReport {
            repository: "acme/widgets".into(),
            branch: "main".into(),
            from: at(0),
            to: at(29),
            footprints: "predicted from changed files".into(),
            skipped: 2,
            capacity: crate::capacity::capacity(
                &all.iter().map(|c| c.footprint.clone()).collect::<Vec<_>>(),
                0.8,
            ),
            replay: r,
        };
        let text = report.to_text(false);
        assert!(text.contains("less waiting and"), "{text}");
        assert!(text.contains("CI takes 15 min per run"));
        assert!(text.contains("2 landings without a footprint left out"));
        assert!(text.contains("Parallelism"));
        let md = report.to_markdown();
        assert!(md.starts_with("# Sheltie ROI report: acme/widgets"));
        assert!(md.contains("| Extra CI runs | 30 |"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_words(40), "40 s");
        assert_eq!(duration_words(12 * 60), "12 min");
        assert_eq!(duration_words(3 * 3600 + 20 * 60), "3 h 20 min");
        assert_eq!(duration_words(28 * 3600), "1 d 4 h");
        assert_eq!(duration_words(-90 * 60), "−1 h 30 min");
    }

    proptest! {
        #[test]
        fn every_change_lands_and_sheltie_never_needs_more_runs(
            raw in proptest::collection::vec((0i64..600, 0u8..6, 0u8..6), MIN_CHANGES..40),
            ci in 60i64..3_600,
        ) {
            let all: Vec<_> = raw.iter().map(|(m, w, r)| change(*m, &[&format!("t{w}")], &[&format!("t{r}")])).collect();
            let r = replay(&all, ci).unwrap();
            let d = r.decisions;
            prop_assert_eq!(d.landed_at_once + d.reverified + d.built_together + d.merge_engine, all.len() as u64);
            prop_assert!(r.sheltie.ci_runs <= r.serial.ci_runs);
            prop_assert!(r.sheltie.total_wait_seconds >= 0);
            prop_assert!(r.sheltie.median_wait_seconds <= r.sheltie.p90_wait_seconds);
        }
    }
}
