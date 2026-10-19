//! Acceptance tests found mechanically: fail-to-pass.
//!
//! A test the pull request adds or changes, that passes on its head and fails on its base,
//! shows what the change does. Such tests are found by reading the diff (no model, no
//! cooperation from the author), and each candidate runs alone so that the command's exit
//! code is the verdict, whatever the test framework.
//!
//! Some test runners exit with success when a filter matches nothing (`cargo test`,
//! `go test`). A test is therefore never run in a tree where it is not defined: there it is
//! [`TestOutcome::Missing`], which counts as failing on the base and as failed on a replay
//! result.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use sheltie_core::{AcceptanceTest, MAX_ACCEPTANCE_CANDIDATES, TestLanguage, TestOutcome};

use crate::engine::EngineError;
use crate::rules::ReplayConfig;

/// Longest a single test run may take.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Longest the fail-to-pass step may take in total.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A test found in a pull request's diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The name passed to the test command.
    pub name: String,
    /// Its language.
    pub language: TestLanguage,
    /// The file that defines it.
    pub file: String,
    /// The name of its definition in that file (function or method).
    definition: String,
}

/// A test's definition in a file: the name it is selected by, the name of its function, and
/// its first and last line (1-based, inclusive).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Definition {
    name: String,
    function: String,
    first: usize,
    last: usize,
}

/// The language whose tests `path` may contain, by file name.
pub fn language(path: &str) -> Option<TestLanguage> {
    let file = path.rsplit('/').next().unwrap_or(path);
    let ext = file.rsplit_once('.').map(|(_, e)| e)?;
    match ext {
        "rs" => Some(TestLanguage::Rust),
        "go" if file.ends_with("_test.go") => Some(TestLanguage::Go),
        "py" if file.starts_with("test_") || file.ends_with("_test.py") => {
            Some(TestLanguage::Python)
        }
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts"
            if file.contains(".test.") || file.contains(".spec.") =>
        {
            Some(TestLanguage::Javascript)
        }
        "java" | "kt" => Some(TestLanguage::Java),
        _ => None,
    }
}

/// The line where a brace-delimited body that opens at or after `start` closes. Braces in
/// strings and `//` comments are ignored; an unbalanced body ends at the last line.
fn brace_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i64;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(start) {
        let mut quote: Option<char> = None;
        let mut prev = ' ';
        for c in line.chars() {
            match quote {
                Some(q) if c == q && prev != '\\' => quote = None,
                Some(_) => {}
                None => match c {
                    '"' | '\'' | '`' => quote = Some(c),
                    '/' if prev == '/' => break,
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                },
            }
            prev = if prev == '\\' && c == '\\' { ' ' } else { c };
        }
        if opened && depth <= 0 {
            return i;
        }
    }
    lines.len().saturating_sub(1)
}

/// The line where an indented Python block that starts at `start` ends.
fn indent_end(lines: &[&str], start: usize) -> usize {
    let indent = |l: &str| l.len() - l.trim_start().len();
    let base = indent(lines[start]);
    let mut last = start;
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim().is_empty() {
            continue;
        }
        if indent(line) <= base {
            break;
        }
        last = i;
    }
    last
}

fn re(pattern: &str) -> Regex {
    // Every pattern is a constant, covered by the tests below.
    Regex::new(pattern).unwrap_or_else(|e| unreachable!("constant regex {pattern}: {e}"))
}

static RUST_ATTR: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*#\[\s*(?:[A-Za-z_]\w*::)*test\s*(?:\(.*\))?\s*\]"));
static RUST_FN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_]\w*)")
});
static GO_TEST: LazyLock<Regex> =
    LazyLock::new(|| re(r"^func\s+(Test[A-Za-z0-9_]*)\s*\(\s*\w+\s+\*testing\.T\s*\)"));
static PY_CLASS: LazyLock<Regex> = LazyLock::new(|| re(r"^class\s+(Test\w*)\s*[(:]"));
static PY_DEF: LazyLock<Regex> = LazyLock::new(|| re(r"^(\s*)(?:async\s+)?def\s+(test\w*)\s*\("));
static JS_TEST: LazyLock<Regex> = LazyLock::new(|| {
    re(r#"^\s*(?:it|test)(?:\.only|\.concurrent)?\s*\(\s*(?:'([^'\\]+)'|"([^"\\]+)"|`([^`\\$]+)`)"#)
});
static JAVA_ATTR: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*@(?:org\.junit\.(?:jupiter\.api\.)?)?Test\b\s*(?:\(.*\))?\s*$"));
static JAVA_METHOD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"^\s*(?:(?:public|protected|private|internal|static|final|open|override|suspend)\s+)*(?:void\s+|fun\s+)([A-Za-z_]\w*)\s*\(",
    )
});

/// The tests defined in a file.
fn definitions(language: TestLanguage, path: &str, text: &str) -> Vec<Definition> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let stem = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or_default();
    match language {
        TestLanguage::Rust | TestLanguage::Java => {
            let (attr, func) = if language == TestLanguage::Rust {
                (&*RUST_ATTR, &*RUST_FN)
            } else {
                (&*JAVA_ATTR, &*JAVA_METHOD)
            };
            let mut pending: Option<usize> = None;
            for (i, line) in lines.iter().enumerate() {
                let trimmed = line.trim_start();
                if attr.is_match(line) {
                    pending.get_or_insert(i);
                } else if let Some(start) = pending {
                    if let Some(m) = func.captures(line) {
                        let function = m[1].to_owned();
                        let name = if language == TestLanguage::Rust {
                            function.clone()
                        } else {
                            format!("{stem}#{function}")
                        };
                        out.push(Definition {
                            name,
                            function,
                            first: start + 1,
                            last: brace_end(&lines, i) + 1,
                        });
                        pending = None;
                    } else if !(trimmed.starts_with("#[")
                        || trimmed.starts_with('@')
                        || trimmed.starts_with("//")
                        || trimmed.is_empty())
                    {
                        pending = None;
                    }
                }
            }
        }
        TestLanguage::Go => {
            for (i, line) in lines.iter().enumerate() {
                if let Some(m) = GO_TEST.captures(line) {
                    out.push(Definition {
                        name: m[1].to_owned(),
                        function: m[1].to_owned(),
                        first: i + 1,
                        last: brace_end(&lines, i) + 1,
                    });
                }
            }
        }
        TestLanguage::Python => {
            let mut class: Option<(String, usize)> = None;
            for (i, line) in lines.iter().enumerate() {
                if let Some(m) = PY_CLASS.captures(line) {
                    class = Some((m[1].to_owned(), indent_end(&lines, i)));
                    continue;
                }
                if class.as_ref().is_some_and(|(_, end)| i > *end) {
                    class = None;
                }
                if let Some(m) = PY_DEF.captures(line) {
                    let nested = !m[1].is_empty();
                    let function = m[2].to_owned();
                    let name = match (&class, nested) {
                        (Some((c, _)), true) => format!("{path}::{c}::{function}"),
                        (None, false) => format!("{path}::{function}"),
                        // A nested function that is not a method of a test class.
                        _ => continue,
                    };
                    out.push(Definition {
                        name,
                        function,
                        first: i + 1,
                        last: indent_end(&lines, i) + 1,
                    });
                }
            }
        }
        TestLanguage::Javascript => {
            for (i, line) in lines.iter().enumerate() {
                if let Some(m) = JS_TEST.captures(line) {
                    let title = m
                        .get(1)
                        .or_else(|| m.get(2))
                        .or_else(|| m.get(3))
                        .map(|t| t.as_str().to_owned())
                        .unwrap_or_default();
                    out.push(Definition {
                        name: title.clone(),
                        function: title,
                        first: i + 1,
                        last: brace_end(&lines, i) + 1,
                    });
                }
            }
        }
    }
    out
}

/// Lines a unified diff with no context (`git diff -U0`) changed in each new file: added
/// lines, and for a pure deletion the lines around it.
pub fn changed_lines(diff: &str) -> BTreeMap<String, Vec<usize>> {
    let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut file: Option<String> = None;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            file = path.strip_prefix("b/").map(str::to_owned);
            continue;
        }
        let (Some(f), Some(hunk)) = (&file, line.strip_prefix("@@ ")) else {
            continue;
        };
        let Some(new) = hunk.split_whitespace().find(|p| p.starts_with('+')) else {
            continue;
        };
        let mut parts = new[1..].split(',');
        let start: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        let count: usize = parts.next().and_then(|s| s.parse().ok()).unwrap_or(1);
        let lines = out.entry(f.clone()).or_default();
        if count == 0 {
            lines.extend([start, start + 1]);
        } else {
            lines.extend(start..start + count);
        }
    }
    out
}

/// Tests the diff touches: definitions in the new version of each changed file with a changed
/// line inside them, in diff order, at most [`MAX_ACCEPTANCE_CANDIDATES`]. `read` returns a
/// file's text in the new tree.
pub fn candidates(diff: &str, read: impl Fn(&str) -> Option<String>) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for (file, changed) in changed_lines(diff) {
        let Some(language) = language(&file) else {
            continue;
        };
        let Some(text) = read(&file) else {
            continue;
        };
        for d in definitions(language, &file, &text) {
            let touched = changed.iter().any(|l| (d.first..=d.last).contains(l));
            if touched
                && !out
                    .iter()
                    .any(|c| c.name == d.name && c.language == language)
            {
                out.push(Candidate {
                    name: d.name,
                    language,
                    file: file.clone(),
                    definition: d.function,
                });
            }
        }
    }
    out.truncate(MAX_ACCEPTANCE_CANDIDATES);
    out
}

impl Candidate {
    /// Whether this test is defined in `text`, the content of its file in some tree.
    pub fn defined_in(&self, text: Option<&str>) -> bool {
        text.is_some_and(|t| {
            definitions(self.language, &self.file, t)
                .iter()
                .any(|d| d.name == self.name && d.function == self.definition)
        })
    }
}

/// Quotes a string for a POSIX shell.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Runs one test command; replaced in tests.
pub trait Runner {
    /// Runs `command` in `dir`, stopping it after `timeout`.
    fn run(&mut self, dir: &Path, command: &str, timeout: Duration) -> TestOutcome;
}

/// Runs commands with `sh -c`, without the agent's API key, and kills the whole process
/// group on timeout.
#[derive(Debug, Default)]
pub struct ShellRunner;

impl Runner for ShellRunner {
    fn run(&mut self, dir: &Path, command: &str, timeout: Duration) -> TestOutcome {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(dir)
            // Tests run the pull request's code: they do not get the agent's API key.
            .env_remove("ANTHROPIC_API_KEY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        let Ok(mut child) = cmd.spawn() else {
            return TestOutcome::Failed;
        };
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return TestOutcome::Passed,
                Ok(Some(_)) | Err(_) => return TestOutcome::Failed,
                Ok(None) if started.elapsed() >= timeout => {
                    #[cfg(unix)]
                    let _ = Command::new("kill")
                        .args(["-KILL", &format!("-{}", child.id())])
                        .status();
                    let _ = child.kill();
                    let _ = child.wait();
                    return TestOutcome::TimedOut;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            }
        }
    }
}

/// The test command for one candidate, or `None` when its language has no command.
pub fn command(config: &ReplayConfig, test: &Candidate) -> Option<String> {
    config
        .command_for(test.language)
        .map(|c| c.replace("{tests}", &shell_quote(&test.name)))
}

fn git(repo: &Path, args: &[&str]) -> Result<String, EngineError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| EngineError::Io(format!("running git: {e}")))?;
    if !out.status.success() {
        return Err(EngineError::Git {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The text of `path` at `rev`, if it exists there.
pub fn show(repo: &Path, rev: &str, path: &str) -> Option<String> {
    git(repo, &["show", &format!("{rev}:{path}")]).ok()
}

/// Checks out `rev` in `repo`, discarding what earlier test runs left behind.
fn checkout(repo: &Path, rev: &str) -> Result<(), EngineError> {
    git(repo, &["reset", "-q", "--hard"])?;
    git(repo, &["clean", "-q", "-fd"])?;
    git(repo, &["checkout", "-q", "-f", "--detach", rev])?;
    Ok(())
}

/// Finds the tests the pull request (`base..head`) adds or changes and runs each on `head`
/// and, when it passes there, on `base`. Leaves `repo` checked out at `base`.
pub fn fail_to_pass(
    repo: &Path,
    base: &str,
    head: &str,
    config: &ReplayConfig,
    runner: &mut dyn Runner,
) -> Result<Vec<(Candidate, AcceptanceTest)>, EngineError> {
    let diff = git(
        repo,
        &["diff", "-U0", "--no-color", "--no-renames", base, head],
    )?;
    let found = candidates(&diff, |f| show(repo, head, f));
    let mut tests: Vec<(Candidate, AcceptanceTest)> = found
        .into_iter()
        .map(|c| {
            let t = AcceptanceTest {
                name: c.name.clone(),
                language: c.language,
                head: TestOutcome::NotRun,
                base: TestOutcome::NotRun,
                result: TestOutcome::NotRun,
            };
            (c, t)
        })
        .collect();
    if tests.is_empty() {
        return Ok(tests);
    }
    let started = Instant::now();
    let left = || STEP_TIMEOUT.saturating_sub(started.elapsed());
    checkout(repo, head)?;
    for (c, t) in &mut tests {
        if let Some(cmd) = command(config, c)
            && !left().is_zero()
        {
            t.head = runner.run(repo, &cmd, TEST_TIMEOUT.min(left()));
            eprintln!("fail-to-pass: {} on head: {:?}", t.name, t.head);
        }
    }
    checkout(repo, base)?;
    for (c, t) in &mut tests {
        if t.head != TestOutcome::Passed {
            continue;
        }
        if !c.defined_in(show(repo, base, &c.file).as_deref()) {
            t.base = TestOutcome::Missing;
        } else if let Some(cmd) = command(config, c)
            && !left().is_zero()
        {
            t.base = runner.run(repo, &cmd, TEST_TIMEOUT.min(left()));
        }
        eprintln!("fail-to-pass: {} on base: {:?}", t.name, t.base);
    }
    Ok(tests)
}

/// Runs the kept tests on the working tree of `repo` (the replay result), recording each
/// outcome. A test the result no longer defines is missing, so it fails.
pub fn run_on_result(
    repo: &Path,
    config: &ReplayConfig,
    tests: &mut [(Candidate, AcceptanceTest)],
    runner: &mut dyn Runner,
) {
    for (c, t) in tests.iter_mut().filter(|(_, t)| t.kept()) {
        let text = std::fs::read_to_string(repo.join(&c.file)).ok();
        t.result = if !c.defined_in(text.as_deref()) {
            TestOutcome::Missing
        } else if let Some(cmd) = command(config, c) {
            runner.run(repo, &cmd, TEST_TIMEOUT)
        } else {
            TestOutcome::NotRun
        };
        eprintln!("acceptance: {} on the result: {:?}", t.name, t.result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(path: &str, text: &str, changed: &[usize]) -> Vec<String> {
        let hunks: String = changed
            .iter()
            .map(|l| format!("@@ -0,0 +{l},1 @@\n+x\n"))
            .collect();
        let diff = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{hunks}");
        candidates(&diff, |_| Some(text.to_owned()))
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    #[test]
    fn languages_by_file_name() {
        assert_eq!(language("src/lib.rs"), Some(TestLanguage::Rust));
        assert_eq!(language("pkg/a_test.go"), Some(TestLanguage::Go));
        assert_eq!(language("pkg/a.go"), None);
        assert_eq!(language("tests/test_api.py"), Some(TestLanguage::Python));
        assert_eq!(language("api_test.py"), Some(TestLanguage::Python));
        assert_eq!(language("api.py"), None);
        assert_eq!(language("src/a.test.ts"), Some(TestLanguage::Javascript));
        assert_eq!(language("src/a.spec.jsx"), Some(TestLanguage::Javascript));
        assert_eq!(language("src/a.ts"), None);
        assert_eq!(language("src/test/java/AT.java"), Some(TestLanguage::Java));
        assert_eq!(language("README.md"), None);
    }

    const RUST: &str = "pub fn rate() -> u32 { 20 }\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn rate_is_twenty() {\n        assert_eq!(rate(), 20);\n    }\n\n    #[tokio::test]\n    #[ignore]\n    async fn later() {\n        let s = \"}\";\n        assert!(true);\n    }\n}\n";

    #[test]
    fn rust_tests_touched_by_the_diff() {
        assert_eq!(found("src/lib.rs", RUST, &[9]), ["rate_is_twenty"]);
        assert_eq!(found("src/lib.rs", RUST, &[17]), ["later"]);
        assert_eq!(found("src/lib.rs", RUST, &[1]), Vec::<String>::new());
        assert_eq!(
            found("src/lib.rs", RUST, &[8, 16]),
            ["rate_is_twenty", "later"]
        );
    }

    #[test]
    fn go_tests() {
        let go = "package billing\n\nimport \"testing\"\n\nfunc TestRetries(t *testing.T) {\n\tif n := retries(); n != 3 {\n\t\tt.Fatal(n)\n\t}\n}\n\nfunc helper(t *testing.T) {}\n\nfunc BenchmarkX(b *testing.B) {}\n";
        assert_eq!(found("billing/a_test.go", go, &[6]), ["TestRetries"]);
        assert!(found("billing/a_test.go", go, &[11, 13]).is_empty());
    }

    #[test]
    fn python_tests() {
        let py = "import pytest\n\n\ndef test_retries():\n    assert retries() == 3\n\n\nclass TestClient:\n    def test_timeout(self):\n        assert True\n\n    def helper(self):\n        pass\n\n\ndef test_other():\n    def inner():\n        pass\n    assert True\n";
        assert_eq!(
            found("tests/test_client.py", py, &[5]),
            ["tests/test_client.py::test_retries"]
        );
        assert_eq!(
            found("tests/test_client.py", py, &[10]),
            ["tests/test_client.py::TestClient::test_timeout"]
        );
        assert!(found("tests/test_client.py", py, &[13]).is_empty());
        assert_eq!(
            found("tests/test_client.py", py, &[18]),
            ["tests/test_client.py::test_other"]
        );
    }

    #[test]
    fn javascript_tests() {
        let js = "import { retries } from './client';\n\ndescribe('client', () => {\n  it('retries three times', () => {\n    expect(retries()).toBe(3);\n  });\n\n  test(\"times out\", async () => {\n    expect(1).toBe(1);\n  });\n\n  it(`uses ${x}`, () => {});\n});\n";
        assert_eq!(
            found("src/client.test.ts", js, &[5]),
            ["retries three times"]
        );
        assert_eq!(found("src/client.test.ts", js, &[9]), ["times out"]);
        assert!(
            found("src/client.test.ts", js, &[12]).is_empty(),
            "template titles are skipped"
        );
    }

    #[test]
    fn java_and_kotlin_tests() {
        let java = "class ClientTest {\n    @Test\n    void retriesThreeTimes() {\n        assertEquals(3, retries());\n    }\n\n    @Test\n    @DisplayName(\"x\")\n    public void timesOut() {\n    }\n\n    void helper() {}\n}\n";
        assert_eq!(
            found("src/test/java/ClientTest.java", java, &[4]),
            ["ClientTest#retriesThreeTimes"]
        );
        assert_eq!(
            found("src/test/java/ClientTest.java", java, &[10]),
            ["ClientTest#timesOut"]
        );
        assert!(found("src/test/java/ClientTest.java", java, &[12]).is_empty());
        let kt = "class ClientTest {\n    @Test\n    fun retries() {\n        assertEquals(3, retries())\n    }\n}\n";
        assert_eq!(
            found("src/test/kotlin/ClientTest.kt", kt, &[4]),
            ["ClientTest#retries"]
        );
    }

    #[test]
    fn changed_lines_of_a_diff() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -3,0 +4,2 @@\n+x\n+y\n@@ -10 +11,0 @@\n-z\ndiff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n";
        let c = changed_lines(diff);
        assert_eq!(c["a.rs"], [4, 5, 11, 12]);
        assert!(
            !c.contains_key("gone.rs"),
            "deleted files have no new lines"
        );
    }

    #[test]
    fn candidates_are_capped() {
        let many: String = (0..20)
            .map(|i| format!("#[test]\nfn t{i}() {{\n}}\n"))
            .collect();
        let all: Vec<usize> = (1..=60).collect();
        assert_eq!(
            found("src/lib.rs", &many, &all).len(),
            MAX_ACCEPTANCE_CANDIDATES
        );
    }

    #[test]
    fn definitions_are_checked_by_name_and_function() {
        let c = Candidate {
            name: "rate_is_twenty".into(),
            language: TestLanguage::Rust,
            file: "src/lib.rs".into(),
            definition: "rate_is_twenty".into(),
        };
        assert!(c.defined_in(Some(RUST)));
        assert!(
            !c.defined_in(Some("pub fn rate_is_twenty() {}\n")),
            "not a test"
        );
        assert!(!c.defined_in(None));
    }

    #[test]
    fn commands_quote_the_name() {
        let config = ReplayConfig {
            test_command: Some("npx vitest run -t {tests}".into()),
            ..ReplayConfig::default()
        };
        let c = Candidate {
            name: "it's done".into(),
            language: TestLanguage::Javascript,
            file: "a.test.ts".into(),
            definition: "it's done".into(),
        };
        assert_eq!(
            command(&config, &c).as_deref(),
            Some("npx vitest run -t 'it'\\''s done'")
        );
        assert_eq!(command(&ReplayConfig::default(), &c), None);
    }

    #[test]
    fn the_shell_runner_reports_exit_codes_and_timeouts() {
        let dir = std::env::temp_dir();
        let mut r = ShellRunner;
        assert_eq!(r.run(&dir, "true", TEST_TIMEOUT), TestOutcome::Passed);
        assert_eq!(r.run(&dir, "exit 3", TEST_TIMEOUT), TestOutcome::Failed);
        let t = Instant::now();
        assert_eq!(
            r.run(&dir, "sleep 30", Duration::from_millis(300)),
            TestOutcome::TimedOut
        );
        assert!(t.elapsed() < Duration::from_secs(10));
        assert_eq!(
            r.run(&dir, "test -z \"$ANTHROPIC_API_KEY\"", TEST_TIMEOUT),
            TestOutcome::Passed
        );
    }
}
