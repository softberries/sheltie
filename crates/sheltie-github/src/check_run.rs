//! What the "Sheltie" check run says for each change state.

use sheltie_core::{ChangeState, CommitSha, GuaranteeTier, ResolutionTier};
use sheltie_store::{FootprintSummary, Landing, ReadinessFlags};

/// Name of the check run Sheltie posts on every pull request head.
pub const CHECK_NAME: &str = "Sheltie";

/// Check run status as GitHub models it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    /// Waiting to start.
    Queued,
    /// Running.
    InProgress,
    /// Finished with a conclusion.
    Completed(CheckConclusion),
}

/// Conclusion of a completed check run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckConclusion {
    /// Landed.
    Success,
    /// Not landed, but not a failure of the PR: closed or superseded.
    Neutral,
    /// Sheltie could not land it; the author or an agent must act.
    Failure,
}

/// The content of a check run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRunSpec {
    /// Status and, when completed, conclusion.
    pub status: CheckStatus,
    /// One-line title shown next to the check.
    pub title: String,
    /// Markdown summary on the check's page.
    pub summary: String,
}

/// Where the user documentation lives.
pub const DOCS_URL: &str = "https://sheltie.dev/docs";

/// What a check run is rendered from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckContext<'a> {
    /// The change's state.
    pub state: ChangeState,
    /// Its head commit.
    pub head: &'a CommitSha,
    /// Its footprint, if uploaded.
    pub footprint: Option<&'a FootprintSummary>,
    /// The last readiness check, if any.
    pub readiness: Option<ReadinessFlags>,
    /// The shadow-mode prediction, for ready changes.
    pub prediction: Option<&'a str>,
    /// Why the change was returned, if it was.
    pub returned_reason: Option<&'a str>,
    /// Whether Sheltie lands changes in this repository (otherwise it only predicts).
    pub merge_mode: bool,
    /// The repository's default branch, when known.
    pub default_branch: Option<&'a str>,
    /// How it landed, once landed.
    pub landing: Option<&'a Landing>,
    /// The tier that resolved its latest conflict, if any.
    pub resolution: Option<ResolutionTier>,
    /// The replay tier in progress, if an agent is re-deriving it.
    pub replaying: Option<ResolutionTier>,
}

fn short(sha: &CommitSha) -> &str {
    sha.as_str().get(..7).unwrap_or(sha.as_str())
}

/// How a conflict was resolved, in words.
pub fn tier_words(tier: ResolutionTier) -> &'static str {
    match tier {
        ResolutionTier::Commute => "no conflict to resolve",
        ResolutionTier::Structural => "structural merge (mergiraf)",
        ResolutionTier::Regenerate => "regenerated derived files",
        ResolutionTier::Declared => "declared merge rules",
        ResolutionTier::ReplayHunks => "re-derived conflicting hunks from its intent (4a)",
        ResolutionTier::ReplayFull => "re-derived from its intent (4b)",
        ResolutionTier::Human => "resolved by a person",
    }
}

fn guarantee_words(tier: GuaranteeTier) -> &'static str {
    match tier {
        GuaranteeTier::FootprintProven => {
            "footprint-proven: verified on a base, and nothing that landed since touches what it reads"
        }
        GuaranteeTier::SpeculationVerified => {
            "verified in a speculative build of the exact combined state"
        }
        GuaranteeTier::SweepVerified => "covered by a full build of the branch",
    }
}

/// Renders the check run for a change.
pub fn spec_for(ctx: &CheckContext<'_>) -> CheckRunSpec {
    let branch = ctx.default_branch.unwrap_or("the default branch");
    let mut body = String::new();
    let (status, title, heading) = match ctx.state {
        ChangeState::Pending => {
            let (title, text) = pending(ctx);
            body.push_str(&text);
            (CheckStatus::Queued, title, "⏳ Waiting to be ready")
        }
        ChangeState::Ready if ctx.merge_mode => {
            let plan = ctx.prediction.map(|p| p.replacen("would ", "will ", 1));
            body.push_str(&format!(
                "Sheltie lands this pull request itself; there is no need to merge it by hand.\n\n**Next:** {}.",
                plan.as_deref().unwrap_or("planned in the next epoch")
            ));
            let title = plan.map_or_else(|| "Queued".into(), |p| format!("Queued: {p}"));
            (CheckStatus::Queued, title, "🚦 Queued to land")
        }
        ChangeState::Ready => {
            let plan = ctx.prediction.unwrap_or("not planned yet");
            body.push_str(&format!(
                "This repository is in **shadow mode**: Sheltie records what it would do and merges nothing. Merge as usual.\n\n**Sheltie {plan}.**"
            ));
            (
                CheckStatus::Queued,
                format!("Shadow: {plan}"),
                "👀 Shadow mode",
            )
        }
        ChangeState::Merging => match ctx.replaying {
            Some(tier) => {
                body.push_str(&format!(
                    "This change conflicts with what landed, and the merge engine could not combine them. An agent is now re-deriving it from the intent in the description, on your CI runner ({}).\n\nThe result lands only if it passes the acceptance tests, stays inside this change's files, and is close to the reviewed diff.",
                    if tier == ResolutionTier::ReplayHunks {
                        "resolving the conflicting hunks, tier 4a"
                    } else {
                        "re-implementing the change on the new head, tier 4b"
                    }
                ));
                (
                    CheckStatus::InProgress,
                    "Re-deriving from its intent".into(),
                    "🤖 Re-deriving from intent",
                )
            }
            None => {
                body.push_str(&format!(
                    "This change conflicts with what landed on `{branch}`. The merge engine is combining them on your CI runner (structural merge, regenerated files, declared rules); the result is verified before it lands."
                ));
                (
                    CheckStatus::InProgress,
                    "Resolving a conflict".into(),
                    "🔀 Resolving a conflict",
                )
            }
        },
        ChangeState::Verifying => {
            body.push_str(&format!(
                "This change is merged onto the current `{branch}` on a `sheltie/` branch, where your CI runs. It lands when CI passes there."
            ));
            if let Some(tier) = ctx.resolution {
                body.push_str(&format!("\n\nConflict resolved: {}.", tier_words(tier)));
            }
            (
                CheckStatus::InProgress,
                format!("Verifying on {branch}"),
                "🔍 Verifying",
            )
        }
        ChangeState::Committable => {
            body.push_str(&format!("Verified. Sheltie is moving `{branch}` to it."));
            (CheckStatus::InProgress, "Landing".into(), "🚀 Landing")
        }
        ChangeState::Landed => {
            let title = match ctx.landing {
                Some(l) => {
                    body.push_str(&format!(
                        "On `{branch}` as `{}`, {}.",
                        short(&l.commit),
                        guarantee_words(l.tier)
                    ));
                    format!("Landed as {}", short(&l.commit))
                }
                None => {
                    body.push_str(&format!("This change is on `{branch}`."));
                    "Landed".into()
                }
            };
            if let Some(tier) = ctx.resolution {
                body.push_str(&format!("\n\nConflict resolved: {}.", tier_words(tier)));
            }
            (
                CheckStatus::Completed(CheckConclusion::Success),
                title,
                "✅ Landed",
            )
        }
        ChangeState::Returned => {
            let reason = ctx
                .returned_reason
                .map(|r| r.trim().trim_end_matches('.').to_owned())
                .unwrap_or_else(|| "it could not be landed automatically".into());
            body.push_str(&format!(
                "Sheltie could not land this change: {reason}.\n\n**What to do:** fix it and push a new commit; Sheltie picks it up again."
            ));
            (
                CheckStatus::Completed(CheckConclusion::Failure),
                "Needs attention".into(),
                "⚠️ Needs attention",
            )
        }
        ChangeState::Closed => {
            body.push_str("The pull request was closed, or a newer commit replaced this one.");
            (
                CheckStatus::Completed(CheckConclusion::Neutral),
                "No longer tracked".into(),
                "⏹️ No longer tracked",
            )
        }
    };
    // A closed change's footprint no longer matters; a pending one lists it in its checklist.
    let footprint = match (ctx.state, ctx.footprint) {
        (ChangeState::Closed, _) | (ChangeState::Pending, None) => String::new(),
        (_, f) => format!("{}\n\n", footprint_section(f)),
    };
    let mode = if ctx.merge_mode {
        "Merge mode"
    } else {
        "Shadow mode"
    };
    CheckRunSpec {
        status,
        title,
        summary: format!(
            "## {heading}\n\n{body}\n\n{footprint}<sub>{mode} · commit `{}` · [How Sheltie decides]({DOCS_URL}/introduction.html)</sub>",
            short(ctx.head)
        ),
    }
}

/// The checklist of what a pending change still lacks.
fn pending(ctx: &CheckContext<'_>) -> (String, String) {
    let Some(r) = ctx.readiness else {
        return (
            "Checking approval and CI".into(),
            format!(
                "Sheltie is reading the pull request's reviews and checks on GitHub. It also needs the footprint from your CI ([setup]({DOCS_URL}/workflows.html))."
            ),
        );
    };
    let items = [
        (!r.draft, "Ready for review (not a draft)", "out of draft"),
        (r.approved, "Approved", "approval"),
        (r.checks_green, "Other checks passed", "checks"),
        (
            ctx.footprint.is_some(),
            "Footprint uploaded by CI",
            "footprint",
        ),
    ];
    let missing: Vec<&str> = items
        .iter()
        .filter(|(ok, _, _)| !ok)
        .map(|(_, _, short)| *short)
        .collect();
    let mut text = String::from("Sheltie takes this pull request once all of these hold:\n\n");
    for (ok, label, _) in items {
        text.push_str(&format!("- [{}] {label}\n", if ok { "x" } else { " " }));
    }
    let title = if missing.is_empty() {
        text.push_str("\nAll set; it must also target the default branch.");
        "Waiting for the default branch".into()
    } else {
        format!("Waiting for {}", missing.join(", "))
    };
    (title, text.trim_end().to_owned())
}

fn names(list: &[String], total: usize) -> String {
    if list.is_empty() {
        return "none".into();
    }
    let mut out = list
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(" ");
    if total > list.len() {
        out.push_str(&format!(" and {} more", total - list.len()));
    }
    out
}

fn plural(n: usize, one: &str) -> String {
    format!("{n} {one}{}", if n == 1 { "" } else { "s" })
}

fn footprint_section(f: Option<&FootprintSummary>) -> String {
    let Some(f) = f else {
        return format!(
            "**Footprint:** not uploaded yet. It comes from the footprint workflow in your CI ([setup]({DOCS_URL}/workflows.html))."
        );
    };
    let mut s = format!(
        "<details>\n<summary><b>Footprint</b> · writes {} · affects {} · {}{}</summary>\n\n",
        plural(f.writes, "target"),
        f.reads,
        plural(f.tests, "test group"),
        if f.leaky { " · <b>leaky</b>" } else { "" }
    );
    s.push_str(&format!(
        "**Writes** {}\n\n",
        names(&f.write_names, f.writes)
    ));
    s.push_str(&format!(
        "**Affects** {}\n\n",
        names(&f.read_names, f.reads)
    ));
    s.push_str(&format!("**Tests** {}\n", names(&f.test_names, f.tests)));
    if f.leaky {
        s.push_str("\n**Leaky:** a non-hermetic part of the build widened what it may affect:\n");
        for reason in &f.leak_reasons {
            s.push_str(&format!("- {reason}\n"));
        }
    }
    s.push_str(
        "\nTwo changes conflict only when one writes what the other writes or affects.\n</details>",
    );
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head() -> CommitSha {
        CommitSha::parse("0123456789abcdef0123456789abcdef01234567").unwrap()
    }

    fn footprint(leaky: bool) -> FootprintSummary {
        FootprintSummary {
            writes: 2,
            reads: 3,
            tests: 1,
            leaky,
            write_names: vec!["shop::cart".into(), "shop::pricing".into()],
            read_names: vec![
                "shop::cart".into(),
                "shop::checkout".into(),
                "shop::pricing".into(),
            ],
            test_names: vec!["shop".into()],
            leak_reasons: if leaky {
                vec!["build.rs in shop reads the environment".into()]
            } else {
                vec![]
            },
        }
    }

    fn render(ctx: &CheckContext<'_>) -> String {
        let spec = spec_for(ctx);
        format!(
            "status: {:?}\ntitle: {}\n---\n{}",
            spec.status, spec.title, spec.summary
        )
    }

    #[test]
    fn every_state_renders() {
        let h = head();
        let fp = footprint(false);
        let leaky = footprint(true);
        let landing = Landing {
            commit: CommitSha::parse("fedcba9876543210fedcba9876543210fedcba98").unwrap(),
            tier: GuaranteeTier::FootprintProven,
        };
        let base = CheckContext {
            state: ChangeState::Pending,
            head: &h,
            footprint: None,
            readiness: None,
            prediction: None,
            returned_reason: None,
            merge_mode: false,
            default_branch: Some("main"),
            landing: None,
            resolution: None,
            replaying: None,
        };
        let readiness = ReadinessFlags {
            approved: false,
            checks_green: true,
            draft: false,
        };
        let cases: Vec<(&str, CheckContext<'_>)> = vec![
            ("pending_unchecked", base),
            (
                "pending_missing",
                CheckContext {
                    readiness: Some(readiness),
                    ..base
                },
            ),
            (
                "ready_shadow",
                CheckContext {
                    state: ChangeState::Ready,
                    footprint: Some(&fp),
                    prediction: Some("would land now, together with #12"),
                    ..base
                },
            ),
            (
                "ready_merge",
                CheckContext {
                    state: ChangeState::Ready,
                    footprint: Some(&fp),
                    prediction: Some("would land now"),
                    merge_mode: true,
                    ..base
                },
            ),
            (
                "merging",
                CheckContext {
                    state: ChangeState::Merging,
                    footprint: Some(&fp),
                    merge_mode: true,
                    ..base
                },
            ),
            (
                "replaying",
                CheckContext {
                    state: ChangeState::Merging,
                    footprint: Some(&fp),
                    merge_mode: true,
                    replaying: Some(ResolutionTier::ReplayFull),
                    ..base
                },
            ),
            (
                "verifying",
                CheckContext {
                    state: ChangeState::Verifying,
                    footprint: Some(&leaky),
                    merge_mode: true,
                    resolution: Some(ResolutionTier::Regenerate),
                    ..base
                },
            ),
            (
                "landed",
                CheckContext {
                    state: ChangeState::Landed,
                    footprint: Some(&fp),
                    merge_mode: true,
                    landing: Some(&landing),
                    ..base
                },
            ),
            (
                "returned",
                CheckContext {
                    state: ChangeState::Returned,
                    footprint: Some(&fp),
                    merge_mode: true,
                    returned_reason: Some(
                        "CI failed with the change merged onto `main` at abc123456789.",
                    ),
                    ..base
                },
            ),
            (
                "closed",
                CheckContext {
                    state: ChangeState::Closed,
                    ..base
                },
            ),
        ];
        for (name, ctx) in cases {
            insta::assert_snapshot!(name, render(&ctx));
        }
    }

    #[test]
    fn only_finished_states_are_completed() {
        let h = head();
        for state in ChangeState::ALL {
            let ctx = CheckContext {
                state: *state,
                head: &h,
                footprint: None,
                readiness: None,
                prediction: None,
                returned_reason: None,
                merge_mode: false,
                default_branch: None,
                landing: None,
                resolution: None,
                replaying: None,
            };
            let completed = matches!(spec_for(&ctx).status, CheckStatus::Completed(_));
            let finished = matches!(
                state,
                ChangeState::Landed | ChangeState::Closed | ChangeState::Returned
            );
            assert_eq!(completed, finished, "{state:?}");
        }
    }

    #[test]
    fn long_footprints_are_truncated() {
        let mut f = footprint(false);
        f.reads = 45;
        assert!(footprint_section(Some(&f)).contains("and 42 more"));
    }
}
