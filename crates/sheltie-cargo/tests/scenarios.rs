//! Golden footprints for the scripted scenarios in `fixtures/scenarios` (M2 "done when").
//!
//! Needs `git` and `cargo` on PATH. Snapshots live in `tests/snapshots/`; review changes with
//! `cargo insta review`.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sheltie_cargo::{FootprintReport, Git, PathPolicy};
use sheltie_core::{Footprint, Relation};

#[derive(Deserialize)]
struct ScenarioFile {
    #[serde(default)]
    independent: Vec<Vec<String>>,
    scenarios: Vec<Scenario>,
}

#[derive(Deserialize)]
struct Scenario {
    name: String,
    description: String,
    #[serde(default)]
    replace: Vec<Replace>,
    #[serde(default)]
    write: Vec<Write>,
}

#[derive(Deserialize)]
struct Replace {
    path: String,
    find: String,
    replace: String,
}

#[derive(Deserialize)]
struct Write {
    path: String,
    content: String,
}

/// What is snapshotted: the footprint and why it leaks, without commit ids.
#[derive(Serialize)]
struct Golden<'a> {
    description: &'a str,
    footprint: &'a Footprint,
    leak_reasons: &'a [String],
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() != "target" {
                copy_dir(&entry.path(), &target);
            }
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
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
            // No detached background maintenance outliving the test.
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

fn run_scenario(fixture: &str, scenario: &Scenario) -> FootprintReport {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("scenarios")
        .join(fixture)
        .join(&scenario.name);
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    copy_dir(&repo_root().join("fixtures").join(fixture), &work);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    for r in &scenario.replace {
        let path = work.join(&r.path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.matches(&r.find).count(),
            1,
            "{}: `find` must occur once in {}",
            scenario.name,
            r.path
        );
        std::fs::write(&path, text.replace(&r.find, &r.replace)).unwrap();
    }
    for w in &scenario.write {
        let path = work.join(&w.path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, &w.content).unwrap();
    }
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", &scenario.name]);

    let repo = Git::open(&work).unwrap();
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"));
    sheltie_cargo::compute(&repo, "main~1", "main", &PathPolicy::default(), scratch).unwrap()
}

fn check_fixture(fixture: &str) {
    let spec =
        std::fs::read_to_string(repo_root().join(format!("fixtures/scenarios/{fixture}.toml")))
            .unwrap();
    let spec: ScenarioFile = toml::from_str(&spec).unwrap();
    let mut footprints: BTreeMap<String, Footprint> = BTreeMap::new();
    for scenario in &spec.scenarios {
        let report = run_scenario(fixture, scenario);
        insta::assert_json_snapshot!(
            format!("{fixture}__{}", scenario.name),
            Golden {
                description: &scenario.description,
                footprint: &report.footprint,
                leak_reasons: &report.leak_reasons,
            }
        );
        footprints.insert(scenario.name.clone(), report.footprint);
    }
    for group in &spec.independent {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                let (fa, fb) = (&footprints[a], &footprints[b]);
                assert_eq!(
                    fa.relation(fb),
                    Relation::Independent,
                    "{fixture}: {a} and {b} should commute"
                );
            }
        }
    }
}

#[test]
fn single_crate_scenarios() {
    check_fixture("cargo-single");
}

#[test]
fn workspace_scenarios() {
    check_fixture("cargo-workspace");
}
