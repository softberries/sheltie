//! Shadow-mode analysis: suspected footprint misses and the prediction-vs-reality report.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::Serialize;
use sheltie_core::{Footprint, Relation, TargetId};

/// A change that landed in a window between a green and a red commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedInWindow<K> {
    /// The change.
    pub key: K,
    /// Its footprint, if one was uploaded.
    pub footprint: Option<Footprint>,
}

/// Two changes Sheltie judged independent that landed together before a red commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SuspectedMiss<K> {
    /// The change with the smaller key.
    pub a: K,
    /// The other change.
    pub b: K,
    /// What `a` wrote.
    pub writes_a: BTreeSet<TargetId>,
    /// What `b` wrote.
    pub writes_b: BTreeSet<TargetId>,
}

/// Pairs of changes in a red window whose footprints say they commute.
///
/// When the default branch turns red after several changes landed, a pair judged independent
/// is a candidate footprint miss: either the pair broke the build together, or one of them is
/// simply broken (bisect in M4 tells which). A window with a single change has no pairs.
/// Changes without a footprint cannot be judged and are skipped.
pub fn suspected_misses<K: Ord + Clone>(landed: &[LandedInWindow<K>]) -> Vec<SuspectedMiss<K>> {
    let mut known: Vec<(&K, &Footprint)> = landed
        .iter()
        .filter_map(|l| l.footprint.as_ref().map(|f| (&l.key, f)))
        .collect();
    known.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = Vec::new();
    for (i, (ka, fa)) in known.iter().enumerate() {
        for (kb, fb) in &known[i + 1..] {
            if fa.relation(fb) == Relation::Independent {
                out.push(SuspectedMiss {
                    a: (*ka).clone(),
                    b: (*kb).clone(),
                    writes_a: fa.writes.clone(),
                    writes_b: fb.writes.clone(),
                });
            }
        }
    }
    out
}

/// A pull request that merged during the report window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedChange {
    /// Pull request number.
    pub pr: u64,
    /// When Sheltie considered it ready, if it ever did.
    pub ready_at: Option<DateTime<Utc>>,
    /// When it merged.
    pub merged_at: DateTime<Utc>,
    /// Sheltie's last prediction before the merge, if any (a decision label).
    pub predicted: Option<String>,
    /// When Sheltie first predicted "land now", if it did before the merge.
    pub first_land_now: Option<DateTime<Utc>>,
    /// Targets it wrote, if its footprint is known.
    pub writes: Option<BTreeSet<TargetId>>,
}

/// Everything the report is computed from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportInput {
    /// Repository `owner/name`.
    pub repository: String,
    /// Start of the window.
    pub since: DateTime<Utc>,
    /// Pull requests merged in the window.
    pub merged: Vec<MergedChange>,
    /// Sizes of "land now" batches predicted in the window, one per change of prediction.
    pub land_now_batches: Vec<usize>,
    /// Default-branch commits whose CI was red in the window.
    pub red_commits: usize,
    /// Suspected misses detected in the window.
    pub misses: Vec<SuspectedMiss<u64>>,
}

/// The shadow-mode report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShadowReport {
    /// Repository `owner/name`.
    pub repository: String,
    /// Start of the window.
    pub since: DateTime<Utc>,
    /// Pull requests merged.
    pub merged: usize,
    /// Of those, how many Sheltie had judged ready before they merged.
    pub merged_ready: usize,
    /// Median minutes from ready to merged.
    pub ready_to_merged_p50: Option<i64>,
    /// 95th percentile minutes from ready to merged.
    pub ready_to_merged_p95: Option<i64>,
    /// Sheltie's last prediction before each merge, by decision.
    pub predicted_at_merge: BTreeMap<String, usize>,
    /// Minutes between Sheltie saying "land now" and the actual merge, summed: the wait a
    /// parallel queue would have removed.
    pub land_now_wait_minutes: i64,
    /// Average size of a "land now" batch: the parallelism the design calls k.
    pub average_land_now_batch: Option<f64>,
    /// Default-branch commits with red CI.
    pub red_commits: usize,
    /// Suspected footprint misses.
    pub suspected_misses: usize,
    /// Suspected misses per 1,000 merges.
    pub misses_per_1000_merges: Option<f64>,
    /// Targets written by the most merged changes.
    pub hot_targets: Vec<(TargetId, usize)>,
    /// The suspected misses, for inspection.
    pub misses: Vec<SuspectedMiss<u64>>,
}

/// Computes the report.
pub fn build_report(input: &ReportInput) -> ShadowReport {
    let mut waits: Vec<i64> = input
        .merged
        .iter()
        .filter_map(|m| m.ready_at.map(|r| (m.merged_at - r).num_minutes().max(0)))
        .collect();
    waits.sort_unstable();
    let mut predicted_at_merge = BTreeMap::new();
    for m in &input.merged {
        let label = m.predicted.clone().unwrap_or_else(|| "never_ready".into());
        *predicted_at_merge.entry(label).or_insert(0) += 1;
    }
    let land_now_wait_minutes = input
        .merged
        .iter()
        .filter_map(|m| {
            m.first_land_now
                .map(|f| (m.merged_at - f).num_minutes().max(0))
        })
        .sum();
    let mut hot: BTreeMap<TargetId, usize> = BTreeMap::new();
    for writes in input.merged.iter().filter_map(|m| m.writes.as_ref()) {
        for t in writes {
            *hot.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let mut hot_targets: Vec<(TargetId, usize)> = hot.into_iter().filter(|(_, n)| *n > 1).collect();
    hot_targets.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hot_targets.truncate(10);
    let merged = input.merged.len();
    ShadowReport {
        repository: input.repository.clone(),
        since: input.since,
        merged,
        merged_ready: waits.len(),
        ready_to_merged_p50: percentile(&waits, 50),
        ready_to_merged_p95: percentile(&waits, 95),
        predicted_at_merge,
        land_now_wait_minutes,
        average_land_now_batch: (!input.land_now_batches.is_empty()).then(|| {
            input.land_now_batches.iter().sum::<usize>() as f64
                / input.land_now_batches.len() as f64
        }),
        red_commits: input.red_commits,
        suspected_misses: input.misses.len(),
        misses_per_1000_merges: (merged > 0)
            .then(|| input.misses.len() as f64 * 1000.0 / merged as f64),
        hot_targets,
        misses: input.misses.clone(),
    }
}

/// Nearest-rank percentile of sorted values.
fn percentile(sorted: &[i64], p: usize) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (p * sorted.len()).div_ceil(100).max(1);
    sorted.get(rank - 1).copied()
}

impl ShadowReport {
    /// The report as Markdown.
    pub fn to_markdown(&self) -> String {
        let opt = |v: Option<i64>| v.map_or("n/a".to_owned(), |m| format!("{m} min"));
        let mut out = format!(
            "# Sheltie shadow report: {}\n\nSince {}.\n\n| Metric | Value |\n| --- | --- |\n",
            self.repository,
            self.since.format("%Y-%m-%d %H:%M UTC")
        );
        let rows = [
            ("PRs merged", self.merged.to_string()),
            (
                "…of which Sheltie judged ready first",
                self.merged_ready.to_string(),
            ),
            ("Ready → merged, median", opt(self.ready_to_merged_p50)),
            ("Ready → merged, p95", opt(self.ready_to_merged_p95)),
            (
                "Minutes merges waited after \"land now\"",
                self.land_now_wait_minutes.to_string(),
            ),
            (
                "Average \"land now\" batch (k)",
                self.average_land_now_batch
                    .map_or("n/a".into(), |k| format!("{k:.2}")),
            ),
            (
                "Red commits on the default branch",
                self.red_commits.to_string(),
            ),
            (
                "Suspected footprint misses",
                self.suspected_misses.to_string(),
            ),
            (
                "…per 1,000 merges",
                self.misses_per_1000_merges
                    .map_or("n/a".into(), |r| format!("{r:.1}")),
            ),
        ];
        for (name, value) in rows {
            out.push_str(&format!("| {name} | {value} |\n"));
        }
        out.push_str("\n## Prediction at merge time\n\n");
        for (label, n) in &self.predicted_at_merge {
            out.push_str(&format!("- `{label}`: {n}\n"));
        }
        if !self.hot_targets.is_empty() {
            out.push_str("\n## Most-written targets\n\n");
            for (t, n) in &self.hot_targets {
                out.push_str(&format!("- `{t}`: {n} PRs\n"));
            }
        }
        if !self.misses.is_empty() {
            out.push_str("\n## Suspected footprint misses\n\nPairs judged independent that landed together before a red commit.\n\n");
            for m in &self.misses {
                let w = |s: &BTreeSet<TargetId>| {
                    s.iter()
                        .map(|t| format!("`{t}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                out.push_str(&format!(
                    "- #{} ({}) and #{} ({})\n",
                    m.a,
                    w(&m.writes_a),
                    m.b,
                    w(&m.writes_b)
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
        let set = |v: &[&str]| v.iter().map(|t| TargetId::new(*t)).collect::<BTreeSet<_>>();
        let mut r = set(reads);
        r.extend(set(writes));
        Footprint {
            writes: set(writes),
            reads: r,
            tests: BTreeSet::new(),
            leaky: false,
        }
    }

    #[test]
    fn independent_pairs_in_a_red_window_are_suspects() {
        let landed = [
            LandedInWindow {
                key: 3,
                footprint: Some(fp(&["a"], &[])),
            },
            LandedInWindow {
                key: 1,
                footprint: Some(fp(&["b"], &[])),
            },
            LandedInWindow {
                key: 2,
                footprint: Some(fp(&["c"], &["a"])),
            },
            LandedInWindow {
                key: 4,
                footprint: None,
            },
        ];
        let pairs: Vec<(i32, i32)> = suspected_misses(&landed)
            .iter()
            .map(|m| (m.a, m.b))
            .collect();
        // 2 reads what 3 writes (semantic), so that pair was not judged independent.
        assert_eq!(pairs, [(1, 2), (1, 3)]);
    }

    #[test]
    fn a_single_change_is_no_pair() {
        assert!(
            suspected_misses(&[LandedInWindow {
                key: 1,
                footprint: Some(fp(&["a"], &[]))
            }])
            .is_empty()
        );
    }

    #[test]
    fn report_metrics() {
        let t = |m: i64| Utc.timestamp_opt(1_800_000_000 + m * 60, 0).unwrap();
        let merged = |pr,
                      ready: Option<i64>,
                      at,
                      predicted: Option<&str>,
                      land_now: Option<i64>,
                      writes: &[&str]| MergedChange {
            pr,
            ready_at: ready.map(t),
            merged_at: t(at),
            predicted: predicted.map(str::to_owned),
            first_land_now: land_now.map(t),
            writes: Some(writes.iter().map(|w| TargetId::new(*w)).collect()),
        };
        let input = ReportInput {
            repository: "acme/widgets".into(),
            since: t(0),
            merged: vec![
                merged(1, Some(0), 10, Some("land_now"), Some(2), &["core", "a"]),
                merged(2, Some(0), 30, Some("merge_engine"), None, &["core"]),
                merged(3, None, 40, None, None, &["b"]),
            ],
            land_now_batches: vec![1, 3],
            red_commits: 1,
            misses: vec![SuspectedMiss {
                a: 1,
                b: 3,
                writes_a: BTreeSet::new(),
                writes_b: BTreeSet::new(),
            }],
        };
        let r = build_report(&input);
        assert_eq!((r.merged, r.merged_ready), (3, 2));
        assert_eq!(
            (r.ready_to_merged_p50, r.ready_to_merged_p95),
            (Some(10), Some(30))
        );
        assert_eq!(r.land_now_wait_minutes, 8);
        assert_eq!(r.average_land_now_batch, Some(2.0));
        assert_eq!(r.predicted_at_merge.get("never_ready"), Some(&1));
        assert_eq!(r.hot_targets, vec![(TargetId::new("core"), 2)]);
        assert!((r.misses_per_1000_merges.unwrap() - 333.33).abs() < 0.01);
        let md = r.to_markdown();
        assert!(md.contains("| PRs merged | 3 |"), "{md}");
        assert!(md.contains("- #1 () and #3 ()"), "{md}");
    }

    #[test]
    fn percentiles() {
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[5], 95), Some(5));
        assert_eq!(percentile(&[1, 2, 3, 4], 50), Some(2));
    }
}
