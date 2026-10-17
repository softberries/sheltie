//! `.sheltie/merge.toml`: declared merge behaviour for tiers 2 and 3.
//!
//! Read from the default branch only (never from the pull request being merged) and parsed
//! strictly: unknown keys, invalid globs or regexes, oversized files and suspicious commands
//! are rejected.
//!
//! ```toml
//! [[derived]]          # regenerate instead of merging
//! paths = ["Cargo.lock"]
//! regenerate = "cargo update -w --offline"
//!
//! [[set]]              # order-insensitive lines: union of both sides
//! paths = ["config/feature_flags.txt"]
//!
//! [[sequence]]         # numbered files: renumber the later side on collision
//! paths = ["migrations/*.sql"]
//! key = "^(\\d+)_"
//!
//! [replay]             # tier 4: re-derive a change from its intent (off by default)
//! enabled = true
//! test_command = "cargo test --workspace -- {tests}"
//! model = "sonnet"
//! max_turns = 30
//! max_budget_usd = 3.0
//! daily_budget_usd = 20.0
//! similarity_threshold = 0.6
//! max_attempts = 2
//! ```

use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;
use serde::Deserialize;

/// Largest accepted rules file.
pub const MAX_RULES_BYTES: usize = 64 * 1024;
/// Most rules of all kinds together.
pub const MAX_RULES: usize = 100;

/// Why a rules file was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid .sheltie/merge.toml: {0}")]
pub struct RulesError(pub String);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRules {
    #[serde(default)]
    derived: Vec<RawDerived>,
    #[serde(default)]
    set: Vec<RawPaths>,
    #[serde(default)]
    sequence: Vec<RawSequence>,
    #[serde(default)]
    replay: Option<ReplayConfig>,
    #[serde(default)]
    estimates: Estimates,
}

/// Assumptions for the dashboard's time-saved estimate, `[estimates]`. Unset values use
/// Sheltie's defaults (15 minutes per conflict, 30 per replay).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Estimates {
    /// Minutes an author spends fixing a conflict the merge engine resolved (tiers 1-3).
    pub minutes_per_conflict: Option<u32>,
    /// Minutes an author spends redoing a change that was re-derived from its intent (tier 4).
    pub minutes_per_replay: Option<u32>,
}

impl Estimates {
    fn validate(&self) -> Result<(), RulesError> {
        for (name, v) in [
            ("minutes_per_conflict", self.minutes_per_conflict),
            ("minutes_per_replay", self.minutes_per_replay),
        ] {
            if v.is_some_and(|m| m > 1440) {
                return Err(RulesError(format!(
                    "[estimates] `{name}` must be at most 1440"
                )));
            }
        }
        Ok(())
    }
}

/// Intent replay settings (tier 4). Off unless `enabled`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ReplayConfig {
    /// Whether Sheltie may replay changes in this repository.
    pub enabled: bool,
    /// Runs acceptance tests; `{tests}` is replaced by the intent's test names, each quoted.
    pub test_command: Option<String>,
    /// Model for the agent.
    pub model: String,
    /// Most agent turns per run.
    pub max_turns: u32,
    /// Most estimated spend per run, in US dollars.
    pub max_budget_usd: f64,
    /// Most estimated spend per repository per day, in US dollars.
    pub daily_budget_usd: f64,
    /// Below this similarity to the original diff, a replay needs re-approval.
    pub similarity_threshold: f32,
    /// Most replay attempts per change before a human takes over.
    pub max_attempts: u32,
}

impl Default for ReplayConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            test_command: None,
            model: "sonnet".into(),
            max_turns: 30,
            max_budget_usd: 3.0,
            daily_budget_usd: 20.0,
            similarity_threshold: 0.6,
            max_attempts: 2,
        }
    }
}

impl ReplayConfig {
    fn validate(&self) -> Result<(), RulesError> {
        let fail = |m: &str| Err(RulesError(format!("[replay] {m}")));
        if let Some(cmd) = &self.test_command
            && (cmd.trim().is_empty()
                || cmd.len() > 500
                || cmd.chars().any(char::is_control)
                || !cmd.contains("{tests}"))
        {
            return fail(
                "`test_command` must be one line of at most 500 characters containing {tests}",
            );
        }
        if self.model.is_empty()
            || self.model.len() > 100
            || !self
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
        {
            return fail("`model` must be a model alias or id");
        }
        if !(1..=200).contains(&self.max_turns) {
            return fail("`max_turns` must be 1..=200");
        }
        if !(self.max_budget_usd > 0.0 && self.max_budget_usd <= 100.0) {
            return fail("`max_budget_usd` must be in (0, 100]");
        }
        if !(self.daily_budget_usd > 0.0 && self.daily_budget_usd <= 10_000.0) {
            return fail("`daily_budget_usd` must be in (0, 10000]");
        }
        if !(0.0..=1.0).contains(&self.similarity_threshold) {
            return fail("`similarity_threshold` must be in [0, 1]");
        }
        if !(1..=5).contains(&self.max_attempts) {
            return fail("`max_attempts` must be 1..=5");
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDerived {
    paths: Vec<String>,
    regenerate: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPaths {
    paths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSequence {
    paths: Vec<String>,
    key: String,
}

/// A file regenerated by a command instead of being merged (tier 2).
#[derive(Debug, Clone)]
pub struct Derived {
    paths: GlobSet,
    /// The shell command that regenerates the files, run in the repository root.
    pub regenerate: String,
}

/// Numbered files whose numbers must stay unique (tier 3).
#[derive(Debug, Clone)]
pub struct Sequence {
    paths: GlobSet,
    /// Captures the number in group 1, matched against the file name.
    pub key: Regex,
}

/// What a path's rule says to do with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind<'a> {
    /// Regenerate with this rule.
    Derived(&'a Derived),
    /// Union of lines.
    Set,
    /// Renumber on collision.
    Sequence(&'a Sequence),
}

impl PartialEq for Derived {
    fn eq(&self, other: &Self) -> bool {
        self.regenerate == other.regenerate
    }
}

impl Eq for Derived {}

impl PartialEq for Sequence {
    fn eq(&self, other: &Self) -> bool {
        self.key.as_str() == other.key.as_str()
    }
}

impl Eq for Sequence {}

/// Parsed merge rules.
#[derive(Debug, Clone, Default)]
pub struct Rules {
    /// Tier-2 rules.
    pub derived: Vec<Derived>,
    sets: Vec<GlobSet>,
    /// Tier-3 sequence rules.
    pub sequences: Vec<Sequence>,
    /// Tier-4 settings.
    pub replay: ReplayConfig,
    /// Time-saved assumptions.
    pub estimates: Estimates,
}

fn globs(paths: &[String]) -> Result<GlobSet, RulesError> {
    if paths.is_empty() {
        return Err(RulesError("a rule needs at least one path".into()));
    }
    let mut b = GlobSetBuilder::new();
    for p in paths {
        if p.starts_with('/') || p.split('/').any(|s| s == "..") {
            return Err(RulesError(format!(
                "path {p:?} must be relative and stay in the repository"
            )));
        }
        b.add(Glob::new(p).map_err(|e| RulesError(format!("glob {p:?}: {e}")))?);
    }
    b.build().map_err(|e| RulesError(e.to_string()))
}

impl Rules {
    /// Parses a rules file.
    pub fn parse(text: &str) -> Result<Self, RulesError> {
        if text.len() > MAX_RULES_BYTES {
            return Err(RulesError(format!("larger than {MAX_RULES_BYTES} bytes")));
        }
        let raw: RawRules = toml::from_str(text).map_err(|e| RulesError(e.to_string()))?;
        if raw.derived.len() + raw.set.len() + raw.sequence.len() > MAX_RULES {
            return Err(RulesError(format!("more than {MAX_RULES} rules")));
        }
        let derived = raw
            .derived
            .into_iter()
            .map(|d| {
                let cmd = d.regenerate.trim();
                if cmd.is_empty() || cmd.len() > 500 || cmd.chars().any(|c| c.is_control()) {
                    return Err(RulesError(
                        "`regenerate` must be one line of at most 500 characters".into(),
                    ));
                }
                Ok(Derived {
                    paths: globs(&d.paths)?,
                    regenerate: cmd.to_owned(),
                })
            })
            .collect::<Result<_, _>>()?;
        let sets = raw
            .set
            .iter()
            .map(|s| globs(&s.paths))
            .collect::<Result<_, _>>()?;
        let sequences = raw
            .sequence
            .into_iter()
            .map(|s| {
                let key =
                    Regex::new(&s.key).map_err(|e| RulesError(format!("key {:?}: {e}", s.key)))?;
                if key.captures_len() < 2 {
                    return Err(RulesError(format!(
                        "key {:?} needs a capture group for the number",
                        s.key
                    )));
                }
                Ok(Sequence {
                    paths: globs(&s.paths)?,
                    key,
                })
            })
            .collect::<Result<_, _>>()?;
        let replay = raw.replay.unwrap_or_default();
        replay.validate()?;
        raw.estimates.validate()?;
        Ok(Self {
            derived,
            sets,
            sequences,
            replay,
            estimates: raw.estimates,
        })
    }

    /// The rule for `path`, if any. Derived rules win over set rules over sequence rules.
    pub fn kind(&self, path: &str) -> Option<Kind<'_>> {
        if let Some(d) = self.derived.iter().find(|d| d.paths.is_match(path)) {
            return Some(Kind::Derived(d));
        }
        if self.sets.iter().any(|s| s.is_match(path)) {
            return Some(Kind::Set);
        }
        self.sequences
            .iter()
            .find(|s| s.paths.is_match(path))
            .map(Kind::Sequence)
    }

    /// Whether `path` is a derived file of some rule.
    pub fn is_derived(&self, path: &str) -> bool {
        matches!(self.kind(path), Some(Kind::Derived(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
[[derived]]
paths = ["Cargo.lock"]
regenerate = "cargo update -w --offline"

[[set]]
paths = ["config/*.txt"]

[[sequence]]
paths = ["migrations/*.sql"]
key = "^(\\d+)_"
"#;

    #[test]
    fn estimates() {
        let r = Rules::parse("[estimates]\nminutes_per_conflict = 20\n").unwrap();
        assert_eq!(r.estimates.minutes_per_conflict, Some(20));
        assert_eq!(r.estimates.minutes_per_replay, None);
        assert_eq!(Rules::parse("").unwrap().estimates, Estimates::default());
        assert!(Rules::parse("[estimates]\nminutes_per_replay = 5000\n").is_err());
        assert!(Rules::parse("[estimates]\nhours = 1\n").is_err());
    }

    #[test]
    fn parses_and_classifies() {
        let r = Rules::parse(EXAMPLE).unwrap();
        assert!(
            matches!(r.kind("Cargo.lock"), Some(Kind::Derived(d)) if d.regenerate == "cargo update -w --offline")
        );
        assert_eq!(r.kind("config/flags.txt"), Some(Kind::Set));
        assert!(matches!(
            r.kind("migrations/0001_init.sql"),
            Some(Kind::Sequence(_))
        ));
        assert_eq!(r.kind("src/lib.rs"), None);
    }

    #[test]
    fn replay_defaults_and_bounds() {
        assert!(!Rules::parse("").unwrap().replay.enabled);
        let r =
            Rules::parse("[replay]\nenabled = true\ntest_command = \"cargo test -- {tests}\"\n")
                .unwrap();
        assert!(r.replay.enabled);
        assert_eq!((r.replay.max_turns, r.replay.max_attempts), (30, 2));
        for bad in [
            "[replay]\ntest_command = \"cargo test\"",
            "[replay]\nmax_turns = 0",
            "[replay]\nsimilarity_threshold = 1.5",
            "[replay]\nmodel = \"x; rm\"",
            "[replay]\nunknown = 1",
        ] {
            assert!(Rules::parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "[[derived]]\npaths = [\"x\"]\nregenerate = \"\"",
            "[[derived]]\npaths = [\"x\"]\nregenerate = \"a\\nb\"",
            "[[set]]\npaths = []",
            "[[set]]\npaths = [\"../x\"]",
            "[[set]]\npaths = [\"/etc/passwd\"]",
            "[[sequence]]\npaths = [\"m/*\"]\nkey = \"\\\\d+\"",
            "[[sequence]]\npaths = [\"m/*\"]\nkey = \"(\"",
            "[[set]]\npaths = [\"x\"]\nextra = 1",
            "[[unknown]]\npaths = [\"x\"]",
        ] {
            assert!(Rules::parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(Rules::parse(&"#".repeat(MAX_RULES_BYTES + 1)).is_err());
    }
}
