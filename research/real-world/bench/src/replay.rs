//! `sheltie_scheduler::roi::replay`, copied, with each change based on the branch as of when
//! its own CI ran (`based_at`) instead of when it became ready.
#![allow(clippy::all)]
use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use sheltie_core::{Footprint, Relation, TargetId};
use sheltie_scheduler::roi::{Decisions, QueueOutcome, Replay};
use sheltie_scheduler::savings::serial_queue_waits;
use sheltie_scheduler::{Candidate, Decision, plan_epoch};

pub struct Change {
    pub ready_at: DateTime<Utc>,
    pub based_at: DateTime<Utc>,
    pub footprint: Footprint,
}

fn percentile(sorted: &[i64], p: usize) -> Option<i64> {
    if sorted.is_empty() { return None; }
    let i = (sorted.len() * p).div_ceil(100).saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[i])
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
pub fn replay(changes: &[Change], ci_seconds: i64) -> Option<(Replay, Vec<(i64, &'static str)>)> {
    if changes.len() < 2 || ci_seconds <= 0 {
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
                let since = changes[p].based_at;
                let landed_writes: BTreeSet<TargetId> = landed_log
                    .iter()
                    .filter(|(at, _)| *at > since)
                    .flat_map(|(_, m)| changes[*m].footprint.writes.iter().cloned())
                    .collect();
                Candidate {
                    key: p,
                    priority: 0,
                    waited_minutes: (now - changes[p].ready_at).num_minutes(),
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
    let per: Vec<(i64, &'static str)> = waits.iter().copied().zip(final_decision.iter().copied()).collect();
    Some((Replay {
        changes: n,
        ci_seconds,
        serial,
        sheltie: outcome(waits, ci_runs),
        decisions,
        known_base: n,
    }, per))
}

