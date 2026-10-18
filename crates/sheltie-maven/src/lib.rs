//! Maven adapter for Sheltie: computes footprints of changes from a repository's POMs and its
//! Java and Kotlin sources, read straight from git (no `mvn` or JDK needed).
//!
//! - Units are the modules reachable from the root `pom.xml` through `<modules>` (and the
//!   parents they name by `relativePath`), identified as `groupId:artifactId`. A module owns
//!   its POM and everything under its `src/`.
//! - A module reads the modules it depends on (`<dependencies>`, build plugins, plugin
//!   dependencies and annotation processor paths; never `<dependencyManagement>`) and its
//!   parent.
//! - Its interface is the public surface of `src/main` ([`sheltie_adapter::jvm`]) plus its
//!   whole POM, normalised: a dependency change propagates to dependents (pessimistic).
//! - `.mvn/**`, `mvnw` and `mvnw.cmd` are workspace configuration, read by every module.
//! - A module whose build (or parent's build) runs `exec-maven-plugin`, `maven-antrun-plugin`
//!   or `build-helper-maven-plugin` is not hermetic.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use sheltie_adapter::jvm;
use sheltie_adapter::scan::hash_parts;
use sheltie_adapter::{Error, Git, Tool, TreeFiles, Unit, Workspace};
use sheltie_core::{RepoPath, TargetId, TestId};

mod pom;
mod xml;

use pom::{LEAKY_PLUGINS, Pom};

/// The Maven [`Tool`]; run it with [`sheltie_adapter::Generic`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Maven;

impl Tool for Maven {
    const NAME: &'static str = "sheltie-maven";
    const VERSION: &'static str = concat!("sheltie-maven/", env!("CARGO_PKG_VERSION"));
    const PLAIN: &'static [&'static str] =
        &["checkstyle*.xml", "**/checkstyle*.xml", ".editorconfig"];

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        workspace(git, files)
    }
}

/// Whether a file is workspace-wide Maven configuration.
fn is_config(path: &str) -> bool {
    path.starts_with(".mvn/") || path == "mvnw" || path == "mvnw.cmd"
}

/// One module found in the tree.
struct Module {
    dir: String,
    pom_path: String,
    text: String,
    pom: Pom,
}

/// The modules and configuration of one tree.
pub fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
    let mut ws = Workspace {
        config: files
            .paths()
            .filter(|p| is_config(p.as_str()))
            .cloned()
            .collect(),
        ..Workspace::default()
    };
    let modules = discover(git, files)?;
    if modules.is_empty() {
        return Ok(ws);
    }

    // Coordinates: groupId falls back to the parent's.
    let coords: BTreeMap<&str, (String, String)> = modules
        .iter()
        .map(|(path, m)| {
            let group = m
                .pom
                .group
                .clone()
                .or_else(|| m.pom.parent.as_ref().and_then(|p| p.group.clone()))
                .unwrap_or_default();
            let artifact = m
                .pom
                .artifact
                .clone()
                .unwrap_or_else(|| m.dir.rsplit('/').next().unwrap_or_default().to_owned());
            (path.as_str(), (group, artifact))
        })
        .collect();
    // Coordinates shared by modules of different reactors get the directory in their id.
    let mut by_coords_all: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
    for (path, (g, a)) in &coords {
        by_coords_all
            .entry((g.as_str(), a.as_str()))
            .or_default()
            .push(*path);
    }
    let id_of = |path: &str| {
        coords.get(path).map(|(g, a)| {
            if by_coords_all
                .get(&(g.as_str(), a.as_str()))
                .is_some_and(|v| v.len() > 1)
            {
                let dir = parent_dir(path);
                format!("{g}:{a}@{}", if dir.is_empty() { "." } else { dir })
            } else {
                format!("{g}:{a}")
            }
        })
    };
    // A dependency on shared coordinates resolves to the module nearest the dependent.
    let nearest = |from: &str, candidates: &[&'_ str]| -> Option<String> {
        candidates
            .iter()
            .max_by_key(|c| common_prefix(parent_dir(from), parent_dir(c)))
            .map(|c| (*c).to_owned())
    };
    let by_coords: BTreeMap<(&str, &str), &str> = by_coords_all
        .iter()
        .filter_map(|(k, v)| v.first().map(|p| (*k, *p)))
        .collect();

    let parent_of: BTreeMap<&str, &str> = modules
        .iter()
        .filter_map(|(path, m)| {
            let parent = m.pom.parent.as_ref()?;
            let group = parent.group.as_deref().unwrap_or_default();
            let wanted = (group, parent.artifact.as_str());
            let by_path = parent_path(m, parent).filter(|p| {
                coords
                    .get(p.as_str())
                    .is_some_and(|(g, a)| (g.as_str(), a.as_str()) == wanted)
            });
            let found = match by_path {
                Some(p) => coords.get_key_value(p.as_str()).map(|(k, _)| *k),
                None => by_coords.get(&wanted).copied(),
            };
            found.filter(|p| *p != path).map(|p| (path.as_str(), p))
        })
        .collect();

    // Properties for interpolation: the parent's, then the module's own.
    let mut props: BTreeMap<&str, BTreeMap<String, String>> = BTreeMap::new();
    for path in modules.keys() {
        properties(path, &modules, &coords, &parent_of, &mut props, 0);
    }

    // Module dependencies, by path.
    let mut reads: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut test_jars: BTreeSet<&str> = BTreeSet::new();
    for (path, m) in &modules {
        let p = &props[path.as_str()];
        let entry = reads.entry(path.as_str()).or_default();
        for d in &m.pom.deps {
            let group = pom::interpolate(&d.group, p);
            let artifact = pom::interpolate(&d.artifact, p);
            let resolved = by_coords_all
                .get(&(group.as_str(), artifact.as_str()))
                .and_then(|c| nearest(path, c));
            let target = resolved
                .as_deref()
                .and_then(|r| coords.get_key_value(r).map(|(k, _)| *k))
                .or_else(|| {
                    // An unresolved group: match the artifact id if exactly one module has it.
                    if !group.contains("${") {
                        return None;
                    }
                    let mut same = coords.iter().filter(|(_, (_, a))| *a == artifact);
                    match (same.next(), same.next()) {
                        (Some((path, _)), None) => Some(*path),
                        _ => None,
                    }
                });
            if let Some(target) = target {
                entry.insert(target);
                if d.tests {
                    test_jars.insert(target);
                }
            }
        }
        if let Some(parent) = parent_of.get(path.as_str()) {
            entry.insert(*parent);
        }
    }

    for (path, m) in &modules {
        let Some(id) = id_of(path) else { continue };
        let with_tests = test_jars.contains(path.as_str());
        let mut parts = jvm::interface_parts(git, files, &m.dir, with_tests)?;
        parts.push(format!("{}\n{}", m.pom_path, pom::normalized(&m.text)));
        let mut unit_files = jvm::unit_files(files, &m.dir);
        // Custom source directories (`<build><sourceDirectory>`, inherited from parents).
        let (main_dir, test_dir) = source_dirs(path, &modules, &parent_of);
        for (dir, visible) in [(main_dir, true), (test_dir, with_tests)] {
            let Some(dir) = dir else { continue };
            let dir = jvm::join(&m.dir, &dir);
            for (p, oid) in files.under(&dir) {
                unit_files.insert(p.clone());
                if visible && let Some(lang) = jvm::Lang::of(p.as_str()) {
                    let surface = jvm::surface(lang, &git.text(oid)?);
                    if surface.lines().any(|l| !l.starts_with("package ")) {
                        parts.push(format!("{p}\n{surface}"));
                    }
                }
            }
        }
        unit_files.insert(RepoPath::new(m.pom_path.as_str()));
        ws.units.push(Unit {
            id: TargetId::new(id.as_str()),
            // The root module's directory is the repository: only its sources are its own,
            // so files elsewhere at the root are judged by the path policy.
            root: if m.dir.is_empty() {
                "src".into()
            } else {
                m.dir.clone()
            },
            files: unit_files,
            reads: reads[path.as_str()]
                .iter()
                .filter_map(|p| id_of(p))
                .map(TargetId::new)
                .collect(),
            public_iface: hash_parts(parts),
            tests: [TestId::new(id)].into(),
            leaks: leaks(path, &modules, &parent_of),
        });
    }
    Ok(ws)
}

/// The modules of every reactor in the tree, by POM path.
fn discover(git: &Git, files: &TreeFiles) -> Result<BTreeMap<String, Module>, Error> {
    let mut modules = BTreeMap::new();
    // The root POM first, then every other POM outside sources and build output: a
    // repository can hold several reactors (guava's `android/` flavour), each a set of modules.
    let mut queue: VecDeque<String> = VecDeque::from(["pom.xml".to_owned()]);
    queue.extend(
        files
            .paths()
            .map(|p| p.as_str())
            .filter(|p| {
                (p.ends_with("/pom.xml"))
                    && !p.split('/').any(|seg| {
                        matches!(seg, "src" | "target" | "node_modules") || seg.starts_with('.')
                    })
            })
            .map(str::to_owned),
    );
    while let Some(pom_path) = queue.pop_front() {
        if modules.contains_key(&pom_path) {
            continue;
        }
        let Some(oid) = files.oid(&pom_path) else {
            continue;
        };
        let text = git.text(oid)?;
        let pom = Pom::parse(&text).map_err(|e| Error::Tool(format!("{pom_path}: {e}")))?;
        let dir = parent_dir(&pom_path).to_owned();
        for module in &pom.modules {
            if let Some(path) = resolve(&dir, module) {
                queue.push_back(pom_file(path));
            }
        }
        let module = Module {
            dir,
            pom_path: pom_path.clone(),
            text,
            pom,
        };
        if let Some(parent) = module
            .pom
            .parent
            .as_ref()
            .and_then(|p| parent_path(&module, p))
        {
            queue.push_back(parent);
        }
        modules.insert(pom_path, module);
    }
    Ok(modules)
}

/// A module's `<sourceDirectory>` and `<testSourceDirectory>` relative to its directory, its
/// own or inherited from its parents; `None` for the standard `src/main/java` layout.
fn source_dirs(
    path: &str,
    modules: &BTreeMap<String, Module>,
    parent_of: &BTreeMap<&str, &str>,
) -> (Option<String>, Option<String>) {
    let clean = |d: &str| -> Option<String> {
        let d = d
            .trim_start_matches("${project.basedir}")
            .trim_start_matches("${basedir}")
            .trim_start_matches('/')
            .trim_end_matches('/');
        // Absolute or still-interpolated paths cannot be placed in the repository.
        (!d.is_empty() && !d.contains("${") && !d.split('/').any(|s| s == ".."))
            .then(|| d.to_owned())
            .filter(|d| !d.starts_with("src/main/") && !d.starts_with("src/test/"))
    };
    let (mut main, mut test) = (None, None);
    let mut at = Some(path);
    let mut depth = 0;
    while let Some(p) = at {
        let Some(m) = modules.get(p) else { break };
        main = main.or_else(|| m.pom.source_dir.clone());
        test = test.or_else(|| m.pom.test_source_dir.clone());
        at = parent_of.get(p).copied();
        depth += 1;
        if depth > 32 {
            break;
        }
    }
    (
        main.as_deref().and_then(clean),
        test.as_deref().and_then(clean),
    )
}

/// Length of the common directory prefix of two paths, in segments.
fn common_prefix(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y && !x.is_empty())
        .count()
}

/// The POM a `<parent>` points to by `relativePath`, if lookup by path is on.
fn parent_path(m: &Module, parent: &pom::Parent) -> Option<String> {
    let rel = parent.relative_path.as_deref().unwrap_or("../pom.xml");
    if rel.is_empty() {
        return None;
    }
    resolve(&m.dir, rel).map(pom_file)
}

/// A `<module>` or `relativePath` value as a POM path: directories get `/pom.xml`.
fn pom_file(path: String) -> String {
    if path.ends_with(".xml") {
        path
    } else {
        jvm::join(&path, "pom.xml")
    }
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// `rel` resolved against `dir`, normalised; `None` outside the repository.
fn resolve(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// Fills `props[path]`: the parent's properties, the module's own and its coordinates.
fn properties<'a>(
    path: &'a str,
    modules: &BTreeMap<String, Module>,
    coords: &BTreeMap<&str, (String, String)>,
    parent_of: &BTreeMap<&'a str, &'a str>,
    props: &mut BTreeMap<&'a str, BTreeMap<String, String>>,
    depth: usize,
) {
    if props.contains_key(path) {
        return;
    }
    let mut p = BTreeMap::new();
    if let Some(parent) = parent_of.get(path).copied().filter(|_| depth < 32) {
        properties(parent, modules, coords, parent_of, props, depth + 1);
        if let Some(inherited) = props.get(parent) {
            p.extend(inherited.clone());
        }
    }
    let Some(m) = modules.get(path) else { return };
    p.extend(m.pom.properties.clone());
    let (group, artifact) = coords.get(path).cloned().unwrap_or_default();
    let version = m
        .pom
        .version
        .clone()
        .or_else(|| m.pom.parent.as_ref().and_then(|p| p.version.clone()))
        .unwrap_or_default();
    for prefix in ["project.", "pom.", ""] {
        p.insert(format!("{prefix}groupId"), group.clone());
        p.insert(format!("{prefix}artifactId"), artifact.clone());
        p.insert(format!("{prefix}version"), version.clone());
    }
    if let Some(parent) = &m.pom.parent {
        for prefix in ["project.parent.", "parent."] {
            p.insert(
                format!("{prefix}groupId"),
                parent.group.clone().unwrap_or_default(),
            );
            p.insert(format!("{prefix}artifactId"), parent.artifact.clone());
            p.insert(
                format!("{prefix}version"),
                parent.version.clone().unwrap_or_default(),
            );
        }
    }
    props.insert(path, p);
}

/// Why a module's build is not a pure function of its files: plugins in its own or an
/// ancestor's `<build><plugins>` that run code.
fn leaks(
    path: &str,
    modules: &BTreeMap<String, Module>,
    parent_of: &BTreeMap<&str, &str>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut current = Some(path);
    let mut found = BTreeSet::new();
    while let Some(p) = current.filter(|p| seen.insert(*p)) {
        if let Some(m) = modules.get(p) {
            found.extend(
                m.pom
                    .plugins
                    .iter()
                    .filter(|a| LEAKY_PLUGINS.contains(&a.as_str())),
            );
        }
        current = parent_of.get(p).copied();
    }
    if found.is_empty() {
        Vec::new()
    } else {
        let names: Vec<&str> = found.into_iter().map(String::as_str).collect();
        vec![format!("build plugin runs code ({})", names.join(", "))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_resolve_inside_the_repository() {
        assert_eq!(resolve("", "core").as_deref(), Some("core"));
        assert_eq!(resolve("a/b", "../c/./d").as_deref(), Some("a/c/d"));
        assert_eq!(resolve("a", "../pom.xml").as_deref(), Some("pom.xml"));
        assert_eq!(resolve("", "../outside"), None);
        assert_eq!(pom_file("core".into()), "core/pom.xml");
        assert_eq!(pom_file("".into()), "pom.xml");
        assert_eq!(pom_file("x/alt.xml".into()), "x/alt.xml");
        assert_eq!(parent_dir("a/b/pom.xml"), "a/b");
        assert_eq!(parent_dir("pom.xml"), "");
        assert!(is_config(".mvn/maven.config") && is_config("mvnw") && !is_config("pom.xml"));
    }
}
