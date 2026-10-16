//! Building a [`GraphSnapshot`] for one tree of a Cargo workspace.

use std::collections::{BTreeMap, BTreeSet};

use petgraph::algo::tarjan_scc;
use petgraph::graphmap::DiGraphMap;
use sha2::{Digest, Sha256};
use sheltie_core::{RepoPath, TargetId, TestId};
use sheltie_graph::{EdgeKind, GraphSnapshot, IfaceHash, Target, Zone};

use crate::Error;
use crate::git::{Git, TreeFiles};
use crate::metadata::{CrateKind, CrateTarget, Layout, Package};
use crate::modules::{ModuleTree, PathRef, UseEntry};
use crate::policy::PathPolicy;

/// Target owning workspace-wide build settings; every target reads it.
pub(crate) const WORKSPACE: &str = "$workspace";
/// Target owning `Cargo.lock`.
pub(crate) const LOCKFILE: &str = "$lockfile";
/// Files that configure the whole workspace build.
const WORKSPACE_FILES: [&str; 5] = [
    "Cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config",
    ".cargo/config.toml",
];

/// A snapshot plus what the caller needs to explain and extend it.
#[derive(Debug, Default)]
pub(crate) struct Built {
    pub(crate) snapshot: GraphSnapshot,
    /// Targets of each package, for lockfile-driven reads.
    pub(crate) package_targets: BTreeMap<String, BTreeSet<TargetId>>,
    /// Why packages are not hermetic.
    pub(crate) leaks: BTreeMap<String, Vec<String>>,
}

/// Builds the snapshot of one tree. `extra_paths` are files (usually the changed ones) that
/// must have an owner even if no module claims them.
pub(crate) fn build(
    git: &Git,
    files: &TreeFiles,
    layout: &Layout,
    policy: &PathPolicy,
    extra_paths: &BTreeSet<RepoPath>,
) -> Result<Built, Error> {
    let mut built = Built::default();
    let mut public_hashes: BTreeMap<String, String> = BTreeMap::new();

    for pkg in dependency_order(layout) {
        let root_target = package_root_target(pkg);
        let mut crates = Vec::new();
        for ct in pkg.lib.iter().chain(&pkg.others) {
            let tree = ModuleTree::parse(git, files, &ct.root)?;
            crates.push((ct, tree));
        }
        let mut leaks: Vec<String> = Vec::new();
        if let Some(b) = &pkg.build_script {
            leaks.push(format!("build script {b}"));
        }
        if pkg.proc_macro {
            leaks.push("proc-macro crate".into());
        }
        for (_, tree) in &crates {
            for m in &tree.modules {
                leaks.extend(m.leaks.iter().cloned());
            }
        }
        let hermetic = leaks.is_empty();
        if !hermetic {
            built.leaks.insert(pkg.name.clone(), leaks);
        }

        // Package-level inputs folded into the root interface: manifest and build script.
        let mut pkg_inputs = String::new();
        for path in [Some(&pkg.manifest), pkg.build_script.as_ref()]
            .into_iter()
            .flatten()
        {
            pkg_inputs.push_str(files.oid(path.as_str()).map_or("-", String::as_str));
            pkg_inputs.push('\n');
        }

        for (ct, tree) in &crates {
            let mut externs = pkg.deps.clone();
            if ct.kind != CrateKind::Lib
                && let Some(lib) = &pkg.lib
            {
                externs.insert(lib.name.replace('-', "_"), pkg.name.clone());
            }
            let analysis = analyze(pkg, ct, tree, &externs, &public_hashes, &pkg_inputs);
            if ct.kind == CrateKind::Lib {
                public_hashes.insert(pkg.name.clone(), analysis.root_public.clone());
            }
            for (id, mut target) in analysis.targets {
                target.hermetic = hermetic;
                target
                    .reads
                    .insert(TargetId::new(WORKSPACE), EdgeKind::Local);
                built
                    .package_targets
                    .entry(pkg.name.clone())
                    .or_default()
                    .insert(id.clone());
                built.snapshot.targets.insert(id, target);
            }
            for (file, owner) in analysis.owners {
                built.snapshot.owners.entry(file).or_default().insert(owner);
            }
        }
        for path in [Some(&pkg.manifest), pkg.build_script.as_ref()]
            .into_iter()
            .flatten()
        {
            built
                .snapshot
                .owners
                .entry(path.clone())
                .or_default()
                .insert(root_target.clone());
        }
    }

    add_special_targets(&mut built, files, layout, policy, extra_paths);
    Ok(built)
}

/// The target that owns a package's manifest: its library root, else its first crate.
fn package_root_target(pkg: &Package) -> TargetId {
    pkg.lib.iter().chain(&pkg.others).next().map_or_else(
        || TargetId::new(format!("{}/files", pkg.name)),
        |ct| TargetId::new(ct.id_prefix(&pkg.name)),
    )
}

/// Packages ordered so that a library comes before the packages that depend on it.
/// Dev-dependency cycles are broken arbitrarily; they never appear in signatures.
fn dependency_order(layout: &Layout) -> Vec<&Package> {
    let by_name: BTreeMap<&str, &Package> = layout
        .packages
        .iter()
        .map(|p| (p.name.as_str(), p))
        .collect();
    let mut order = Vec::new();
    let mut state: BTreeMap<&str, bool> = BTreeMap::new(); // false = visiting, true = done
    fn visit<'a>(
        p: &'a Package,
        by_name: &BTreeMap<&str, &'a Package>,
        state: &mut BTreeMap<&'a str, bool>,
        order: &mut Vec<&'a Package>,
    ) {
        if state.contains_key(p.name.as_str()) {
            return;
        }
        state.insert(&p.name, false);
        for dep in p.deps.values() {
            if let Some(d) = by_name.get(dep.as_str()) {
                visit(d, by_name, state, order);
            }
        }
        state.insert(&p.name, true);
        order.push(p);
    }
    for p in &layout.packages {
        visit(p, &by_name, &mut state, &mut order);
    }
    order
}

/// Path resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Resolved {
    /// A module of this crate.
    Module(usize),
    /// An item defined in a module of this crate.
    Item(usize),
    /// Another workspace package (by package name).
    Extern(String),
}

struct Resolver<'a> {
    tree: &'a ModuleTree,
    externs: &'a BTreeMap<String, String>,
    edition_2015: bool,
    aliases: Vec<BTreeMap<String, Resolved>>,
    globs: Vec<Vec<usize>>,
}

impl<'a> Resolver<'a> {
    fn new(
        tree: &'a ModuleTree,
        externs: &'a BTreeMap<String, String>,
        edition_2015: bool,
    ) -> Self {
        let n = tree.modules.len();
        let mut r = Self {
            tree,
            externs,
            edition_2015,
            aliases: vec![BTreeMap::new(); n],
            globs: vec![Vec::new(); n],
        };
        // Aliases can refer to other aliases; a few passes settle realistic chains.
        for _ in 0..3 {
            for m in 0..n {
                for entry in &tree.modules[m].uses {
                    match entry {
                        UseEntry::Name { path, name } => {
                            if name == "_" {
                                continue;
                            }
                            if let Some(res) = r.resolve(m, path) {
                                r.aliases[m].insert(name.clone(), res);
                            }
                        }
                        UseEntry::Glob { path } => {
                            if let Some(Resolved::Module(g)) = r.resolve(m, path)
                                && !r.globs[m].contains(&g)
                            {
                                r.globs[m].push(g);
                            }
                        }
                    }
                }
            }
        }
        r
    }

    fn child(&self, m: usize, name: &str) -> Option<usize> {
        self.tree.modules[m].children.get(name).map(|(i, _)| *i)
    }

    fn resolve(&self, m: usize, segs: &PathRef) -> Option<Resolved> {
        let first = segs.first()?;
        let (mut cur, mut i) = match first.as_str() {
            "::" => {
                return segs
                    .get(1)
                    .and_then(|name| self.externs.get(name))
                    .map(|p| Resolved::Extern(p.clone()));
            }
            "crate" | "$crate" => (0, 1),
            "self" => (m, 1),
            "super" => {
                let mut cur = m;
                let mut i = 0;
                while segs.get(i).is_some_and(|s| s == "super") {
                    cur = self.tree.modules[cur].parent?;
                    i += 1;
                }
                (cur, i)
            }
            "Self" => return None,
            name => {
                if let Some(c) = self.child(m, name) {
                    (c, 1)
                } else if let Some(res) = self.aliases[m].get(name) {
                    match res {
                        Resolved::Module(x) => (*x, 1),
                        other => return Some(other.clone()),
                    }
                } else if self.edition_2015 && self.child(0, name).is_some() {
                    (self.child(0, name)?, 1)
                } else if let Some(pkg) = self.externs.get(name) {
                    return Some(Resolved::Extern(pkg.clone()));
                } else {
                    (self.globs[m].iter().find_map(|g| self.child(*g, name))?, 1)
                }
            }
        };
        while let Some(c) = segs.get(i).and_then(|s| self.child(cur, s)) {
            cur = c;
            i += 1;
        }
        Some(if i < segs.len() {
            Resolved::Item(cur)
        } else {
            Resolved::Module(cur)
        })
    }
}

struct Analysis {
    targets: BTreeMap<TargetId, Target>,
    owners: Vec<(RepoPath, TargetId)>,
    root_public: String,
}

fn analyze(
    pkg: &Package,
    ct: &CrateTarget,
    tree: &ModuleTree,
    externs: &BTreeMap<String, String>,
    public_hashes: &BTreeMap<String, String>,
    pkg_inputs: &str,
) -> Analysis {
    let resolver = Resolver::new(tree, externs, ct.edition_2015);
    let prefix = ct.id_prefix(&pkg.name);
    let n = tree.modules.len();

    // Test modules belong to their nearest non-test ancestor.
    let owner: Vec<usize> = (0..n)
        .map(|mut m| {
            while tree.modules[m].is_test {
                match tree.modules[m].parent {
                    Some(p) => m = p,
                    None => break,
                }
            }
            m
        })
        .collect();
    let id_of = |m: usize| -> TargetId {
        let path = &tree.modules[m].path;
        if path.is_empty() {
            TargetId::new(prefix.clone())
        } else {
            TargetId::new(format!("{prefix}::{}", path.join("::")))
        }
    };
    let resolve_all = |m: usize, paths: &mut dyn Iterator<Item = &PathRef>| -> Vec<Resolved> {
        paths.filter_map(|p| resolver.resolve(m, p)).collect()
    };

    let live: Vec<usize> = (0..n).filter(|m| !tree.modules[*m].is_test).collect();
    let macro_modules: BTreeSet<usize> = live
        .iter()
        .copied()
        .filter(|m| tree.modules[*m].defines_macros)
        .collect();

    // Own hashes and fold edges for the interface hashes.
    let mut own = BTreeMap::new();
    let mut local_edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    let mut public_edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for &m in &live {
        let module = &tree.modules[m];
        let mut extern_refs = BTreeSet::new();
        let mut local = BTreeSet::new();
        for r in resolve_all(m, &mut module.sig_paths.iter()) {
            match r {
                Resolved::Module(x) | Resolved::Item(x) => {
                    if owner[x] != m {
                        local.insert(owner[x]);
                    }
                }
                Resolved::Extern(p) => {
                    extern_refs.insert(
                        public_hashes
                            .get(&p)
                            .cloned()
                            .unwrap_or_else(|| format!("extern:{p}")),
                    );
                }
            }
        }
        let mut text = module.signature.clone();
        for h in &extern_refs {
            text.push_str("\nextern ");
            text.push_str(h);
        }
        if m == 0 {
            text.push_str("\npackage ");
            text.push_str(pkg_inputs);
        }
        own.insert(m, sha256(&text));
        let mut public = local.clone();
        public.extend(
            module
                .children
                .values()
                .filter(|(c, is_pub)| *is_pub && !tree.modules[*c].is_test)
                .map(|(c, _)| *c),
        );
        if m == 0 {
            public.extend(macro_modules.iter().copied().filter(|x| *x != 0));
        }
        local_edges.insert(m, local);
        public_edges.insert(m, public);
    }
    let local_hash = fold_hashes(&own, &local_edges);
    let public_hash = fold_hashes(&own, &public_edges);

    // Targets with their read edges and tests.
    let mut targets: BTreeMap<TargetId, Target> = BTreeMap::new();
    for &m in &live {
        targets.insert(
            id_of(m),
            Target {
                zone: Zone::Group(pkg.name.clone()),
                iface: IfaceHash(local_hash.get(&m).cloned().unwrap_or_default()),
                public_iface: IfaceHash(public_hash.get(&m).cloned().unwrap_or_default()),
                reads: BTreeMap::new(),
                tests: BTreeSet::new(),
                hermetic: true,
            },
        );
    }
    for m in 0..n {
        let module = &tree.modules[m];
        let me = id_of(owner[m]);
        let use_paths: Vec<PathRef> = module
            .uses
            .iter()
            .map(|u| match u {
                UseEntry::Name { path, .. } | UseEntry::Glob { path } => path.clone(),
            })
            .collect();
        let mut reads = BTreeMap::new();
        for r in resolve_all(m, &mut module.paths.iter().chain(&use_paths)) {
            match r {
                Resolved::Module(x) | Resolved::Item(x) => {
                    let t = id_of(owner[x]);
                    if t != me {
                        reads.entry(t).or_insert(EdgeKind::Local);
                    }
                }
                Resolved::Extern(p) => {
                    reads.insert(TargetId::new(p), EdgeKind::Public);
                }
            }
        }
        for &mm in &macro_modules {
            let t = id_of(mm);
            if t != me {
                reads.entry(t).or_insert(EdgeKind::Local);
            }
        }
        // A module's public surface includes its public children: when a child's interface
        // changes, the parent is revisited so readers of its public interface see it.
        if !module.is_test {
            for (c, is_pub) in module.children.values() {
                if *is_pub && !tree.modules[*c].is_test {
                    reads.entry(id_of(*c)).or_insert(EdgeKind::Local);
                }
            }
        }
        let has_tests = module.has_tests
            || (module.is_test && m != owner[m])
            || (ct.kind == CrateKind::Test && m == 0);
        if let Some(target) = targets.get_mut(&me) {
            target.reads.extend(reads);
            if has_tests {
                target.tests.insert(TestId::new(me.as_str()));
            }
        }
    }

    let owners = (0..n)
        .flat_map(|m| tree.modules[m].files.iter().map(move |f| (f.clone(), m)))
        .map(|(f, m)| (f, id_of(owner[m])))
        .collect();
    Analysis {
        targets,
        owners,
        root_public: public_hash.get(&0).cloned().unwrap_or_default(),
    }
}

/// Folds each node's own hash with the folded hashes of the nodes it references. Nodes in
/// a cycle share one hash for the cycle, salted with their own hash.
fn fold_hashes(
    own: &BTreeMap<usize, String>,
    edges: &BTreeMap<usize, BTreeSet<usize>>,
) -> BTreeMap<usize, String> {
    let mut graph = DiGraphMap::<usize, ()>::new();
    for &n in own.keys() {
        graph.add_node(n);
    }
    for (&a, targets) in edges {
        for &b in targets {
            if own.contains_key(&b) {
                graph.add_edge(a, b, ());
            }
        }
    }
    // tarjan_scc yields components in reverse topological order: referenced nodes first.
    let mut scc_hash: BTreeMap<usize, String> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for scc in tarjan_scc(&graph) {
        let members: BTreeSet<usize> = scc.iter().copied().collect();
        let mut parts: Vec<String> = members.iter().filter_map(|m| own.get(m).cloned()).collect();
        parts.sort();
        let mut external: Vec<String> = members
            .iter()
            .flat_map(|m| edges.get(m).into_iter().flatten())
            .filter(|t| !members.contains(t))
            .filter_map(|t| scc_hash.get(t).cloned())
            .collect();
        external.sort();
        external.dedup();
        let h = sha256(&format!(
            "{}\n--\n{}",
            parts.join("\n"),
            external.join("\n")
        ));
        for &m in &members {
            scc_hash.insert(m, h.clone());
            out.insert(
                m,
                sha256(&format!("{h}\n{}", own.get(&m).map_or("", String::as_str))),
            );
        }
    }
    out
}

fn add_special_targets(
    built: &mut Built,
    files: &TreeFiles,
    layout: &Layout,
    policy: &PathPolicy,
    extra_paths: &BTreeSet<RepoPath>,
) {
    let special = |iface: String, zone: Zone, hermetic: bool| Target {
        zone,
        public_iface: IfaceHash(iface.clone()),
        iface: IfaceHash(iface),
        reads: BTreeMap::new(),
        tests: BTreeSet::new(),
        hermetic,
    };
    let workspace_inputs: String = WORKSPACE_FILES
        .iter()
        .map(|f| format!("{f}={}\n", files.oid(f).map_or("-", String::as_str)))
        .collect();
    built.snapshot.targets.insert(
        TargetId::new(WORKSPACE),
        special(sha256(&workspace_inputs), Zone::All, true),
    );
    for f in WORKSPACE_FILES {
        built
            .snapshot
            .owners
            .entry(RepoPath::new(f))
            .or_default()
            .insert(TargetId::new(WORKSPACE));
    }
    let lock = files.oid("Cargo.lock").cloned().unwrap_or_default();
    built
        .snapshot
        .targets
        .insert(TargetId::new(LOCKFILE), special(lock, Zone::All, true));
    built
        .snapshot
        .owners
        .entry(RepoPath::new("Cargo.lock"))
        .or_default()
        .insert(TargetId::new(LOCKFILE));

    for path in extra_paths {
        // A file absent from this tree is owned by whichever tree has it.
        if files.oid(path.as_str()).is_none()
            || built.snapshot.owners.contains_key(path)
            || policy.is_ignored(path)
        {
            continue;
        }
        let (id, target) = match owning_package(layout, path) {
            Some(pkg) => (
                TargetId::new(format!("{}/files", pkg.name)),
                special("files".into(), Zone::Group(pkg.name.clone()), false),
            ),
            None => {
                let oid = files.oid(path.as_str()).cloned().unwrap_or_default();
                let hermetic = policy.is_plain(path);
                (
                    TargetId::new(format!("file:{path}")),
                    special(oid, Zone::All, hermetic),
                )
            }
        };
        if let Some(pkg) = owning_package(layout, path) {
            built
                .package_targets
                .entry(pkg.name.clone())
                .or_default()
                .insert(id.clone());
        }
        built.snapshot.targets.entry(id.clone()).or_insert(target);
        built
            .snapshot
            .owners
            .entry(path.clone())
            .or_default()
            .insert(id);
    }
}

/// The package whose directory contains `path`. A package at the repository root only
/// claims its conventional source directories.
fn owning_package<'a>(layout: &'a Layout, path: &RepoPath) -> Option<&'a Package> {
    let p = path.as_str();
    layout
        .packages
        .iter()
        .filter(|pkg| {
            if pkg.dir.is_empty() {
                ["src/", "tests/", "benches/", "examples/"]
                    .iter()
                    .any(|d| p.starts_with(d))
                    || p == "build.rs"
            } else {
                p.starts_with(&format!("{}/", pkg.dir))
            }
        })
        .max_by_key(|pkg| pkg.dir.len())
}

pub(crate) fn sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_orders_dependencies_first_and_handles_cycles() {
        let own: BTreeMap<usize, String> = (0..4).map(|i| (i, format!("own{i}"))).collect();
        // 0 -> 1 -> 2 <-> 3
        let edges: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::from([
            (0, BTreeSet::from([1])),
            (1, BTreeSet::from([2])),
            (2, BTreeSet::from([3])),
            (3, BTreeSet::from([2])),
        ]);
        let base = fold_hashes(&own, &edges);
        assert_eq!(base.len(), 4);

        // Changing node 3 changes everything that reaches it; nothing else changes.
        let mut own2 = own.clone();
        own2.insert(3, "changed".into());
        let changed = fold_hashes(&own2, &edges);
        for i in 0..4 {
            assert_ne!(base[&i], changed[&i], "node {i}");
        }

        // Changing node 0 changes only node 0.
        let mut own3 = own.clone();
        own3.insert(0, "changed".into());
        let changed = fold_hashes(&own3, &edges);
        assert_ne!(base[&0], changed[&0]);
        for i in 1..4 {
            assert_eq!(base[&i], changed[&i], "node {i}");
        }
    }
}
