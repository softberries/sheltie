//! Shadow mode (M3): run scheduler epochs and record what Sheltie would do, without merging.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use std::collections::BTreeSet;

use sheltie_core::{PrNumber, RepoId};
use sheltie_github::GitHubApp;
use sheltie_scheduler::report::{LandedInWindow, suspected_misses};
use sheltie_scheduler::{Candidate, plan_epoch};
use sheltie_store::{CiState, PredictionRow, RepoMode, Store, StoreError};

use crate::committer::Committer;
use tokio::sync::Notify;

/// Polls CI results of recent default-branch commits and records suspected footprint misses
/// when one turns red.
#[derive(Debug, Clone)]
pub struct BranchCiWorker {
    store: Store,
    github: Arc<GitHubApp>,
    /// Poll interval when not woken.
    pub poll: Duration,
}

impl BranchCiWorker {
    /// Creates the worker.
    pub fn new(store: Store, github: Arc<GitHubApp>) -> Self {
        Self {
            store,
            github,
            poll: Duration::from_secs(15),
        }
    }

    /// Checks one batch of commits; returns how many were checked.
    pub async fn run_once(&self) -> Result<usize, StoreError> {
        let jobs = self
            .store
            .claim_branch_ci(20, Duration::from_secs(60))
            .await?;
        let count = jobs.len();
        for job in jobs {
            match self
                .github
                .commit_ci(job.installation, &job.full_name, &job.sha)
                .await
            {
                Ok(data) => {
                    let state = sheltie_github::ci::evaluate(&data);
                    if state == CiState::Success
                        && let Some(seconds) = sheltie_github::ci::duration_seconds(&data)
                    {
                        self.store
                            .record_ci_seconds(job.repo, &job.sha, seconds)
                            .await?;
                    }
                    if let Some(window) = self.store.apply_branch_ci(&job, state).await? {
                        let landed: Vec<LandedInWindow<i64>> = window
                            .landed
                            .iter()
                            .map(|(key, footprint)| LandedInWindow {
                                key: *key,
                                footprint: footprint.clone(),
                            })
                            .collect();
                        let misses = suspected_misses(&landed);
                        tracing::warn!(
                            repo = %job.full_name,
                            commit = %job.sha,
                            landed = landed.len(),
                            suspected_misses = misses.len(),
                            "default branch turned red"
                        );
                        self.store.record_misses(job.repo, &window, &misses).await?;
                    }
                }
                Err(error) => {
                    tracing::warn!(repo = %job.full_name, commit = %job.sha, error = %error, "CI lookup failed; will retry");
                    let secs = 2u64
                        .saturating_pow(job.attempts.clamp(1, 10).unsigned_abs())
                        .min(600);
                    self.store
                        .branch_ci_failed(&job, Duration::from_secs(secs))
                        .await?;
                }
            }
        }
        Ok(count)
    }

    /// Runs until `shutdown` resolves; polls every `poll`, or sooner when woken.
    pub async fn run(self, wake: Arc<Notify>, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            if let Err(error) = self.run_once().await {
                tracing::error!(error = %error, "branch CI worker failed");
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = wake.notified() => {}
                () = tokio::time::sleep(self.poll) => {}
            }
        }
    }
}

/// Runs an epoch for every repository with ready changes.
#[derive(Debug, Clone)]
pub struct ShadowScheduler {
    store: Store,
    /// Time between epochs.
    pub epoch: Duration,
    /// Notified when an epoch changed predictions, so check runs refresh promptly.
    pub changed: Arc<Notify>,
    /// Lands changes in repositories in merge mode; `None` means shadow mode everywhere.
    pub committer: Option<Committer>,
}

impl ShadowScheduler {
    /// Creates the scheduler loop.
    pub fn new(store: Store, epoch: Duration, changed: Arc<Notify>) -> Self {
        Self {
            store,
            epoch,
            changed,
            committer: None,
        }
    }

    /// Lets the scheduler land changes in repositories switched to merge mode.
    pub fn with_committer(mut self, committer: Committer) -> Self {
        self.committer = Some(committer);
        self
    }

    /// Runs one epoch across repositories; returns how many predictions changed.
    pub async fn run_once(&self) -> Result<usize, StoreError> {
        let mut changed = 0;
        let mut repos: BTreeSet<RepoId> = self
            .store
            .repos_with_ready_changes()
            .await?
            .into_iter()
            .collect();
        repos.extend(self.store.repos_with_verifying().await?);
        repos.extend(self.store.repos_with_merging().await?);
        for repo in repos {
            let candidates = self.store.epoch_candidates(repo).await?;
            let now = Utc::now();
            let pr_of: BTreeMap<i64, PrNumber> = candidates
                .iter()
                .map(|c| (c.change_db_id, c.change.pr))
                .collect();
            let input: Vec<Candidate<i64>> = candidates
                .iter()
                .map(|c| Candidate {
                    key: c.change_db_id,
                    priority: c.priority,
                    waited_minutes: (now - c.ready_at).num_minutes().max(0),
                    footprint: c.footprint.footprint.clone(),
                    landed_writes: c.landed_writes.clone(),
                })
                .collect();
            let plan = plan_epoch(&input);
            let rows: Vec<PredictionRow> = plan
                .iter()
                .map(|(&id, decision)| {
                    let by_pr = decision.map_keys(|k| pr_of.get(k).copied().unwrap_or(PrNumber(0)));
                    let me = pr_of.get(&id).copied().unwrap_or(PrNumber(0));
                    PredictionRow {
                        change_db_id: id,
                        decision: decision.label().to_owned(),
                        detail: serde_json::to_value(&by_pr).unwrap_or(serde_json::Value::Null),
                        summary: by_pr.summary(&me),
                    }
                })
                .collect();
            changed += self.store.record_predictions(repo, now, &rows).await?;
            if let Some(committer) = &self.committer {
                self.commit(committer, repo, &plan).await;
            }
        }
        if changed > 0 {
            self.changed.notify_one();
        }
        Ok(changed)
    }

    /// Lets the committer act on a repository in merge mode, under its commit lock. Errors are
    /// logged; the next epoch tries again.
    async fn commit(
        &self,
        committer: &Committer,
        repo: RepoId,
        plan: &BTreeMap<i64, sheltie_scheduler::Decision<i64>>,
    ) {
        let result: anyhow::Result<()> = async {
            let Some(access) = self.store.repo_access(repo).await? else {
                return Ok(());
            };
            if access.mode != RepoMode::Merge {
                return Ok(());
            }
            let Some(_lock) = self.store.try_lock_repo(repo).await? else {
                return Ok(()); // another replica is committing here
            };
            committer.act(repo, &access, plan).await
        }
        .await;
        if let Err(error) = result {
            tracing::error!(repo = repo.0, error = %format!("{error:#}"), "committer failed");
        }
    }

    /// Runs epochs until `shutdown` resolves.
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            match self.run_once().await {
                Ok(changed) if changed > 0 => {
                    tracing::info!(changed, "shadow epoch changed predictions")
                }
                Ok(_) => {}
                Err(error) => tracing::error!(error = %error, "shadow epoch failed"),
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(self.epoch) => {}
            }
        }
    }
}
