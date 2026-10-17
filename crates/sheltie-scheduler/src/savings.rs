//! What Sheltie saves in a repository, for the dashboard.
//!
//! Two kinds of saving, kept apart because they are not the same time:
//!
//! - **Waiting time** (wall clock): how much sooner pull requests landed than they would have
//!   in a one-at-a-time merge queue. The queue is simulated from the real ready times: each
//!   pull request lands at `max(ready, previous landing) + CI time`, with the median CI time
//!   measured on the default branch. Sheltie's actual landing times are measured.
//! - **Author time**: conflicts Sheltie resolved without the author, times the minutes a manual
//!   fix takes. The minutes are an assumption, configurable per repository.
//!
//! See "Time saved" in `docs/dashboard.md`.

use chrono::{DateTime, Utc};

/// Fewer landings than this are too few to estimate from.
pub const MIN_LANDINGS: usize = 10;
/// Assumed author minutes per conflict resolved by the merge engine (tiers 1-3).
pub const DEFAULT_MINUTES_PER_CONFLICT: u32 = 15;
/// Assumed author minutes per change re-derived from its intent (tier 4).
pub const DEFAULT_MINUTES_PER_REPLAY: u32 = 30;

/// A change Sheltie landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Landing {
    /// When it became ready (approved, CI green).
    pub ready_at: DateTime<Utc>,
    /// When it landed.
    pub landed_at: DateTime<Utc>,
}

/// What the estimate is computed from, for one repository and period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavingsInput {
    /// Changes Sheltie landed in the period.
    pub landings: Vec<Landing>,
    /// Measured CI durations on the default branch, in seconds.
    pub ci_seconds: Vec<i64>,
    /// Landed changes whose conflict the merge engine resolved (tiers 1-3).
    pub conflicts_resolved: u32,
    /// Landed changes re-derived from their intent (tier 4).
    pub replays: u32,
    /// Bad commits Sheltie reverted.
    pub reverts: u32,
    /// Assumed minutes per manual conflict fix.
    pub minutes_per_conflict: u32,
    /// Assumed minutes per manual rewrite of a semantically conflicting change.
    pub minutes_per_replay: u32,
}

/// The estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Savings {
    /// Changes landed in the period.
    pub landings: usize,
    /// Median CI time on the default branch, in seconds, if any was measured.
    pub ci_seconds: Option<i64>,
    /// Median time from ready to landed with Sheltie, in seconds.
    pub median_actual_seconds: Option<i64>,
    /// Median time from ready to landed in the simulated one-at-a-time queue, in seconds.
    pub median_serial_seconds: Option<i64>,
    /// Waiting time saved against that queue, summed over the landings, in seconds. Negative
    /// when Sheltie was slower. `None` without enough data or a CI time.
    pub wait_saved_seconds: Option<i64>,
    /// Conflicts resolved by the merge engine.
    pub conflicts_resolved: u32,
    /// Changes re-derived from their intent.
    pub replays: u32,
    /// Bad commits reverted.
    pub reverts: u32,
    /// Author minutes saved under the assumed minutes per fix.
    pub author_minutes_saved: u64,
    /// The assumed minutes per conflict.
    pub minutes_per_conflict: u32,
    /// The assumed minutes per replay.
    pub minutes_per_replay: u32,
}

impl Savings {
    /// Whether there were enough landings for the waiting-time estimate.
    pub fn enough_data(&self) -> bool {
        self.landings >= MIN_LANDINGS
    }
}

/// Waiting times (ready to landed, seconds) in a one-at-a-time queue where each change needs
/// one CI run of `ci_seconds` on top of the previous landing. In order of `ready`.
pub fn serial_queue_waits(ready: &[DateTime<Utc>], ci_seconds: i64) -> Vec<i64> {
    let mut order: Vec<DateTime<Utc>> = ready.to_vec();
    order.sort_unstable();
    let ci = chrono::Duration::seconds(ci_seconds.max(0));
    let mut previous: Option<DateTime<Utc>> = None;
    order
        .iter()
        .map(|&r| {
            let start = previous.map_or(r, |p| p.max(r));
            let landed = start + ci;
            previous = Some(landed);
            (landed - r).num_seconds()
        })
        .collect()
}

fn median(values: &[i64]) -> Option<i64> {
    let mut v = values.to_vec();
    v.sort_unstable();
    v.get(v.len().checked_sub(1)? / 2).copied()
}

/// Computes the estimate.
pub fn estimate(input: &SavingsInput) -> Savings {
    let actual: Vec<i64> = input
        .landings
        .iter()
        .map(|l| (l.landed_at - l.ready_at).num_seconds().max(0))
        .collect();
    let ci = median(&input.ci_seconds);
    let ready: Vec<DateTime<Utc>> = input.landings.iter().map(|l| l.ready_at).collect();
    let serial = ci.map(|ci| serial_queue_waits(&ready, ci));
    let enough = input.landings.len() >= MIN_LANDINGS;
    let wait_saved_seconds = match (&serial, enough) {
        (Some(s), true) => Some(s.iter().sum::<i64>() - actual.iter().sum::<i64>()),
        _ => None,
    };
    Savings {
        landings: input.landings.len(),
        ci_seconds: ci,
        median_actual_seconds: median(&actual),
        median_serial_seconds: serial.as_deref().and_then(median),
        wait_saved_seconds,
        conflicts_resolved: input.conflicts_resolved,
        replays: input.replays,
        reverts: input.reverts,
        author_minutes_saved: u64::from(input.conflicts_resolved)
            * u64::from(input.minutes_per_conflict)
            + u64::from(input.replays) * u64::from(input.minutes_per_replay),
        minutes_per_conflict: input.minutes_per_conflict,
        minutes_per_replay: input.minutes_per_replay,
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use proptest::prelude::*;

    use super::*;

    fn at(s: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + s, 0).unwrap()
    }

    #[test]
    fn a_burst_queues_up_one_ci_run_at_a_time() {
        // Three changes ready together, 10-minute CI: they land at 10, 20 and 30 minutes.
        assert_eq!(
            serial_queue_waits(&[at(0), at(0), at(0)], 600),
            vec![600, 1200, 1800]
        );
        // Spread out, nobody waits for anyone.
        assert_eq!(serial_queue_waits(&[at(0), at(3600)], 600), vec![600, 600]);
    }

    #[test]
    fn estimate_compares_with_the_serial_queue() {
        // Ten changes ready at once; Sheltie lands them all 2 minutes later, CI takes 10.
        let landings: Vec<Landing> = (0..10)
            .map(|_| Landing {
                ready_at: at(0),
                landed_at: at(120),
            })
            .collect();
        let s = estimate(&SavingsInput {
            landings,
            ci_seconds: vec![500, 600, 700],
            conflicts_resolved: 3,
            replays: 1,
            reverts: 1,
            minutes_per_conflict: 15,
            minutes_per_replay: 30,
        });
        assert!(s.enough_data());
        assert_eq!(s.ci_seconds, Some(600));
        assert_eq!(s.median_actual_seconds, Some(120));
        // Serial: 600, 1200, ..., 6000 → median 3000 (fifth of ten), sum 33000.
        assert_eq!(s.median_serial_seconds, Some(3000));
        assert_eq!(s.wait_saved_seconds, Some(33_000 - 1_200));
        assert_eq!(s.author_minutes_saved, 3 * 15 + 30);
    }

    #[test]
    fn too_little_data_gives_no_waiting_estimate() {
        let s = estimate(&SavingsInput {
            landings: vec![Landing {
                ready_at: at(0),
                landed_at: at(60),
            }],
            ci_seconds: vec![600],
            conflicts_resolved: 0,
            replays: 0,
            reverts: 0,
            minutes_per_conflict: 15,
            minutes_per_replay: 30,
        });
        assert!(!s.enough_data());
        assert_eq!(s.wait_saved_seconds, None);
        let no_ci = estimate(&SavingsInput {
            ci_seconds: vec![],
            landings: vec![
                Landing {
                    ready_at: at(0),
                    landed_at: at(60)
                };
                12
            ],
            ..estimate_input()
        });
        assert_eq!(no_ci.wait_saved_seconds, None, "no CI time measured");
    }

    fn estimate_input() -> SavingsInput {
        SavingsInput {
            landings: vec![],
            ci_seconds: vec![],
            conflicts_resolved: 0,
            replays: 0,
            reverts: 0,
            minutes_per_conflict: DEFAULT_MINUTES_PER_CONFLICT,
            minutes_per_replay: DEFAULT_MINUTES_PER_REPLAY,
        }
    }

    proptest! {
        #[test]
        fn serial_waits_are_at_least_one_ci_run(
            ready in prop::collection::vec(0i64..100_000, 0..30),
            ci in 0i64..5_000,
        ) {
            let ready: Vec<_> = ready.into_iter().map(at).collect();
            let waits = serial_queue_waits(&ready, ci);
            prop_assert_eq!(waits.len(), ready.len());
            prop_assert!(waits.iter().all(|w| *w >= ci));
        }

        #[test]
        fn a_longer_ci_never_shortens_the_serial_queue(
            ready in prop::collection::vec(0i64..100_000, 1..30),
            ci in 0i64..5_000,
            extra in 0i64..5_000,
        ) {
            let ready: Vec<_> = ready.into_iter().map(at).collect();
            let a: i64 = serial_queue_waits(&ready, ci).iter().sum();
            let b: i64 = serial_queue_waits(&ready, ci + extra).iter().sum();
            prop_assert!(b >= a);
        }

        #[test]
        fn landing_right_after_ready_never_loses_to_the_queue(
            ready in prop::collection::vec(0i64..100_000, MIN_LANDINGS..30),
            ci in 1i64..5_000,
        ) {
            // Parallel landing within one CI run of ready is at least as fast as serial.
            let landings: Vec<Landing> = ready
                .iter()
                .map(|&r| Landing { ready_at: at(r), landed_at: at(r + ci) })
                .collect();
            let s = estimate(&SavingsInput { landings, ci_seconds: vec![ci], ..estimate_input() });
            prop_assert!(s.wait_saved_seconds.unwrap() >= 0);
        }
    }
}
