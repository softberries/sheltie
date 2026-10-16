use proptest::prelude::*;

use super::*;

fn set(names: &[&str]) -> BTreeSet<TargetId> {
    names.iter().map(|n| TargetId::new(*n)).collect()
}

fn cand(key: u32, writes: &[&str], reads: &[&str]) -> Candidate<u32> {
    let mut reads = set(reads);
    reads.extend(set(writes));
    Candidate {
        key,
        priority: 0,
        waited_minutes: 0,
        footprint: Footprint {
            writes: set(writes),
            reads,
            tests: BTreeSet::new(),
            leaky: false,
        },
        landed_writes: Some(BTreeSet::new()),
    }
}

#[test]
fn independent_changes_land_together() {
    let plan = plan_epoch(&[
        cand(1, &["a"], &[]),
        cand(2, &["b"], &[]),
        cand(3, &["c"], &[]),
    ]);
    for k in 1..=3 {
        assert_eq!(
            plan[&k],
            Decision::LandNow {
                batch: vec![1, 2, 3]
            }
        );
    }
}

#[test]
fn textual_conflicts_go_to_the_merge_engine() {
    let plan = plan_epoch(&[
        cand(1, &["a"], &[]),
        cand(2, &["a"], &[]),
        cand(3, &["c"], &[]),
    ]);
    assert_eq!(
        plan[&1],
        Decision::MergeEngine {
            with: vec![2],
            landed: false
        }
    );
    assert_eq!(
        plan[&2],
        Decision::MergeEngine {
            with: vec![1],
            landed: false
        }
    );
    assert_eq!(plan[&3], Decision::LandNow { batch: vec![3] });
}

#[test]
fn semantic_groups_become_chains_in_priority_order() {
    let mut low = cand(1, &["core"], &[]);
    low.priority = 1;
    let mut high = cand(2, &["api"], &["core"]);
    high.priority = 5;
    let plan = plan_epoch(&[low, high]);
    assert_eq!(
        plan[&2],
        Decision::SpeculativeChain {
            chain: vec![2, 1],
            position: 0
        }
    );
    assert_eq!(
        plan[&1],
        Decision::SpeculativeChain {
            chain: vec![2, 1],
            position: 1
        }
    );
}

#[test]
fn waiting_raises_priority() {
    let mut old = cand(1, &["core"], &[]);
    old.waited_minutes = 120;
    let mut new = cand(2, &["api"], &["core"]);
    new.priority = 100;
    let plan = plan_epoch(&[old, new]);
    assert_eq!(
        plan[&1],
        Decision::SpeculativeChain {
            chain: vec![1, 2],
            position: 0
        }
    );
}

#[test]
fn large_groups_are_batches() {
    // A star: every change reads "hub", one change writes it.
    let mut cands = vec![cand(0, &["hub"], &[])];
    for k in 1..=MAX_CHAIN as u32 {
        cands.push(cand(k, &[&format!("t{k}")], &["hub"]));
    }
    let plan = plan_epoch(&cands);
    assert!(matches!(&plan[&0], Decision::GroupBatch { group } if group.len() == MAX_CHAIN + 1));
}

#[test]
fn landed_writes_decide_reverify_or_merge() {
    let mut reads_landed = cand(1, &["a"], &["shared"]);
    reads_landed.landed_writes = Some(set(&["shared"]));
    let mut writes_landed = cand(2, &["b"], &[]);
    writes_landed.landed_writes = Some(set(&["b"]));
    let mut unknown = cand(3, &["c"], &[]);
    unknown.landed_writes = None;
    let plan = plan_epoch(&[reads_landed, writes_landed, unknown]);
    assert_eq!(
        plan[&1],
        Decision::Reverify {
            targets: set(&["shared"])
        }
    );
    assert_eq!(
        plan[&2],
        Decision::MergeEngine {
            with: vec![],
            landed: true
        }
    );
    assert_eq!(
        plan[&3],
        Decision::Reverify {
            targets: BTreeSet::new()
        }
    );
}

#[test]
fn labels_are_complete() {
    let all = [
        Decision::<u32>::LandNow { batch: vec![] }.label(),
        Decision::<u32>::Reverify {
            targets: BTreeSet::new(),
        }
        .label(),
        Decision::<u32>::MergeEngine {
            with: vec![],
            landed: false,
        }
        .label(),
        Decision::<u32>::SpeculativeChain {
            chain: vec![],
            position: 0,
        }
        .label(),
        Decision::<u32>::GroupBatch { group: vec![] }.label(),
    ];
    assert_eq!(all, DECISION_LABELS);
}

#[test]
fn summaries_read_well() {
    let land = Decision::LandNow {
        batch: vec![1, 2, 3],
    };
    assert_eq!(land.summary(&2), "would land now, together with 1, 3");
    assert_eq!(
        Decision::LandNow { batch: vec![2] }.summary(&2),
        "would land now"
    );
    let chain = Decision::SpeculativeChain {
        chain: vec![4, 7],
        position: 1,
    };
    assert_eq!(
        chain.summary(&7),
        "would be built in a speculative chain, 2 of 2 (4 → 7)"
    );
    let merge = Decision::MergeEngine {
        with: vec![9],
        landed: false,
    };
    assert_eq!(
        merge.summary(&1),
        "writes the same targets as 9; would go to the merge engine"
    );
    let mapped = merge.map_keys(|k| format!("#{k}"));
    assert_eq!(
        mapped,
        Decision::MergeEngine {
            with: vec!["#9".to_owned()],
            landed: false
        }
    );
}

/// Random candidates over a small alphabet of targets, so conflicts are common.
fn arb_candidates() -> impl Strategy<Value = Vec<Candidate<u32>>> {
    let targets = || prop::collection::btree_set("[a-h]", 0..3);
    prop::collection::vec(
        (
            targets(),
            targets(),
            -5i64..5,
            0i64..10,
            prop::option::weighted(0.8, targets()),
        ),
        0..12,
    )
    .prop_map(|items| {
        items
            .into_iter()
            .enumerate()
            .map(|(i, (writes, reads, priority, waited, landed))| {
                let writes: BTreeSet<TargetId> = writes.into_iter().map(TargetId::new).collect();
                let mut reads: BTreeSet<TargetId> = reads.into_iter().map(TargetId::new).collect();
                reads.extend(writes.iter().cloned());
                Candidate {
                    key: i as u32,
                    priority,
                    waited_minutes: waited,
                    footprint: Footprint {
                        writes,
                        reads,
                        tests: BTreeSet::new(),
                        leaky: false,
                    },
                    landed_writes: landed.map(|l| l.into_iter().map(TargetId::new).collect()),
                }
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn every_candidate_gets_exactly_one_decision(cands in arb_candidates()) {
        let plan = plan_epoch(&cands);
        let keys: BTreeSet<u32> = cands.iter().map(|c| c.key).collect();
        prop_assert_eq!(plan.keys().copied().collect::<BTreeSet<_>>(), keys);
    }

    #[test]
    fn changes_landing_together_commute(cands in arb_candidates()) {
        let plan = plan_epoch(&cands);
        let landing: Vec<&Candidate<u32>> = cands.iter().filter(|c| matches!(plan[&c.key], Decision::LandNow { .. })).collect();
        for (i, a) in landing.iter().enumerate() {
            for b in &landing[i + 1..] {
                prop_assert_eq!(a.footprint.relation(&b.footprint), Relation::Independent);
            }
        }
    }

    #[test]
    fn landing_now_means_nothing_landed_touches_it(cands in arb_candidates()) {
        let plan = plan_epoch(&cands);
        for c in &cands {
            if matches!(plan[&c.key], Decision::LandNow { .. }) {
                let landed = c.landed_writes.as_ref().expect("unknown landed writes never land now");
                prop_assert!(landed.is_disjoint(&c.footprint.reads));
            }
        }
    }

    #[test]
    fn textual_conflicts_never_land_or_chain_together(cands in arb_candidates()) {
        let plan = plan_epoch(&cands);
        for (i, a) in cands.iter().enumerate() {
            for b in &cands[i + 1..] {
                if a.footprint.relation(&b.footprint) == Relation::Textual {
                    let merging = |c: &Candidate<u32>| matches!(plan[&c.key], Decision::MergeEngine { .. });
                    prop_assert!(merging(a) || merging(b));
                }
            }
        }
    }

    #[test]
    fn order_of_input_does_not_matter(cands in arb_candidates()) {
        let mut reversed = cands.clone();
        reversed.reverse();
        prop_assert_eq!(plan_epoch(&cands), plan_epoch(&reversed));
    }
}
