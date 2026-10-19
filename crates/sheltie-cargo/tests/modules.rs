//! Module files declared with a raw identifier (`mod r#type;`), a `#[path]` in a non-`mod.rs`
//! file, or inside a macro invocation (`cfg_x! { mod inner; }`) are found, so changing them
//! does not make the footprint leaky.
//!
//! Needs `git` on PATH.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_adapter::Adapter as _;
use sheltie_cargo::{DEFAULT_IGNORE, DEFAULT_PLAIN, Git, PathPolicy};

fn git(dir: &Path, args: &[&str]) -> String {
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
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn write(dir: &Path, path: &str, text: &str) {
    let p = dir.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn repo() -> PathBuf {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("modules");
    if work.exists() {
        std::fs::remove_dir_all(&work).unwrap();
    }
    std::fs::create_dir_all(&work).unwrap();
    write(
        &work,
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[workspace]\n",
    );
    write(
        &work,
        "src/lib.rs",
        "pub mod r#type;\n#[cfg(test)]\nmod tests;\n\nmacro_rules! cfg_x {\n    ($($i:item)*) => { $($i)* };\n}\n\ncfg_x! {\n    pub mod inner;\n}\n",
    );
    write(&work, "src/type.rs", "pub fn kind() -> u32 { 1 }\n");
    write(&work, "src/inner.rs", "pub fn depth() -> u32 { 1 }\n");
    write(
        &work,
        "src/tests.rs",
        "#[path = \"../test_data/runner.rs\"]\nmod runner;\n",
    );
    write(&work, "test_data/runner.rs", "#[test]\nfn runs() {}\n");
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["add", "-A"]);
    git(&work, &["commit", "-q", "-m", "base"]);
    work
}

#[test]
fn raw_identifiers_and_relative_paths_resolve() {
    let work = repo();
    let policy = PathPolicy::new(DEFAULT_IGNORE, DEFAULT_PLAIN).unwrap();
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR"));
    for (path, text) in [
        ("src/type.rs", "pub fn kind() -> u32 { 2 }\n"),
        ("src/inner.rs", "pub fn depth() -> u32 { 2 }\n"),
        (
            "test_data/runner.rs",
            "#[test]\nfn runs() { assert!(true) }\n",
        ),
    ] {
        git(&work, &["switch", "-q", "-C", "change", "main"]);
        write(&work, path, text);
        git(&work, &["commit", "-qam", path]);
        let g = Git::open(&work).unwrap();
        let report = sheltie_cargo::Cargo
            .compute(&g, "main", "change", &policy, scratch)
            .unwrap();
        assert!(!report.footprint.leaky, "{path}: {:?}", report.leak_reasons);
        assert!(
            report
                .footprint
                .writes
                .iter()
                .all(|t| !t.as_str().starts_with("file:")),
            "{path}: {:?}",
            report.footprint.writes
        );
    }
}
