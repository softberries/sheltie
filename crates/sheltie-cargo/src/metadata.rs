//! Workspace layout from `cargo metadata`.

use std::collections::BTreeMap;
use std::path::Path;

use cargo_metadata::{MetadataCommand, TargetKind};
use sheltie_core::RepoPath;

use crate::Error;

/// What kind of crate a compilation target is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CrateKind {
    Lib,
    Bin,
    Test,
    Example,
    Bench,
}

impl CrateKind {
    fn label(self) -> &'static str {
        match self {
            CrateKind::Lib => "lib",
            CrateKind::Bin => "bin",
            CrateKind::Test => "test",
            CrateKind::Example => "example",
            CrateKind::Bench => "bench",
        }
    }
}

/// One compilation target of a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrateTarget {
    pub(crate) kind: CrateKind,
    pub(crate) name: String,
    /// Root source file, relative to the repository.
    pub(crate) root: RepoPath,
    pub(crate) edition_2015: bool,
}

impl CrateTarget {
    /// Prefix of target ids in this crate: `pkg` for the library, `pkg/bin/name` otherwise.
    pub(crate) fn id_prefix(&self, package: &str) -> String {
        match self.kind {
            CrateKind::Lib => package.to_owned(),
            kind => format!("{package}/{}/{}", kind.label(), self.name),
        }
    }
}

/// A workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    pub(crate) name: String,
    /// Directory of the manifest, relative to the repository ("" for the root).
    pub(crate) dir: String,
    pub(crate) manifest: RepoPath,
    pub(crate) build_script: Option<RepoPath>,
    pub(crate) proc_macro: bool,
    pub(crate) lib: Option<CrateTarget>,
    pub(crate) others: Vec<CrateTarget>,
    /// Workspace members this package depends on (any kind): extern name → package name.
    pub(crate) deps: BTreeMap<String, String>,
}

/// The workspace members of one tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Layout {
    pub(crate) packages: Vec<Package>,
}

/// Runs `cargo metadata --no-deps --offline` in `checkout` (a worktree whose root is the
/// repository root) and converts paths to repository-relative ones.
pub(crate) fn layout(checkout: &Path) -> Result<Layout, Error> {
    let manifest = checkout.join("Cargo.toml");
    let meta = MetadataCommand::new()
        .manifest_path(&manifest)
        .no_deps()
        .other_options(vec!["--offline".to_owned()])
        .exec()
        .map_err(|e| Error::Tool(format!("cargo metadata failed: {e}")))?;
    // Canonicalize: the worktree may sit behind a symlink (e.g. /tmp on macOS).
    let root = checkout
        .canonicalize()
        .map_err(|e| Error::Io(format!("canonicalizing {}: {e}", checkout.display())))?;
    let rel = |p: &Path| -> Result<RepoPath, Error> {
        let p = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
        let rel = p.strip_prefix(&root).map_err(|_| {
            Error::Tool(format!(
                "cargo metadata failed: {} is outside the repository",
                p.display()
            ))
        })?;
        let s = rel.to_str().ok_or_else(|| {
            Error::Tool(format!(
                "cargo metadata failed: non-UTF-8 path {}",
                rel.display()
            ))
        })?;
        Ok(RepoPath::new(s.replace('\\', "/")))
    };

    let members: BTreeMap<String, String> = meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
        .map(|p| {
            let lib = p
                .targets
                .iter()
                .find(|t| t.is_lib() || t.is_proc_macro())
                .map_or_else(|| p.name.to_string(), |t| t.name.clone());
            (p.name.to_string(), lib.replace('-', "_"))
        })
        .collect();

    let mut packages = Vec::new();
    for p in meta
        .packages
        .iter()
        .filter(|p| meta.workspace_members.contains(&p.id))
    {
        let manifest = rel(p.manifest_path.as_std_path())?;
        let dir = manifest
            .as_str()
            .rsplit_once('/')
            .map_or(String::new(), |(d, _)| d.to_owned());
        let mut lib = None;
        let mut others = Vec::new();
        let mut build_script = None;
        let mut proc_macro = false;
        for t in &p.targets {
            let root = rel(t.src_path.as_std_path())?;
            let kind = if t.is_lib()
                || t.kind.iter().any(|k| {
                    matches!(
                        k,
                        TargetKind::RLib
                            | TargetKind::DyLib
                            | TargetKind::CDyLib
                            | TargetKind::StaticLib
                    )
                }) {
                CrateKind::Lib
            } else if t.is_proc_macro() {
                proc_macro = true;
                CrateKind::Lib
            } else if t.is_custom_build() {
                build_script = Some(root);
                continue;
            } else if t.is_bin() {
                CrateKind::Bin
            } else if t.is_test() {
                CrateKind::Test
            } else if t.is_example() {
                CrateKind::Example
            } else if t.is_bench() {
                CrateKind::Bench
            } else {
                continue;
            };
            let target = CrateTarget {
                kind,
                name: t.name.clone(),
                root,
                edition_2015: t.edition.as_str() == "2015",
            };
            if kind == CrateKind::Lib {
                lib = Some(target);
            } else {
                others.push(target);
            }
        }
        others.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
        let deps = p
            .dependencies
            .iter()
            .filter_map(|d| {
                let lib_name = members.get(d.name.as_str())?;
                let extern_name = d
                    .rename
                    .as_ref()
                    .map_or(lib_name.clone(), |r| r.replace('-', "_"));
                Some((extern_name, d.name.clone()))
            })
            .collect();
        packages.push(Package {
            name: p.name.to_string(),
            dir,
            manifest,
            build_script,
            proc_macro,
            lib,
            others,
            deps,
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Layout { packages })
}
