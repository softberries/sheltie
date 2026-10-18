//! Golden footprints for the scripted scenarios in `fixtures/scenarios/maven-multimodule.toml`.
//!
//! Needs `git` on PATH (not Maven). Snapshots live in `tests/snapshots/`; review changes with
//! `cargo insta review`.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sheltie_adapter::{Adapter, Generic, Git, PathPolicy};
use sheltie_core::{Footprint, FootprintReport, Relation, TargetId};
use sheltie_maven::Maven;

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
    Generic::<Maven>::new()
        .compute(&repo, "main~1", "main", &policy(), scratch)
        .unwrap()
}

/// The CLI's default policy for this adapter.
fn policy() -> PathPolicy {
    let plain: Vec<&str> = sheltie_adapter::policy::COMMON_PLAIN
        .iter()
        .chain(<Maven as sheltie_adapter::Tool>::PLAIN)
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
fn multimodule_scenarios() {
    check_fixture("maven-multimodule");
}

#[test]
fn graph_has_modules_edges_and_workspace() {
    let work = fixture_repo("maven-multimodule", "graph");
    let repo = Git::open(&work).unwrap();
    let snap = Generic::<Maven>::new()
        .graph(
            &repo,
            "HEAD",
            &policy(),
            Path::new(env!("CARGO_TARGET_TMPDIR")),
        )
        .unwrap();
    let targets: Vec<&str> = snap.targets.keys().map(TargetId::as_str).collect();
    assert_eq!(
        targets,
        [
            "$workspace",
            "com.acme.shop:api",
            "com.acme.shop:app",
            "com.acme.shop:core",
            "com.acme.shop:shop-parent"
        ]
    );
    let reads = |id: &str| -> Vec<&str> {
        snap.targets[&TargetId::new(id)]
            .reads
            .keys()
            .map(TargetId::as_str)
            .collect()
    };
    assert_eq!(
        reads("com.acme.shop:api"),
        [
            "$workspace",
            "com.acme.shop:core",
            "com.acme.shop:shop-parent"
        ]
    );
    assert_eq!(
        reads("com.acme.shop:app"),
        [
            "$workspace",
            "com.acme.shop:api",
            "com.acme.shop:shop-parent"
        ]
    );
    let owners = |p: &str| -> Vec<&str> {
        snap.owners[&sheltie_core::RepoPath::new(p)]
            .iter()
            .map(TargetId::as_str)
            .collect()
    };
    assert_eq!(owners("pom.xml"), ["com.acme.shop:shop-parent"]);
    assert_eq!(owners("api/pom.xml"), ["com.acme.shop:api"]);
    assert_eq!(
        owners("core/src/test/java/com/acme/shop/core/PriceCalculatorTest.java"),
        ["com.acme.shop:core"]
    );
    assert_eq!(owners(".mvn/maven.config"), ["$workspace"]);
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// A parent no `<modules>` lists, found by `relativePath`; a `test-jar` dependency; a leaky
/// plugin inherited from the parent.
#[test]
fn unlisted_parent_test_jar_and_inherited_leak() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("maven-edge");
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q", "-b", "main"]);
    write(
        &work,
        "pom.xml",
        "<project><groupId>g</groupId><artifactId>agg</artifactId>\
         <modules><module>a</module><module>b</module></modules></project>",
    );
    write(
        &work,
        "parent/pom.xml",
        "<project><groupId>g</groupId><artifactId>base</artifactId><build><plugins>\
         <plugin><artifactId>maven-antrun-plugin</artifactId></plugin></plugins></build></project>",
    );
    write(
        &work,
        "a/pom.xml",
        "<project><parent><groupId>g</groupId><artifactId>base</artifactId>\
         <relativePath>../parent</relativePath></parent><artifactId>a</artifactId></project>",
    );
    write(
        &work,
        "a/src/test/java/a/Support.java",
        "package a;\npublic class Support {\n    public static int answer() { return 42; }\n}\n",
    );
    write(
        &work,
        "b/pom.xml",
        "<project><parent><groupId>g</groupId><artifactId>base</artifactId>\
         <relativePath>../parent/pom.xml</relativePath></parent><artifactId>b</artifactId>\
         <dependencies><dependency><groupId>g</groupId><artifactId>a</artifactId>\
         <type>test-jar</type><scope>test</scope></dependency></dependencies></project>",
    );
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    write(
        &work,
        "a/src/test/java/a/Support.java",
        "package a;\npublic class Support {\n    public static long answer() { return 42; }\n}\n",
    );
    git(&work, &["commit", "-q", "-am", "change"]);

    let repo = Git::open(&work).unwrap();
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let snap = Generic::<Maven>::new()
        .graph(&repo, "HEAD", &policy(), scratch)
        .unwrap();
    let targets: Vec<&str> = snap.targets.keys().map(TargetId::as_str).collect();
    assert_eq!(targets, ["$workspace", "g:a", "g:agg", "g:b", "g:base"]);

    let report = Generic::<Maven>::new()
        .compute(&repo, "main~1", "main", &policy(), scratch)
        .unwrap();
    let ids = |s: &std::collections::BTreeSet<TargetId>| -> Vec<String> {
        s.iter().map(|t| t.as_str().to_owned()).collect()
    };
    assert_eq!(ids(&report.footprint.writes), ["g:a"]);
    // b uses a's test classes, so a's test sources are part of a's interface.
    assert_eq!(ids(&report.footprint.reads), ["g:a", "g:b"]);
    assert!(report.footprint.leaky);
    assert_eq!(
        report.leak_reasons,
        [
            "g:a: build plugin runs code (maven-antrun-plugin)",
            "g:b: build plugin runs code (maven-antrun-plugin)"
        ]
    );
}
