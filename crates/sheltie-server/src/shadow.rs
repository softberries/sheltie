//! Shadow mode (M3): run scheduler epochs and record what Sheltie would do, without merging.

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use sheltie_core::PrNumber;
use sheltie_scheduler::{Candidate, plan_epoch};
use sheltie_store::{PredictionRow, Store, StoreError};
use tokio::sync::Notify;

/// Runs an epoch for every repository with ready changes.
#[derive(Debug, Clone)]
pub struct ShadowScheduler {
    store: Store,
    /// Time between epochs.
    pub epoch: Duration,
    /// Notified when an epoch changed predictions, so check runs refresh promptly.
    pub changed: Arc<Notify>,
}

impl ShadowScheduler {
    /// Creates the scheduler loop.
    pub fn new(store: Store, epoch: Duration, changed: Arc<Notify>) -> Self {
        Self {
            store,
            epoch,
            changed,
        }
    }

    /// Runs one epoch across repositories; returns how many predictions changed.
    pub async fn run_once(&self) -> Result<usize, StoreError> {
        let mut changed = 0;
        for repo in self.store.repos_with_ready_changes().await? {
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
            let rows: Vec<PredictionRow> = plan_epoch(&input)
                .into_iter()
                .map(|(id, decision)| {
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
        }
        if changed > 0 {
            self.changed.notify_one();
        }
        Ok(changed)
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
