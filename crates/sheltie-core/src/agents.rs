//! Which agent opened a pull request, and per-agent scorecards.
//!
//! A pull request's [`PrOrigin`] (author, branch, labels) is matched against the repository's
//! `[[agents]]` rules from `.sheltie/merge.toml`, first match wins. Without a match, a bot
//! account is its own agent (`claude[bot]` → `claude`) and people are grouped together, so
//! the scorecards never rank individual colleagues. [`scorecards`] aggregates the outcomes
//! of each agent's pull requests.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::Login;

/// The agent name for pull requests opened by people (no rule matched, not a bot).
pub const PEOPLE: &str = "people";
/// The agent name for pull requests whose author Sheltie has not recorded.
pub const UNKNOWN: &str = "unknown";
/// Most `[[agents]]` rules.
pub const MAX_AGENT_RULES: usize = 50;
/// Most labels kept per pull request.
pub const MAX_LABELS: usize = 50;
/// Longest label, branch prefix or author in a rule.
pub const MAX_MATCH: usize = 255;

/// Who opened a pull request and how it is tagged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrOrigin {
    /// The author's login.
    pub author: Login,
    /// Whether the author is a bot or app account.
    pub bot: bool,
    /// The head branch, e.g. `codex/fix-login`.
    pub head_ref: String,
    /// The pull request's labels, at most [`MAX_LABELS`].
    pub labels: Vec<String>,
}

/// One `[[agents]]` rule. Every condition given must match; at least one is required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRule {
    /// The agent's name in the scorecards, e.g. `codex`.
    pub name: String,
    /// The model, when the rule identifies it, e.g. `gpt-5-codex`.
    #[serde(default)]
    pub model: Option<String>,
    /// The author's login, exactly (case-insensitive).
    #[serde(default)]
    pub author: Option<String>,
    /// A prefix of the head branch.
    #[serde(default)]
    pub branch: Option<String>,
    /// A label the pull request carries (case-insensitive).
    #[serde(default)]
    pub label: Option<String>,
}

/// Why an `[[agents]]` rule is rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("[[agents]] {0}")]
pub struct InvalidAgentRule(pub String);

fn name_ok(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.len())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-' | '/'))
}

impl AgentRule {
    /// Checks names and conditions.
    pub fn validate(&self) -> Result<(), InvalidAgentRule> {
        if !name_ok(&self.name, 40) {
            return Err(InvalidAgentRule(format!(
                "`name` {:?} must be 1-40 letters, digits, spaces or . _ - /",
                self.name
            )));
        }
        if let Some(m) = &self.model
            && !name_ok(m, 60)
        {
            return Err(InvalidAgentRule(format!(
                "`model` {m:?} must be 1-60 letters, digits, spaces or . _ - /"
            )));
        }
        let conditions = [&self.author, &self.branch, &self.label];
        if conditions.iter().all(|c| c.is_none()) {
            return Err(InvalidAgentRule(format!(
                "{:?} needs `author`, `branch` or `label`",
                self.name
            )));
        }
        for c in conditions.into_iter().flatten() {
            if c.is_empty() || c.len() > MAX_MATCH || c.chars().any(char::is_control) {
                return Err(InvalidAgentRule(format!(
                    "{:?}: conditions must be 1-{MAX_MATCH} characters",
                    self.name
                )));
            }
        }
        Ok(())
    }

    fn matches(&self, o: &PrOrigin) -> bool {
        self.author
            .as_ref()
            .is_none_or(|a| a.eq_ignore_ascii_case(o.author.as_str()))
            && self
                .branch
                .as_ref()
                .is_none_or(|b| o.head_ref.starts_with(b.as_str()))
            && self
                .label
                .as_ref()
                .is_none_or(|l| o.labels.iter().any(|x| x.eq_ignore_ascii_case(l)))
    }
}

/// Checks a whole rule list.
pub fn validate_rules(rules: &[AgentRule]) -> Result<(), InvalidAgentRule> {
    if rules.len() > MAX_AGENT_RULES {
        return Err(InvalidAgentRule(format!(
            "more than {MAX_AGENT_RULES} rules"
        )));
    }
    rules.iter().try_for_each(AgentRule::validate)
}

/// An agent, and the model when a rule names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct AgentKey {
    /// The agent's name.
    pub agent: String,
    /// Its model, if known.
    pub model: Option<String>,
}

/// The agent behind a pull request.
pub fn classify(rules: &[AgentRule], origin: Option<&PrOrigin>) -> AgentKey {
    let Some(o) = origin else {
        return AgentKey {
            agent: UNKNOWN.into(),
            model: None,
        };
    };
    if let Some(r) = rules.iter().find(|r| r.matches(o)) {
        return AgentKey {
            agent: r.name.clone(),
            model: r.model.clone(),
        };
    }
    let agent = if o.bot {
        o.author
            .as_str()
            .strip_suffix("[bot]")
            .unwrap_or(o.author.as_str())
            .to_owned()
    } else {
        PEOPLE.to_owned()
    };
    AgentKey { agent, model: None }
}

/// What became of one pull request.
#[derive(Debug, Clone, PartialEq)]
pub struct PrOutcome {
    /// Who opened it.
    pub origin: Option<PrOrigin>,
    /// When Sheltie first saw it.
    pub opened_at: DateTime<Utc>,
    /// When it merged, if it did.
    pub merged_at: Option<DateTime<Utc>>,
    /// Closed without merging.
    pub closed_unmerged: bool,
    /// Heads pushed (one plus the number of updates).
    pub heads: u32,
    /// Sheltie returned it to its author at least once.
    pub returned: bool,
    /// Sheltie reverted it after it landed.
    pub reverted: bool,
    /// Sheltie re-derived it with an agent.
    pub replayed: bool,
    /// Reviews that requested changes.
    pub changes_requested: u32,
    /// Estimated cost of Sheltie's replays of it, in US dollars.
    pub replay_cost_usd: f64,
}

/// One agent's numbers.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scorecard {
    /// The agent.
    pub key: AgentKey,
    /// Pull requests opened.
    pub prs: u32,
    /// Merged.
    pub merged: u32,
    /// Closed without merging.
    pub closed: u32,
    /// Returned to the author by Sheltie at least once.
    pub returned: u32,
    /// Merged and later reverted by Sheltie.
    pub reverted: u32,
    /// Merged after Sheltie re-derived them with an agent.
    pub replayed: u32,
    /// Merged / (merged + closed); `None` before any finished.
    pub merge_rate: Option<f64>,
    /// Reviews requesting changes per merged pull request.
    pub review_rounds: Option<f64>,
    /// Pushes per pull request.
    pub heads_per_pr: f64,
    /// Median hours from opened to merged.
    pub median_hours_to_merge: Option<f64>,
    /// Sheltie's replay spend on this agent's pull requests, in US dollars.
    pub replay_cost_usd: f64,
}

impl Scorecard {
    /// `part / whole`, or `None` when `whole` is 0.
    pub fn rate(part: u32, whole: u32) -> Option<f64> {
        (whole > 0).then(|| f64::from(part) / f64::from(whole))
    }

    /// Replay spend per merged pull request.
    pub fn cost_per_merged(&self) -> Option<f64> {
        (self.merged > 0).then(|| self.replay_cost_usd / f64::from(self.merged))
    }
}

/// Scorecards per agent, most pull requests first.
pub fn scorecards(rules: &[AgentRule], prs: &[PrOutcome]) -> Vec<Scorecard> {
    let mut by_agent: std::collections::BTreeMap<AgentKey, Vec<&PrOutcome>> =
        std::collections::BTreeMap::new();
    for p in prs {
        by_agent
            .entry(classify(rules, p.origin.as_ref()))
            .or_default()
            .push(p);
    }
    let mut cards: Vec<Scorecard> = by_agent
        .into_iter()
        .map(|(key, list)| {
            let count = |f: &dyn Fn(&PrOutcome) -> bool| {
                u32::try_from(list.iter().filter(|p| f(p)).count()).unwrap_or(u32::MAX)
            };
            let prs = u32::try_from(list.len()).unwrap_or(u32::MAX);
            let merged = count(&|p| p.merged_at.is_some());
            let closed = count(&|p| p.merged_at.is_none() && p.closed_unmerged);
            let mut hours: Vec<f64> = list
                .iter()
                .filter_map(|p| {
                    p.merged_at
                        .map(|m| (m - p.opened_at).num_seconds().max(0) as f64 / 3600.0)
                })
                .collect();
            hours.sort_by(f64::total_cmp);
            let changes_requested: u32 = list
                .iter()
                .filter(|p| p.merged_at.is_some())
                .map(|p| p.changes_requested)
                .sum();
            let heads: u32 = list.iter().map(|p| p.heads.max(1)).sum();
            Scorecard {
                prs,
                merged,
                closed,
                returned: count(&|p| p.returned),
                reverted: count(&|p| p.merged_at.is_some() && p.reverted),
                replayed: count(&|p| p.merged_at.is_some() && p.replayed),
                merge_rate: Scorecard::rate(merged, merged + closed),
                review_rounds: Scorecard::rate(changes_requested, merged),
                heads_per_pr: f64::from(heads) / f64::from(prs.max(1)),
                median_hours_to_merge: hours
                    .get(hours.len().saturating_sub(1) / 2)
                    .copied()
                    .filter(|_| !hours.is_empty()),
                replay_cost_usd: list.iter().map(|p| p.replay_cost_usd).sum(),
                key,
            }
        })
        .collect();
    cards.sort_by(|a, b| b.prs.cmp(&a.prs).then_with(|| a.key.cmp(&b.key)));
    cards
}

fn agent_label(key: &AgentKey) -> String {
    let name = match key.agent.as_str() {
        PEOPLE => "People".to_owned(),
        UNKNOWN => "Not recorded".to_owned(),
        a => a.to_owned(),
    };
    match &key.model {
        Some(m) => format!("{name} ({m})"),
        None => name,
    }
}

/// The scorecards as rows of cells: agent, PRs, merged, returned, reverted, replayed, review
/// rounds, median time to merge, replay cost per merged pull request.
fn rows(cards: &[Scorecard]) -> Vec<[String; 9]> {
    let pct = |r: Option<f64>| r.map_or_else(|| "—".to_owned(), |r| format!("{:.0}%", r * 100.0));
    let of = |n: u32, whole: u32| format!("{n} ({})", pct(Scorecard::rate(n, whole)));
    cards
        .iter()
        .map(|c| {
            [
                agent_label(&c.key),
                c.prs.to_string(),
                format!("{} ({})", c.merged, pct(c.merge_rate)),
                of(c.returned, c.prs),
                of(c.reverted, c.merged),
                of(c.replayed, c.merged),
                c.review_rounds
                    .map_or_else(|| "—".into(), |r| format!("{r:.1}")),
                c.median_hours_to_merge
                    .map_or_else(|| "—".into(), |h| format!("{h:.1} h")),
                c.cost_per_merged()
                    .map_or_else(|| "—".into(), |v| format!("${v:.2}")),
            ]
        })
        .collect()
}

const HEADERS: [&str; 9] = [
    "Agent",
    "PRs",
    "Merged",
    "Returned",
    "Reverted",
    "Replayed",
    "Review rounds",
    "Time to merge",
    "Replay $/merged",
];

/// The scorecards as an aligned table for a terminal.
pub fn to_text(cards: &[Scorecard], days: u32) -> String {
    let rows = rows(cards);
    let mut width = HEADERS.map(|h| h.chars().count());
    for r in &rows {
        for (w, cell) in width.iter_mut().zip(r) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let line = |cells: [&str; 9]| {
        let mut out = String::from(" ");
        for (i, (cell, w)) in cells.iter().zip(width).enumerate() {
            let pad = " ".repeat(w - cell.chars().count());
            if i == 0 {
                out.push_str(&format!(" {cell}{pad}"));
            } else {
                out.push_str(&format!("  {pad}{cell}"));
            }
        }
        out.trim_end().to_owned() + "\n"
    };
    let mut out = format!("Agent scorecards · pull requests opened in the last {days} days\n\n");
    out.push_str(&line(HEADERS));
    for r in &rows {
        out.push_str(&line(r.each_ref().map(String::as_str)));
    }
    if rows.is_empty() {
        out.push_str("  no pull requests\n");
    }
    out
}

/// The scorecards as a Markdown table.
pub fn to_markdown(cards: &[Scorecard], days: u32) -> String {
    let mut out = format!(
        "# Agent scorecards\n\nPull requests opened in the last {days} days.\n\n| {} |\n|{}\n",
        HEADERS.join(" | "),
        " --- |".repeat(HEADERS.len())
    );
    for r in rows(cards) {
        out.push_str(&format!("| {} |\n", r.join(" | ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn origin(author: &str, bot: bool, branch: &str, labels: &[&str]) -> PrOrigin {
        PrOrigin {
            author: Login::new(author),
            bot,
            head_ref: branch.into(),
            labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        }
    }

    fn rule(
        name: &str,
        author: Option<&str>,
        branch: Option<&str>,
        label: Option<&str>,
    ) -> AgentRule {
        AgentRule {
            name: name.into(),
            model: None,
            author: author.map(Into::into),
            branch: branch.map(Into::into),
            label: label.map(Into::into),
        }
    }

    #[test]
    fn rules_first_then_bots_then_people() {
        let rules = vec![
            AgentRule {
                model: Some("gpt-5-codex".into()),
                ..rule("codex", None, Some("codex/"), None)
            },
            rule("claude", None, None, Some("Agent: Claude")),
            rule("renovate", Some("renovate[bot]"), None, None),
        ];
        let key = |o: &PrOrigin| classify(&rules, Some(o));
        assert_eq!(
            key(&origin("kris", false, "codex/fix-login", &[])),
            AgentKey {
                agent: "codex".into(),
                model: Some("gpt-5-codex".into())
            }
        );
        assert_eq!(
            key(&origin("kris", false, "fix", &["agent: claude"])).agent,
            "claude"
        );
        assert_eq!(
            key(&origin("Renovate[bot]", true, "renovate/x", &[])).agent,
            "renovate"
        );
        assert_eq!(
            key(&origin("copilot-swe-agent[bot]", true, "copilot/x", &[])).agent,
            "copilot-swe-agent"
        );
        assert_eq!(key(&origin("kris", false, "fix", &[])).agent, PEOPLE);
        assert_eq!(classify(&rules, None).agent, UNKNOWN);
        // Every condition of a rule must match.
        let both = vec![rule("x", Some("a"), Some("b/"), None)];
        assert_eq!(
            classify(&both, Some(&origin("a", false, "c/", &[]))).agent,
            PEOPLE
        );
    }

    #[test]
    fn rule_validation() {
        assert!(validate_rules(&[rule("codex", None, Some("codex/"), None)]).is_ok());
        assert!(rule("codex", None, None, None).validate().is_err());
        assert!(rule("", None, Some("x"), None).validate().is_err());
        assert!(rule("a<b>", None, Some("x"), None).validate().is_err());
        assert!(rule("ok", None, Some(""), None).validate().is_err());
        assert!(
            rule("ok", None, Some(&"x".repeat(MAX_MATCH + 1)), None)
                .validate()
                .is_err()
        );
        let many = vec![rule("a", Some("b"), None, None); MAX_AGENT_RULES + 1];
        assert!(validate_rules(&many).is_err());
    }

    #[test]
    fn scorecards_aggregate_per_agent() {
        let t0 = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        let pr = |o: Option<PrOrigin>, merged_h: Option<i64>, closed: bool| PrOutcome {
            origin: o,
            opened_at: t0,
            merged_at: merged_h.map(|h| t0 + Duration::hours(h)),
            closed_unmerged: closed,
            heads: 1,
            returned: false,
            reverted: false,
            replayed: false,
            changes_requested: 0,
            replay_cost_usd: 0.0,
        };
        let bot = || Some(origin("claude[bot]", true, "claude/x", &[]));
        let prs = vec![
            PrOutcome {
                changes_requested: 2,
                heads: 3,
                replayed: true,
                replay_cost_usd: 1.5,
                ..pr(bot(), Some(2), false)
            },
            PrOutcome {
                reverted: true,
                ..pr(bot(), Some(4), false)
            },
            PrOutcome {
                returned: true,
                ..pr(bot(), None, true)
            },
            pr(bot(), None, false), // still open
            pr(Some(origin("kris", false, "fix", &[])), Some(10), false),
        ];
        let cards = scorecards(&[], &prs);
        assert_eq!(cards.len(), 2);
        let c = &cards[0];
        assert_eq!(c.key.agent, "claude");
        assert_eq!(
            (
                c.prs, c.merged, c.closed, c.returned, c.reverted, c.replayed
            ),
            (4, 2, 1, 1, 1, 1)
        );
        assert_eq!(c.merge_rate, Some(2.0 / 3.0));
        assert_eq!(c.review_rounds, Some(1.0));
        assert_eq!(c.heads_per_pr, 6.0 / 4.0);
        assert_eq!(c.median_hours_to_merge, Some(2.0));
        assert_eq!(c.cost_per_merged(), Some(0.75));
        assert_eq!(cards[1].key.agent, PEOPLE);
        assert_eq!(cards[1].merge_rate, Some(1.0));

        let text = to_text(&cards, 30);
        assert!(text.contains("claude"), "{text}");
        assert!(text.contains("2 (67%)") && text.contains("$0.75"), "{text}");
        let md = to_markdown(&cards, 30);
        assert!(md.contains("| People | 1 | 1 (100%) |"), "{md}");

        let none = scorecards(&[], &[pr(bot(), None, false)]);
        assert_eq!(
            (
                none[0].merge_rate,
                none[0].median_hours_to_merge,
                none[0].cost_per_merged()
            ),
            (None, None, None)
        );
    }
}
