//! Every comment Sheltie posts on pull requests and issues, in one voice: a headline with an
//! icon, what happened, what to do next when the reader has to act, and a short footer.

use crate::check_run::DOCS_URL;

fn comment(icon: &str, headline: &str, body: &str, next: Option<&str>, docs: &str) -> String {
    let mut s = format!("### {icon} {headline}\n\n{body}\n");
    if let Some(next) = next {
        s.push_str(&format!("\n**What to do:** {next}\n"));
    }
    s.push_str(&format!(
        "\n<sub>Sheltie · [{}]({DOCS_URL}/{docs})</sub>",
        match docs {
            "intent-replay.html" => "About replay",
            "agents.html" => "About coordinating agents",
            _ => "How Sheltie works",
        }
    ));
    s
}

fn trim(reason: &str) -> &str {
    reason.trim().trim_end_matches('.')
}

/// A pull request Sheltie gives back to its author.
pub fn returned(reason: &str) -> String {
    comment(
        "⚠️",
        "Sheltie could not land this pull request",
        &format!("{}.", capitalised(trim(reason))),
        Some("fix it and push a new commit; Sheltie picks it up again."),
        "introduction.html",
    )
}

/// A replay whose result differs too much from the reviewed change.
pub fn needs_reapproval(branch: &str, similarity: f32, threshold: f32) -> String {
    comment(
        "🤖",
        "Re-derived, but it needs your review",
        &format!(
            "Sheltie re-derived this change with an agent on top of the current head. The result is on branch `{branch}`, but it differs from the change that was reviewed (similarity {similarity:.2}, threshold {threshold:.2}), so it was not landed."
        ),
        Some(&format!(
            "review `{branch}` and, if it is right, push it to this pull request. Sheltie lands it once it is approved again."
        )),
        "intent-replay.html",
    )
}

/// A full rebuild (4b) that no acceptance test checked.
pub fn untested_rebuild(branch: &str) -> String {
    comment(
        "🤖",
        "Rebuilt, but nothing shows it still works",
        &format!(
            "Sheltie re-implemented this change with an agent on top of the current head. The result is on branch `{branch}`, but this pull request adds no test that fails without it, so nothing shows that the rebuilt change still does what it did. It was not landed."
        ),
        Some(&format!(
            "review `{branch}` and, if it is right, push it to this pull request. Adding a test that fails without the change lets Sheltie check future rebuilds itself."
        )),
        "intent-replay.html",
    )
}

/// The blamed pull request was reverted.
pub fn reverted(branch: &str, revert_pr: u64) -> String {
    comment(
        "↩️",
        "Reverted",
        &format!(
            "`{branch}` turned red after this pull request landed, and bisect blamed it. Sheltie reverted it in #{revert_pr} to get `{branch}` green again."
        ),
        Some("fix the problem and open a new pull request with the change."),
        "introduction.html",
    )
}

/// The blamed pull request could not be reverted automatically.
pub fn revert_conflict(branch: &str) -> String {
    comment(
        "🚨",
        "This pull request broke the build",
        &format!(
            "`{branch}` turned red after this pull request landed, and bisect blamed it. Reverting it automatically conflicts with changes that landed later."
        ),
        Some(&format!(
            "fix or revert it by hand to get `{branch}` green again."
        )),
        "introduction.html",
    )
}

/// Body of an automatic revert pull request.
pub fn revert_pr_body(what: &str, branch: &str, red: &str, culprit: &str) -> String {
    comment(
        "↩️",
        &format!("Automatic revert of {what}"),
        &format!(
            "`{branch}` turned red at `{red}` and stayed red. Sheltie bisected the commits since the last green one and blamed `{culprit}`.\n\nThis pull request undoes it. Sheltie lands it itself; there is nothing to do."
        ),
        None,
        "introduction.html",
    )
}

/// The predicted footprint of a new issue.
pub fn issue_footprint(writes: &str, affects: &str, leaky: bool) -> String {
    comment(
        "🧭",
        "Predicted footprint",
        &format!(
            "Working on this issue will most likely change {writes}, and affect {affects}.{}\n\nAgents assigned to issues that overlap it are serialized, so they do not step on each other.",
            if leaky {
                " A non-hermetic part of the build widens this."
            } else {
                ""
            }
        ),
        None,
        "agents.html",
    )
}

/// An assignee has to wait for an overlapping issue.
pub fn issue_blocked(holder: &str, blocker: u64, blocker_holder: &str, overlap: &str) -> String {
    comment(
        "⏸️",
        &format!("On hold for `{holder}`"),
        &format!(
            "This issue overlaps #{blocker}, which `{blocker_holder}` is working on (both touch {overlap}). To avoid conflicting changes, Sheltie holds it until that work lands or its claim ends, then claims it for `{holder}` automatically."
        ),
        None,
        "agents.html",
    )
}

/// A held issue was claimed for its assignee.
pub fn issue_unblocked(holder: &str) -> String {
    comment(
        "▶️",
        "Ready to start",
        &format!("The overlapping work is done. This issue is now claimed for `{holder}`."),
        None,
        "agents.html",
    )
}

/// An assignee was too late: someone else holds the issue.
pub fn issue_taken(other: &str) -> String {
    comment(
        "🔒",
        "Already claimed",
        &format!("This issue is already claimed by `{other}`."),
        Some("pick another issue, or ask them to release it."),
        "agents.html",
    )
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments() {
        let all = [
            (
                "returned",
                returned(
                    "CI failed with the change merged onto `main` at `abc1234` (branch `sheltie/verify/7-abc`).",
                ),
            ),
            (
                "needs_reapproval",
                needs_reapproval("sheltie/replay/7-0123456789ab", 0.31, 0.6),
            ),
            (
                "untested_rebuild",
                untested_rebuild("sheltie/replay/7-0123456789ab"),
            ),
            ("reverted", reverted("main", 42)),
            ("revert_conflict", revert_conflict("main")),
            (
                "revert_pr_body",
                revert_pr_body("#7", "main", "abc1234", "#7 (`abc1234`)"),
            ),
            (
                "issue_footprint",
                issue_footprint("`shop::pricing`", "`shop::cart`, `shop::pricing`", false),
            ),
            (
                "issue_blocked",
                issue_blocked("agent-b", 1, "agent-a", "`shop::pricing`"),
            ),
            ("issue_unblocked", issue_unblocked("agent-b")),
            ("issue_taken", issue_taken("agent-a")),
        ];
        for (name, text) in all {
            insta::assert_snapshot!(name, text);
        }
    }

    #[test]
    fn reasons_read_as_sentences() {
        assert!(
            returned("it does not merge cleanly onto `main`; rebase it")
                .contains("It does not merge cleanly onto `main`; rebase it.\n")
        );
    }
}
