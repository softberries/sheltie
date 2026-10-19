//! JavaScript and TypeScript adapter for Sheltie: npm, Yarn, pnpm and Bun workspaces, Lerna
//! and Nx.
//!
//! Reads the build graph straight from git blobs, without Node.js or a package manager:
//!
//! - every workspace package (globs from the root `package.json` `workspaces`,
//!   `pnpm-workspace.yaml` or `lerna.json`) and every Nx project (a directory with a
//!   `project.json`) is a unit; with neither, a root `package.json` makes the whole
//!   repository one unit;
//! - a unit reads the workspace packages it depends on (dependency fields, Nx
//!   `implicitDependencies`, and imports of other units by package name, `tsconfig` path
//!   alias or relative path);
//! - its public interface is the published `package.json` fields plus the declarations
//!   dependents can see, extracted line by line from its sources ([`Node`] has the rules);
//! - workspace configuration (`tsconfig.base.json`, `nx.json`, …) is `$workspace` and
//!   lockfiles are `$lockfile`, both read by every unit.
//!
//! [`sheltie_adapter::Generic`] runs the pipeline; the `sheltie-node` binary is its command
//! line.

use std::collections::{BTreeMap, BTreeSet};

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde_json::Value;
use sheltie_adapter::policy::COMMON_PLAIN;
use sheltie_adapter::scan::{hash_parts, normalize};
use sheltie_adapter::{DEFAULT_IGNORE, Error, Git, PathPolicy, Tool, TreeFiles, Unit, Workspace};
use sheltie_core::{RepoPath, TargetId, TestId};

mod manifest;
mod paths;
mod source;

/// Workspace-wide configuration files at the repository root (the `$workspace` target).
pub const CONFIG: &[&str] = &[
    "package.json",
    "tsconfig.json",
    "tsconfig.base.json",
    "nx.json",
    "pnpm-workspace.yaml",
    "lerna.json",
    "turbo.json",
    ".npmrc",
    ".nvmrc",
    ".node-version",
    ".yarnrc",
    ".yarnrc.yml",
    ".pnpmfile.cjs",
];

/// Dependency lockfiles at the repository root (the `$lockfile` target).
pub const LOCKFILES: &[&str] = &[
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "bun.lock",
];

/// Lint and format configuration: tracked (two changes to one conflict) but outside the
/// build.
pub const NODE_PLAIN: &[&str] = &[
    ".eslintrc*",
    "**/.eslintrc*",
    "eslint.config.*",
    "**/eslint.config.*",
    ".prettierrc*",
    "**/.prettierrc*",
    "prettier.config.*",
    "**/prettier.config.*",
    ".prettierignore",
    "**/.prettierignore",
    ".eslintignore",
    "**/.eslintignore",
    ".editorconfig",
    "**/.editorconfig",
];

/// Directories inside a unit that hold dependencies or build output, never sources.
pub const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "out",
    "coverage",
    ".next",
    ".turbo",
];

/// The JavaScript/TypeScript build tool: npm, Yarn, pnpm and Bun workspaces, Lerna and Nx.
///
/// A unit's public interface hashes:
///
/// 1. the `package.json` fields `name`, `version`, `main`, `module`, `types`, `typings`,
///    `exports`, `bin`, `type`, `dependencies` and `peerDependencies`;
/// 2. from its non-test sources (`.ts .tsx .mts .cts .js .jsx .mjs .cjs`), without comments
///    and whitespace differences: every `.d.ts` file whole; every top-level `export`
///    statement; every top-level `type`, `interface`, `enum`, `declare` and `class`, exported
///    or not; declarations exported by name (`export { f }`, `module.exports = { f }`). Of
///    functions and methods only the signature counts when the body can be told apart;
///    otherwise the whole statement does.
#[derive(Debug, Clone, Copy, Default)]
pub struct Node;

impl Tool for Node {
    const NAME: &'static str = "sheltie-node";
    const VERSION: &'static str = concat!("sheltie-node/", env!("CARGO_PKG_VERSION"));
    const PLAIN: &'static [&'static str] = NODE_PLAIN;

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        build(files, |path| match files.oid(path) {
            Some(oid) => git.text(oid),
            None => Err(Error::Io(format!("{path} is not in the tree"))),
        })
    }
}

/// The path policy the command line uses by default: default ignores, common and Node plain
/// files.
pub fn default_policy() -> Result<PathPolicy, Error> {
    let plain: Vec<&str> = COMMON_PLAIN.iter().chain(NODE_PLAIN).copied().collect();
    PathPolicy::new(DEFAULT_IGNORE, &plain)
}

/// A unit before its files and interface are known.
struct Found {
    dir: String,
    pkg: Option<Result<Value, String>>,
    project: Option<Value>,
}

/// The workspace of one tree; `read` returns a file's text.
fn build(
    files: &TreeFiles,
    read: impl Fn(&str) -> Result<String, Error>,
) -> Result<Workspace, Error> {
    let json = |path: &str| -> Result<Option<Value>, Error> {
        if !files.contains(path) {
            return Ok(None);
        }
        serde_json::from_str(strip_bom(&read(path)?))
            .map(Some)
            .map_err(|e| Error::Tool(format!("{path}: {e}")))
    };
    let root_pkg = json("package.json")?;

    let mut globs: Vec<String> = root_pkg
        .as_ref()
        .map(manifest::npm_workspaces)
        .unwrap_or_default();
    if files.contains("pnpm-workspace.yaml") {
        globs.extend(manifest::pnpm_packages(&read("pnpm-workspace.yaml")?));
    }
    if let Some(lerna) = json("lerna.json")? {
        globs.extend(manifest::lerna_packages(&lerna));
    }
    let (include, exclude) = glob_sets(&globs)?;

    let mut dirs: BTreeMap<String, Found> = BTreeMap::new();
    for path in files.paths().map(RepoPath::as_str) {
        let dir = paths::parent(path);
        if dir.split('/').any(|s| s == "node_modules") {
            continue;
        }
        let file = path.rsplit('/').next().unwrap_or(path);
        let is_package = file == "package.json"
            && !dir.is_empty()
            && include.is_match(dir)
            && !exclude.is_match(dir);
        if is_package || file == "project.json" {
            dirs.entry(dir.to_owned()).or_insert_with(|| Found {
                dir: dir.to_owned(),
                pkg: None,
                project: None,
            });
        }
    }
    if dirs.is_empty() && root_pkg.is_some() {
        dirs.insert(
            String::new(),
            Found {
                dir: String::new(),
                pkg: None,
                project: None,
            },
        );
    }
    for found in dirs.values_mut() {
        let at = |name: &str| {
            if found.dir.is_empty() {
                name.to_owned()
            } else {
                format!("{}/{name}", found.dir)
            }
        };
        let pkg_path = at("package.json");
        if files.contains(&pkg_path) {
            let text = read(&pkg_path)?;
            let parsed = serde_json::from_str(strip_bom(&text));
            found.pkg = Some(parsed.map_err(|_| text));
        }
        let project_path = at("project.json");
        if files.contains(&project_path) {
            // A broken project.json only loses its name and implicit dependencies.
            found.project = manifest::parse_jsonc(&read(&project_path)?);
        }
    }
    let found: Vec<Found> = dirs.into_values().collect();

    // Ids: package name, else Nx project name, else the directory. Both names resolve.
    let mut ids: Vec<String> = Vec::new();
    let mut by_name: BTreeMap<String, usize> = BTreeMap::new();
    for (i, f) in found.iter().enumerate() {
        let pkg_name = f
            .pkg
            .as_ref()
            .and_then(|p| p.as_ref().ok())
            .and_then(manifest::name);
        let project_name = f.project.as_ref().and_then(manifest::name);
        let dir_id = if f.dir.is_empty() { "." } else { &f.dir };
        let mut id = pkg_name.or(project_name).unwrap_or(dir_id).to_owned();
        if ids.contains(&id) {
            id = dir_id.to_owned();
        }
        for n in [pkg_name, project_name].into_iter().flatten() {
            by_name.entry(n.to_owned()).or_insert(i);
        }
        ids.push(id);
    }

    let mut aliases: Vec<(String, Vec<String>)> = Vec::new();
    for tsconfig in ["tsconfig.base.json", "tsconfig.json"] {
        if files.contains(tsconfig)
            && let Some(v) = manifest::parse_jsonc(&read(tsconfig)?)
        {
            aliases.extend(manifest::tsconfig_paths(&v, ""));
        }
    }

    let policy = default_policy()?;
    let mut config = BTreeSet::new();
    let mut lockfiles = BTreeSet::new();
    let mut owned: Vec<BTreeSet<RepoPath>> = vec![BTreeSet::new(); found.len()];
    for path in files.paths() {
        let p = path.as_str();
        if CONFIG.contains(&p) {
            config.insert(path.clone());
            continue;
        }
        if LOCKFILES.contains(&p) {
            lockfiles.insert(path.clone());
            continue;
        }
        if policy.is_ignored(path) || policy.is_plain(path) {
            continue;
        }
        let Some(i) = unit_of(&found, p) else {
            continue;
        };
        let rel = paths::relative(p, &found[i].dir);
        if rel.split('/').any(|s| EXCLUDED_DIRS.contains(&s)) {
            continue;
        }
        owned[i].insert(path.clone());
    }

    let mut units = Vec::with_capacity(found.len());
    for (i, f) in found.iter().enumerate() {
        let mut reads: BTreeSet<TargetId> = BTreeSet::new();
        let mut leaks = Vec::new();
        let mut iface = Vec::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        match &f.pkg {
            Some(Ok(pkg)) => {
                names.extend(manifest::dependency_names(pkg));
                names.extend(manifest::implicit_dependencies(pkg));
                if !manifest::install_scripts(pkg).is_empty() {
                    leaks.push("install script".to_owned());
                }
                iface.push(manifest::iface_fields(pkg));
            }
            Some(Err(text)) => {
                leaks.push("package.json is not valid JSON".to_owned());
                iface.push(format!("invalid package.json: {}", normalize(text)));
            }
            None => iface.push("no package.json".to_owned()),
        }
        if let Some(project) = &f.project {
            names.extend(manifest::implicit_dependencies(project));
        }
        for name in &names {
            if let Some(&j) = by_name.get(name) {
                reads.insert(TargetId::new(ids[j].clone()));
            }
        }

        for path in &owned[i] {
            let p = path.as_str();
            if !source::is_source(p) {
                continue;
            }
            let text = read(p)?;
            for spec in source::imports(&text) {
                if let Some(j) = resolve(&spec, p, &found, &by_name, &aliases) {
                    reads.insert(TargetId::new(ids[j].clone()));
                }
            }
            if source::is_test(paths::relative(p, &f.dir)) {
                continue;
            }
            let parts = source::interface(p, &text);
            if !parts.is_empty() {
                iface.push(format!("file {p}"));
                iface.extend(parts);
            }
        }
        reads.remove(&TargetId::new(ids[i].clone()));
        units.push(Unit {
            id: TargetId::new(ids[i].clone()),
            root: f.dir.clone(),
            files: std::mem::take(&mut owned[i]),
            reads,
            public_iface: hash_parts(iface),
            tests: [TestId::new(ids[i].clone())].into(),
            leaks,
        });
    }
    Ok(Workspace {
        units,
        config,
        lockfiles,
    })
}

/// Include and exclude (`!pattern`) sets of workspace globs.
fn glob_sets(globs: &[String]) -> Result<(GlobSet, GlobSet), Error> {
    let mut include = GlobSetBuilder::new();
    let mut exclude = GlobSetBuilder::new();
    for g in globs {
        let (set, pattern) = match g.trim().strip_prefix('!') {
            Some(p) => (&mut exclude, p),
            None => (&mut include, g.trim()),
        };
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        if pattern.is_empty() {
            continue;
        }
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|e| Error::Config(format!("workspace glob {g:?}: {e}")))?;
        set.add(glob);
    }
    let build = |b: GlobSetBuilder| b.build().map_err(|e| Error::Config(e.to_string()));
    Ok((build(include)?, build(exclude)?))
}

/// The unit whose directory most deeply contains `path`.
fn unit_of(found: &[Found], path: &str) -> Option<usize> {
    found
        .iter()
        .enumerate()
        .filter(|(_, f)| paths::within(path, &f.dir))
        .max_by_key(|(_, f)| f.dir.len())
        .map(|(i, _)| i)
}

/// The unit a module specifier imported from `file` points into: a relative path, a
/// workspace package name (or a subpath of one), or a `tsconfig` path alias.
fn resolve(
    spec: &str,
    file: &str,
    found: &[Found],
    by_name: &BTreeMap<String, usize>,
    aliases: &[(String, Vec<String>)],
) -> Option<usize> {
    if spec.starts_with('.') {
        let target = paths::join(paths::parent(file), spec)?;
        return unit_of(found, &target);
    }
    let segments: Vec<&str> = spec.split('/').collect();
    let package = if spec.starts_with('@') {
        segments.get(..2).map(|s| s.join("/"))
    } else {
        segments.first().map(|s| (*s).to_owned())
    };
    if let Some(&i) = package.as_ref().and_then(|p| by_name.get(p)) {
        return Some(i);
    }
    for (alias, targets) in aliases {
        let star = match alias.split_once('*') {
            Some((pre, post)) => spec
                .strip_prefix(pre)
                .and_then(|r| r.strip_suffix(post))
                .map(str::to_owned),
            None => (alias == spec).then(String::new),
        };
        let Some(star) = star else { continue };
        for t in targets {
            if let Some(i) = unit_of(found, &t.replace('*', &star)).filter(|&i| {
                // An alias resolving to the root unit says nothing.
                !found[i].dir.is_empty()
            }) {
                return Some(i);
            }
        }
    }
    None
}

/// The text without a leading UTF-8 byte order mark, which Node accepts in `package.json`.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(tree: &[(&str, &str)]) -> Workspace {
        let files = TreeFiles(
            tree.iter()
                .map(|(p, _)| (RepoPath::new(*p), format!("oid-{p}")))
                .collect(),
        );
        let text: BTreeMap<&str, &str> = tree.iter().copied().collect();
        build(&files, |p| {
            text.get(p)
                .map(|t| (*t).to_owned())
                .ok_or_else(|| Error::Io(p.to_owned()))
        })
        .unwrap()
    }

    fn units(w: &Workspace) -> Vec<(&str, &str, Vec<&str>)> {
        w.units
            .iter()
            .map(|u| {
                (
                    u.id.as_str(),
                    u.root.as_str(),
                    u.reads.iter().map(TargetId::as_str).collect(),
                )
            })
            .collect()
    }

    fn unit<'a>(w: &'a Workspace, id: &str) -> &'a Unit {
        w.units.iter().find(|u| u.id.as_str() == id).unwrap()
    }

    #[test]
    fn package_json_with_a_byte_order_mark_is_read() {
        let w = ws(&[
            (
                "package.json",
                "\u{feff}{ \"workspaces\": [\"packages/*\"] }",
            ),
            ("packages/a/package.json", "\u{feff}{ \"name\": \"a\" }"),
            ("packages/a/index.js", ""),
        ]);
        assert_eq!(units(&w), [("a", "packages/a", vec![])]);
        assert!(unit(&w, "a").leaks.is_empty());
    }

    #[test]
    fn npm_workspaces_and_dependency_edges() {
        let w = ws(&[
            ("package.json", r#"{ "workspaces": ["packages/*"] }"#),
            ("package-lock.json", "{}"),
            ("packages/a/package.json", r#"{ "name": "a" }"#),
            (
                "packages/b/package.json",
                r#"{ "name": "@x/b", "devDependencies": { "a": "workspace:^", "left-pad": "1" } }"#,
            ),
            ("packages/b/src/index.ts", "export const b = 1;\n"),
            ("packages/b/dist/index.js", "exports.b = 1;\n"),
            ("packages/b/node_modules/a/index.js", ""),
            ("packages/b/README.md", "# b\n"),
            ("packages/b/.eslintrc.json", "{}"),
            ("tools/package.json", r#"{ "name": "tools" }"#),
        ]);
        assert_eq!(
            units(&w),
            [
                ("a", "packages/a", vec![]),
                ("@x/b", "packages/b", vec!["a"])
            ]
        );
        let b: Vec<&str> = unit(&w, "@x/b")
            .files
            .iter()
            .map(RepoPath::as_str)
            .collect();
        assert_eq!(b, ["packages/b/package.json", "packages/b/src/index.ts"]);
        assert_eq!(
            w.config.iter().map(RepoPath::as_str).collect::<Vec<_>>(),
            ["package.json"]
        );
        assert_eq!(w.lockfiles.len(), 1);
    }

    #[test]
    fn workspaces_object_pnpm_negation_and_lerna() {
        let w = ws(&[
            (
                "package.json",
                r#"{ "workspaces": { "packages": ["libs/*"] } }"#,
            ),
            ("libs/one/package.json", r#"{ "name": "one" }"#),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'apps/**'\n  - '!apps/legacy/**'\n",
            ),
            (
                "apps/web/package.json",
                r#"{ "name": "web", "dependencies": { "one": "*" } }"#,
            ),
            ("apps/legacy/old/package.json", r#"{ "name": "old" }"#),
            ("lerna.json", r#"{ "packages": ["modules/*"] }"#),
            ("modules/m/package.json", "{}"),
        ]);
        assert_eq!(
            units(&w),
            [
                ("web", "apps/web", vec!["one"]),
                ("one", "libs/one", vec![]),
                ("modules/m", "modules/m", vec![]),
            ]
        );
    }

    #[test]
    fn nx_projects_imports_and_aliases() {
        let w = ws(&[
            (
                "package.json",
                r#"{ "name": "repo", "scripts": { "prepare": "husky" } }"#,
            ),
            ("nx.json", "{}"),
            (
                "tsconfig.base.json",
                "{ \"compilerOptions\": { \"paths\": { \"@repo/shared\": [\"libs/shared/src/index.ts\"] } } }",
            ),
            ("libs/shared/project.json", r#"{ "name": "shared" }"#),
            ("libs/shared/src/index.ts", "export const s = 1;\n"),
            ("libs/data/project.json", r#"{ "name": "data" }"#),
            (
                "libs/data/src/index.ts",
                "import { s } from '../../shared/src';\nexport const d = s;\n",
            ),
            (
                "apps/shop/project.json",
                r#"{ "name": "shop", "implicitDependencies": ["data", "!shared"] }"#,
            ),
            (
                "apps/shop/package.json",
                r#"{ "name": "@repo/shop", "scripts": { "postinstall": "x" } }"#,
            ),
            (
                "apps/shop/src/main.ts",
                "import { s } from '@repo/shared';\n",
            ),
        ]);
        assert_eq!(
            units(&w),
            [
                ("@repo/shop", "apps/shop", vec!["data", "shared"]),
                ("data", "libs/data", vec!["shared"]),
                ("shared", "libs/shared", vec![]),
            ]
        );
        assert_eq!(unit(&w, "@repo/shop").leaks, ["install script"]);
        assert!(unit(&w, "data").leaks.is_empty());
        assert_eq!(w.config.len(), 3);
    }

    #[test]
    fn a_plain_package_is_one_unit() {
        let w = ws(&[
            ("package.json", r#"{ "name": "app", "main": "index.js" }"#),
            ("yarn.lock", ""),
            ("index.js", "module.exports = function () {};\n"),
            ("test/index.test.js", ""),
            (".github/workflows/ci.yml", ""),
        ]);
        assert_eq!(units(&w), [("app", "", vec![])]);
        let files: Vec<&str> = w.units[0].files.iter().map(RepoPath::as_str).collect();
        assert_eq!(files, ["index.js", "test/index.test.js"]);
        assert!(ws(&[("src/a.ts", "")]).units.is_empty());
    }

    #[test]
    fn interface_ignores_tests_and_bodies() {
        let tree = |index: &str, test: &str| {
            let w = ws(&[
                ("package.json", r#"{ "name": "p", "version": "1.0.0" }"#),
                ("src/index.ts", index),
                ("src/index.test.ts", test),
            ]);
            w.units[0].public_iface.clone()
        };
        let base = tree("export function f() {\n  return 1;\n}\n", "test('x')");
        assert_eq!(
            base,
            tree("export function f() {\n  return 2;\n}\n", "test('y')")
        );
        assert_ne!(
            base,
            tree("export function f(a = 1) {\n  return 1;\n}\n", "test('x')")
        );
    }

    #[test]
    fn invalid_root_manifest_is_an_error_and_package_manifest_a_leak() {
        let files = TreeFiles([(RepoPath::new("package.json"), "o".to_owned())].into());
        assert!(build(&files, |_| Ok("{".to_owned())).is_err());
        let w = ws(&[
            ("package.json", r#"{ "workspaces": ["p/*"] }"#),
            ("p/a/package.json", "{ broken"),
        ]);
        assert_eq!(w.units[0].id.as_str(), "p/a");
        assert_eq!(w.units[0].leaks, ["package.json is not valid JSON"]);
    }
}
