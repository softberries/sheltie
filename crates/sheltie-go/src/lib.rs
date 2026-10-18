//! Go build-graph adapter for Sheltie (library and `sheltie-go` CLI).
//!
//! Reads a tree straight from git, without the `go` toolchain:
//!
//! - every `go.mod` is a module (its path from the `module` directive);
//! - every directory with `.go` files is a package, the unit Sheltie schedules, identified by
//!   its import path;
//! - a package reads the packages of the repository it imports (test files included);
//! - its public interface is the package clause, build constraints, exported function and
//!   method signatures, exported types, and exported constants and variables with their
//!   values, plus the unexported types and constants those mention. Bodies, comments and
//!   formatting do not count, so a body-only change does not reach dependents;
//! - `go.mod` and `go.work` files are workspace configuration; `go.sum`, `go.work.sum` and
//!   vendored code are lockfiles;
//! - cgo (`import "C"`) and `//go:embed` make a package non-hermetic.
//!
//! See `docs/go.md` for what this does and does not claim.

use std::collections::BTreeMap;

use sheltie_adapter::scan::{hash_parts, strip_c_comments};
use sheltie_adapter::{Error, Git, Tool, TreeFiles, Unit, Workspace};
use sheltie_core::{RepoPath, TargetId, TestId};

mod source;

/// The Go build tool, for [`sheltie_adapter::Generic`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Go;

impl Tool for Go {
    const NAME: &'static str = "sheltie-go";
    const VERSION: &'static str = concat!("sheltie-go/", env!("CARGO_PKG_VERSION"));
    const PLAIN: &'static [&'static str] = &[".golangci.yml", ".golangci.yaml"];

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        let mut ws = Workspace::default();
        // Module root directory -> module path.
        let mut modules: BTreeMap<String, String> = BTreeMap::new();
        // Package directory -> its .go files.
        let mut packages: BTreeMap<&str, Vec<(&RepoPath, &str)>> = BTreeMap::new();
        for (path, oid) in &files.0 {
            let (dir, name) = split(path.as_str());
            if dir.split('/').any(|s| s == "vendor") {
                ws.lockfiles.insert(path.clone());
                continue;
            }
            if skipped(dir) {
                continue;
            }
            match name {
                "go.mod" => {
                    ws.config.insert(path.clone());
                    let module = module_path(&git.text(oid)?).unwrap_or_else(|| {
                        // No module directive: the go command rejects it; keep the
                        // directory a module boundary so its packages stay apart.
                        if dir.is_empty() {
                            ".".into()
                        } else {
                            dir.into()
                        }
                    });
                    modules.insert(dir.to_owned(), module);
                }
                "go.work" => {
                    ws.config.insert(path.clone());
                }
                "go.sum" | "go.work.sum" => {
                    ws.lockfiles.insert(path.clone());
                }
                _ if name.ends_with(".go") => {
                    packages.entry(dir).or_default().push((path, oid));
                }
                _ => {}
            }
        }
        for (dir, sources) in packages {
            let Some((root, module)) = modules
                .iter()
                .filter(|(root, _)| contains(root, dir))
                .max_by_key(|(root, _)| root.len())
            else {
                continue; // outside every module: not buildable as a package
            };
            let rel = if root.is_empty() {
                dir
            } else {
                dir.get(root.len() + 1..).unwrap_or_default()
            };
            let id = if rel.is_empty() {
                module.clone()
            } else {
                format!("{module}/{rel}")
            };
            ws.units.push(package(git, id, dir, &sources)?);
        }
        Ok(ws)
    }
}

/// One package: its files, the imports that resolve to other packages (filtered by
/// [`sheltie_adapter::workspace::assemble`]), interface and leaks.
fn package(git: &Git, id: String, dir: &str, sources: &[(&RepoPath, &str)]) -> Result<Unit, Error> {
    let mut parsed = Vec::new();
    let mut reads = std::collections::BTreeSet::new();
    let mut leaks = Vec::new();
    for (path, oid) in sources {
        let file = source::parse(&git.text(oid)?);
        reads.extend(file.imports.iter().map(TargetId::new));
        if file.cgo() && !leaks.contains(&"cgo") {
            leaks.push("cgo");
        }
        if file.embeds && !leaks.contains(&"embeds files") {
            leaks.push("embeds files");
        }
        if !path.as_str().ends_with("_test.go") {
            parsed.push(file);
        }
    }
    leaks.sort_unstable();
    Ok(Unit {
        public_iface: hash_parts(source::interface(&parsed)),
        tests: [TestId::new(id.clone())].into(),
        id: TargetId::new(id),
        root: dir.to_owned(),
        files: sources.iter().map(|(p, _)| (*p).clone()).collect(),
        reads,
        leaks: leaks.into_iter().map(str::to_owned).collect(),
    })
}

/// Directory and file name of a path (`""` for the root directory).
fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

/// Whether the go command ignores a directory: `testdata`, or a segment starting with `.`
/// or `_`. Vendored code is handled separately.
fn skipped(dir: &str) -> bool {
    !dir.is_empty()
        && dir
            .split('/')
            .any(|s| s == "testdata" || s.starts_with('.') || s.starts_with('_'))
}

/// Whether directory `dir` is `root` or below it.
fn contains(root: &str, dir: &str) -> bool {
    root.is_empty()
        || dir
            .strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The path in a `go.mod`'s `module` directive.
fn module_path(go_mod: &str) -> Option<String> {
    strip_c_comments(go_mod).lines().find_map(|line| {
        let rest = line.trim().strip_prefix("module")?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let path = rest.trim().trim_matches(|c| c == '"' || c == '`');
        (!path.is_empty()).then(|| path.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_directive() {
        assert_eq!(
            module_path("// c\nmodule example.com/shop // trailing\n\ngo 1.22\n").as_deref(),
            Some("example.com/shop")
        );
        assert_eq!(
            module_path("module \"example.com/q\"\n").as_deref(),
            Some("example.com/q")
        );
        assert_eq!(module_path("modules x\ngo 1.22\n"), None);
    }

    #[test]
    fn ignored_directories() {
        assert!(skipped("pkg/testdata"));
        assert!(skipped(".git/hooks"));
        assert!(skipped("_examples/a"));
        assert!(!skipped(""));
        assert!(!skipped("internal/util"));
        assert!(contains("", "a/b"));
        assert!(contains("a", "a"));
        assert!(contains("a", "a/b"));
        assert!(!contains("a", "ab"));
    }
}
