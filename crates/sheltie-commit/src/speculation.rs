//! Speculative chains and group-tested batches.
//!
//! A group of changes with semantic conflicts is ordered by priority and built as prefixes
//! on the current head: A, A+B, A+B+C, … Each prefix is a branch the repository's CI runs on.
//! The longest green prefix lands with one fast-forward; the change whose addition turned a
//! prefix red is returned. Small groups build every prefix (a speculative chain); large ones
//! build exponentially spaced prefixes (1, 2, 4, …, n), which tests the batch as a whole and
//! bisects on failure, as the design's group-tested batches do.

use crate::MAX_CHAIN;

/// What CI says about one prefix build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// Passed.
    Green,
    /// Failed.
    Red,
    /// Still running.
    Pending,
}

/// The outcome of a round of prefix builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Not all the builds that matter have finished.
    Wait,
    /// Land the first `land` changes (may be 0); return the change at index `culprit` if set.
    /// Changes after the landed prefix that were not returned go back to the queue.
    Settle {
        /// How many changes of the group land.
        land: usize,
        /// The change whose addition broke the build, if pinned down.
        culprit: Option<usize>,
    },
}

/// The prefix lengths to build for a group of `n` changes, ascending.
pub fn prefix_lengths(n: usize) -> Vec<usize> {
    if n <= MAX_CHAIN {
        return (1..=n).collect();
    }
    let mut lengths: Vec<usize> = std::iter::successors(Some(1usize), |l| l.checked_mul(2))
        .take_while(|l| *l < n)
        .collect();
    lengths.push(n);
    lengths
}

/// Decides a round from the results of the builds of `lengths` (same order).
///
/// The first red prefix L and the longest green prefix G below it bound the culprit: if L is
/// G + 1, the change at index G broke the build and is returned; otherwise it is somewhere in
/// (G, L] and those changes are retried in the next round. Either way the first G changes land.
pub fn decide(lengths: &[usize], results: &[Build]) -> Verdict {
    let builds: Vec<(usize, Build)> = lengths
        .iter()
        .copied()
        .zip(results.iter().copied())
        .collect();
    match builds.iter().position(|(_, b)| *b == Build::Red) {
        None => {
            if builds.iter().all(|(_, b)| *b == Build::Green) {
                let land = builds.iter().map(|(l, _)| *l).max().unwrap_or(0);
                Verdict::Settle {
                    land,
                    culprit: None,
                }
            } else {
                Verdict::Wait
            }
        }
        Some(red_at) => {
            let below = &builds[..red_at];
            if below.iter().any(|(_, b)| *b == Build::Pending) {
                return Verdict::Wait;
            }
            let red_len = builds[red_at].0;
            let green_len = below.iter().map(|(l, _)| *l).max().unwrap_or(0);
            let culprit = (red_len == green_len + 1).then_some(green_len);
            Verdict::Settle {
                land: green_len,
                culprit,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use Build::*;

    #[test]
    fn lengths() {
        assert_eq!(prefix_lengths(3), [1, 2, 3]);
        assert_eq!(prefix_lengths(8), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(prefix_lengths(9), [1, 2, 4, 8, 9]);
        assert_eq!(prefix_lengths(16), [1, 2, 4, 8, 16]);
    }

    #[test]
    fn chain_verdicts() {
        let l = [1, 2, 3];
        assert_eq!(
            decide(&l, &[Green, Green, Green]),
            Verdict::Settle {
                land: 3,
                culprit: None
            }
        );
        assert_eq!(
            decide(&l, &[Green, Red, Red]),
            Verdict::Settle {
                land: 1,
                culprit: Some(1)
            }
        );
        assert_eq!(
            decide(&l, &[Red, Pending, Pending]),
            Verdict::Settle {
                land: 0,
                culprit: Some(0)
            }
        );
        assert_eq!(decide(&l, &[Pending, Red, Pending]), Verdict::Wait);
        assert_eq!(decide(&l, &[Green, Green, Pending]), Verdict::Wait);
    }

    #[test]
    fn batch_narrows_without_naming_a_culprit() {
        // 16 changes: 1, 2, 4 green; 8 red. The culprit is one of changes 4..8: land 4, retry.
        let l = prefix_lengths(16);
        let r = [Green, Green, Green, Red, Red];
        assert_eq!(
            decide(&l, &r),
            Verdict::Settle {
                land: 4,
                culprit: None
            }
        );
    }

    /// Simulates CI: a prefix is red iff it contains a bad change.
    fn simulate(n: usize, bad: &[bool]) -> Vec<Build> {
        prefix_lengths(n)
            .iter()
            .map(|&l| {
                if bad[..l].iter().any(|b| *b) {
                    Red
                } else {
                    Green
                }
            })
            .collect()
    }

    proptest! {
        #[test]
        fn every_finished_round_makes_progress(bad in prop::collection::vec(prop::bool::weighted(0.3), 1..20)) {
            let n = bad.len();
            match decide(&prefix_lengths(n), &simulate(n, &bad)) {
                Verdict::Settle { land, culprit } => prop_assert!(land > 0 || culprit.is_some()),
                Verdict::Wait => prop_assert!(false, "all builds finished"),
            }
        }

        #[test]
        fn landed_prefixes_are_clean_and_culprits_are_bad(bad in prop::collection::vec(prop::bool::weighted(0.3), 1..20)) {
            let n = bad.len();
            if let Verdict::Settle { land, culprit } = decide(&prefix_lengths(n), &simulate(n, &bad)) {
                prop_assert!(bad[..land].iter().all(|b| !*b), "landed a bad change");
                if let Some(c) = culprit {
                    prop_assert!(bad[c], "returned a good change");
                    prop_assert_eq!(c, land);
                }
            }
        }

        #[test]
        fn repeated_rounds_land_every_good_change(bad in prop::collection::vec(prop::bool::weighted(0.2), 1..30)) {
            // Run rounds on what is left until the group is empty: every good change lands.
            let mut queue: Vec<(usize, bool)> = bad.iter().copied().enumerate().collect();
            let mut landed = Vec::new();
            for _ in 0..100 {
                if queue.is_empty() { break; }
                let flags: Vec<bool> = queue.iter().map(|(_, b)| *b).collect();
                let Verdict::Settle { land, culprit } = decide(&prefix_lengths(flags.len()), &simulate(flags.len(), &flags)) else {
                    prop_assert!(false, "wait with all builds done");
                    unreachable!()
                };
                landed.extend(queue.drain(..land).map(|(i, _)| i));
                if culprit.is_some() {
                    queue.remove(0);
                }
            }
            let good: Vec<usize> = bad.iter().enumerate().filter(|(_, b)| !**b).map(|(i, _)| i).collect();
            prop_assert_eq!(landed, good);
        }
    }
}
