//! Sweeps (M4): keep the verified prefix, find the culprit of a red default branch, revert it.
//!
//! The repository's CI on each push to the default branch is the sweep. The newest green tip
//! is the verified prefix; Sheltie points `sheltie/verified` at it so deploy pipelines can
//! read from it. When the newest tip is red, the first-parent commits since the last green
//! tip are analysed ([`sheltie_sweeper::analyse`]): unknown commits are probed by pointing a
//! `sheltie/bisect/<sha>` branch at them (the repository's CI runs on it), and a culprit whose
//! red persisted is reverted through the same compare-and-swap as any landing.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sheltie_core::{CommitSha, GuaranteeTier, RepoId};
use sheltie_github::GitHubApp;
use sheltie_github::git::{FastForward, MergeResult};
use sheltie_store::{CiState, RepoAccess, RepoMode, Store};
use sheltie_sweeper::{Analysis, Ci, analyse};

/// Branch that always points at the verified prefix.
pub const VERIFIED_BRANCH: &str = "sheltie/verified";

/// Runs sweeps for repositories in merge mode.
#[derive(Debug, Clone)]
pub struct Sweeper {
    store: Store,
    github: Arc<GitHubApp>,
    /// Time between sweeps.
    pub poll: Duration,
    /// The red tip each repository was last warned about, so a red stretch is logged once.
    warned: Arc<Mutex<HashMap<RepoId, CommitSha>>>,
}

fn short(sha: &CommitSha) -> &str {
    &sha.as_str()[..12]
}

impl Sweeper {
    /// Creates the sweeper.
    pub fn new(store: Store, github: Arc<GitHubApp>, poll: Duration) -> Self {
        Self {
            store,
            github,
            poll,
            warned: Arc::default(),
        }
    }

    /// Sweeps every repository in merge mode once.
    pub async fn run_once(&self) -> anyhow::Result<()> {
        for repo in self.store.repos_in_merge_mode().await? {
            let Some(access) = self.store.repo_access(repo).await? else {
                continue;
            };
            if access.mode != RepoMode::Merge {
                continue;
            }
            let Some(_lock) = self.store.try_lock_repo(repo).await? else {
                continue;
            };
            if let Err(error) = self.sweep(repo, &access).await {
                tracing::error!(repo = %access.full_name, error = %format!("{error:#}"), "sweep failed");
            }
        }
        Ok(())
    }

    async fn sweep(&self, repo: RepoId, access: &RepoAccess) -> anyhow::Result<()> {
        let green = self.store.newest_green_tip(repo).await?;
        if let Some(green) = &green
            && self.store.set_verified(repo, &green.sha).await?
        {
            self.github
                .reset_branch(
                    access.installation,
                    &access.full_name,
                    VERIFIED_BRANCH,
                    &green.sha,
                )
                .await?;
            tracing::info!(repo = %access.full_name, verified = %green.sha, "verified prefix advanced");
        }
        let Some(tip) = self.store.newest_tip(repo).await? else {
            return Ok(());
        };
        if !matches!(tip.ci, Some(CiState::Failure | CiState::Error)) {
            return Ok(());
        }
        let Some(green) = green else {
            let first = self
                .warned
                .lock()
                .map(|mut w| w.insert(repo, tip.sha.clone()).as_ref() != Some(&tip.sha))
                .unwrap_or(true);
            if first {
                tracing::warn!(repo = %access.full_name, tip = %short(&tip.sha), "default branch is red and no green commit is known; not bisecting");
            }
            return Ok(());
        };
        // Already handling this red stretch?
        if let Some((culprit, red_seq)) = self.store.latest_culprit(repo).await?
            && red_seq > green.seq
        {
            if culprit.state == "reverting" {
                self.revert(
                    repo,
                    access,
                    &culprit.culprit,
                    &culprit.red,
                    culprit.revert_pr,
                )
                .await?;
            }
            return Ok(());
        }
        let Some(chain) = self
            .github
            .first_parent_chain(access.installation, &access.full_name, &green.sha, &tip.sha)
            .await?
        else {
            tracing::warn!(repo = %access.full_name, "cannot read the commits since the last green one");
            return Ok(());
        };
        let known = self.store.ci_states(repo, &chain).await?;
        let mut window = Vec::with_capacity(chain.len());
        for commit in &chain {
            let state = match known.get(commit) {
                Some(state) => *state,
                None => sheltie_github::ci::evaluate(
                    &self
                        .github
                        .commit_ci(access.installation, &access.full_name, commit)
                        .await?,
                ),
            };
            window.push(match state {
                CiState::Success => Ci::Green,
                CiState::Failure | CiState::Error => Ci::Red,
                CiState::Pending | CiState::None => Ci::Unknown,
            });
        }
        match analyse(&window) {
            Analysis::Probe(i) => {
                let branch = format!("sheltie/bisect/{}", short(&chain[i]));
                self.github
                    .reset_branch(access.installation, &access.full_name, &branch, &chain[i])
                    .await?;
                tracing::info!(repo = %access.full_name, probe = %chain[i], "bisect: running CI on a commit");
            }
            Analysis::Culprit(i) => {
                for commit in &chain {
                    let branch = format!("sheltie/bisect/{}", short(commit));
                    self.github
                        .delete_branch(access.installation, &access.full_name, &branch)
                        .await?;
                }
                tracing::warn!(repo = %access.full_name, culprit = %chain[i], red = %tip.sha, "culprit found; reverting");
                self.revert(repo, access, &chain[i], &tip.sha, None).await?;
            }
            Analysis::Wait | Analysis::Green => {}
        }
        Ok(())
    }

    /// Reverts `culprit` on top of the current head and lands the revert.
    ///
    /// The revert is built without a checkout: a commit with the culprit's first parent's tree
    /// on top of the culprit is the culprit undone; merging it onto the head (three-way, with
    /// the culprit as base) gives the head minus the culprit's change.
    async fn revert(
        &self,
        repo: RepoId,
        access: &RepoAccess,
        culprit: &CommitSha,
        red: &CommitSha,
        open_pr: Option<sheltie_core::PrNumber>,
    ) -> anyhow::Result<()> {
        let (inst, name) = (access.installation, &access.full_name);
        let info = self.github.commit_info(inst, name, culprit).await?;
        let Some(parent) = info.parents.first() else {
            anyhow::bail!("culprit {culprit} has no parent");
        };
        let parent_tree = self.github.commit_info(inst, name, parent).await?.tree;
        let landed = self.store.change_by_merge_commit(repo, culprit).await?;
        let what = match &landed {
            Some(c) => format!("#{}", c.pr.0),
            None => format!("commit {}", short(culprit)),
        };
        let title = match &landed {
            Some(c) => self
                .github
                .pr_title(inst, name, c.pr)
                .await
                .unwrap_or_default(),
            None => String::new(),
        };
        let undo = self
            .github
            .create_commit(
                inst,
                name,
                &parent_tree,
                std::slice::from_ref(culprit),
                &format!("Undo {}", short(culprit)),
            )
            .await?;
        let head = self
            .github
            .branch_head(inst, name, &access.default_branch)
            .await?;
        let branch = format!("sheltie/revert/{}", short(culprit));
        self.github.reset_branch(inst, name, &branch, &head).await?;
        let message = format!(
            "Revert {what}{}\n\nThis reverts commit {culprit}: `{}` turned red at {} and stayed red.\n\nSheltie-Revert: {culprit}\n",
            if title.is_empty() {
                String::new()
            } else {
                format!(": {title}")
            },
            access.default_branch,
            short(red),
        );
        let revert = match self
            .github
            .merge_into(inst, name, &branch, &undo, &message)
            .await?
        {
            MergeResult::Merged(sha) => sha,
            MergeResult::AlreadyMerged => return Ok(()),
            MergeResult::Conflict => {
                self.github.delete_branch(inst, name, &branch).await?;
                self.store
                    .record_culprit(repo, culprit, red, "conflict", None, None)
                    .await?;
                if let Some(c) = &landed {
                    let body = sheltie_github::comments::revert_conflict(&access.default_branch);
                    self.github.comment(inst, name, c.pr, &body).await?;
                }
                return Ok(());
            }
        };
        let pr = match open_pr {
            Some(pr) => pr,
            None => {
                let body = sheltie_github::comments::revert_pr_body(
                    &what,
                    &access.default_branch,
                    short(red),
                    short(culprit),
                );
                self.github
                    .open_pr(
                        inst,
                        name,
                        &branch,
                        &access.default_branch,
                        &format!("Revert {what}"),
                        &body,
                    )
                    .await?
            }
        };
        self.store
            .record_culprit(repo, culprit, red, "reverting", Some(&revert), Some(pr))
            .await?;
        match self
            .github
            .fast_forward(inst, name, &access.default_branch, &revert)
            .await?
        {
            FastForward::Moved => Ok(()), // retried on the next sweep, on the new head
            FastForward::Done => {
                self.store
                    .record_culprit(repo, culprit, red, "reverted", Some(&revert), Some(pr))
                    .await?;
                if let Some(c) = &landed {
                    let body = sheltie_github::comments::reverted(&access.default_branch, pr.0);
                    self.github.comment(inst, name, c.pr, &body).await?;
                    if c.tier == Some(GuaranteeTier::FootprintProven) {
                        let confirmed = self.store.confirm_misses(repo, c.change_db_id).await?;
                        tracing::warn!(repo = %name, change = c.pr.0, confirmed, "footprint-proven change blamed: misses confirmed");
                    }
                }
                tracing::info!(repo = %name, culprit = %culprit, revert = %revert, "reverted");
                Ok(())
            }
        }
    }

    /// Runs until `shutdown` resolves.
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            if let Err(error) = self.run_once().await {
                tracing::error!(error = %format!("{error:#}"), "sweeper failed");
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(self.poll) => {}
            }
        }
    }
}
