//! Merge mode (M4): act on the scheduler's decisions by landing changes on GitHub.
//!
//! Landing is the commit protocol from `docs/design.md`: read the head H, check the change
//! against what landed since its base, merge it onto H on a work branch, and fast-forward the
//! default branch from H to that merge commit. The fast-forward never forces, so if anything
//! else moved the branch the attempt loses and is retried from the check.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use sheltie_commit::{CommitCheck, check, land_branch, merge_message, verify_branch};
use sheltie_core::{CommitSha, GuaranteeTier, RepoId, TargetId};
use sheltie_github::GitHubApp;
use sheltie_github::git::{FastForward, MergeResult};
use sheltie_scheduler::Decision;
use sheltie_store::{CiState, CommitterChange, RepoAccess, Store, VerifyRun};

/// Lost compare-and-swaps tolerated per change and epoch before trying again next epoch.
const MAX_CAS_RETRIES: usize = 3;
/// A verify branch on which no CI reports for this long is given up on.
const NO_CI_TIMEOUT_MINUTES: i64 = 30;

/// Lands changes in repositories in merge mode.
#[derive(Debug, Clone)]
pub struct Committer {
    store: Store,
    github: Arc<GitHubApp>,
}

/// What happened to one change.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Landed(CommitSha),
    Verifying,
    Returned(String),
    Waiting,
}

impl Committer {
    /// Creates the committer.
    pub fn new(store: Store, github: Arc<GitHubApp>) -> Self {
        Self { store, github }
    }

    /// Acts on one epoch's decisions for a repository in merge mode, then advances verify
    /// runs. `plan` is keyed by change database id.
    pub async fn act(
        &self,
        repo_id: RepoId,
        repo: &RepoAccess,
        plan: &BTreeMap<i64, Decision<i64>>,
    ) -> anyhow::Result<()> {
        // Land-now changes go first; so do changes the scheduler saw conflicting with what
        // landed: the commit check against the real head then lands, verifies or returns
        // them with the exact targets.
        let mut land_now = Vec::new();
        let mut verify = Vec::new();
        for (id, decision) in plan {
            match decision {
                Decision::LandNow { .. } => land_now.push(*id),
                Decision::Reverify { .. } | Decision::SpeculativeChain { position: 0, .. } => {
                    verify.push(*id)
                }
                Decision::GroupBatch { group } if group.first() == Some(id) => verify.push(*id),
                Decision::MergeEngine { landed: true, .. } => land_now.push(*id),
                // Until the merge engine (M5), of changes writing the same targets the one with
                // the smallest id goes first; the others then meet a write conflict.
                Decision::MergeEngine {
                    with,
                    landed: false,
                } if with.iter().all(|w| w > id) => verify.push(*id),
                _ => {}
            }
        }
        for id in land_now {
            self.handle(repo, id, None).await?;
        }
        for id in verify {
            self.start_verification(repo, id).await?;
        }
        self.advance(repo, repo_id).await
    }

    /// Advances verify runs of a repository that has no ready changes this epoch.
    pub async fn advance(&self, repo: &RepoAccess, repo_id: RepoId) -> anyhow::Result<()> {
        for id in self.store.verifying_changes(repo_id).await? {
            self.advance_verification(repo, id).await?;
        }
        Ok(())
    }

    /// Tries to land a change: footprint-proven when `verified` is `None`, otherwise
    /// speculation-verified by that run.
    async fn handle(
        &self,
        repo: &RepoAccess,
        id: i64,
        verified: Option<&VerifyRun>,
    ) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        let Some(fp) = c.footprint.clone() else {
            return Ok(Outcome::Waiting);
        };
        for _ in 0..MAX_CAS_RETRIES {
            let head = self
                .github
                .branch_head(repo.installation, &repo.full_name, &repo.default_branch)
                .await?;
            // A verified run that is still on the head lands as is.
            if let Some(run) = verified
                && run.onto == head
            {
                match self
                    .github
                    .fast_forward(
                        repo.installation,
                        &repo.full_name,
                        &repo.default_branch,
                        &run.verify_sha,
                    )
                    .await?
                {
                    FastForward::Done => {
                        return self
                            .landed(
                                repo,
                                &c,
                                &run.verify_sha,
                                GuaranteeTier::SpeculationVerified,
                                Some(&run.branch),
                            )
                            .await;
                    }
                    FastForward::Moved => continue,
                }
            }
            // Otherwise: what landed between the base (or the verified head) and now?
            let since = verified.map_or(&fp.base, |run| &run.onto);
            let landed = match self
                .github
                .first_parent_chain(repo.installation, &repo.full_name, since, &head)
                .await?
            {
                Some(chain) => {
                    self.store
                        .merge_commit_writes(c.change.repo, &chain)
                        .await?
                }
                None => None,
            };
            match check(&fp.footprint, landed.as_ref()) {
                CommitCheck::WriteConflict(targets) => {
                    let reason = format!(
                        "it writes {} which changed on `{}` since its base; rebase it",
                        list(&targets),
                        repo.default_branch
                    );
                    return self.return_change(repo, id, &reason).await;
                }
                CommitCheck::Reverify(_) | CommitCheck::Unknown => {
                    return self.verify_on(repo, &c, &head).await;
                }
                CommitCheck::Commute => {
                    let tier = if verified.is_some() {
                        GuaranteeTier::SpeculationVerified
                    } else {
                        GuaranteeTier::FootprintProven
                    };
                    let title = self
                        .github
                        .pr_title(repo.installation, &repo.full_name, c.change.pr)
                        .await?;
                    let message = merge_message(c.change.pr, &title, tier, since);
                    let branch = land_branch(c.change.pr, &c.change.head);
                    self.github
                        .reset_branch(repo.installation, &repo.full_name, &branch, &head)
                        .await?;
                    let merged = self
                        .github
                        .merge_into(
                            repo.installation,
                            &repo.full_name,
                            &branch,
                            &c.change.head,
                            &message,
                        )
                        .await?;
                    let merge_commit = match merged {
                        MergeResult::Merged(sha) => sha,
                        MergeResult::AlreadyMerged => {
                            self.github
                                .delete_branch(repo.installation, &repo.full_name, &branch)
                                .await?;
                            return Ok(Outcome::Waiting); // GitHub will report the merge
                        }
                        MergeResult::Conflict => {
                            self.github
                                .delete_branch(repo.installation, &repo.full_name, &branch)
                                .await?;
                            let reason = format!(
                                "it does not merge cleanly onto `{}`; rebase it",
                                repo.default_branch
                            );
                            return self.return_change(repo, id, &reason).await;
                        }
                    };
                    let ff = self
                        .github
                        .fast_forward(
                            repo.installation,
                            &repo.full_name,
                            &repo.default_branch,
                            &merge_commit,
                        )
                        .await?;
                    self.github
                        .delete_branch(repo.installation, &repo.full_name, &branch)
                        .await?;
                    match ff {
                        FastForward::Done => {
                            let run_branch = verified.map(|r| r.branch.as_str());
                            return self.landed(repo, &c, &merge_commit, tier, run_branch).await;
                        }
                        FastForward::Moved => {
                            tracing::info!(repo = %repo.full_name, change = %c.change, "lost the race for the head; retrying");
                        }
                    }
                }
            }
        }
        Ok(Outcome::Waiting)
    }

    async fn landed(
        &self,
        repo: &RepoAccess,
        c: &CommitterChange,
        merge_commit: &CommitSha,
        tier: GuaranteeTier,
        verify_branch: Option<&str>,
    ) -> anyhow::Result<Outcome> {
        self.store
            .mark_landed(c.change.repo, c.change_db_id, merge_commit, tier)
            .await?;
        if let Some(branch) = verify_branch {
            self.github
                .delete_branch(repo.installation, &repo.full_name, branch)
                .await?;
        }
        tracing::info!(repo = %repo.full_name, change = %c.change, commit = %merge_commit, %tier, "landed");
        Ok(Outcome::Landed(merge_commit.clone()))
    }

    async fn start_verification(&self, repo: &RepoAccess, id: i64) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        if self.store.verify_run(id).await?.is_some() {
            return Ok(Outcome::Verifying);
        }
        let head = self
            .github
            .branch_head(repo.installation, &repo.full_name, &repo.default_branch)
            .await?;
        self.verify_on(repo, &c, &head).await
    }

    /// Merges the change onto `head` on its verify branch, where the repository's CI runs.
    async fn verify_on(
        &self,
        repo: &RepoAccess,
        c: &CommitterChange,
        head: &CommitSha,
    ) -> anyhow::Result<Outcome> {
        let branch = verify_branch(c.change.pr, &c.change.head);
        let title = self
            .github
            .pr_title(repo.installation, &repo.full_name, c.change.pr)
            .await?;
        let message = merge_message(
            c.change.pr,
            &title,
            GuaranteeTier::SpeculationVerified,
            head,
        );
        self.github
            .reset_branch(repo.installation, &repo.full_name, &branch, head)
            .await?;
        match self
            .github
            .merge_into(
                repo.installation,
                &repo.full_name,
                &branch,
                &c.change.head,
                &message,
            )
            .await?
        {
            MergeResult::Merged(verify_sha) => {
                let run = VerifyRun {
                    branch,
                    verify_sha,
                    onto: head.clone(),
                };
                self.store.start_verify_run(c.change_db_id, &run).await?;
                tracing::info!(repo = %repo.full_name, change = %c.change, branch = %run.branch, "verifying");
                Ok(Outcome::Verifying)
            }
            MergeResult::AlreadyMerged => {
                self.github
                    .delete_branch(repo.installation, &repo.full_name, &branch)
                    .await?;
                Ok(Outcome::Waiting)
            }
            MergeResult::Conflict => {
                self.github
                    .delete_branch(repo.installation, &repo.full_name, &branch)
                    .await?;
                let reason = format!(
                    "it does not merge cleanly onto `{}`; rebase it",
                    repo.default_branch
                );
                self.return_change(repo, c.change_db_id, &reason).await
            }
        }
    }

    /// Checks a verify run's CI and lands, returns or restarts the change.
    async fn advance_verification(&self, repo: &RepoAccess, id: i64) -> anyhow::Result<Outcome> {
        let Some(run) = self.store.verify_run(id).await? else {
            return Ok(Outcome::Waiting);
        };
        let data = self
            .github
            .commit_ci(repo.installation, &repo.full_name, &run.verify_sha)
            .await?;
        match sheltie_github::ci::evaluate(&data) {
            CiState::Pending => Ok(Outcome::Verifying),
            CiState::None => {
                let started = self.store.verify_started_at(id).await?;
                if started.is_some_and(|t| {
                    chrono::Utc::now() - t > chrono::Duration::minutes(NO_CI_TIMEOUT_MINUTES)
                }) {
                    let reason = format!(
                        "no CI ran on `{}`; configure CI to run on pushes to `sheltie/verify/**`",
                        run.branch
                    );
                    self.github
                        .delete_branch(repo.installation, &repo.full_name, &run.branch)
                        .await?;
                    return self.return_change(repo, id, &reason).await;
                }
                Ok(Outcome::Verifying)
            }
            CiState::Failure | CiState::Error => {
                let reason = format!(
                    "CI failed with the change merged onto `{}` at {} (branch `{}`)",
                    repo.default_branch,
                    &run.onto.as_str()[..12],
                    run.branch
                );
                self.github
                    .delete_branch(repo.installation, &repo.full_name, &run.branch)
                    .await?;
                self.return_change(repo, id, &reason).await
            }
            CiState::Success => match self.handle(repo, id, Some(&run)).await? {
                Outcome::Waiting => {
                    // Lost too many races or the base moved under it: start over next epoch.
                    self.store.abandon_verify_run(id).await?;
                    Ok(Outcome::Waiting)
                }
                other => Ok(other),
            },
        }
    }

    async fn return_change(
        &self,
        repo: &RepoAccess,
        id: i64,
        reason: &str,
    ) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        self.store.mark_returned(id, reason).await?;
        let body = format!(
            "Sheltie could not land this pull request: {reason}.\n\nPush a new commit and Sheltie will pick it up again."
        );
        self.github
            .comment(repo.installation, &repo.full_name, c.change.pr, &body)
            .await
            .context("commenting on the pull request")?;
        tracing::info!(repo = %repo.full_name, change = %c.change, reason, "returned");
        Ok(Outcome::Returned(reason.to_owned()))
    }
}

fn list(targets: &std::collections::BTreeSet<TargetId>) -> String {
    targets
        .iter()
        .take(5)
        .map(|t| format!("`{t}`"))
        .collect::<Vec<_>>()
        .join(", ")
}
