//! The coordination worker (M7): ends finished leases, processes claims by assignment, and
//! posts the resulting issue comments and labels (an outbox, retried with backoff).

use std::sync::Arc;
use std::time::Duration;

use sheltie_api::coordination::Coordinator;
use sheltie_core::PrNumber;
use sheltie_github::GitHubApp;
use sheltie_store::{PendingNotice, Store};
use tokio::sync::Notify;

/// How long a claimed notice is reserved for one worker.
const NOTICE_LEASE: Duration = Duration::from_secs(60);
/// Longest wait between attempts to post a notice.
const MAX_BACKOFF: Duration = Duration::from_secs(3600);

/// Runs [`Coordinator::tick`] and sends issue notices.
#[derive(Debug)]
pub struct CoordinationWorker {
    store: Store,
    github: Arc<GitHubApp>,
    coordinator: Arc<Coordinator>,
    period: Duration,
}

impl CoordinationWorker {
    /// Creates the worker; it runs every `period` and whenever woken.
    pub fn new(
        store: Store,
        github: Arc<GitHubApp>,
        coordinator: Arc<Coordinator>,
        period: Duration,
    ) -> Self {
        Self {
            store,
            github,
            coordinator,
            period,
        }
    }

    /// One pass: tick, then send due notices.
    pub async fn run_once(&self) -> anyhow::Result<()> {
        self.coordinator.tick().await?;
        for notice in self.store.claim_notices(20, NOTICE_LEASE).await? {
            match self.send(&notice).await {
                Ok(()) => self.store.notice_sent(notice.id).await?,
                Err(error) => {
                    let backoff = Duration::from_secs(30)
                        .saturating_mul(1 << notice.attempts.clamp(0, 10))
                        .min(MAX_BACKOFF);
                    tracing::warn!(repo = %notice.repo, issue = %notice.issue, error = %format!("{error:#}"), "posting an issue notice failed");
                    self.store.notice_failed(notice.id, backoff).await?;
                }
            }
        }
        Ok(())
    }

    async fn send(&self, n: &PendingNotice) -> anyhow::Result<()> {
        let Some(access) = self.store.repo_access(n.repo).await? else {
            // The App is gone from the repository: nothing can be posted.
            return Ok(());
        };
        let (inst, repo, number) = (access.installation, &access.full_name, PrNumber(n.issue.0));
        if let Some(label) = &n.notice.remove_label {
            self.github.remove_label(inst, repo, number, label).await?;
        }
        if let Some(label) = &n.notice.add_label {
            self.github.add_labels(inst, repo, number, &[label]).await?;
        }
        if let Some(body) = &n.notice.comment {
            self.github.comment(inst, repo, number, body).await?;
        }
        Ok(())
    }

    /// Runs until `shutdown`.
    pub async fn run(self, wake: Arc<Notify>, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            if let Err(error) = self.run_once().await {
                tracing::error!(error = %format!("{error:#}"), "coordination failed");
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = wake.notified() => {}
                () = tokio::time::sleep(self.period) => {}
            }
        }
    }
}
