//! Gradle adapter for Sheltie: computes footprints of changes from a repository's settings
//! and build scripts (Groovy or Kotlin DSL) and its Java and Kotlin sources, read straight
//! from git (no Gradle or JDK needed, nothing is evaluated).
//!
//! - Units are the projects of `settings.gradle(.kts)` (`include` statements, their parent
//!   projects, and `projectDir` overrides), identified by Gradle path (`:a:b`), plus the root
//!   project `:` if it has a `src/` directory. A project owns its build script, its
//!   `gradle.properties` and everything under its `src/`.
//! - A project reads the projects its build script names: `project(":x")` in any form, and
//!   type-safe accessors (`projects.x.y`).
//! - Its interface is the public surface of its main sources ([`sheltie_adapter::jvm`]) plus
//!   its build script, normalised (pessimistic: a dependency change propagates).
//! - Settings, the root build script, `gradle.properties`, version catalogs, the wrapper,
//!   `buildSrc/`, `build-logic/` and included builds are workspace configuration, read by
//!   every project; `gradle.lockfile`s and `gradle/verification-metadata.xml` are lockfiles.

use std::collections::{BTreeMap, BTreeSet};

use sheltie_adapter::jvm;
use sheltie_adapter::scan::hash_parts;
use sheltie_adapter::{Error, Git, Tool, TreeFiles, Unit, Workspace};
use sheltie_core::{RepoPath, TargetId, TestId};

mod scripts;

/// The Gradle [`Tool`]; run it with [`sheltie_adapter::Generic`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Gradle;

impl Tool for Gradle {
    const NAME: &'static str = "sheltie-gradle";
    const VERSION: &'static str = concat!("sheltie-gradle/", env!("CARGO_PKG_VERSION"));
    const PLAIN: &'static [&'static str] = &[".editorconfig", "detekt.yml", "**/detekt.yml"];

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        workspace(git, files)
    }
}

const SETTINGS: &[&str] = &["settings.gradle.kts", "settings.gradle"];
const BUILD_FILES: &[&str] = &["build.gradle.kts", "build.gradle"];

/// Whether a file is workspace-wide build configuration.
fn is_config(path: &str, included_builds: &[String]) -> bool {
    SETTINGS.contains(&path)
        || BUILD_FILES.contains(&path)
        || matches!(path, "gradle.properties" | "gradlew" | "gradlew.bat")
        || path
            .strip_prefix("gradle/")
            .is_some_and(|f| !f.contains('/') && f.ends_with(".versions.toml"))
        || ["gradle/wrapper/", "buildSrc/", "build-logic/"]
            .iter()
            .any(|dir| path.starts_with(dir))
        || included_builds.iter().any(|b| {
            path.strip_prefix(b.as_str())
                .is_some_and(|r| r.starts_with('/'))
        })
}

/// Whether a file is a dependency lockfile.
fn is_lockfile(path: &str) -> bool {
    path == "gradle.lockfile"
        || path.ends_with("/gradle.lockfile")
        || path == "gradle/verification-metadata.xml"
}

/// The first of `names` (in `dir`) present in the tree.
fn first_present(files: &TreeFiles, dir: &str, names: &[&str]) -> Option<String> {
    names
        .iter()
        .map(|n| jvm::join(dir, n))
        .find(|p| files.contains(p))
}

/// The projects and configuration of one tree.
pub fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
    let text = |path: &str| -> Result<Option<String>, Error> {
        files.oid(path).map(|oid| git.text(oid)).transpose()
    };
    let settings = match first_present(files, "", SETTINGS) {
        Some(path) => scripts::settings(&text(&path)?.unwrap_or_default()),
        None => scripts::Settings::default(),
    };

    // Project path -> directory: includes, their implicit parents, and the root if it has
    // sources of its own.
    let mut projects: BTreeMap<String, String> = BTreeMap::new();
    for include in &settings.includes {
        let mut path = include.as_str();
        while path.len() > 1 {
            projects.entry(path.to_owned()).or_insert_with(|| {
                settings
                    .dirs
                    .get(path)
                    .cloned()
                    .unwrap_or_else(|| scripts::default_dir(path))
            });
            path = path.rsplit_once(':').map_or("", |(parent, _)| parent);
        }
    }
    if files.under("src").next().is_some() {
        projects.insert(":".into(), String::new());
    }

    let accessors: BTreeMap<Vec<String>, &str> = projects
        .keys()
        .map(|p| (scripts::accessor(p), p.as_str()))
        .collect();

    let mut ws = Workspace {
        config: files
            .paths()
            .filter(|p| is_config(p.as_str(), &settings.builds))
            .cloned()
            .collect(),
        lockfiles: files
            .paths()
            .filter(|p| is_lockfile(p.as_str()))
            .cloned()
            .collect(),
        ..Workspace::default()
    };

    for (path, dir) in &projects {
        let build_file = first_present(files, dir, BUILD_FILES);
        let script = match &build_file {
            Some(f) => text(f)?.unwrap_or_default(),
            None => String::new(),
        };
        let mut reads: BTreeSet<TargetId> = scripts::project_refs(&script)
            .into_iter()
            .filter(|r| projects.contains_key(r))
            .map(TargetId::new)
            .collect();
        for chain in scripts::accessor_refs(&script) {
            // The longest prefix naming a project: `projects.core.dependencyProject` is `:core`.
            let target = (1..=chain.len())
                .rev()
                .find_map(|n| accessors.get(&chain[..n]));
            if let Some(target) = target {
                reads.insert(TargetId::new(*target));
            }
        }

        let mut parts = jvm::interface_parts(git, files, dir, false)?;
        let mut unit_files = jvm::unit_files(files, dir);
        // The root project's build script and properties are workspace configuration.
        if !dir.is_empty() {
            if let Some(f) = &build_file {
                parts.push(format!("{f}\n{}", scripts::normalized(&script)));
                unit_files.insert(RepoPath::new(f.as_str()));
            }
            let props = jvm::join(dir, "gradle.properties");
            if let Some(t) = text(&props)? {
                parts.push(format!("{props}\n{}", sheltie_adapter::scan::normalize(&t)));
                unit_files.insert(RepoPath::new(props));
            }
        }
        ws.units.push(Unit {
            id: TargetId::new(path.as_str()),
            // The root project's directory is the repository: only its sources are its own.
            root: if dir.is_empty() {
                "src".into()
            } else {
                dir.clone()
            },
            files: unit_files,
            reads,
            public_iface: hash_parts(parts),
            tests: [TestId::new(path.as_str())].into(),
            leaks: Vec::new(),
        });
    }
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_lockfiles() {
        let builds = vec!["tools/conventions".to_owned()];
        for config in [
            "settings.gradle.kts",
            "build.gradle",
            "gradle.properties",
            "gradle/libs.versions.toml",
            "gradle/wrapper/gradle-wrapper.properties",
            "buildSrc/src/main/kotlin/java.gradle.kts",
            "build-logic/build.gradle.kts",
            "tools/conventions/build.gradle.kts",
        ] {
            assert!(is_config(config, &builds), "{config}");
        }
        for other in [
            "core/build.gradle.kts",
            "gradle/other/x.versions.toml",
            "tools/conventionsX/a",
        ] {
            assert!(!is_config(other, &builds), "{other}");
        }
        assert!(is_lockfile("gradle.lockfile") && is_lockfile("core/gradle.lockfile"));
        assert!(is_lockfile("gradle/verification-metadata.xml") && !is_lockfile("x.lockfile"));
    }
}
