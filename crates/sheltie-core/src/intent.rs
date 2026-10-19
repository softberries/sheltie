//! Agent intents: what a pull request is meant to do, so it can be re-derived (tier 4).
//!
//! Replay does not need one: the replay client works out the goal from the pull request
//! itself and finds acceptance tests in its diff ([`AcceptanceTest`]). An author can still add
//! an explicit intent, a fenced block in the pull request description:
//!
//! ````text
//! ```sheltie-intent
//! {
//!   "task": "Add retries to the billing client",
//!   "plan": "Wrap send() in a loop with backoff; add a test",
//!   "acceptance": ["billing::client::tests::retries_three_times"]
//! }
//! ```
//! ````
//!
//! The description is written by whoever opened the pull request, so the intent is untrusted:
//! it is size-limited, unknown fields are rejected, and acceptance test names may only contain
//! characters that are safe to pass to a test command.

use serde::{Deserialize, Serialize};

use crate::ids::{CommitSha, PrNumber, RepoPath};
use crate::resolution::ResolutionTier;
use crate::score::Score;

/// The fence that marks an intent block.
pub const INTENT_FENCE: &str = "```sheltie-intent";

/// Limits on an intent.
pub mod limits {
    /// Longest task description.
    pub const MAX_TASK: usize = 8_000;
    /// Longest plan.
    pub const MAX_PLAN: usize = 16_000;
    /// Most acceptance tests.
    pub const MAX_TESTS: usize = 50;
    /// Longest acceptance test name.
    pub const MAX_TEST_NAME: usize = 300;
}

/// What a change is meant to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// The task the agent was given.
    pub task: String,
    /// The plan the agent followed, if recorded.
    #[serde(default)]
    pub plan: Option<String>,
    /// Tests that must pass for the change to count as done.
    #[serde(default)]
    pub acceptance: Vec<String>,
}

/// Why an intent block was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid sheltie-intent block: {0}")]
pub struct InvalidIntent(pub String);

impl Intent {
    /// Finds and parses the intent block in a pull request description. `Ok(None)` when there
    /// is no block.
    pub fn from_description(body: &str) -> Result<Option<Self>, InvalidIntent> {
        let Some(start) = body.find(INTENT_FENCE) else {
            return Ok(None);
        };
        let rest = &body[start + INTENT_FENCE.len()..];
        let Some(end) = rest.find("```") else {
            return Err(InvalidIntent("the block is not closed with ```".into()));
        };
        let intent: Intent =
            serde_json::from_str(rest[..end].trim()).map_err(|e| InvalidIntent(e.to_string()))?;
        intent.validate()?;
        Ok(Some(intent))
    }

    /// Checks sizes and characters.
    pub fn validate(&self) -> Result<(), InvalidIntent> {
        use limits::*;
        let fail = |m: &str| Err(InvalidIntent(m.to_owned()));
        if self.task.trim().is_empty() || self.task.chars().count() > MAX_TASK {
            return fail("`task` must be non-empty and at most 8000 characters");
        }
        if self
            .plan
            .as_ref()
            .is_some_and(|p| p.chars().count() > MAX_PLAN)
        {
            return fail("`plan` is longer than 16000 characters");
        }
        if self.acceptance.len() > MAX_TESTS {
            return fail("more than 50 acceptance tests");
        }
        let safe = |t: &str| {
            !t.is_empty()
                && t.len() <= MAX_TEST_NAME
                && t.bytes().all(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'_' | b':' | b'.' | b'-' | b'/')
                })
        };
        if !self.acceptance.iter().all(|t| safe(t)) {
            return fail("acceptance test names may only contain letters, digits and _ : . - /");
        }
        Ok(())
    }
}

/// Results of the checks the replay client runs on the agent's work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayChecks {
    /// Files the replay changed outside the original pull request's files and the conflicted
    /// ones; must be empty.
    pub outside_scope: Vec<RepoPath>,
    /// Whether the acceptance tests passed; `None` when the intent lists none.
    pub acceptance_passed: Option<bool>,
    /// How close the replayed diff is to the original.
    pub similarity: Score,
}

/// Tokens an agent run used, as the agent reported them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens not read from or written to the prompt cache.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
}

impl TokenUsage {
    /// Upper bound for each count in an upload; far above any real run.
    pub const MAX: u64 = 10_000_000_000;

    /// All tokens, input and output.
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    fn in_range(&self) -> bool {
        [self.input, self.output, self.cache_read, self.cache_write]
            .iter()
            .all(|&n| n <= Self::MAX)
    }
}

/// Most tests the replay client checks fail-to-pass.
pub const MAX_ACCEPTANCE_CANDIDATES: usize = 10;

/// What the replay prompt was built from, besides the diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayContext {
    /// A valid `sheltie-intent` block in the description supplied the task.
    pub intent_block: bool,
    /// The pull request's title and description.
    pub description: bool,
    /// The linked issue (first closing reference in the description), when one was read.
    pub issue: Option<u64>,
    /// Commit messages included.
    pub commits: u32,
}

/// The language of a test found in a diff; selects the test command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLanguage {
    /// `#[test]` functions.
    Rust,
    /// `func TestX(t *testing.T)` in `_test.go` files.
    Go,
    /// pytest `test_*` functions.
    Python,
    /// `it`/`test` cases in `*.test.*` and `*.spec.*` files (JavaScript and TypeScript).
    Javascript,
    /// `@Test` methods (Java and Kotlin).
    Java,
}

impl TestLanguage {
    /// Every language, in a stable order.
    pub const ALL: [TestLanguage; 5] = [
        TestLanguage::Rust,
        TestLanguage::Go,
        TestLanguage::Python,
        TestLanguage::Javascript,
        TestLanguage::Java,
    ];

    /// The name used in configuration and reports.
    pub fn as_str(self) -> &'static str {
        match self {
            TestLanguage::Rust => "rust",
            TestLanguage::Go => "go",
            TestLanguage::Python => "python",
            TestLanguage::Javascript => "javascript",
            TestLanguage::Java => "java",
        }
    }
}

/// How one run of one test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestOutcome {
    /// The command exited with success.
    Passed,
    /// The command exited with failure.
    Failed,
    /// The command ran past its time limit and was stopped.
    TimedOut,
    /// The test is not defined in that tree, so it was not run; on the base this counts as
    /// failing, on the replay result as failed.
    Missing,
    /// Not run (an earlier run already decided the test, or no command for its language).
    NotRun,
}

/// A test the pull request added or changed, checked fail-to-pass: kept as an acceptance
/// test when it passes on the pull request's head and fails (or is missing) on its base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceTest {
    /// The name passed to the test command.
    pub name: String,
    /// Its language.
    pub language: TestLanguage,
    /// On the pull request's head.
    pub head: TestOutcome,
    /// On the merge base.
    pub base: TestOutcome,
    /// On the replay result; [`TestOutcome::NotRun`] when it was not kept.
    pub result: TestOutcome,
}

impl AcceptanceTest {
    /// Whether the test shows the change's behaviour: passes with the change, not without.
    pub fn kept(&self) -> bool {
        self.head == TestOutcome::Passed
            && matches!(self.base, TestOutcome::Failed | TestOutcome::Missing)
    }
}

/// What the replay client reports after running the agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayReport {
    /// The pull request.
    pub pr: PrNumber,
    /// Its head.
    pub head: CommitSha,
    /// The default-branch commit it was replayed onto.
    pub onto: CommitSha,
    /// [`ResolutionTier::ReplayHunks`] (4a) or [`ResolutionTier::ReplayFull`] (4b).
    pub tier: ResolutionTier,
    /// The merge commit (parents `onto`, `head`), when the agent finished and the checks ran.
    pub result: Option<CommitSha>,
    /// The checks, when the agent finished.
    pub checks: Option<ReplayChecks>,
    /// Estimated cost of the agent run in US dollars, as the agent reported it.
    pub cost_usd: f64,
    /// Agent turns used.
    pub turns: u32,
    /// Tokens the agent used, when it reported them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenUsage>,
    /// The model the agent used most, e.g. `claude-sonnet-4-5`, when it reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why the agent run failed, if it did.
    pub error: Option<String>,
    /// What the prompt was built from; absent in reports from clients before 0.6.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ReplayContext>,
    /// Tests found in the pull request's diff and how they ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub acceptance: Vec<AcceptanceTest>,
}

impl ReplayReport {
    /// Bounds sizes and checks consistency; uploads are untrusted.
    pub fn validate(&self) -> Result<(), crate::InvalidReport> {
        let fail = |m: &str| Err(crate::InvalidReport(m.to_owned()));
        if !self.tier.is_replay() {
            return fail("tier must be replay_hunks or replay_full");
        }
        if !(self.cost_usd.is_finite() && self.cost_usd >= 0.0 && self.cost_usd < 10_000.0) {
            return fail("cost out of range");
        }
        if self.tokens.is_some_and(|t| !t.in_range()) {
            return fail("token count out of range");
        }
        if self.model.as_ref().is_some_and(|m| {
            !(1..=100).contains(&m.len())
                || !m.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '.' | '-' | '_' | ':' | '/' | '[' | ']')
                })
        }) {
            return fail("invalid model name");
        }
        if self.error.as_ref().is_some_and(|e| e.len() > 2_000) {
            return fail("error too long");
        }
        if self
            .checks
            .as_ref()
            .is_some_and(|c| c.outside_scope.len() > 10_000)
        {
            return fail("too many files");
        }
        if self.result.is_some() && self.checks.is_none() {
            return fail("a result needs checks");
        }
        if self.acceptance.len() > MAX_ACCEPTANCE_CANDIDATES {
            return fail("too many acceptance tests");
        }
        if self.acceptance.iter().any(|t| {
            t.name.is_empty()
                || t.name.len() > limits::MAX_TEST_NAME
                || t.name.chars().any(char::is_control)
        }) {
            return fail("invalid acceptance test name");
        }
        if self.context.as_ref().is_some_and(|c| {
            c.commits > 10_000 || c.issue.is_some_and(|i| i == 0 || i > u64::from(u32::MAX))
        }) {
            return fail("invalid context");
        }
        Ok(())
    }

    /// The tests the replay was checked against: kept fail-to-pass tests. Tests listed in an
    /// intent block are not in this list; [`ReplayChecks::acceptance_passed`] covers both.
    pub fn acceptance_tests(&self) -> impl Iterator<Item = &AcceptanceTest> {
        self.acceptance.iter().filter(|t| t.kept())
    }

    /// Whether every check passed (scope, acceptance) and a result exists. Similarity is
    /// judged by the caller against its threshold.
    pub fn checks_passed(&self) -> bool {
        self.result.is_some()
            && self.error.is_none()
            && self
                .checks
                .as_ref()
                .is_some_and(|c| c.outside_scope.is_empty() && c.acceptance_passed != Some(false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(json: &str) -> String {
        format!("Adds retries.\n\n{INTENT_FENCE}\n{json}\n```\n\nThanks!")
    }

    #[test]
    fn parses_a_block() {
        let i = Intent::from_description(&body(
            r#"{"task":"Add retries","acceptance":["billing::tests::retries"]}"#,
        ))
        .unwrap()
        .unwrap();
        assert_eq!(i.task, "Add retries");
        assert_eq!(i.acceptance, ["billing::tests::retries"]);
    }

    #[test]
    fn no_block_is_none() {
        assert_eq!(Intent::from_description("Just a description."), Ok(None));
    }

    #[test]
    fn rejects_bad_blocks() {
        for bad in [
            r#"{"task":""}"#,
            r#"{"task":"x","extra":1}"#,
            r#"{"task":"x","acceptance":["a; rm -rf /"]}"#,
            r#"{"task":"x","acceptance":["$(curl evil)"]}"#,
            "not json",
        ] {
            assert!(
                Intent::from_description(&body(bad)).is_err(),
                "accepted {bad}"
            );
        }
        assert!(
            Intent::from_description(&format!("{INTENT_FENCE}\n{{\"task\":\"x\"}}")).is_err(),
            "unclosed"
        );
        let long = format!(r#"{{"task":"{}"}}"#, "x".repeat(limits::MAX_TASK + 1));
        assert!(Intent::from_description(&body(&long)).is_err());
    }

    fn report(extra: &str) -> ReplayReport {
        serde_json::from_str(&format!(
            r#"{{"pr":7,"head":"{h}","onto":"{o}","tier":"replay_full","result":null,
                "checks":null,"cost_usd":0.5,"turns":3,"error":"failed"{extra}}}"#,
            h = "a".repeat(40),
            o = "b".repeat(40),
        ))
        .unwrap()
    }

    #[test]
    fn reports_without_tokens_still_parse() {
        let r = report("");
        assert_eq!(r.tokens, None);
        r.validate().unwrap();
        assert!(!serde_json::to_string(&r).unwrap().contains("tokens"));
    }

    #[test]
    fn model_names_are_checked() {
        assert_eq!(report("").model, None);
        let r = report(r#","model":"claude-sonnet-5-5[1m]""#);
        assert_eq!(r.model.as_deref(), Some("claude-sonnet-5-5[1m]"));
        r.validate().unwrap();
        for bad in [r#""""#, r#""a b""#, r#""<script>""#] {
            let r = report(&format!(r#","model":{bad}"#));
            assert!(r.validate().is_err(), "{bad}");
        }
        let long = format!(r#","model":"{}""#, "m".repeat(101));
        assert!(report(&long).validate().is_err());
    }

    #[test]
    fn reports_from_older_clients_have_no_context_or_tests() {
        let r = report("");
        assert_eq!(r.context, None);
        assert!(r.acceptance.is_empty());
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("context") && !json.contains("acceptance\""));
    }

    fn test(name: &str, head: TestOutcome, base: TestOutcome) -> AcceptanceTest {
        AcceptanceTest {
            name: name.into(),
            language: TestLanguage::Rust,
            head,
            base,
            result: TestOutcome::NotRun,
        }
    }

    #[test]
    fn a_test_is_kept_when_it_fails_without_the_change() {
        use TestOutcome::*;
        assert!(test("a", Passed, Failed).kept());
        assert!(test("a", Passed, Missing).kept());
        assert!(!test("a", Passed, Passed).kept());
        assert!(!test("a", Passed, TimedOut).kept());
        assert!(!test("a", Failed, Failed).kept());
        assert!(!test("a", TimedOut, Missing).kept());
    }

    #[test]
    fn context_and_tests_are_bounded() {
        let mut r = report(
            r#","context":{"intent_block":false,"description":true,"issue":12,"commits":3},
               "acceptance":[{"name":"retries works","language":"javascript","head":"passed","base":"missing","result":"passed"}]"#,
        );
        r.validate().unwrap();
        assert_eq!(r.context.as_ref().unwrap().issue, Some(12));
        assert_eq!(r.acceptance_tests().count(), 1);
        r.acceptance[0].name = "a\nb".into();
        assert!(r.validate().is_err());
        r.acceptance = vec![
            test("a", TestOutcome::Passed, TestOutcome::Failed);
            MAX_ACCEPTANCE_CANDIDATES + 1
        ];
        assert!(r.validate().is_err());
        r.acceptance.clear();
        r.context = Some(ReplayContext {
            issue: Some(0),
            ..ReplayContext::default()
        });
        assert!(r.validate().is_err());
    }

    #[test]
    fn token_counts_are_bounded() {
        let r = report(r#","tokens":{"input":10,"output":20,"cache_read":300,"cache_write":40}"#);
        let tokens = r.tokens.unwrap();
        assert_eq!(tokens.total(), 370);
        r.validate().unwrap();
        let too_many = ReplayReport {
            tokens: Some(TokenUsage {
                output: TokenUsage::MAX + 1,
                ..tokens
            }),
            ..r
        };
        assert!(too_many.validate().is_err());
    }
}
