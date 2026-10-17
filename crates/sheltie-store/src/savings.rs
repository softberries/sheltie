//! Inputs of the time-saved estimate, and the per-repository assumptions it uses.

use chrono::{DateTime, Utc};
use sheltie_core::RepoId;
use sheltie_scheduler::savings::{
    DEFAULT_MINUTES_PER_CONFLICT, DEFAULT_MINUTES_PER_REPLAY, Landing, SavingsInput,
};
use sqlx::Row;

use crate::{Store, StoreError, from_db_id, to_db_id};

fn minutes(v: Option<i32>, default: u32) -> u32 {
    v.and_then(|m| u32::try_from(m).ok()).unwrap_or(default)
}

fn count(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Store {
    /// Repositories of live installations.
    pub async fn installed_repos(&self) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT r.id FROM repositories r JOIN installations i ON i.id = r.installation_id
             WHERE i.deleted_at IS NULL AND i.suspended_at IS NULL ORDER BY r.id",
        )
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|i| from_db_id(i, "repository id").map(RepoId))
            .collect()
    }

    /// Stores the repository's `[estimates]` (`None` uses the default).
    pub async fn set_estimates(
        &self,
        repo: RepoId,
        minutes_per_conflict: Option<u32>,
        minutes_per_replay: Option<u32>,
    ) -> Result<(), StoreError> {
        let db = |m: Option<u32>| m.map(|m| i32::try_from(m.min(1440)).unwrap_or(1440));
        sqlx::query(
            "UPDATE repositories SET minutes_per_conflict = $2, minutes_per_replay = $3
             WHERE id = $1 AND (minutes_per_conflict IS DISTINCT FROM $2
                                OR minutes_per_replay IS DISTINCT FROM $3)",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(db(minutes_per_conflict))
        .bind(db(minutes_per_replay))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// What the time-saved estimate for `repo` since `since` is computed from.
    pub async fn savings_input(
        &self,
        repo: RepoId,
        since: DateTime<Utc>,
    ) -> Result<SavingsInput, StoreError> {
        let id = to_db_id(repo.0, "repository id")?;
        // Landings of changes (reverts excluded) that had become ready first.
        let landings = sqlx::query(
            "SELECT c.ready_at, h.created_at
             FROM head_events h JOIN changes c ON c.id = h.change_id
             WHERE h.repo_id = $1 AND h.created_at >= $2 AND c.ready_at IS NOT NULL
               AND NOT EXISTS (SELECT 1 FROM culprits k
                               WHERE k.repo_id = h.repo_id AND k.revert_sha = h.commit_sha)
             ORDER BY h.seq",
        )
        .bind(id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| -> Result<Landing, StoreError> {
            Ok(Landing {
                ready_at: r.try_get("ready_at")?,
                landed_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<_, _>>()?;
        let ci_seconds: Vec<i32> = sqlx::query_scalar(
            "SELECT ci_seconds FROM branch_commits
             WHERE repo_id = $1 AND is_tip AND ci_state = 'success' AND ci_seconds IS NOT NULL
               AND pushed_at >= $2",
        )
        .bind(id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let row = sqlx::query(
            "SELECT
                count(*) FILTER (WHERE res.tier IN ('structural', 'regenerate', 'declared')) AS conflicts,
                count(*) FILTER (WHERE res.tier IN ('replay_hunks', 'replay_full')) AS replays,
                (SELECT count(*) FROM culprits k
                 WHERE k.repo_id = $1 AND k.state = 'reverted' AND k.created_at >= $2) AS reverts,
                (SELECT minutes_per_conflict FROM repositories WHERE id = $1) AS per_conflict,
                (SELECT minutes_per_replay FROM repositories WHERE id = $1) AS per_replay
             FROM head_events h
             JOIN changes c ON c.id = h.change_id
             LEFT JOIN LATERAL (
                 SELECT rs.tier FROM resolutions rs
                 JOIN resolution_changes rc ON rc.resolution_id = rs.id
                 WHERE rc.change_id = c.id ORDER BY rs.id DESC LIMIT 1) res ON true
             WHERE h.repo_id = $1 AND h.created_at >= $2",
        )
        .bind(id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(SavingsInput {
            landings,
            ci_seconds: ci_seconds.into_iter().map(i64::from).collect(),
            conflicts_resolved: count(row.try_get("conflicts")?),
            replays: count(row.try_get("replays")?),
            reverts: count(row.try_get("reverts")?),
            minutes_per_conflict: minutes(
                row.try_get("per_conflict")?,
                DEFAULT_MINUTES_PER_CONFLICT,
            ),
            minutes_per_replay: minutes(row.try_get("per_replay")?, DEFAULT_MINUTES_PER_REPLAY),
        })
    }
}
