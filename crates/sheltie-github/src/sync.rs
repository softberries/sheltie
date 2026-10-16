//! Background worker that pushes each change's state to its "Sheltie" check run.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use sheltie_store::{CheckRunJob, Store, StoreError};
use tokio::sync::Notify;

use crate::check_run::spec_for;
use crate::client::GitHubApp;

/// Longest wait between retries of a failing sync.
const MAX_BACKOFF: Duration = Duration::from_secs(600);

/// Syncs check runs from the store's outbox to GitHub.
#[derive(Debug, Clone)]
pub struct CheckRunSync {
    store: Store,
    github: Arc<GitHubApp>,
    /// Jobs claimed per poll.
    pub batch: i64,
    /// How long a claimed job is reserved; must exceed the time one sync can take.
    pub lease: Duration,
    /// Poll interval when not woken by [`Notify`].
    pub poll: Duration,
}

impl CheckRunSync {
    /// Creates a worker with default batch size, lease and poll interval.
    pub fn new(store: Store, github: Arc<GitHubApp>) -> Self {
        Self {
            store,
            github,
            batch: 20,
            lease: Duration::from_secs(60),
            poll: Duration::from_secs(5),
        }
    }

    /// Claims and syncs one batch; returns how many jobs were attempted.
    pub async fn run_once(&self) -> Result<usize, StoreError> {
        let jobs = self.store.claim_check_runs(self.batch, self.lease).await?;
        let count = jobs.len();
        for job in jobs {
            self.sync(job).await?;
        }
        Ok(count)
    }

    /// Runs until `shutdown` resolves. Polls every `poll`, or sooner when `wake` is notified.
    pub async fn run(self, wake: Arc<Notify>, shutdown: impl Future<Output = ()>) {
        tokio::pin!(shutdown);
        loop {
            // Drain everything that is due before sleeping again.
            loop {
                match self.run_once().await {
                    Ok(n) if n as i64 == self.batch => continue,
                    Ok(_) => break,
                    Err(error) => {
                        tracing::error!(error = %error_chain(&error), "check-run sync failed");
                        break;
                    }
                }
            }
            tokio::select! {
                () = &mut shutdown => return,
                () = wake.notified() => {}
                () = tokio::time::sleep(self.poll) => {}
            }
        }
    }

    async fn sync(&self, job: CheckRunJob) -> Result<(), StoreError> {
        let spec = spec_for(job.state, &job.change.head);
        let result = match job.github_id {
            Some(id) => self
                .github
                .update_check_run(job.installation, &job.repo, id, &spec)
                .await
                .map(|()| None),
            // A change that closed before its check run was created needs no check run.
            None if !job.state.is_live() => Ok(None),
            None => self
                .github
                .create_check_run(
                    job.installation,
                    &job.repo,
                    &job.change.head,
                    &job.change_db_id.to_string(),
                    &spec,
                )
                .await
                .map(Some),
        };
        match result {
            Ok(github_id) => {
                tracing::debug!(change = %job.change, state = %job.state, "check run synced");
                self.store
                    .check_run_synced(job.change_db_id, job.version, github_id)
                    .await
            }
            Err(error) => {
                let error = error_chain(&error);
                let retry_in = backoff(job.attempts);
                tracing::warn!(
                    change = %job.change,
                    attempts = job.attempts,
                    retry_in_secs = retry_in.as_secs(),
                    error = %error,
                    "check run sync failed; will retry"
                );
                self.store
                    .check_run_failed(job.change_db_id, retry_in, &error)
                    .await
            }
        }
    }
}

/// Exponential backoff: 2s, 4s, 8s … capped at [`MAX_BACKOFF`].
fn backoff(attempts: i32) -> Duration {
    let exp = attempts.clamp(1, 16).unsigned_abs();
    Duration::from_secs(1u64 << exp).min(MAX_BACKOFF)
}

/// An error and its sources, joined with `: `.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(s) = source {
        out.push_str(": ");
        out.push_str(&s.to_string());
        source = s.source();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(8));
        assert_eq!(backoff(30), MAX_BACKOFF);
        assert_eq!(backoff(0), Duration::from_secs(2));
    }
}
