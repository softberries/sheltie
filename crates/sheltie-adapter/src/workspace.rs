//! A workspace as a set of units (packages, modules, projects), turned into a snapshot.
//!
//! Tools whose build graph can be read from their manifests and sources describe each tree
//! as a [`Workspace`]; [`assemble`] turns it into a [`GraphSnapshot`]:
//!
//! - one target per [`Unit`], reading the units it depends on through their public interface.
//!   A unit's interface hash folds in the interfaces of the units it reads: line-based
//!   adapters cannot tell which dependency types a unit's own API exposes, so an interface
//!   change reaches dependents of dependents too (pessimistic; body-only changes still stay
//!   local);
//! - `$workspace`, owning workspace-wide configuration, read by every unit;
//! - `$lockfile`, owning dependency lockfiles, read by every unit through its public
//!   interface (so a lockfile change affects everything: coarse, but never missed);
//! - for changed files nobody owns: `<unit>/files` (inside a unit's directory, not hermetic)
//!   or `file:<path>` (plain files anywhere, and other files outside every unit; hermetic only
//!   for plain files), as for Cargo.

use std::collections::{BTreeMap, BTreeSet};

use sheltie_core::{RepoPath, TargetId, TestId};
use sheltie_graph::{EdgeKind, GraphSnapshot, IfaceHash, Target, Zone};

use crate::git::TreeFiles;
use crate::policy::PathPolicy;
use crate::scan::hash_parts;

/// Target owning workspace-wide build configuration; every unit reads it.
pub const WORKSPACE: &str = "$workspace";
/// Target owning dependency lockfiles; every unit reads it.
pub const LOCKFILE: &str = "$lockfile";

/// One buildable unit: a Go package, an npm workspace package, a Maven module…
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// Its target id, e.g. `example.com/shop/cart` or `@acme/ui`.
    pub id: TargetId,
    /// Its directory, relative to the repository root (empty for the root). Changed files
    /// under it that no unit owns become `<id>/files`.
    pub root: String,
    /// The files it owns (sources, its manifest, tests).
    pub files: BTreeSet<RepoPath>,
    /// Units it depends on (ids of other units; unknown ids are ignored).
    pub reads: BTreeSet<TargetId>,
    /// Hash of its public interface: what dependents see. Equal hashes mean dependents are
    /// unaffected by a change.
    pub public_iface: String,
    /// Tests to run when it is affected.
    pub tests: BTreeSet<TestId>,
    /// Why its build is not a pure function of its files (empty: hermetic).
    pub leaks: Vec<String>,
}

/// The units of one tree and its workspace-wide files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    /// The units.
    pub units: Vec<Unit>,
    /// Workspace-wide configuration (root manifests, toolchain files).
    pub config: BTreeSet<RepoPath>,
    /// Dependency lockfiles.
    pub lockfiles: BTreeSet<RepoPath>,
}

impl Workspace {
    /// The unit whose directory most deeply contains `path`.
    pub fn unit_of(&self, path: &str) -> Option<&Unit> {
        self.units
            .iter()
            .filter(|u| {
                u.root.is_empty()
                    || path
                        .strip_prefix(u.root.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .max_by_key(|u| u.root.len())
    }
}

fn special(iface: String, zone: Zone, hermetic: bool) -> Target {
    Target {
        zone,
        public_iface: IfaceHash(iface.clone()),
        iface: IfaceHash(iface),
        reads: BTreeMap::new(),
        tests: BTreeSet::new(),
        hermetic,
    }
}

/// The snapshot of one tree. `extra_paths` (usually the changed files) get an owner even if
/// no unit claims them.
pub fn assemble(
    files: &TreeFiles,
    ws: &Workspace,
    policy: &PathPolicy,
    extra_paths: &BTreeSet<RepoPath>,
) -> GraphSnapshot {
    let mut snapshot = GraphSnapshot::default();
    let known: BTreeSet<&TargetId> = ws.units.iter().map(|u| &u.id).collect();
    let present = |set: &BTreeSet<RepoPath>| -> Vec<String> {
        set.iter()
            .map(|p| format!("{p}={}", files.oid(p.as_str()).map_or("-", String::as_str)))
            .collect()
    };
    let effective = effective_ifaces(ws);
    for unit in &ws.units {
        let iface = effective
            .get(&unit.id)
            .cloned()
            .unwrap_or_else(|| unit.public_iface.clone());
        let mut reads: BTreeMap<TargetId, EdgeKind> = unit
            .reads
            .iter()
            .filter(|r| known.contains(r) && **r != unit.id)
            .map(|r| (r.clone(), EdgeKind::Public))
            .collect();
        reads.insert(TargetId::new(WORKSPACE), EdgeKind::Local);
        if !ws.lockfiles.is_empty() {
            reads.insert(TargetId::new(LOCKFILE), EdgeKind::Public);
        }
        snapshot.targets.insert(
            unit.id.clone(),
            Target {
                zone: Zone::Group(unit.id.as_str().to_owned()),
                iface: IfaceHash(iface.clone()),
                public_iface: IfaceHash(iface),
                reads,
                tests: unit.tests.clone(),
                hermetic: unit.leaks.is_empty(),
            },
        );
        for f in &unit.files {
            snapshot
                .owners
                .entry(f.clone())
                .or_default()
                .insert(unit.id.clone());
        }
    }
    snapshot.targets.insert(
        TargetId::new(WORKSPACE),
        special(hash_parts(present(&ws.config)), Zone::All, true),
    );
    for f in &ws.config {
        snapshot
            .owners
            .entry(f.clone())
            .or_default()
            .insert(TargetId::new(WORKSPACE));
    }
    if !ws.lockfiles.is_empty() {
        snapshot.targets.insert(
            TargetId::new(LOCKFILE),
            special(hash_parts(present(&ws.lockfiles)), Zone::All, true),
        );
        for f in &ws.lockfiles {
            snapshot
                .owners
                .entry(f.clone())
                .or_default()
                .insert(TargetId::new(LOCKFILE));
        }
    }

    for path in extra_paths {
        // A file absent from this tree is owned by whichever tree has it.
        if files.oid(path.as_str()).is_none()
            || snapshot.owners.contains_key(path)
            || policy.is_ignored(path)
        {
            continue;
        }
        // Plain files stay plain even inside a unit's directory (a root package owns every
        // path, `.github/**` included).
        let unit = if policy.is_plain(path) {
            None
        } else {
            ws.unit_of(path.as_str())
        };
        let (id, target) = match unit {
            Some(unit) => (
                TargetId::new(format!("{}/files", unit.id)),
                special(
                    "files".into(),
                    Zone::Group(unit.id.as_str().to_owned()),
                    false,
                ),
            ),
            None => {
                let oid = files.oid(path.as_str()).cloned().unwrap_or_default();
                (
                    TargetId::new(format!("file:{path}")),
                    special(oid, Zone::All, policy.is_plain(path)),
                )
            }
        };
        snapshot.targets.entry(id.clone()).or_insert(target);
        snapshot.owners.entry(path.clone()).or_default().insert(id);
    }
    snapshot
}

/// Each unit's interface hash with the (effective) interfaces of the units it reads folded
/// in. In a dependency cycle, the edge that closes it contributes the unit's own hash.
fn effective_ifaces(ws: &Workspace) -> BTreeMap<TargetId, String> {
    let units: BTreeMap<&TargetId, &Unit> = ws.units.iter().map(|u| (&u.id, u)).collect();
    let mut done: BTreeMap<TargetId, String> = BTreeMap::new();
    fn visit(
        id: &TargetId,
        units: &BTreeMap<&TargetId, &Unit>,
        done: &mut BTreeMap<TargetId, String>,
        visiting: &mut BTreeSet<TargetId>,
    ) -> Option<String> {
        if let Some(h) = done.get(id) {
            return Some(h.clone());
        }
        let unit = units.get(id)?;
        if !visiting.insert(id.clone()) {
            return Some(unit.public_iface.clone());
        }
        let mut parts = vec![unit.public_iface.clone()];
        for dep in &unit.reads {
            if dep != id
                && let Some(h) = visit(dep, units, done, visiting)
            {
                parts.push(format!("{dep}={h}"));
            }
        }
        visiting.remove(id);
        let h = if parts.len() == 1 {
            unit.public_iface.clone()
        } else {
            hash_parts(&parts)
        };
        done.insert(id.clone(), h.clone());
        Some(h)
    }
    for id in units.keys() {
        visit(id, &units, &mut done, &mut BTreeSet::new());
    }
    done
}

/// Why a leaky footprint is leaky: the leaks of the units it reads, and changed paths
/// outside every unit's sources.
pub fn leak_reasons(
    trees: &[(&Workspace, &GraphSnapshot)],
    reads: &BTreeSet<TargetId>,
    relevant: &BTreeSet<RepoPath>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    for (ws, _) in trees {
        for unit in &ws.units {
            if reads.contains(&unit.id) {
                reasons.extend(unit.leaks.iter().map(|r| format!("{}: {r}", unit.id)));
            }
        }
    }
    for p in relevant {
        let outside = trees.iter().any(|(_, s)| {
            s.owners.get(p).into_iter().flatten().any(|o| {
                s.targets.get(o).is_some_and(|t| !t.hermetic)
                    && (o.as_str().starts_with("file:") || o.as_str().ends_with("/files"))
            })
        });
        if outside {
            reasons.push(format!("{p} is outside the module graph"));
        }
    }
    reasons.sort();
    reasons.dedup();
    reasons
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::COMMON_PLAIN;

    fn tree(paths: &[&str]) -> TreeFiles {
        TreeFiles(
            paths
                .iter()
                .map(|p| (RepoPath::new(*p), format!("oid-{p}")))
                .collect(),
        )
    }

    fn unit(id: &str, root: &str, files: &[&str], reads: &[&str], iface: &str) -> Unit {
        Unit {
            id: TargetId::new(id),
            root: root.into(),
            files: files.iter().map(|f| RepoPath::new(*f)).collect(),
            reads: reads.iter().map(|r| TargetId::new(*r)).collect(),
            public_iface: iface.into(),
            tests: [TestId::new(id)].into(),
            leaks: vec![],
        }
    }

    fn policy() -> PathPolicy {
        PathPolicy::new(crate::DEFAULT_IGNORE, COMMON_PLAIN).unwrap()
    }

    fn ws(core_iface: &str) -> Workspace {
        Workspace {
            units: vec![
                unit("core", "core", &["core/a.go"], &[], core_iface),
                unit(
                    "api",
                    "api",
                    &["api/b.go"],
                    &["core", "external/dep"],
                    "api-1",
                ),
                unit("web", "web", &["web/c.go"], &["api"], "web-1"),
            ],
            config: [RepoPath::new("go.mod")].into(),
            lockfiles: [RepoPath::new("go.sum")].into(),
        }
    }

    fn footprint(base: &Workspace, head: &Workspace, changed: &[&str]) -> sheltie_core::Footprint {
        let all = [
            "core/a.go",
            "api/b.go",
            "web/c.go",
            "go.mod",
            "go.sum",
            "core/data.json",
            "scripts/x.sh",
            ".github/ci.yml",
        ];
        let base_files = tree(&all);
        let mut head_files = base_files.clone();
        for c in changed {
            head_files.0.insert(RepoPath::new(*c), format!("new-{c}"));
        }
        let changed: BTreeSet<RepoPath> = changed.iter().map(|c| RepoPath::new(*c)).collect();
        let b = assemble(&base_files, base, &policy(), &changed);
        let h = assemble(&head_files, head, &policy(), &changed);
        sheltie_graph::footprint(&b, &h, &changed, &BTreeSet::new())
    }

    fn ids(set: &BTreeSet<TargetId>) -> Vec<&str> {
        set.iter().map(TargetId::as_str).collect()
    }

    #[test]
    fn interface_changes_reach_dependents_transitively() {
        let f = footprint(&ws("core-1"), &ws("core-2"), &["core/a.go"]);
        assert_eq!(ids(&f.writes), ["core"]);
        // api reads core; web reads api, which may expose core's types in its own API.
        assert_eq!(ids(&f.reads), ["api", "core", "web"]);
        assert!(!f.leaky);
        assert_eq!(f.tests.len(), 3);
    }

    #[test]
    fn dependency_cycles_terminate() {
        let mut w = ws("core-1");
        w.units[0].reads.insert(TargetId::new("web"));
        let mut h = w.clone();
        h.units[0].public_iface = "core-2".into();
        let f = footprint(&w, &h, &["core/a.go"]);
        assert_eq!(ids(&f.reads), ["api", "core", "web"]);
        let f = footprint(&w, &w, &["core/a.go"]);
        assert_eq!(ids(&f.reads), ["core"]);
    }

    #[test]
    fn body_changes_stay_local() {
        let f = footprint(&ws("core-1"), &ws("core-1"), &["core/a.go"]);
        assert_eq!(ids(&f.reads), ["core"]);
    }

    #[test]
    fn workspace_files_and_lockfiles_affect_everything() {
        let f = footprint(&ws("core-1"), &ws("core-1"), &["go.mod"]);
        assert_eq!(ids(&f.writes), ["$workspace"]);
        assert_eq!(ids(&f.reads), ["$workspace", "api", "core", "web"]);
        let f = footprint(&ws("core-1"), &ws("core-1"), &["go.sum"]);
        assert_eq!(ids(&f.reads), ["$lockfile", "api", "core", "web"]);
    }

    #[test]
    fn unowned_files() {
        // Inside a unit: not hermetic, leaks into the unit and its readers.
        let f = footprint(&ws("core-1"), &ws("core-1"), &["core/data.json"]);
        assert_eq!(ids(&f.writes), ["core/files"]);
        assert!(f.leaky && f.reads.contains(&TargetId::new("api")));
        // Plain files outside every unit: tracked, not leaky.
        let f = footprint(&ws("core-1"), &ws("core-1"), &[".github/ci.yml"]);
        assert_eq!(ids(&f.writes), ["file:.github/ci.yml"]);
        assert!(!f.leaky);
        // Other files outside every unit: assumed read by the build.
        let f = footprint(&ws("core-1"), &ws("core-1"), &["scripts/x.sh"]);
        assert!(f.leaky);
    }

    #[test]
    fn leak_reasons_name_units_and_paths() {
        let mut w = ws("core-1");
        w.units[0].leaks = vec!["embeds files".into()];
        let files = tree(&["core/a.go", "scripts/x.sh"]);
        let changed: BTreeSet<RepoPath> = [RepoPath::new("scripts/x.sh")].into();
        let s = assemble(&files, &w, &policy(), &changed);
        let reads: BTreeSet<TargetId> = [TargetId::new("core")].into();
        assert_eq!(
            leak_reasons(&[(&w, &s)], &reads, &changed),
            [
                "core: embeds files",
                "scripts/x.sh is outside the module graph"
            ]
        );
    }

    #[test]
    fn plain_files_stay_plain_under_a_root_unit() {
        let w = Workspace {
            units: vec![unit("app", "", &["main.go"], &[], "a")],
            ..Workspace::default()
        };
        let files = tree(&["main.go", ".github/ci.yml", "data.json"]);
        let changed: BTreeSet<RepoPath> =
            [RepoPath::new(".github/ci.yml"), RepoPath::new("data.json")].into();
        let s = assemble(&files, &w, &policy(), &changed);
        let owner = |p: &str| ids(&s.owners[&RepoPath::new(p)]).join(",");
        assert_eq!(owner(".github/ci.yml"), "file:.github/ci.yml");
        assert!(s.targets[&TargetId::new("file:.github/ci.yml")].hermetic);
        assert_eq!(owner("data.json"), "app/files");
    }

    #[test]
    fn the_deepest_unit_contains_a_path() {
        let w = Workspace {
            units: vec![
                unit("root", "", &[], &[], "r"),
                unit("app", "apps/app", &[], &[], "a"),
            ],
            ..Workspace::default()
        };
        assert_eq!(
            w.unit_of("apps/app/src/x.ts").map(|u| u.id.as_str()),
            Some("app")
        );
        assert_eq!(
            w.unit_of("apps/application/x").map(|u| u.id.as_str()),
            Some("root")
        );
    }
}
