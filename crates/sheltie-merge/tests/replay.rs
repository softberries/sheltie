//! The replay client on real git repositories, with a scripted agent standing in for Claude.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_core::{CommitSha, Intent, PrNumber, ResolutionTier};
use sheltie_merge::replay::{Mode, ReplayRequest, replay};

struct Repo {
    dir: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Self {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("replay")
            .join(name);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).unwrap();
        }
        std::fs::create_dir_all(&dir).unwrap();
        let r = Self { dir };
        r.git(&["init", "-q", "-b", "main"]);
        r
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

    fn commit(&self, msg: &str) -> CommitSha {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", msg]);
        CommitSha::parse(self.git(&["rev-parse", "HEAD"])).unwrap()
    }

    /// base: `rate()`; main renames it to `standard_rate()`; the PR adds `doubled()` calling
    /// `rate()`. Textually clean, semantically broken: a seeded semantic conflict.
    fn semantic_conflict(&self) -> (CommitSha, CommitSha) {
        self.write("src/lib.rs", "pub fn rate() -> u32 {\n    20\n}\n");
        self.commit("base");
        self.git(&["checkout", "-q", "-b", "pr"]);
        self.write(
            "src/lib.rs",
            "pub fn rate() -> u32 {\n    20\n}\n\npub fn doubled() -> u32 {\n    rate() * 2\n}\n",
        );
        let head = self.commit("add doubled");
        self.git(&["checkout", "-q", "main"]);
        self.write("src/lib.rs", "pub fn standard_rate() -> u32 {\n    20\n}\n");
        self.write("README", "rate renamed\n");
        let onto = self.commit("rename rate");
        (onto, head)
    }

    fn request(
        &self,
        onto: CommitSha,
        head: CommitSha,
        mode: Mode,
        agent_script: &str,
    ) -> ReplayRequest {
        // Outside the checkout: the scope check would (rightly) reject it otherwise.
        let script = self.dir.with_extension("agent.sh");
        std::fs::write(&script, agent_script).unwrap();
        ReplayRequest {
            repo: self.dir.clone(),
            pr: PrNumber(5),
            onto,
            head,
            mode,
            intent: Intent {
                task: "Add doubled(), twice the standard rate".into(),
                plan: None,
                acceptance: vec!["src/lib.rs".into()],
            },
            message: "Merge #5: doubled\n\nSheltie-Replay: 4b\n".into(),
            agent: vec![
                "sh".into(),
                script.to_string_lossy().into_owned(),
                "{prompt}".into(),
            ],
            // The "acceptance test": the file must call the renamed function.
            test_command: Some("grep -q 'standard_rate() \\* 2' {tests}".into()),
        }
    }
}

const OK_JSON: &str = r#"echo '{"type":"result","subtype":"success","is_error":false,"num_turns":4,"total_cost_usd":0.12}'"#;

#[test]
fn full_replay_re_derives_a_semantic_conflict() {
    let r = Repo::new("full-ok");
    let (onto, head) = r.semantic_conflict();
    let agent = format!(
        "cat > src/lib.rs <<'EOF2'\npub fn standard_rate() -> u32 {{\n    20\n}}\n\npub fn doubled() -> u32 {{\n    standard_rate() * 2\n}}\nEOF2\n{OK_JSON}\n"
    );
    let report = replay(&r.request(onto.clone(), head.clone(), Mode::Full, &agent)).unwrap();
    assert_eq!(report.tier, ResolutionTier::ReplayFull);
    assert!(report.checks_passed(), "{report:?}");
    let checks = report.checks.clone().unwrap();
    assert_eq!(checks.acceptance_passed, Some(true));
    assert!(
        checks.similarity.get() > 0.0 && checks.similarity.get() < 1.0,
        "{}",
        checks.similarity
    );
    assert_eq!((report.cost_usd, report.turns), (0.12, 4));
    let result = report.result.unwrap();
    assert_eq!(
        r.git(&["show", "-s", "--format=%P", result.as_str()]),
        format!("{onto} {head}")
    );
    assert!(r.read("src/lib.rs").contains("standard_rate() * 2"));
    assert_eq!(
        r.read("README"),
        "rate renamed\n",
        "main's other changes kept"
    );
}

#[test]
fn edits_outside_the_change_are_rejected() {
    let r = Repo::new("scope");
    let (onto, head) = r.semantic_conflict();
    let agent = format!(
        "cat > src/lib.rs <<'EOF2'\npub fn standard_rate() -> u32 {{\n    20\n}}\n\npub fn doubled() -> u32 {{\n    standard_rate() * 2\n}}\nEOF2\necho hacked > README\n{OK_JSON}\n"
    );
    let report = replay(&r.request(onto.clone(), head, Mode::Full, &agent)).unwrap();
    assert_eq!(report.result, None);
    let outside: Vec<String> = report
        .checks
        .unwrap()
        .outside_scope
        .iter()
        .map(|p| p.to_string())
        .collect();
    assert_eq!(outside, ["README"]);
    assert_eq!(
        r.git(&["rev-parse", "HEAD"]),
        onto.as_str(),
        "nothing committed"
    );
    assert_eq!(r.git(&["status", "--porcelain"]), "", "checkout restored");
}

#[test]
fn failing_acceptance_tests_reject_the_replay() {
    let r = Repo::new("acceptance");
    let (onto, head) = r.semantic_conflict();
    // Keeps calling the old name: the acceptance test fails.
    let agent = format!(
        "printf 'pub fn standard_rate() -> u32 {{ 20 }}\\npub fn doubled() -> u32 {{ rate() * 2 }}\\n' > src/lib.rs\n{OK_JSON}\n"
    );
    let report = replay(&r.request(onto, head, Mode::Full, &agent)).unwrap();
    assert_eq!(report.result, None);
    assert_eq!(report.checks.unwrap().acceptance_passed, Some(false));
}

#[test]
fn agent_errors_are_reported() {
    let r = Repo::new("agent-error");
    let (onto, head) = r.semantic_conflict();
    let agent = "echo '{\"subtype\":\"error_max_turns\",\"is_error\":true,\"num_turns\":30,\"total_cost_usd\":2.5}'\nexit 1\n";
    let report = replay(&r.request(onto, head, Mode::Full, agent)).unwrap();
    assert!(
        report.error.as_deref().unwrap().contains("error_max_turns"),
        "{report:?}"
    );
    assert_eq!((report.cost_usd, report.turns), (2.5, 30));
    assert!(!report.checks_passed());
}

#[test]
fn hunks_replay_resolves_conflict_markers() {
    let r = Repo::new("hunks");
    r.write("notes.txt", "one\n");
    r.commit("base");
    r.git(&["checkout", "-q", "-b", "pr"]);
    r.write("notes.txt", "one\nfrom pr\n");
    let head = r.commit("pr");
    r.git(&["checkout", "-q", "main"]);
    r.write("notes.txt", "one\nfrom main\n");
    let onto = r.commit("main");
    let mut req = r.request(
        onto,
        head,
        Mode::Hunks,
        &format!("printf 'one\\nfrom main\\nfrom pr\\n' > notes.txt\n{OK_JSON}\n"),
    );
    req.intent.acceptance.clear();
    let report = replay(&req).unwrap();
    assert!(report.checks_passed(), "{report:?}");
    assert_eq!(report.tier, ResolutionTier::ReplayHunks);
    assert_eq!(r.read("notes.txt"), "one\nfrom main\nfrom pr\n");

    // An agent that leaves the markers is rejected.
    let r2 = Repo::new("hunks-markers");
    r2.write("notes.txt", "one\n");
    r2.commit("base");
    r2.git(&["checkout", "-q", "-b", "pr"]);
    r2.write("notes.txt", "one\nfrom pr\n");
    let head = r2.commit("pr");
    r2.git(&["checkout", "-q", "main"]);
    r2.write("notes.txt", "one\nfrom main\n");
    let onto = r2.commit("main");
    let mut req = r2.request(onto, head, Mode::Hunks, &format!("true\n{OK_JSON}\n"));
    req.intent.acceptance.clear();
    let report = replay(&req).unwrap();
    assert!(
        report
            .error
            .as_deref()
            .unwrap()
            .contains("conflict markers"),
        "{report:?}"
    );
}
