//! From the files of a tree to a [`Workspace`]: projects, source roots, units, their
//! imports and interfaces.

use std::collections::{BTreeMap, BTreeSet};

use sheltie_adapter::scan::hash_parts;
use sheltie_adapter::{Error, TreeFiles, Unit, Workspace};
use sheltie_core::{RepoPath, TargetId, TestId};

use crate::iface;
use crate::imports::{self, Import};
use crate::scanner;

/// Directories never scanned (besides every dot-directory).
const SKIP_DIRS: &[&str] = &["venv", "node_modules", "build", "dist", "__pycache__"];

/// Files that make a directory a project.
const PROJECT_FILES: &[&str] = &["pyproject.toml", "setup.py", "setup.cfg"];

/// Workspace-wide configuration, wherever it is (outside skipped directories).
const CONFIG_FILES: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "tox.ini",
    "noxfile.py",
    "pytest.ini",
    ".python-version",
    "Pipfile",
];

/// Dependency lockfiles, wherever they are.
const LOCKFILES: &[&str] = &["uv.lock", "poetry.lock", "Pipfile.lock", "pdm.lock"];

/// Python files at a source root that are build scripts, not importable modules.
const SCRIPTS: &[&str] = &["setup.py", "conftest.py", "noxfile.py"];

fn parent(path: &str) -> &str {
    path.rfind('/').map_or("", |i| &path[..i])
}

fn file_name(path: &str) -> &str {
    path.rfind('/').map_or(path, |i| &path[i + 1..])
}

fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_owned(),
        (_, true) => dir.to_owned(),
        _ => format!("{dir}/{rest}"),
    }
}

/// Whether `path` is `dir` or below it (every path is below the root, `""`).
fn within(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` relative to `dir` (which contains it).
fn relative<'a>(path: &'a str, dir: &str) -> &'a str {
    if dir.is_empty() {
        path
    } else {
        path.get(dir.len() + 1..).unwrap_or("")
    }
}

/// Whether any directory on the way to `path` is skipped.
fn skipped(path: &str) -> bool {
    let dir = parent(path);
    !dir.is_empty()
        && dir
            .split('/')
            .any(|d| d.starts_with('.') || SKIP_DIRS.contains(&d))
}

fn is_python(path: &str) -> bool {
    path.ends_with(".py") || path.ends_with(".pyi")
}

fn stem(path: &str) -> Option<&str> {
    let name = file_name(path);
    name.strip_suffix(".py")
        .or_else(|| name.strip_suffix(".pyi"))
}

/// A pytest module: `test_*.py` or `*_test.py`.
fn is_test_module(path: &str) -> bool {
    stem(path).is_some_and(|s| s.starts_with("test_") || s.ends_with("_test"))
}

/// A test module or `conftest.py`: never part of an interface.
fn is_test_file(path: &str) -> bool {
    is_test_module(path) || stem(path) == Some("conftest")
}

fn is_lockfile(name: &str) -> bool {
    LOCKFILES.contains(&name)
        || (name.ends_with(".txt")
            && (name.starts_with("requirements") || name.starts_with("constraints")))
}

/// The deepest of `dirs` containing `path`.
fn deepest<'a>(dirs: &'a BTreeSet<String>, path: &str) -> Option<&'a str> {
    dirs.iter()
        .filter(|d| within(path, d))
        .max_by_key(|d| d.len())
        .map(String::as_str)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A directory with `__init__.py`.
    Package,
    /// One module at a source root.
    Module,
    /// A directory of tests that is not a package.
    Tests,
}

#[derive(Debug)]
struct Draft {
    kind: Kind,
    /// Dotted import name (packages and modules) or `tests:<dir>`.
    name: String,
    /// The directory (for a module: the module's file, so it claims no directory).
    root: String,
    /// The directory imports are relative to (packages and modules).
    import_root: Option<String>,
    files: BTreeSet<String>,
}

/// Builds the workspace of one tree; `read` returns a file's text.
pub(crate) fn workspace(
    files: &TreeFiles,
    read: &dyn Fn(&str) -> Result<String, Error>,
) -> Result<Workspace, Error> {
    let paths: Vec<&str> = files
        .paths()
        .map(RepoPath::as_str)
        .filter(|p| !skipped(p))
        .collect();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for p in &paths {
        let mut d = parent(p);
        loop {
            if !dirs.insert(d.to_owned()) || d.is_empty() {
                break;
            }
            d = parent(d);
        }
    }

    let projects = projects(&paths, &dirs, read)?;
    let mut ws = Workspace::default();
    for p in &paths {
        let name = file_name(p);
        if CONFIG_FILES.contains(&name) || *p == "conftest.py" {
            ws.config.insert(RepoPath::new(*p));
        } else if is_lockfile(name) {
            ws.lockfiles.insert(RepoPath::new(*p));
        }
    }

    let source_roots: BTreeSet<String> = if projects.is_empty() {
        [String::new()].into()
    } else {
        projects
            .values()
            .flat_map(|p| p.source_roots.clone())
            .collect()
    };
    let mut import_roots = source_roots.clone();
    import_roots.extend(projects.keys().cloned());

    let mut drafts = discover(&paths, &dirs, &source_roots, &import_roots, files);
    drafts.sort_by(|a, b| a.name.cmp(&b.name).then(a.root.cmp(&b.root)));

    // Unique target ids: a dotted name used twice (two projects with a `utils` package)
    // gets its root appended; imports of the name read both.
    let mut by_name: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, d) in drafts.iter().enumerate() {
        by_name.entry(d.name.clone()).or_default().push(i);
    }
    let ids: Vec<String> = drafts
        .iter()
        .map(|d| {
            if by_name.get(&d.name).is_some_and(|v| v.len() > 1) {
                format!("{}@{}", d.name, d.root)
            } else {
                d.name.clone()
            }
        })
        .collect();

    let mut surfaces: Vec<Vec<String>> = Vec::with_capacity(drafts.len());
    let mut reexports: Vec<BTreeSet<usize>> = Vec::with_capacity(drafts.len());
    let mut reads: Vec<BTreeSet<usize>> = Vec::with_capacity(drafts.len());
    for (i, d) in drafts.iter().enumerate() {
        let mut parts = Vec::new();
        let mut exported = BTreeSet::new();
        let mut read_units = BTreeSet::new();
        for f in &d.files {
            if !is_python(f) {
                continue;
            }
            let text = read(f)?;
            let package = d.import_root.as_deref().map(|r| {
                let dir = relative(parent(f), r);
                dir.replace('/', ".")
            });
            let stmts = scanner::statements(&text);
            for imp in imports::imports(&stmts) {
                read_units.extend(resolve(&imp, package.as_deref(), &by_name));
            }
            if d.kind != Kind::Tests && !is_test_file(f) {
                let name = file_name(f);
                let s = iface::surface(&text, name == "__init__.py" || name == "__init__.pyi");
                parts.push(format!("module {}", relative(f, parent_dir_of_unit(d))));
                parts.extend(s.parts);
                for imp in &s.reexports {
                    exported.extend(resolve(imp, package.as_deref(), &by_name));
                }
            }
        }
        if d.kind == Kind::Tests {
            // Tests below read the conftest.py fixtures of tests above them.
            let conftest = join(&d.root, "conftest.py");
            parts.push(format!(
                "tests {}",
                files.oid(&conftest).map_or("-", String::as_str)
            ));
            for (j, other) in drafts.iter().enumerate() {
                if other.kind == Kind::Tests && j != i && within(&d.root, &other.root) {
                    read_units.insert(j);
                }
            }
        }
        exported.remove(&i);
        read_units.remove(&i);
        surfaces.push(parts);
        reexports.push(exported);
        reads.push(read_units);
    }

    let surface_hashes: Vec<String> = surfaces.iter().map(hash_parts).collect();
    let ifaces: Vec<String> = (0..drafts.len())
        .map(|i| interface(i, &surface_hashes, &reexports, &ids))
        .collect();

    // A unit's tests include the test directories importing it, which a body change
    // can break as well.
    let mut tests: Vec<BTreeSet<TestId>> = ids
        .iter()
        .map(|id| [TestId::new(id.clone())].into())
        .collect();
    for (i, d) in drafts.iter().enumerate() {
        if d.kind == Kind::Tests {
            for &j in &reads[i] {
                if drafts[j].kind != Kind::Tests {
                    tests[j].insert(TestId::new(ids[i].clone()));
                }
            }
        }
    }

    for (i, d) in drafts.iter().enumerate() {
        let project = projects
            .iter()
            .filter(|(dir, _)| within(&d.root, dir))
            .max_by_key(|(dir, _)| dir.len())
            .map(|(_, p)| p);
        let leaks = project
            .filter(|p| p.builds_extensions)
            .map(|_| vec!["setup.py builds extensions".to_owned()])
            .unwrap_or_default();
        let id = TargetId::new(ids[i].clone());
        ws.units.push(Unit {
            tests: std::mem::take(&mut tests[i]),
            id,
            root: d.root.clone(),
            files: d.files.iter().map(RepoPath::new).collect(),
            reads: reads[i]
                .iter()
                .map(|&j| TargetId::new(ids[j].clone()))
                .collect(),
            public_iface: ifaces[i].clone(),
            leaks,
        });
    }
    Ok(ws)
}

/// The directory module paths in a unit's interface are relative to.
fn parent_dir_of_unit(d: &Draft) -> &str {
    match d.kind {
        Kind::Module => parent(&d.root),
        _ => &d.root,
    }
}

/// A unit's interface hash: its own surface and those of every unit it re-exports,
/// directly or through other re-exports, so a change there reaches its dependents too.
/// Units re-exporting each other (a package and its subpackage) share their surfaces.
fn interface(
    i: usize,
    surfaces: &[String],
    reexports: &[BTreeSet<usize>],
    ids: &[String],
) -> String {
    let mut seen: BTreeSet<usize> = [i].into();
    let mut queue = vec![i];
    while let Some(u) = queue.pop() {
        for &j in &reexports[u] {
            if seen.insert(j) {
                queue.push(j);
            }
        }
    }
    let mut parts = vec![surfaces[i].clone()];
    let mut others: Vec<usize> = seen.into_iter().filter(|&j| j != i).collect();
    others.sort_by(|a, b| ids[*a].cmp(&ids[*b]));
    parts.extend(
        others
            .into_iter()
            .map(|j| format!("reexport {} {}", ids[j], surfaces[j])),
    );
    hash_parts(parts)
}

/// The units an import reads: the deepest unit its module names (`a.b.c`, else `a.b`, else
/// `a`) and, for `from a.b import c`, the unit `a.b.c` if there is one.
fn resolve(
    imp: &Import,
    package: Option<&str>,
    by_name: &BTreeMap<String, Vec<usize>>,
) -> Vec<usize> {
    let Some(module) = imports::absolute(imp, package) else {
        return Vec::new();
    };
    let mut out: Vec<usize> = imports::candidates(&module)
        .find_map(|c| by_name.get(c))
        .cloned()
        .unwrap_or_default();
    for name in &imp.names {
        if let Some(units) = by_name.get(&format!("{module}.{name}")) {
            out.extend(units);
        }
    }
    out
}

#[derive(Debug, Default)]
struct Project {
    source_roots: Vec<String>,
    builds_extensions: bool,
}

/// Every project directory, with its source roots.
fn projects(
    paths: &[&str],
    dirs: &BTreeSet<String>,
    read: &dyn Fn(&str) -> Result<String, Error>,
) -> Result<BTreeMap<String, Project>, Error> {
    let present: BTreeSet<&str> = paths.iter().copied().collect();
    let mut found: BTreeSet<String> = paths
        .iter()
        .filter(|p| PROJECT_FILES.contains(&file_name(p)))
        .map(|p| parent(p).to_owned())
        .collect();
    let mut explicit: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dir in found.clone() {
        let manifest = join(&dir, "pyproject.toml");
        if !present.contains(manifest.as_str()) {
            continue;
        }
        let Ok(toml) = read(&manifest)?.parse::<toml::Table>() else {
            continue; // Unreadable: still a project, and its manifest is configuration.
        };
        let tool = toml.get("tool");
        let uv = tool
            .and_then(|t| t.get("uv"))
            .and_then(|u| u.get("workspace"));
        let globs = |key: &str| -> Vec<String> {
            uv.and_then(|w| w.get(key))
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(|g| join(&dir, g.trim_end_matches('/')))
                .collect()
        };
        let members = glob_set(&globs("members"));
        let exclude = glob_set(&globs("exclude"));
        if let Some(members) = members {
            for d in dirs {
                if members.is_match(d) && !exclude.as_ref().is_some_and(|e| e.is_match(d)) {
                    found.insert(d.clone());
                }
            }
        }
        let setuptools = tool.and_then(|t| t.get("setuptools"));
        let mut roots: Vec<String> = Vec::new();
        if let Some(d) = setuptools
            .and_then(|s| s.get("package-dir"))
            .and_then(|p| p.get(""))
            .and_then(toml::Value::as_str)
        {
            roots.push(join(&dir, d.trim_matches('/')));
        }
        if let Some(wheres) = setuptools
            .and_then(|s| s.get("packages"))
            .and_then(|p| p.get("find"))
            .and_then(|f| f.get("where"))
            .and_then(toml::Value::as_array)
        {
            roots.extend(
                wheres
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(|w| join(&dir, w.trim_matches('/').trim_start_matches("./"))),
            );
        }
        roots.retain(|r| dirs.contains(r));
        if !roots.is_empty() {
            explicit.insert(dir, roots);
        }
    }
    let mut out = BTreeMap::new();
    for dir in found {
        let src = join(&dir, "src");
        let source_roots = explicit.remove(&dir).unwrap_or_else(|| {
            if dirs.contains(&src) {
                vec![src]
            } else {
                vec![dir.clone()]
            }
        });
        let setup = join(&dir, "setup.py");
        let builds_extensions = if present.contains(setup.as_str()) {
            let text = read(&setup)?;
            text.contains("ext_modules") || text.contains("cmdclass")
        } else {
            false
        };
        out.insert(
            dir,
            Project {
                source_roots,
                builds_extensions,
            },
        );
    }
    Ok(out)
}

fn glob_set(globs: &[String]) -> Option<globset::GlobSet> {
    if globs.is_empty() {
        return None;
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(glob) = globset::GlobBuilder::new(g).literal_separator(true).build() {
            b.add(glob);
        }
    }
    b.build().ok()
}

/// Packages, single modules and test directories, with their files.
fn discover(
    paths: &[&str],
    dirs: &BTreeSet<String>,
    source_roots: &BTreeSet<String>,
    import_roots: &BTreeSet<String>,
    files: &TreeFiles,
) -> Vec<Draft> {
    let mut drafts: Vec<Draft> = Vec::new();
    // Directory -> index of the package or test unit rooted there.
    let mut at_dir: BTreeMap<String, usize> = BTreeMap::new();

    for dir in dirs {
        let init = join(dir, "__init__.py");
        if !files.contains(&init) && !files.contains(&join(dir, "__init__.pyi")) {
            continue;
        }
        let Some(root) = deepest(import_roots, dir).filter(|r| r != dir) else {
            continue;
        };
        at_dir.insert(dir.clone(), drafts.len());
        drafts.push(Draft {
            kind: Kind::Package,
            name: relative(dir, root).replace('/', "."),
            root: dir.clone(),
            import_root: Some(root.to_owned()),
            files: BTreeSet::new(),
        });
    }

    // Single modules: Python files directly at a source root.
    let mut module_files: BTreeSet<&str> = BTreeSet::new();
    let mut modules: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for p in paths {
        let dir = parent(p);
        let name = file_name(p);
        if !source_roots.contains(dir)
            || !is_python(p)
            || is_test_file(p)
            || SCRIPTS.contains(&name)
            || name.starts_with("__init__.")
        {
            continue;
        }
        let stem = name.trim_end_matches(".pyi").trim_end_matches(".py");
        module_files.insert(p);
        modules
            .entry((dir.to_owned(), stem.to_owned()))
            .or_default()
            .insert((*p).to_owned());
    }
    for ((dir, stem), group) in modules {
        let root = group
            .iter()
            .find(|f| f.ends_with(".py"))
            .or_else(|| group.iter().next())
            .cloned()
            .unwrap_or_default();
        drafts.push(Draft {
            kind: Kind::Module,
            name: stem,
            root,
            import_root: Some(dir),
            files: group,
        });
    }

    // Test directories: named tests/test or holding test files, and not packages.
    let mut python_in: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for p in paths.iter().copied().filter(|p| is_python(p)) {
        python_in.entry(parent(p)).or_default().push(p);
    }
    for dir in dirs {
        if at_dir.contains_key(dir) {
            continue;
        }
        let direct: &[&str] = python_in.get(dir.as_str()).map_or(&[], Vec::as_slice);
        let named = matches!(file_name(dir), "tests" | "test");
        if direct.is_empty() || !(named || direct.iter().any(|p| is_test_module(p))) {
            continue;
        }
        // At the repository root the directory would claim every stray file; use the
        // first test file instead, which claims none.
        let root = if dir.is_empty() {
            direct
                .iter()
                .find(|p| is_test_module(p))
                .copied()
                .unwrap_or(direct[0])
                .to_owned()
        } else {
            dir.clone()
        };
        at_dir.insert(dir.clone(), drafts.len());
        drafts.push(Draft {
            kind: Kind::Tests,
            name: format!("tests:{}", if dir.is_empty() { "." } else { dir }),
            root,
            import_root: None,
            files: BTreeSet::new(),
        });
    }

    // Files of packages and test directories: Python files (and `py.typed`) in the
    // directory or in subdirectories that are no unit of their own, without crossing an
    // import root.
    for p in paths {
        // The root conftest.py is workspace configuration; setup.py and noxfile.py are
        // configuration wherever they are.
        if module_files.contains(p)
            || *p == "conftest.py"
            || (SCRIPTS.contains(&file_name(p)) && !is_test_file(p))
        {
            continue;
        }
        let owned = is_python(p) || file_name(p) == "py.typed";
        if !owned {
            continue;
        }
        let mut dir = parent(p);
        loop {
            if let Some(&i) = at_dir.get(dir) {
                // A test directory that is also an import root (tests next to modules)
                // owns only its own files, not every stray script below it.
                let tests_ok = dir == parent(p) || !import_roots.contains(dir);
                match drafts[i].kind {
                    Kind::Package => {
                        drafts[i].files.insert((*p).to_owned());
                    }
                    Kind::Tests if is_python(p) && tests_ok => {
                        drafts[i].files.insert((*p).to_owned());
                    }
                    _ => {}
                }
                break;
            }
            if dir.is_empty() || import_roots.contains(dir) {
                break;
            }
            dir = parent(dir);
        }
    }
    drafts.retain(|d| !d.files.is_empty());
    drafts
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheltie_adapter::scan::sha256;

    fn build(tree: &[(&str, &str)]) -> Workspace {
        let files = TreeFiles(
            tree.iter()
                .map(|(p, t)| (RepoPath::new(*p), sha256(t)))
                .collect(),
        );
        let texts: BTreeMap<&str, &str> = tree.iter().copied().collect();
        let read = |p: &str| -> Result<String, Error> {
            texts
                .get(p)
                .map(|t| (*t).to_owned())
                .ok_or_else(|| Error::Io(format!("no {p}")))
        };
        workspace(&files, &read).unwrap()
    }

    fn unit<'a>(ws: &'a Workspace, id: &str) -> &'a Unit {
        ws.units
            .iter()
            .find(|u| u.id.as_str() == id)
            .unwrap_or_else(|| panic!("no unit {id}: {:?}", ids(ws)))
    }

    fn ids(ws: &Workspace) -> Vec<&str> {
        ws.units.iter().map(|u| u.id.as_str()).collect()
    }

    fn reads(u: &Unit) -> Vec<&str> {
        u.reads.iter().map(TargetId::as_str).collect()
    }

    fn files(u: &Unit) -> Vec<&str> {
        u.files.iter().map(RepoPath::as_str).collect()
    }

    const UV: &[(&str, &str)] = &[
        (
            "pyproject.toml",
            "[project]\nname = \"ws\"\n[tool.uv.workspace]\nmembers = [\"packages/*\"]\nexclude = [\"packages/old\"]\n",
        ),
        ("uv.lock", "version = 1\n"),
        ("requirements-dev.txt", "pytest\n"),
        ("conftest.py", ""),
        ("README.md", "hi\n"),
        (
            "packages/core/pyproject.toml",
            "[project]\nname = \"core\"\n",
        ),
        (
            "packages/core/src/shop/__init__.py",
            "from .pricing import price\nfrom .cart import Cart\n",
        ),
        (
            "packages/core/src/shop/pricing.py",
            "def price(x):\n    return x\n",
        ),
        ("packages/core/src/shop/py.typed", ""),
        ("packages/core/src/shop/data.json", "{}"),
        (
            "packages/core/src/shop/test_pricing.py",
            "from shop import pricing\n",
        ),
        (
            "packages/core/src/shop/cart/__init__.py",
            "from ..pricing import price\nclass Cart: ...\n",
        ),
        ("packages/core/src/shop/cart/impl/fast.py", "import json\n"),
        ("packages/core/tests/conftest.py", "import pytest\n"),
        (
            "packages/core/tests/test_cart.py",
            "from shop.cart import Cart\n",
        ),
        ("packages/core/tests/unit/test_x.py", "import shop\n"),
        ("packages/core/tests/data/in.json", "{}"),
        ("packages/web/pyproject.toml", "[project]\nname = \"web\"\n"),
        (
            "packages/web/server.py",
            "from shop import cart, pricing\nimport shop.cart.impl\n",
        ),
        ("packages/web/setup.py", "setup(ext_modules=[])\n"),
        ("packages/web/.venv/lib/x/__init__.py", ""),
        ("packages/web/node_modules/y/__init__.py", ""),
        ("packages/old/z.py", ""),
    ];

    #[test]
    fn uv_workspace_units() {
        let ws = build(UV);
        assert_eq!(
            ids(&ws),
            [
                "server",
                "shop",
                "shop.cart",
                "tests:packages/core/tests",
                "tests:packages/core/tests/unit"
            ]
        );
        let shop = unit(&ws, "shop");
        assert_eq!(shop.root, "packages/core/src/shop");
        assert_eq!(
            files(shop),
            [
                "packages/core/src/shop/__init__.py",
                "packages/core/src/shop/pricing.py",
                "packages/core/src/shop/py.typed",
                "packages/core/src/shop/test_pricing.py",
            ]
        );
        let cart = unit(&ws, "shop.cart");
        assert_eq!(
            files(cart),
            [
                "packages/core/src/shop/cart/__init__.py",
                "packages/core/src/shop/cart/impl/fast.py"
            ]
        );
        assert_eq!(reads(cart), ["shop"]);
        assert_eq!(reads(shop), ["shop.cart"]);
        // `from shop import cart, pricing` reads shop and the shop.cart unit.
        let server = unit(&ws, "server");
        assert_eq!(server.root, "packages/web/server.py");
        assert_eq!(reads(server), ["shop", "shop.cart"]);
        assert_eq!(server.leaks, ["setup.py builds extensions"]);
        assert!(shop.leaks.is_empty());

        let tests = unit(&ws, "tests:packages/core/tests");
        assert_eq!(
            files(tests),
            [
                "packages/core/tests/conftest.py",
                "packages/core/tests/test_cart.py"
            ]
        );
        assert_eq!(reads(tests), ["shop.cart"]);
        assert_eq!(
            reads(unit(&ws, "tests:packages/core/tests/unit")),
            ["shop", "tests:packages/core/tests"]
        );

        let config: Vec<&str> = ws.config.iter().map(RepoPath::as_str).collect();
        assert_eq!(
            config,
            [
                "conftest.py",
                "packages/core/pyproject.toml",
                "packages/web/pyproject.toml",
                "packages/web/setup.py",
                "pyproject.toml"
            ]
        );
        let locks: Vec<&str> = ws.lockfiles.iter().map(RepoPath::as_str).collect();
        assert_eq!(locks, ["requirements-dev.txt", "uv.lock"]);
        // A data file in a package directory falls to the package (as `<id>/files`).
        assert_eq!(
            ws.unit_of("packages/core/src/shop/data.json")
                .map(|u| u.id.as_str()),
            Some("shop")
        );
        assert_eq!(ws.unit_of("packages/web/notes.txt"), None);
    }

    #[test]
    fn reexported_interfaces_fold_in() {
        let base = build(UV);
        let mut changed: Vec<(&str, &str)> = UV.to_vec();
        for f in &mut changed {
            if f.0 == "packages/core/src/shop/cart/__init__.py" {
                f.1 = "from ..pricing import price\nclass Cart:\n    def add(self): ...\n";
            }
        }
        let head = build(&changed);
        let iface = |ws: &Workspace, id: &str| unit(ws, id).public_iface.clone();
        assert_ne!(iface(&base, "shop.cart"), iface(&head, "shop.cart"));
        // shop re-exports Cart from shop.cart.
        assert_ne!(iface(&base, "shop"), iface(&head, "shop"));
        assert_eq!(iface(&base, "server"), iface(&head, "server"));
    }

    #[test]
    fn flat_layout_without_projects() {
        let ws = build(&[
            ("app.py", "import util\nfrom lib.sub import thing\n"),
            ("util.py", ""),
            ("test_app.py", "import app\n"),
            ("lib/__init__.py", ""),
            ("lib/sub/__init__.py", "from .. import helpers\n"),
            ("lib/helpers.py", ""),
            ("scripts/run.py", ""),
        ]);
        assert_eq!(ids(&ws), ["app", "lib", "lib.sub", "tests:.", "util"]);
        assert_eq!(reads(unit(&ws, "app")), ["lib.sub", "util"]);
        assert_eq!(reads(unit(&ws, "lib.sub")), ["lib"]);
        let t = unit(&ws, "tests:.");
        assert_eq!(t.root, "test_app.py");
        assert_eq!(files(t), ["test_app.py"]);
        // Nothing claims the repository root.
        assert_eq!(ws.unit_of(".github/ci.yml"), None);
        assert_eq!(ws.unit_of("scripts/run.py"), None);
    }

    #[test]
    fn duplicate_names_get_their_root() {
        let ws = build(&[
            ("a/pyproject.toml", ""),
            ("a/src/utils/__init__.py", ""),
            ("b/pyproject.toml", ""),
            ("b/utils/__init__.py", ""),
            ("b/main.py", "import utils\n"),
        ]);
        assert_eq!(ids(&ws), ["main", "utils@a/src/utils", "utils@b/utils"]);
        assert_eq!(
            reads(unit(&ws, "main")),
            ["utils@a/src/utils", "utils@b/utils"]
        );
    }

    #[test]
    fn setuptools_package_dir() {
        let ws = build(&[
            (
                "pyproject.toml",
                "[tool.setuptools.packages.find]\nwhere = [\"lib\"]\n",
            ),
            ("lib/acme/__init__.py", ""),
            ("lib/acme/core.py", "def f(): ...\n"),
        ]);
        assert_eq!(ids(&ws), ["acme"]);
    }
}
