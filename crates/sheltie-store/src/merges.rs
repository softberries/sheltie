//! Merge-engine runs and recorded resolutions.

use chrono::{DateTime, Utc};
use sheltie_core::{CommitSha, MergeReport, RepoFullName, RepoId, ResolutionTier};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

/// A change waiting for, or with, a merge-engine report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRun {
    /// Database id of the change.
    pub change_db_id: i64,
    /// What it was merged onto.
    pub onto: CommitSha,
    /// When the engine was asked.
    pub requested_at: DateTime<Utc>,
    /// The engine's verdict, once reported.
    pub outcome: Option<MergeOutcome>,
}

/// What the engine reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// Resolved: the merge commit.
    Resolved(CommitSha),
    /// These files could not be resolved.
    Unresolved(Vec<String>),
}

/// Result of [`Store::record_merge_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeReportOutcome {
    /// Stored.
    Stored,
    /// No change of that repository, PR and head is waiting for the engine.
    UnknownChange,
}

impl Store {
    /// Turns the merge engine on or off for a repository; false if Sheltie does not know it.
    pub async fn set_merge_engine(
        &self,
        repo: &RepoFullName,
        enabled: bool,
    ) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "UPDATE repositories SET merge_engine = $2, updated_at = now() WHERE full_name = $1",
        )
        .bind(repo.as_str())
        .bind(enabled)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Whether the merge engine is on for a repository.
    pub async fn merge_engine_enabled(&self, repo: RepoId) -> Result<bool, StoreError> {
        Ok(
            sqlx::query_scalar::<_, bool>("SELECT merge_engine FROM repositories WHERE id = $1")
                .bind(to_db_id(repo.0, "repository id")?)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false),
        )
    }

    /// Records that a change was sent to the merge engine and moves it to `merging`.
    pub async fn start_merge_run(
        &self,
        change_db_id: i64,
        onto: &CommitSha,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO merge_runs (change_id, onto_sha) VALUES ($1, $2)
             ON CONFLICT (change_id) DO UPDATE SET onto_sha = EXCLUDED.onto_sha, requested_at = now(),
                 result_sha = NULL, unresolved = NULL, reported_at = NULL",
        )
        .bind(change_db_id)
        .bind(onto.as_str())
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE changes SET state = 'merging', updated_at = now() WHERE id = $1 AND state NOT IN ('landed', 'closed')")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        crate::intake::request_check_run(&mut tx, change_db_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Stores an engine report for the change of `repo` at the report's PR and head, and the
    /// resolution if it resolved. Only a change in `merging` accepts a report.
    pub async fn record_merge_report(
        &self,
        repo: RepoId,
        report: &MergeReport,
    ) -> Result<MergeReportOutcome, StoreError> {
        let mut tx = self.pool.begin().await?;
        let change: Option<i64> = sqlx::query_scalar(
            "SELECT c.id FROM changes c JOIN merge_runs m ON m.change_id = c.id
             WHERE c.repo_id = $1 AND c.pr_number = $2 AND c.head_sha = $3 AND c.state = 'merging'
               AND m.onto_sha = $4",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(report.pr.0, "pull request number")?)
        .bind(report.head.as_str())
        .bind(report.onto.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(change) = change else {
            return Ok(MergeReportOutcome::UnknownChange);
        };
        let unresolved: Option<Vec<&str>> = report
            .result
            .is_none()
            .then(|| report.unresolved.iter().map(|p| p.as_str()).collect());
        sqlx::query("UPDATE merge_runs SET result_sha = $2, unresolved = $3, reported_at = now() WHERE change_id = $1")
            .bind(change)
            .bind(report.result.as_ref().map(CommitSha::as_str))
            .bind(unresolved)
            .execute(&mut *tx)
            .await?;
        if let Some(result) = &report.result {
            let files: Vec<&str> = report.files.iter().map(|f| f.path.as_str()).collect();
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO resolutions (tier, files, merge_sha) VALUES ($1, $2, $3) RETURNING id",
            )
            .bind(report.tier().as_str())
            .bind(&files)
            .bind(result.as_str())
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO resolution_changes (resolution_id, position, change_id) VALUES ($1, 0, $2)")
                .bind(id)
                .bind(change)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(MergeReportOutcome::Stored)
    }

    /// Merge runs of a repository's changes still in `merging`.
    pub async fn merge_runs(&self, repo: RepoId) -> Result<Vec<MergeRun>, StoreError> {
        let rows = sqlx::query(
            "SELECT m.change_id, m.onto_sha, m.requested_at, m.result_sha, m.unresolved
             FROM merge_runs m JOIN changes c ON c.id = m.change_id
             WHERE c.repo_id = $1 AND c.state = 'merging' ORDER BY m.change_id",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| -> Result<MergeRun, StoreError> {
                let sha = |s: String| {
                    CommitSha::parse(s).map_err(|e| StoreError::InvalidRow(e.to_string()))
                };
                let result: Option<String> = r.try_get("result_sha")?;
                let unresolved: Option<Vec<String>> = r.try_get("unresolved")?;
                Ok(MergeRun {
                    change_db_id: r.try_get("change_id")?,
                    onto: sha(r.try_get("onto_sha")?)?,
                    requested_at: r.try_get("requested_at")?,
                    outcome: match (result, unresolved) {
                        (Some(s), _) => Some(MergeOutcome::Resolved(sha(s)?)),
                        (None, Some(files)) => Some(MergeOutcome::Unresolved(files)),
                        (None, None) => None,
                    },
                })
            })
            .collect()
    }

    /// The tier that resolved a change's latest merge, if any (for display).
    pub async fn resolution_tier(
        &self,
        change_db_id: i64,
    ) -> Result<Option<ResolutionTier>, StoreError> {
        let tier: Option<String> = sqlx::query_scalar(
            "SELECT r.tier FROM resolutions r JOIN resolution_changes rc ON rc.resolution_id = r.id
             WHERE rc.change_id = $1 ORDER BY r.id DESC LIMIT 1",
        )
        .bind(change_db_id)
        .fetch_optional(&self.pool)
        .await?;
        tier.map(|t| {
            t.parse()
                .map_err(|e: sheltie_core::UnknownVariant| StoreError::InvalidRow(e.to_string()))
        })
        .transpose()
    }

    /// Repositories with changes in `merging`.
    pub async fn repos_with_merging(&self) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT repo_id FROM changes WHERE state = 'merging' ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|i| crate::from_db_id(i, "repository id").map(RepoId))
            .collect()
    }
}
