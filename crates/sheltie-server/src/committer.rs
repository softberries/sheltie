//! Merge mode (M4): act on the scheduler's decisions by landing changes on GitHub.
//!
//! Landing follows the commit protocol: read the head H, check the change
//! against what landed since its base, merge it onto H on a work branch, and fast-forward the
//! default branch from H to that merge commit. The fast-forward never forces, so if anything
//! else moved the branch the attempt loses and is retried from the check.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Context as _;
use sheltie_commit::speculation::{Build, Verdict, decide, prefix_lengths};
use sheltie_commit::{CommitCheck, check, land_branch, merge_message, spec_branch, verify_branch};
use sheltie_core::{CommitSha, GuaranteeTier, RepoId, ResolutionTier, Score, TargetId};
use sheltie_github::GitHubApp;
use sheltie_github::git::{FastForward, MergeResult};
use sheltie_merge::rules::{ReplayConfig, Rules};
use sheltie_scheduler::Decision;
use sheltie_store::{
    CiState, CommitterChange, MergeOutcome, ReplayOutcome, RepoAccess, SpecBuild, SpecGroup, Store,
    VerifyRun,
};

/// The `repository_dispatch` event type that starts the merge-engine workflow.
pub const MERGE_EVENT: &str = "sheltie-merge";

/// The `repository_dispatch` event type that starts the intent-replay workflow (tier 4).
pub const REPLAY_EVENT: &str = "sheltie-replay";
/// Label on a pull request that landed through a replay.
pub const REPLAYED_LABEL: &str = sheltie_github::labels::REPLAYED.name;
/// Label on a pull request whose replay needs a human to approve it.
pub const REAPPROVAL_LABEL: &str = sheltie_github::labels::NEEDS_REAPPROVAL.name;

/// A replay that has not reported for this long is given up on (agent runs are slow).
const REPLAY_TIMEOUT_MINUTES: i64 = 150;
/// Most characters of a linked issue's body sent to the runner in the dispatch payload.
const MAX_ISSUE_BODY_IN_PAYLOAD: usize = 8_000;
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
        let mut groups: Vec<Vec<i64>> = Vec::new();
        for (id, decision) in plan {
            match decision {
                Decision::LandNow { .. } => land_now.push(*id),
                Decision::Reverify { .. } => verify.push(*id),
                // Chains and batches: built as prefixes (once per group, from its first member).
                Decision::SpeculativeChain {
                    chain: group,
                    position: 0,
                }
                | Decision::GroupBatch { group }
                    if group.first() == Some(id) =>
                {
                    groups.push(group.clone());
                }
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
        for group in groups {
            self.start_speculation(repo_id, repo, &group).await?;
        }
        self.advance(repo, repo_id).await
    }

    /// Advances verify runs of a repository that has no ready changes this epoch.
    pub async fn advance(&self, repo: &RepoAccess, repo_id: RepoId) -> anyhow::Result<()> {
        for id in self.store.verifying_changes(repo_id).await? {
            self.advance_verification(repo, id).await?;
        }
        for group in self.store.spec_groups(repo_id).await? {
            self.advance_speculation(repo_id, repo, &group).await?;
        }
        self.advance_merges(repo_id, repo).await?;
        self.advance_replays(repo_id, repo).await
    }

    /// Builds a group's prefixes on the current head: merges the members one by one on a work
    /// branch, then points one `sheltie/spec/<group>-<length>` branch per prefix length at the
    /// matching merge commit, so the repository's CI runs on each.
    async fn start_speculation(
        &self,
        repo_id: RepoId,
        repo: &RepoAccess,
        members: &[i64],
    ) -> anyhow::Result<()> {
        let (inst, name) = (repo.installation, &repo.full_name);
        let head = self
            .github
            .branch_head(inst, name, &repo.default_branch)
            .await?;
        let group = self
            .store
            .create_spec_group(repo_id, members, &head)
            .await?;
        let work = format!("sheltie/spec/{group}-work");
        self.github.reset_branch(inst, name, &work, &head).await?;
        let mut member_shas = Vec::with_capacity(members.len());
        for (i, id) in members.iter().enumerate() {
            let Some(c) = self.store.committer_change(*id).await? else {
                break;
            };
            let title = self.github.pr_title(inst, name, c.change.pr).await?;
            let message = merge_message(
                c.change.pr,
                &title,
                GuaranteeTier::SpeculationVerified,
                &head,
            );
            match self
                .github
                .merge_into(inst, name, &work, &c.change.head, &message)
                .await?
            {
                MergeResult::Merged(sha) => member_shas.push(sha),
                MergeResult::AlreadyMerged | MergeResult::Conflict => {
                    // Build what merged so far; the rest is planned again next epoch.
                    if i == 0 {
                        let reason = format!(
                            "it does not merge cleanly onto `{}`; rebase it",
                            repo.default_branch
                        );
                        self.conflicted(repo, *id, &head, &reason).await?;
                    }
                    break;
                }
            }
        }
        self.github.delete_branch(inst, name, &work).await?;
        let builds: Vec<SpecBuild> = prefix_lengths(member_shas.len())
            .into_iter()
            .map(|length| SpecBuild {
                length,
                branch: spec_branch(group, length),
                sha: member_shas[length - 1].clone(),
            })
            .collect();
        for b in &builds {
            self.github
                .reset_branch(inst, name, &b.branch, &b.sha)
                .await?;
        }
        self.store
            .record_spec_builds(group, &member_shas, &builds)
            .await?;
        if builds.is_empty() {
            self.store.end_spec_group(group).await?;
        }
        tracing::info!(repo = %name, group, members = members.len(), builds = builds.len(), "speculation started");
        Ok(())
    }

    /// Reads the CI of a group's prefix builds and lands the longest green prefix.
    async fn advance_speculation(
        &self,
        repo_id: RepoId,
        repo: &RepoAccess,
        group: &SpecGroup,
    ) -> anyhow::Result<()> {
        let (inst, name) = (repo.installation, &repo.full_name);
        let mut results = Vec::with_capacity(group.builds.len());
        for b in &group.builds {
            results.push(
                match sheltie_github::ci::evaluate(
                    &self.github.commit_ci(inst, name, &b.sha).await?,
                ) {
                    CiState::Success => Build::Green,
                    CiState::Failure | CiState::Error => Build::Red,
                    CiState::Pending | CiState::None => Build::Pending,
                },
            );
        }
        let lengths: Vec<usize> = group.builds.iter().map(|b| b.length).collect();
        let Verdict::Settle { land, culprit } = decide(&lengths, &results) else {
            return Ok(());
        };
        if land > 0 {
            match self
                .github
                .fast_forward(
                    inst,
                    name,
                    &repo.default_branch,
                    &group.member_shas[land - 1],
                )
                .await?
            {
                FastForward::Done => {
                    for (id, sha) in group.members.iter().zip(&group.member_shas).take(land) {
                        self.store
                            .mark_landed(repo_id, *id, sha, GuaranteeTier::SpeculationVerified)
                            .await?;
                    }
                    tracing::info!(repo = %name, group = group.id, landed = land, "speculation landed a prefix");
                }
                FastForward::Moved => {
                    tracing::info!(repo = %name, group = group.id, "head moved; rebuilding the group next epoch");
                }
            }
        }
        if let Some(i) = culprit
            && let Some(id) = group.members.get(i)
        {
            let mut before = Vec::new();
            for m in &group.members[..i] {
                if let Some(c) = self.store.committer_change(*m).await? {
                    before.push(format!("#{}", c.change.pr.0));
                }
            }
            let with = if before.is_empty() {
                String::new()
            } else {
                format!(" together with {}", before.join(", "))
            };
            let reason = format!(
                "CI failed when it was merged onto `{}`{with} (branch `{}`)",
                repo.default_branch,
                spec_branch(group.id, i + 1)
            );
            self.return_change(repo, *id, &reason).await?;
        }
        for b in &group.builds {
            self.github.delete_branch(inst, name, &b.branch).await?;
        }
        self.store.end_spec_group(group.id).await?;
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
                    return self.conflicted(repo, id, &head, &reason).await;
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
                    // A verified run lands what it verified: for a merge-engine or replay result
                    // that is the resolution, not the pull request's head. Its first parent is
                    // the run's `onto`, an ancestor of `head`, so merging it brings the change.
                    let tip = verified.map_or(&c.change.head, |run| &run.verify_sha);
                    let merged = self
                        .github
                        .merge_into(repo.installation, &repo.full_name, &branch, tip, &message)
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
                            return self.conflicted(repo, id, &head, &reason).await;
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
                self.conflicted(repo, c.change_db_id, head, &reason).await
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
                    "CI failed with the change merged onto `{}` at `{}` (branch `{}`)",
                    repo.default_branch,
                    &run.onto.as_str()[..7],
                    run.branch
                );
                self.github
                    .delete_branch(repo.installation, &repo.full_name, &run.branch)
                    .await?;
                // Its own CI was green (it was ready), so this is a semantic conflict with what
                // landed: re-derive it on the new head (4b) if it can be.
                self.try_replay(repo, id, ResolutionTier::ReplayFull, &run.onto, &reason)
                    .await
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

    /// A change that cannot land as is: send it to the merge engine when the repository has
    /// it, otherwise return it with `reason`.
    async fn conflicted(
        &self,
        repo: &RepoAccess,
        id: i64,
        onto: &CommitSha,
        reason: &str,
    ) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        if !self.store.merge_engine_enabled(c.change.repo).await? {
            return self
                .try_replay(repo, id, ResolutionTier::ReplayHunks, onto, reason)
                .await;
        }
        let payload = serde_json::json!({
            "pr": c.change.pr.0,
            "head": c.change.head.as_str(),
            "onto": onto.as_str(),
            "branch": repo.default_branch,
        });
        self.github
            .dispatch(repo.installation, &repo.full_name, MERGE_EVENT, payload)
            .await?;
        self.store.start_merge_run(id, onto).await?;
        tracing::info!(repo = %repo.full_name, change = %c.change, onto = %onto, "sent to the merge engine");
        Ok(Outcome::Verifying)
    }

    /// Acts on merge-engine reports: a resolved merge is verified like any change merged onto
    /// the head; an unresolved one is returned; a missing report times out.
    async fn advance_merges(&self, repo_id: RepoId, repo: &RepoAccess) -> anyhow::Result<()> {
        for run in self.store.merge_runs(repo_id).await? {
            let Some(c) = self.store.committer_change(run.change_db_id).await? else {
                continue;
            };
            match run.outcome {
                Some(MergeOutcome::Resolved(sha)) => {
                    let branch = verify_branch(c.change.pr, &c.change.head);
                    self.github
                        .reset_branch(repo.installation, &repo.full_name, &branch, &sha)
                        .await?;
                    let verify = VerifyRun {
                        branch,
                        verify_sha: sha,
                        onto: run.onto.clone(),
                    };
                    self.store
                        .start_verify_run(run.change_db_id, &verify)
                        .await?;
                    tracing::info!(repo = %repo.full_name, change = %c.change, "merge engine resolved the conflict; verifying");
                }
                Some(MergeOutcome::Unresolved(files)) => {
                    let list: Vec<String> =
                        files.iter().take(10).map(|f| format!("`{f}`")).collect();
                    let reason = format!(
                        "the merge engine could not resolve conflicts in {}; rebase it",
                        list.join(", ")
                    );
                    self.try_replay(
                        repo,
                        run.change_db_id,
                        ResolutionTier::ReplayHunks,
                        &run.onto,
                        &reason,
                    )
                    .await?;
                }
                Some(MergeOutcome::Failed) => {
                    let reason = "the `sheltie-merge` workflow failed before it could merge; \
                                  check its run in the repository's Actions tab, then push again";
                    self.return_change(repo, run.change_db_id, reason).await?;
                }
                None if chrono::Utc::now() - run.requested_at
                    > chrono::Duration::minutes(NO_CI_TIMEOUT_MINUTES) =>
                {
                    let reason = "the merge engine did not report back; is the `sheltie-merge` workflow installed?";
                    self.return_change(repo, run.change_db_id, reason).await?;
                }
                None => {}
            }
        }
        Ok(())
    }

    /// The repository's replay settings from `.sheltie/merge.toml` on the default branch, or
    /// why replay cannot be used.
    async fn replay_config(
        &self,
        repo: &RepoAccess,
    ) -> anyhow::Result<Result<ReplayConfig, String>> {
        let text = self
            .github
            .file_at(
                repo.installation,
                &repo.full_name,
                ".sheltie/merge.toml",
                &repo.default_branch,
            )
            .await?;
        Ok(match text.map(|t| Rules::parse(&t)) {
            None => Err("replay is not enabled".into()),
            Some(Err(e)) => Err(format!("`.sheltie/merge.toml` is invalid: {e}")),
            Some(Ok(rules)) if !rules.replay.enabled => Err("replay is not enabled".into()),
            Some(Ok(rules)) => Ok(rules.replay),
        })
    }

    /// Tier 4: asks the runner to re-derive the change onto `onto` with an agent, within the
    /// repository's attempt and budget limits; otherwise returns it with `reason`. No intent
    /// block is needed: the runner works from the pull request itself.
    async fn try_replay(
        &self,
        repo: &RepoAccess,
        id: i64,
        tier: ResolutionTier,
        onto: &CommitSha,
        reason: &str,
    ) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        let config = match self.replay_config(repo).await? {
            Ok(config) => config,
            Err(_) => return self.return_change(repo, id, reason).await,
        };
        let attempts = self.store.replay_attempts(id).await?;
        if attempts >= config.max_attempts {
            let reason = format!("{reason}; {attempts} replay attempts did not land it");
            return self.return_change(repo, id, &reason).await;
        }
        if self.store.replay_cost_last_day(c.change.repo).await? >= config.daily_budget_usd {
            let reason = format!("{reason}; the repository's daily replay budget is spent");
            return self.return_change(repo, id, &reason).await;
        }
        let mode = if tier == ResolutionTier::ReplayHunks {
            "hunks"
        } else {
            "full"
        };
        // The issue the pull request closes, as context for the agent. The runner gets it in
        // the payload, so its workflow needs no permission to read issues.
        let issue = self
            .store
            .closed_issue(c.change.repo, c.change.pr)
            .await?
            .map(|i| {
                serde_json::json!({
                    "number": i.number.0,
                    "title": i.title.chars().take(500).collect::<String>(),
                    "body": i.body.chars().take(MAX_ISSUE_BODY_IN_PAYLOAD).collect::<String>(),
                })
            });
        let payload = serde_json::json!({
            "pr": c.change.pr.0,
            "head": c.change.head.as_str(),
            "onto": onto.as_str(),
            "branch": repo.default_branch,
            "mode": mode,
            "issue": issue,
        });
        self.github
            .dispatch(repo.installation, &repo.full_name, REPLAY_EVENT, payload)
            .await?;
        self.store
            .start_replay(c.change.repo, id, tier, onto)
            .await?;
        tracing::info!(repo = %repo.full_name, change = %c.change, %tier, attempt = attempts + 1, "replaying with an agent");
        Ok(Outcome::Verifying)
    }

    /// Acts on replay reports: a replay that passed its checks and is close enough to the
    /// original is verified like any merge; one that is not is kept on a branch for a human;
    /// a failed one is replayed in full (4b) until the attempt cap or budget is reached.
    async fn advance_replays(&self, repo_id: RepoId, repo: &RepoAccess) -> anyhow::Result<()> {
        let runs = self.store.pending_replays(repo_id).await?;
        if runs.is_empty() {
            return Ok(());
        }
        let threshold = match self.replay_config(repo).await? {
            Ok(config) => config.similarity_threshold,
            // Disabled since: nothing below a perfect match lands without a human.
            Err(_) => 1.0,
        };
        for run in runs {
            let Some(c) = self.store.committer_change(run.change_db_id).await? else {
                continue;
            };
            let pr = c.change.pr;
            match &run.report {
                None if chrono::Utc::now() - run.requested_at
                    > chrono::Duration::minutes(REPLAY_TIMEOUT_MINUTES) =>
                {
                    self.store.handle_replay(&run, false).await?;
                    let reason = "the replay did not report back; is the `sheltie-replay` workflow installed?";
                    self.return_change(repo, run.change_db_id, reason).await?;
                }
                None => {}
                Some(ReplayOutcome {
                    result: Some(sha),
                    checks_passed: true,
                    similarity,
                    details,
                    ..
                }) => {
                    let similarity = similarity.map_or(0.0, Score::get);
                    // A full rebuild that no test checked has only CI to show it builds, not
                    // that it still does what the reviewed change did.
                    let untested =
                        run.tier == ResolutionTier::ReplayFull && !details.acceptance_checked;
                    if similarity >= threshold && !untested {
                        let branch = verify_branch(pr, &c.change.head);
                        self.github
                            .reset_branch(repo.installation, &repo.full_name, &branch, sha)
                            .await?;
                        self.store.handle_replay(&run, true).await?;
                        let verify = VerifyRun {
                            branch,
                            verify_sha: sha.clone(),
                            onto: run.onto.clone(),
                        };
                        self.store
                            .start_verify_run(run.change_db_id, &verify)
                            .await?;
                        self.github
                            .add_labels(repo.installation, &repo.full_name, pr, &[REPLAYED_LABEL])
                            .await?;
                        tracing::info!(repo = %repo.full_name, change = %c.change, similarity, "replay passed its checks; verifying");
                    } else {
                        let branch =
                            format!("sheltie/replay/{}-{}", pr.0, &c.change.head.as_str()[..12]);
                        self.github
                            .reset_branch(repo.installation, &repo.full_name, &branch, sha)
                            .await?;
                        self.store.handle_replay(&run, false).await?;
                        self.github
                            .add_labels(repo.installation, &repo.full_name, pr, &[REAPPROVAL_LABEL])
                            .await?;
                        let (reason, body) = if untested {
                            (
                                format!(
                                    "it was rebuilt on the new head on branch `{branch}`, but it adds no test that fails without it, so nothing shows the rebuilt change still works; review that branch and push it to this pull request to land it"
                                ),
                                sheltie_github::comments::untested_rebuild(&branch),
                            )
                        } else {
                            (
                                format!(
                                    "it was re-derived on branch `{branch}`, but the result differs from the reviewed change (similarity {similarity:.2}, below {threshold:.2}); review that branch and push it to this pull request to land it"
                                ),
                                sheltie_github::comments::needs_reapproval(
                                    &branch, similarity, threshold,
                                ),
                            )
                        };
                        self.return_with(repo, run.change_db_id, &reason, &body)
                            .await?;
                    }
                }
                Some(outcome) => {
                    self.store.handle_replay(&run, false).await?;
                    let why = outcome.error.clone().unwrap_or_else(|| {
                        "the result failed its checks (files outside the change, or acceptance tests)".into()
                    });
                    let reason = format!("re-deriving it with an agent failed: {why}");
                    // 4a escalates to 4b; 4b is retried. Attempts and budget cap both.
                    self.try_replay(
                        repo,
                        run.change_db_id,
                        ResolutionTier::ReplayFull,
                        &run.onto,
                        &reason,
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    async fn return_change(
        &self,
        repo: &RepoAccess,
        id: i64,
        reason: &str,
    ) -> anyhow::Result<Outcome> {
        self.return_with(
            repo,
            id,
            reason,
            &sheltie_github::comments::returned(reason),
        )
        .await
    }

    /// Returns a change with its own comment instead of the generic one.
    async fn return_with(
        &self,
        repo: &RepoAccess,
        id: i64,
        reason: &str,
        body: &str,
    ) -> anyhow::Result<Outcome> {
        let Some(c) = self.store.committer_change(id).await? else {
            return Ok(Outcome::Waiting);
        };
        self.store.mark_returned(id, reason).await?;
        self.github
            .comment(repo.installation, &repo.full_name, c.change.pr, body)
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
