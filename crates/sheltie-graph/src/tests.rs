use proptest::prelude::*;

use super::*;

fn id(s: &str) -> TargetId {
    TargetId::new(s)
}

fn path(s: &str) -> RepoPath {
    RepoPath::new(s)
}

/// A small fluent builder for snapshots.
#[derive(Default, Clone)]
struct Builder(GraphSnapshot);

impl Builder {
    fn target(mut self, name: &str, iface: &str, reads: &[(&str, EdgeKind)]) -> Self {
        self.0.targets.insert(
            id(name),
            Target {
                zone: Zone::Group(name.split("::").next().unwrap_or(name).to_owned()),
                iface: IfaceHash(iface.to_owned()),
                public_iface: IfaceHash(format!("pub-{iface}")),
                reads: reads.iter().map(|(t, k)| (id(t), *k)).collect(),
                tests: BTreeSet::from([TestId::new(format!("{name}#tests"))]),
                hermetic: true,
            },
        );
        self.0
            .owners
            .entry(path(&format!("{name}.rs")))
            .or_default()
            .insert(id(name));
        self
    }

    fn leaky(mut self, name: &str) -> Self {
        if let Some(t) = self.0.targets.get_mut(&id(name)) {
            t.hermetic = false;
        }
        self
    }

    fn build(self) -> GraphSnapshot {
        self.0
    }
}

fn changed(paths: &[&str]) -> BTreeSet<RepoPath> {
    paths.iter().map(|p| path(p)).collect()
}

fn ids(names: &[&str]) -> BTreeSet<TargetId> {
    names.iter().map(|n| id(n)).collect()
}

/// a::tax <- a::invoice <- b (another crate, through the public interface of `a`).
fn base() -> Builder {
    Builder::default()
        .target("a::tax", "tax1", &[])
        .target("a::invoice", "inv1", &[("a::tax", EdgeKind::Local)])
        .target("a", "root1", &[("a::invoice", EdgeKind::Local)])
        .target("b", "b1", &[("a", EdgeKind::Public)])
}

#[test]
fn implementation_change_stays_in_its_target() {
    let base = base().build();
    let fp = footprint(&base, &base, &changed(&["a::tax.rs"]), &BTreeSet::new());
    assert_eq!(fp.writes, ids(&["a::tax"]));
    assert_eq!(fp.reads, ids(&["a::tax"]));
    assert_eq!(fp.tests, BTreeSet::from([TestId::new("a::tax#tests")]));
    assert!(!fp.leaky);
}

#[test]
fn an_added_dependency_is_read() {
    let base = base().target("c", "c1", &[]).build();
    // b starts depending on c; c's tests need not run for it.
    let mut head = base.clone();
    head.targets
        .get_mut(&id("b"))
        .unwrap()
        .reads
        .insert(id("c"), EdgeKind::Public);
    let fp = footprint(&base, &head, &changed(&["b.rs"]), &BTreeSet::new());
    assert_eq!(fp.reads, ids(&["b", "c"]));
    assert_eq!(fp.tests, BTreeSet::from([TestId::new("b#tests")]));
    // A new target's dependencies are all added.
    let head = Builder(base.clone())
        .target("d", "d1", &[("a", EdgeKind::Public)])
        .build();
    let fp = footprint(&base, &head, &changed(&["d.rs"]), &BTreeSet::new());
    assert_eq!(fp.reads, ids(&["a", "d"]));
}

#[test]
fn interface_change_reaches_direct_readers_only() {
    let base = base().build();
    // tax's interface changes; invoice reads it but its own interface is unchanged.
    let head = base.clone();
    let mut head = Builder(head).target("a::tax", "tax2", &[]).build();
    head.owners = base.owners.clone();
    let fp = footprint(&base, &head, &changed(&["a::tax.rs"]), &BTreeSet::new());
    assert_eq!(fp.reads, ids(&["a::tax", "a::invoice"]));
}

#[test]
fn interface_change_propagates_through_changed_readers() {
    let base = base().build();
    // invoice re-exports tax types, so its interface changes too; so does the crate's
    // public surface, which crate b reads.
    let mut head = base.clone();
    for (name, iface) in [("a::tax", "tax2"), ("a::invoice", "inv2"), ("a", "root2")] {
        let t = head.targets.get_mut(&id(name)).unwrap();
        t.iface = IfaceHash(iface.into());
        t.public_iface = IfaceHash(format!("pub-{iface}"));
    }
    let fp = footprint(&base, &head, &changed(&["a::tax.rs"]), &BTreeSet::new());
    assert_eq!(fp.reads, ids(&["a::tax", "a::invoice", "a", "b"]));
}

#[test]
fn public_readers_ignore_local_only_changes() {
    let base = base().build();
    let mut head = base.clone();
    head.targets.get_mut(&id("a")).unwrap().iface = IfaceHash("root2".into());
    let fp = footprint(&base, &head, &changed(&["a.rs"]), &BTreeSet::new());
    assert_eq!(fp.reads, ids(&["a"]), "b reads only the public interface");
}

#[test]
fn leak_widens_to_zone_and_its_readers() {
    let base = base().leaky("a::tax").build();
    let fp = footprint(&base, &base, &changed(&["a::tax.rs"]), &BTreeSet::new());
    assert!(fp.leaky);
    assert_eq!(fp.reads, ids(&["a::tax", "a::invoice", "a", "b"]));
}

#[test]
fn unowned_path_is_leaky_everywhere() {
    let base = base().build();
    let fp = footprint(
        &base,
        &base,
        &changed(&["scripts/deploy.sh"]),
        &BTreeSet::new(),
    );
    assert!(fp.leaky);
    assert!(fp.writes.is_empty());
    assert_eq!(fp.reads, ids(&["a::tax", "a::invoice", "a", "b"]));
}

#[test]
fn deleted_target_counts_as_changed() {
    let base = base().build();
    let mut head = base.clone();
    head.targets.remove(&id("a::tax"));
    let fp = footprint(&base, &head, &changed(&["a::tax.rs"]), &BTreeSet::new());
    assert!(fp.reads.contains(&id("a::invoice")));
}

#[test]
fn extra_reads_are_included_with_their_tests() {
    let base = base().build();
    let fp = footprint(&base, &base, &BTreeSet::new(), &ids(&["b"]));
    assert!(fp.writes.is_empty());
    assert_eq!(fp.reads, ids(&["b"]));
    assert_eq!(fp.tests, BTreeSet::from([TestId::new("b#tests")]));
}

const NAMES: [&str; 6] = ["p::a", "p::b", "p::c", "q::a", "q::b", "r"];

/// Random snapshots over a fixed set of targets: the same graph shape in base and head,
/// with some interfaces changed.
fn arb_change() -> impl Strategy<Value = (GraphSnapshot, GraphSnapshot, BTreeSet<RepoPath>)> {
    let n = NAMES.len();
    (
        prop::collection::vec(prop::collection::vec((0..n, any::<bool>()), 0..3), n),
        prop::collection::vec((any::<bool>(), any::<bool>(), any::<bool>()), n),
        prop::collection::btree_set(0..n + 1, 0..4),
    )
        .prop_map(|(edges, flags, touched)| {
            let mut base = Builder::default();
            let mut head = Builder::default();
            for (i, name) in NAMES.iter().enumerate() {
                let reads: Vec<(&str, EdgeKind)> = edges[i]
                    .iter()
                    .filter(|(j, _)| *j != i)
                    .map(|(j, public)| {
                        (
                            NAMES[*j],
                            if *public {
                                EdgeKind::Public
                            } else {
                                EdgeKind::Local
                            },
                        )
                    })
                    .collect();
                let (iface_changes, public_changes, hermetic) = flags[i];
                base = base.target(name, "v1", &reads);
                head = head.target(name, if iface_changes { "v2" } else { "v1" }, &reads);
                if public_changes {
                    head.0.targets.get_mut(&id(name)).unwrap().public_iface =
                        IfaceHash("pub-v3".into());
                }
                if !hermetic {
                    base = base.leaky(name);
                    head = head.leaky(name);
                }
            }
            // Index n stands for a path no target owns.
            let paths = touched
                .into_iter()
                .map(|i| {
                    NAMES
                        .get(i)
                        .map_or("unowned.txt".to_owned(), |n| format!("{n}.rs"))
                })
                .map(RepoPath::new)
                .collect();
            (base.build(), head.build(), paths)
        })
}

proptest! {
    #[test]
    fn reads_contain_writes((base, head, paths) in arb_change()) {
        let fp = footprint(&base, &head, &paths, &BTreeSet::new());
        prop_assert!(fp.writes.is_subset(&fp.reads));
    }

    #[test]
    fn more_changed_paths_never_shrink_the_footprint(
        (base, head, paths) in arb_change(),
        drop in any::<prop::sample::Index>(),
    ) {
        let full = footprint(&base, &head, &paths, &BTreeSet::new());
        let mut fewer = paths.clone();
        if let Some(p) = paths.iter().nth(drop.index(paths.len().max(1))) {
            fewer.remove(p);
        }
        let part = footprint(&base, &head, &fewer, &BTreeSet::new());
        prop_assert!(part.writes.is_subset(&full.writes));
        prop_assert!(part.reads.is_subset(&full.reads));
        prop_assert!(part.tests.is_subset(&full.tests));
        prop_assert!(!part.leaky || full.leaky);
    }

    #[test]
    fn no_change_means_empty_footprint((base, head, _) in arb_change()) {
        let fp = footprint(&base, &head, &BTreeSet::new(), &BTreeSet::new());
        prop_assert_eq!(fp, Footprint::default());
    }

    #[test]
    fn unchanged_hermetic_interfaces_do_not_propagate((base, _, paths) in arb_change()) {
        let mut hermetic = base.clone();
        for t in hermetic.targets.values_mut() {
            t.hermetic = true;
        }
        let fp = footprint(&hermetic, &hermetic, &paths, &BTreeSet::new());
        if !paths.contains(&RepoPath::new("unowned.txt")) {
            prop_assert_eq!(&fp.reads, &fp.writes);
            prop_assert!(!fp.leaky);
        }
    }

    #[test]
    fn a_non_hermetic_target_in_reads_makes_it_leaky((base, head, paths) in arb_change()) {
        let fp = footprint(&base, &head, &paths, &BTreeSet::new());
        let any_leak = fp.reads.iter().any(|t| head.targets.get(t).is_some_and(|t| !t.hermetic));
        prop_assert!(!any_leak || fp.leaky);
    }
}
