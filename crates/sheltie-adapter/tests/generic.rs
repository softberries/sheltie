//! The generic pipeline end to end, with a toy build tool, in a real git repository.
//!
//! Toy layout: every top-level directory with a `unit.toml` is a unit. `deps` in it names
//! the units it reads; its public interface is `api.txt`. Needs `git` on PATH.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_adapter::policy::COMMON_PLAIN;
use sheltie_adapter::scan::sha256;
use sheltie_adapter::{
    Adapter, DEFAULT_IGNORE, Error, Generic, Git, PathPolicy, Tool, TreeFiles, Unit, Workspace,
};
use sheltie_core::{RepoPath, TargetId, TestId};

struct Toy;

impl Tool for Toy {
    const NAME: &'static str = "sheltie-toy";
    const VERSION: &'static str = "sheltie-toy/0";

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        let mut ws = Workspace {
            config: [RepoPath::new("toy.lock.toml")].into(),
            ..Workspace::default()
        };
        for manifest in files.paths().filter(|p| p.as_str().ends_with("/unit.toml")) {
            let dir = manifest.as_str().trim_end_matches("/unit.toml").to_owned();
            let text = git.text(files.oid(manifest.as_str()).unwrap())?;
            let deps: toml::Table = text.parse().map_err(|e| Error::Tool(format!("{e}")))?;
            let reads = deps
                .get("deps")
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(TargetId::new)
                .collect();
            let api = files
                .oid(&format!("{dir}/api.txt"))
                .map(|o| git.text(o))
                .transpose()?
                .unwrap_or_default();
            ws.units.push(Unit {
                id: TargetId::new(dir.clone()),
                files: files
                    .under(&dir)
                    .filter(|(p, _)| p.as_str().ends_with(".txt") || p.as_str().ends_with(".toml"))
                    .map(|(p, _)| p.clone())
                    .collect(),
                root: dir.clone(),
                reads,
                public_iface: sha256(&api),
                tests: [TestId::new(dir)].into(),
                leaks: vec![],
            });
        }
        Ok(ws)
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=Sheltie Tests",
            "-c",
            "user.email=tests@sheltie.dev",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn repo(name: &str) -> PathBuf {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    write(&work, "core/unit.toml", "deps = []\n");
    write(&work, "core/api.txt", "fn total()\n");
    write(&work, "core/body.txt", "1\n");
    write(&work, "api/unit.toml", "deps = [\"core\"]\n");
    write(&work, "api/api.txt", "fn get()\n");
    write(&work, "web/unit.toml", "deps = [\"api\"]\n");
    write(&work, "web/api.txt", "\n");
    write(&work, "README.md", "toy\n");
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    work
}

fn change(work: &Path, edits: &[(&str, &str)]) -> sheltie_core::FootprintReport {
    git(work, &["switch", "-q", "-c", "change"]);
    for (p, t) in edits {
        write(work, p, t);
    }
    git(work, &["add", "-A"]);
    git(work, &["commit", "-q", "-m", "change"]);
    let g = Git::open(work).unwrap();
    let policy = PathPolicy::new(DEFAULT_IGNORE, COMMON_PLAIN).unwrap();
    Generic::<Toy>::new()
        .compute(
            &g,
            "main",
            "change",
            &policy,
            Path::new(env!("CARGO_TARGET_TMPDIR")),
        )
        .unwrap()
}

fn ids(set: &BTreeSet<TargetId>) -> Vec<&str> {
    set.iter().map(TargetId::as_str).collect()
}

#[test]
fn interface_change_reaches_direct_dependents() {
    let r = change(
        &repo("toy-iface"),
        &[("core/api.txt", "fn total(currency)\n")],
    );
    assert_eq!(ids(&r.footprint.writes), ["core"]);
    assert_eq!(ids(&r.footprint.reads), ["api", "core"]);
    assert_eq!(r.adapter, "sheltie-toy/0");
    assert!(r.diff_hash.as_str().starts_with("sha256:"));
}

#[test]
fn body_change_stays_in_its_unit() {
    let r = change(&repo("toy-body"), &[("core/body.txt", "2\n")]);
    assert_eq!(ids(&r.footprint.reads), ["core"]);
}

#[test]
fn docs_are_ignored_and_scripts_are_leaky() {
    let r = change(&repo("toy-docs"), &[("README.md", "more\n")]);
    assert!(r.footprint.writes.is_empty() && !r.footprint.leaky);
    let r = change(&repo("toy-script"), &[("scripts/run.sh", "echo\n")]);
    assert_eq!(ids(&r.footprint.writes), ["file:scripts/run.sh"]);
    assert!(r.footprint.leaky);
    assert_eq!(
        r.leak_reasons,
        ["scripts/run.sh is outside the module graph"]
    );
}

#[test]
fn graph_has_units_and_workspace() {
    let work = repo("toy-graph");
    let g = Git::open(&work).unwrap();
    let snap = Generic::<Toy>::new()
        .graph(
            &g,
            "HEAD",
            &PathPolicy::default(),
            Path::new(env!("CARGO_TARGET_TMPDIR")),
        )
        .unwrap();
    let targets: Vec<&str> = snap.targets.keys().map(TargetId::as_str).collect();
    assert_eq!(targets, ["$workspace", "api", "core", "web"]);
}
