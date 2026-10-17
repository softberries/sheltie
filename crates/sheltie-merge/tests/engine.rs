//! The merge engine on real git repositories (needs `git`; tier 1 needs `mergiraf`, tier 2
//! needs `cargo`).

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_core::{CommitSha, PrNumber, ResolutionTier};
use sheltie_merge::engine::{MergeRequest, merge};

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("merge-engine")
            .join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Self { dir };
        repo.git(&["init", "-q", "-b", "main"]);
        repo
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.com",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "gc.auto=0",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn write(&self, path: &str, content: &str) {
        let p = self.dir.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).unwrap()
    }

    fn commit(&self, message: &str) -> CommitSha {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        CommitSha::parse(self.git(&["rev-parse", "HEAD"])).unwrap()
    }

    /// Commits `base`, then `ours` on main and `theirs` on a branch; returns (onto, head).
    fn scenario(
        &self,
        base: &[(&str, &str)],
        ours: &[(&str, &str)],
        theirs: &[(&str, &str)],
    ) -> (CommitSha, CommitSha) {
        for (p, c) in base {
            self.write(p, c);
        }
        let base_sha = self.commit("base");
        self.git(&["checkout", "-q", "-b", "pr"]);
        for (p, c) in theirs {
            self.write(p, c);
        }
        let head = self.commit("pr");
        self.git(&["checkout", "-q", "main"]);
        for (p, c) in ours {
            self.write(p, c);
        }
        let onto = if ours.is_empty() {
            base_sha
        } else {
            self.commit("main moved")
        };
        (onto, head)
    }

    fn request(&self, onto: CommitSha, head: CommitSha) -> MergeRequest {
        MergeRequest {
            repo: self.dir.clone(),
            pr: PrNumber(7),
            onto,
            head,
            message: "Merge #7: test\n\nSheltie-Verified: speculation S=x\n".into(),
            mergiraf: which("mergiraf"),
        }
    }
}

fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| PathBuf::from(d).join(program))
        .find(|p| p.is_file())
}

#[test]
fn import_conflict_is_resolved_structurally() {
    if which("mergiraf").is_none() {
        eprintln!("mergiraf not installed; skipping the tier-1 test");
        return;
    }
    let r = Repo::new("imports");
    let (onto, head) = r.scenario(
        &[("src/lib.rs", "use std::fmt;\n\npub fn a() {}\n")],
        &[(
            "src/lib.rs",
            "use std::fmt;\nuse std::io;\n\npub fn a() {}\n",
        )],
        &[(
            "src/lib.rs",
            "use std::fmt;\nuse std::fs;\n\npub fn a() {}\n",
        )],
    );
    let report = merge(&r.request(onto.clone(), head.clone())).unwrap();
    assert!(report.unresolved.is_empty(), "{report:?}");
    assert_eq!(report.tier(), ResolutionTier::Structural);
    let lib = r.read("src/lib.rs");
    assert!(
        lib.contains("use std::io;") && lib.contains("use std::fs;"),
        "{lib}"
    );
    let parents = r.git(&["show", "-s", "--format=%P", "HEAD"]);
    assert_eq!(
        parents,
        format!("{onto} {head}"),
        "a merge commit of onto and the PR head"
    );
}

#[test]
fn set_file_takes_both_additions() {
    let r = Repo::new("set");
    let rules = "[[set]]\npaths = [\"config/*.txt\"]\n";
    let (onto, head) = r.scenario(
        &[
            (".sheltie/merge.toml", rules),
            ("config/flags.txt", "a\nb\n"),
        ],
        &[("config/flags.txt", "a\nb\nfrom_main\n")],
        &[("config/flags.txt", "a\nb\nfrom_pr\n")],
    );
    let report = merge(&r.request(onto, head)).unwrap();
    assert_eq!(report.tier(), ResolutionTier::Declared, "{report:?}");
    assert_eq!(r.read("config/flags.txt"), "a\nb\nfrom_main\nfrom_pr\n");
}

#[test]
fn colliding_migration_numbers_are_renumbered() {
    let r = Repo::new("sequence");
    let rules = "[[sequence]]\npaths = [\"migrations/*.sql\"]\nkey = \"^(\\\\d+)_\"\n";
    let (onto, head) = r.scenario(
        &[
            (".sheltie/merge.toml", rules),
            ("migrations/0001_init.sql", "-- init\n"),
        ],
        &[("migrations/0002_orders.sql", "-- orders\n")],
        &[("migrations/0002_invoices.sql", "-- invoices\n")],
    );
    let report = merge(&r.request(onto, head)).unwrap();
    assert!(report.result.is_some(), "{report:?}");
    assert!(r.dir.join("migrations/0003_invoices.sql").exists());
    assert!(!r.dir.join("migrations/0002_invoices.sql").exists());
    assert_eq!(
        report.files[0].path.as_str(),
        "migrations/0003_invoices.sql"
    );
}

#[test]
fn lockfile_changed_on_both_sides_is_regenerated() {
    let r = Repo::new("lockfile");
    let manifest = |name: &str, deps: &str| {
        format!(
            "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}"
        )
    };
    let rules = "[[derived]]\npaths = [\"Cargo.lock\"]\nregenerate = \"cargo generate-lockfile --offline\"\n";
    let ws = "[workspace]\nmembers = [\"a\", \"b\", \"c\"]\nresolver = \"2\"\n";
    let base = vec![
        (".sheltie/merge.toml".to_owned(), rules.to_owned()),
        ("Cargo.toml".to_owned(), ws.to_owned()),
        ("a/Cargo.toml".to_owned(), manifest("a", "")),
        ("a/src/lib.rs".to_owned(), String::new()),
        ("b/Cargo.toml".to_owned(), manifest("b", "")),
        ("b/src/lib.rs".to_owned(), String::new()),
        ("c/Cargo.toml".to_owned(), manifest("c", "")),
        ("c/src/lib.rs".to_owned(), String::new()),
    ];
    for (p, c) in &base {
        r.write(p, c);
    }
    let lock = |r: &Repo| {
        let out = Command::new("cargo")
            .args(["generate-lockfile", "--offline"])
            .current_dir(&r.dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    lock(&r);
    r.commit("base");
    r.git(&["checkout", "-q", "-b", "pr"]);
    r.write("c/Cargo.toml", &manifest("c", "a = { path = \"../a\" }\n"));
    lock(&r);
    let head = r.commit("c uses a");
    r.git(&["checkout", "-q", "main"]);
    r.write("b/Cargo.toml", &manifest("b", "a = { path = \"../a\" }\n"));
    lock(&r);
    let onto = r.commit("b uses a");

    let report = merge(&r.request(onto, head)).unwrap();
    assert!(report.result.is_some(), "{report:?}");
    assert!(
        report
            .files
            .iter()
            .any(|f| f.path.as_str() == "Cargo.lock" && f.tier == ResolutionTier::Regenerate),
        "{report:?}"
    );
    let lockfile = r.read("Cargo.lock");
    let deps_of = |pkg: &str| {
        lockfile
            .split("[[package]]")
            .find(|s| s.contains(&format!("name = \"{pkg}\"")))
            .unwrap()
            .to_owned()
    };
    assert!(deps_of("b").contains("\"a\""), "{lockfile}");
    assert!(deps_of("c").contains("\"a\""), "{lockfile}");
}

#[test]
fn real_conflict_is_reported_and_aborted() {
    let r = Repo::new("conflict");
    let (onto, head) = r.scenario(
        &[("notes.txt", "one\n")],
        &[("notes.txt", "two\n")],
        &[("notes.txt", "three\n")],
    );
    let report = merge(&r.request(onto.clone(), head)).unwrap();
    assert_eq!(report.result, None);
    assert_eq!(report.unresolved.len(), 1);
    assert_eq!(report.unresolved[0].as_str(), "notes.txt");
    assert_eq!(
        r.git(&["rev-parse", "HEAD"]),
        onto.as_str(),
        "merge aborted"
    );
    assert_eq!(r.git(&["status", "--porcelain"]), "", "clean checkout");
    assert!(report.validate().is_ok());
}

#[test]
fn already_merged_head_yields_onto() {
    let r = Repo::new("noop");
    let (onto, _) = r.scenario(&[("x.txt", "x\n")], &[], &[("y.txt", "y\n")]);
    r.git(&["merge", "-q", "pr"]);
    let merged = CommitSha::parse(r.git(&["rev-parse", "HEAD"])).unwrap();
    let head = CommitSha::parse(r.git(&["rev-parse", "pr"])).unwrap();
    let report = merge(&r.request(merged.clone(), head)).unwrap();
    assert_eq!(report.result, Some(merged));
    let _ = onto;
}
