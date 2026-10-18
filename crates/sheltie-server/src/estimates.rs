//! Copies each repository's `[estimates]` and replay budget from `.sheltie/merge.toml` on its
//! default branch into the database, for the dashboard's time-saved and agent-spend panels. A
//! missing or invalid file means the defaults (and replay off).

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sheltie_github::GitHubApp;
use sheltie_merge::rules::Rules;
use sheltie_store::Store;

/// Refreshes the estimate assumptions of every installed repository.
#[derive(Debug, Clone)]
pub struct EstimatesSync {
    store: Store,
    github: Arc<GitHubApp>,
    /// Time between rounds.
    pub poll: Duration,
}

impl EstimatesSync {
    /// Creates the worker.
    pub fn new(store: Store, github: Arc<GitHubApp>, poll: Duration) -> Self {
        Self {
            store,
            github,
            poll,
        }
    }

    /// One round over every installed repository.
    pub async fn run_once(&self) -> anyhow::Result<()> {
        for repo in self.store.installed_repos().await? {
            let Some(access) = self.store.repo_access(repo).await? else {
                continue;
            };
            let text = match self
                .github
                .file_at(
                    access.installation,
                    &access.full_name,
                    ".sheltie/merge.toml",
                    &access.default_branch,
                )
                .await
            {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(repo = %access.full_name, %error, "reading merge.toml failed");
                    continue;
                }
            };
            let rules = text.and_then(|t| Rules::parse(&t).ok());
            let estimates = rules.as_ref().map(|r| r.estimates).unwrap_or_default();
            let budget = rules
                .as_ref()
                .filter(|r| r.replay.enabled)
                .map(|r| r.replay.daily_budget_usd);
            self.store
                .set_estimates(
                    repo,
                    estimates.minutes_per_conflict,
                    estimates.minutes_per_replay,
                )
                .await?;
            self.store.set_replay_budget(repo, budget).await?;
        }
        Ok(())
    }

    /// Runs until `shutdown` resolves.
    pub async fn run(self, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            if let Err(error) = self.run_once().await {
                tracing::error!(error = %format!("{error:#}"), "estimates sync failed");
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = tokio::time::sleep(self.poll) => {}
            }
        }
    }
}
