//! Anonymous usage statistics (`sheltie-server stats`): counts only, no names, no ids.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{Store, StoreError};

/// What happened since a point in time, across all repositories.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    /// Repositories Sheltie knows, by mode (`shadow`, `merge`).
    pub repositories: BTreeMap<String, i64>,
    /// Changes landed by Sheltie, by guarantee tier (`footprint`, `speculation`, `sweep`).
    pub landed: BTreeMap<String, i64>,
    /// Conflicts resolved, by resolution tier.
    pub resolutions: BTreeMap<String, i64>,
    /// Changes returned to their authors.
    pub returned: i64,
    /// Sweep reverts.
    pub reverts: i64,
    /// Intent replays requested.
    pub replays: i64,
    /// Agent leases granted.
    pub leases: i64,
}

async fn counts(
    store: &Store,
    sql: &'static str,
    since: Option<DateTime<Utc>>,
) -> Result<BTreeMap<String, i64>, StoreError> {
    let rows: Vec<(String, i64)> = match since {
        Some(since) => {
            sqlx::query_as(sql)
                .bind(since)
                .fetch_all(&store.pool)
                .await?
        }
        None => sqlx::query_as(sql).fetch_all(&store.pool).await?,
    };
    Ok(rows.into_iter().collect())
}

async fn count(store: &Store, sql: &'static str, since: DateTime<Utc>) -> Result<i64, StoreError> {
    Ok(sqlx::query_scalar(sql)
        .bind(since)
        .fetch_one(&store.pool)
        .await?)
}

impl Store {
    /// Counts since `since`.
    pub async fn stats(&self, since: DateTime<Utc>) -> Result<Stats, StoreError> {
        Ok(Stats {
            repositories: counts(
                self,
                "SELECT mode, count(*) FROM repositories GROUP BY mode",
                None,
            )
            .await?,
            landed: counts(
                self,
                "SELECT tier, count(*) FROM head_events WHERE created_at >= $1 GROUP BY tier",
                Some(since),
            )
            .await?,
            resolutions: counts(
                self,
                "SELECT tier, count(*) FROM resolutions WHERE created_at >= $1 GROUP BY tier",
                Some(since),
            )
            .await?,
            returned: count(
                self,
                "SELECT count(*) FROM changes WHERE state = 'returned' AND updated_at >= $1",
                since,
            )
            .await?,
            reverts: count(
                self,
                "SELECT count(*) FROM culprits WHERE state = 'reverted' AND created_at >= $1",
                since,
            )
            .await?,
            replays: count(
                self,
                "SELECT count(*) FROM replay_runs WHERE requested_at >= $1",
                since,
            )
            .await?,
            leases: count(
                self,
                "SELECT count(*) FROM leases WHERE created_at >= $1",
                since,
            )
            .await?,
        })
    }
}
