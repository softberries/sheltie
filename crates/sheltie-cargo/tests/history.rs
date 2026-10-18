//! The merge history read for the ROI report: landings and their footprints.
//!
//! Needs `git` and `cargo` on PATH.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_cargo::history::{Footprints, history, landings};
use sheltie_cargo::{DEFAULT_IGNORE, DEFAULT_PLAIN, Git, PathPolicy};
use sheltie_core::TargetId;

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
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

fn append(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    let mut s = std::fs::read_to_string(&p).unwrap();
    s.push_str(text);
    std::fs::write(p, s).unwrap();
}

/// The workspace fixture with: a merged branch changing billing, a direct push changing auth,
/// and a README edit.
fn repo() -> PathBuf {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("history");
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/cargo-workspace"),
        &work,
    );
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    git(&work, &["switch", "-q", "-c", "feature"]);
    append(
        &work,
        "crates/ledger-billing/src/tax.rs",
        "\npub fn zero() -> u32 { 0 }\n",
    );
    git(&work, &["commit", "-qam", "billing: zero rate"]);
    git(&work, &["switch", "-q", "main"]);
    git(
        &work,
        &[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            "Merge pull request #1 from feature",
            "feature",
        ],
    );
    append(&work, "crates/ledger-auth/src/lib.rs", "\n// direct push\n");
    git(&work, &["commit", "-qam", "auth: comment"]);
    append(&work, "README.md", "\nMore words.\n");
    git(&work, &["commit", "-qam", "docs"]);
    work
}

#[test]
fn landings_are_first_parent_commits_with_footprints() {
    let work = repo();
    let g = Git::open(&work).unwrap();
    let found = landings(&g, "main", 30).unwrap();
    // The base commit is a root and has no change; the feature commit is not first-parent.
    assert_eq!(found.len(), 3);
    assert!(found.windows(2).all(|w| w[0].at <= w[1].at));
    assert_eq!(found[1].parent, found[0].sha);

    let policy = PathPolicy::new(DEFAULT_IGNORE, DEFAULT_PLAIN).unwrap();
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"));
    for mode in [Footprints::Predicted, Footprints::Exact] {
        let mut calls = 0;
        let h = history(&g, "main", &found, mode, &policy, scratch, |done, total| {
            calls += 1;
            assert_eq!(total, 3);
            assert_eq!(done, calls);
        })
        .unwrap();
        assert_eq!((h.changes.len(), h.skipped), (3, 0), "{mode:?}");
        let writes = |i: usize| -> Vec<String> {
            h.changes[i]
                .footprint
                .writes
                .iter()
                .map(|t: &TargetId| t.as_str().to_owned())
                .collect()
        };
        assert!(
            writes(0).iter().any(|t| t.starts_with("ledger-billing")),
            "{mode:?}: {:?}",
            writes(0)
        );
        assert!(
            writes(1).iter().any(|t| t.starts_with("ledger-auth")),
            "{mode:?}: {:?}",
            writes(1)
        );
        // The README is ignored: an empty footprint, independent of everything.
        assert!(writes(2).is_empty(), "{mode:?}: {:?}", writes(2));
        assert_eq!(h.changes[0].ready_at, found[0].at);
    }
}
