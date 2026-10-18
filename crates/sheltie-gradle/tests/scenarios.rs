//! Golden footprints for the scripted scenarios in `fixtures/scenarios/gradle-multiproject.toml`.
//!
//! Needs `git` on PATH (not Gradle). Snapshots live in `tests/snapshots/`; review changes with
//! `cargo insta review`.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sheltie_adapter::{Adapter, Generic, Git, PathPolicy};
use sheltie_core::{Footprint, FootprintReport, Relation, TargetId};
use sheltie_gradle::Gradle;

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

fn fixture_repo(fixture: &str, name: &str) -> PathBuf {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("scenarios")
        .join(fixture)
        .join(name);
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    copy_dir(&repo_root().join("fixtures").join(fixture), &work);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    work
}

fn run_scenario(fixture: &str, scenario: &Scenario) -> FootprintReport {
    let work = fixture_repo(fixture, &scenario.name);
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
    Generic::<Gradle>::new()
        .compute(&repo, "main~1", "main", &policy(), scratch)
        .unwrap()
}

/// The CLI's default policy for this adapter.
fn policy() -> PathPolicy {
    let plain: Vec<&str> = sheltie_adapter::policy::COMMON_PLAIN
        .iter()
        .chain(<Gradle as sheltie_adapter::Tool>::PLAIN)
        .copied()
        .collect();
    PathPolicy::new(sheltie_adapter::DEFAULT_IGNORE, &plain).unwrap()
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
fn multiproject_scenarios() {
    check_fixture("gradle-multiproject");
}

#[test]
fn graph_has_projects_edges_and_workspace() {
    let work = fixture_repo("gradle-multiproject", "graph");
    let repo = Git::open(&work).unwrap();
    let snap = Generic::<Gradle>::new()
        .graph(
            &repo,
            "HEAD",
            &policy(),
            Path::new(env!("CARGO_TARGET_TMPDIR")),
        )
        .unwrap();
    let targets: Vec<&str> = snap.targets.keys().map(TargetId::as_str).collect();
    assert_eq!(targets, ["$workspace", ":api", ":app", ":core"]);
    let reads = |id: &str| -> Vec<&str> {
        snap.targets[&TargetId::new(id)]
            .reads
            .keys()
            .map(TargetId::as_str)
            .collect()
    };
    assert_eq!(reads(":api"), ["$workspace", ":core"]);
    assert_eq!(reads(":app"), ["$workspace", ":api"]);
    assert_eq!(reads(":core"), ["$workspace"]);
    let owners = |p: &str| -> Vec<&str> {
        snap.owners[&sheltie_core::RepoPath::new(p)]
            .iter()
            .map(TargetId::as_str)
            .collect()
    };
    assert_eq!(owners("api/build.gradle.kts"), [":api"]);
    assert_eq!(
        owners("core/src/test/kotlin/com/acme/shop/core/PricingTest.kt"),
        [":core"]
    );
    for config in [
        "settings.gradle.kts",
        "build.gradle.kts",
        "gradle.properties",
        "gradle/libs.versions.toml",
        "gradle/wrapper/gradle-wrapper.properties",
    ] {
        assert_eq!(owners(config), ["$workspace"], "{config}");
    }
}
