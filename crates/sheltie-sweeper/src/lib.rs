//! Sweeps, culprit finding and auto-revert decisions.
//!
//! The repository's CI on each push to the default branch is the sweep. When the branch turns
//! red, [`analyse`] looks at the first-parent commits since the last green one and decides
//! whether the culprit is known, which commit to probe next (bisect), or that it must wait.
//! A test must fail twice to count: a lone red tip is confirmed by running its CI again
//! (probe), and a red followed by green is treated as flaky and not reverted. This crate is
//! pure; the server does the GitHub calls.
//!
//! See "Sweeps" and "Culprit finding" in `docs/design.md`.

/// What CI says about one commit, reduced to what culprit finding needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ci {
    /// Passed.
    Green,
    /// Failed or errored.
    Red,
    /// Running, or not run yet.
    Unknown,
}

/// The decision for a red window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Analysis {
    /// The commit at this index broke the branch and the red persisted after it: revert it.
    Culprit(usize),
    /// CI must run on the commit at this index to narrow the search (bisect).
    Probe(usize),
    /// The commit at this index is the newest and the only red one, after green ones: run its
    /// CI again; red again makes it the culprit, green means the first run was flaky.
    Confirm(usize),
    /// Nothing to do yet: CI is still running on a commit that matters, or the red did not
    /// persist (a later commit is green, so it was likely flaky).
    Wait,
    /// No red commit in the window.
    Green,
}

/// Analyses the first-parent commits after the last green commit, oldest first.
///
/// The first red commit E is the culprit when every commit before it in the window is known
/// green (so E is the first that broke) and a later commit is red as well (the failure
/// persisted, which rules out a one-off flaky run). When E is the newest commit, its CI is run
/// again instead ([`Analysis::Confirm`]). Commits before E whose CI is unknown are bisected:
/// the one in the middle is probed.
pub fn analyse(window: &[Ci]) -> Analysis {
    let Some(first_red) = window.iter().position(|c| *c == Ci::Red) else {
        return Analysis::Green;
    };
    let unknown_before: Vec<usize> = (0..first_red)
        .filter(|&i| window[i] == Ci::Unknown)
        .collect();
    if let Some(&mid) = unknown_before.get(unknown_before.len() / 2) {
        return Analysis::Probe(mid);
    }
    let after = &window[first_red + 1..];
    if after.contains(&Ci::Red) {
        Analysis::Culprit(first_red)
    } else if after.is_empty() {
        Analysis::Confirm(first_red)
    } else {
        Analysis::Wait
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use Ci::*;

    #[test]
    fn cases() {
        assert_eq!(analyse(&[]), Analysis::Green);
        assert_eq!(analyse(&[Green, Green]), Analysis::Green);
        assert_eq!(analyse(&[Red, Red]), Analysis::Culprit(0));
        assert_eq!(analyse(&[Green, Red, Unknown, Red]), Analysis::Culprit(1));
        assert_eq!(
            analyse(&[Red]),
            Analysis::Confirm(0),
            "a lone red tip is run again"
        );
        assert_eq!(analyse(&[Green, Red]), Analysis::Confirm(1));
        assert_eq!(
            analyse(&[Red, Unknown]),
            Analysis::Wait,
            "a later commit will tell"
        );
        assert_eq!(
            analyse(&[Red, Green]),
            Analysis::Wait,
            "went green again: flaky"
        );
        assert_eq!(
            analyse(&[Unknown, Unknown, Unknown, Red, Red]),
            Analysis::Probe(1)
        );
        assert_eq!(analyse(&[Green, Unknown, Red, Red]), Analysis::Probe(1));
    }

    fn arb_window() -> impl Strategy<Value = Vec<Ci>> {
        prop::collection::vec(prop_oneof![Just(Green), Just(Red), Just(Unknown)], 0..12)
    }

    proptest! {
        #[test]
        fn a_culprit_is_red_preceded_only_by_green_and_followed_by_red(w in arb_window()) {
            if let Analysis::Culprit(i) = analyse(&w) {
                prop_assert_eq!(w[i], Red);
                prop_assert!(w[..i].iter().all(|c| *c == Green));
                prop_assert!(w[i + 1..].contains(&Red));
            }
        }

        #[test]
        fn a_confirmation_is_the_newest_commit_and_the_only_red(w in arb_window()) {
            if let Analysis::Confirm(i) = analyse(&w) {
                prop_assert_eq!(i, w.len() - 1);
                prop_assert_eq!(w[i], Red);
                prop_assert!(w[..i].iter().all(|c| *c == Green));
            }
        }

        #[test]
        fn probes_are_unknown_and_before_the_first_red(w in arb_window()) {
            if let Analysis::Probe(i) = analyse(&w) {
                prop_assert_eq!(w[i], Unknown);
                let first_red = w.iter().position(|c| *c == Red).unwrap();
                prop_assert!(i < first_red);
            }
        }

        #[test]
        fn probing_ends_within_the_window_length(w in arb_window(), answers in prop::collection::vec(any::<bool>(), 12)) {
            // Each probe turns one unknown before the first red into a known state, so after
            // at most `len` answers the analysis has a verdict.
            let mut w = w;
            for answer in answers.iter().take(w.len()) {
                match analyse(&w) {
                    Analysis::Probe(i) => w[i] = if *answer { Green } else { Red },
                    _ => break,
                }
            }
            prop_assert!(!matches!(analyse(&w), Analysis::Probe(_)));
        }
    }
}
