//! Readiness: whether a change is approved, green and footprinted, so it may be scheduled.

use std::time::Duration;

use sheltie_core::{
    ChangeId, ChangeState, CommitSha, InstallationId, PrNumber, RepoFullName, RepoId,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{Store, StoreError, from_db_id};

/// How long a ready change goes without a re-check, absent events.
const RECHECK_READY: Duration = Duration::from_secs(300);
/// How long a pending change goes without a re-check, absent events.
const RECHECK_PENDING: Duration = Duration::from_secs(60);

/// A claimed readiness check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadinessJob {
    /// Database id of the change.
    pub change_db_id: i64,
    /// The change.
    pub change: ChangeId,
    /// Its repository.
    pub repo: RepoFullName,
    /// Installation whose token to use.
    pub installation: InstallationId,
    /// Request version this check covers.
    pub version: i64,
    /// Attempts so far, including this one.
    pub attempts: i32,
}

/// What GitHub says about a pull request, reduced to what readiness needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    /// The pull request's head is still the change's head.
    pub head_matches: bool,
    /// Approved per branch protection (or no review required).
    pub approved: bool,
    /// Every check and status other than Sheltie's own passed.
    pub checks_green: bool,
    /// The pull request is a draft.
    pub draft: bool,
}

impl Store {
    /// Claims due readiness checks for live changes, leasing each for `lease`.
    pub async fn claim_readiness(
        &self,
        limit: i64,
        lease: Duration,
    ) -> Result<Vec<ReadinessJob>, StoreError> {
        let rows = sqlx::query(
            "WITH due AS (
                 SELECT rd.change_id
                 FROM readiness rd
                 JOIN changes c ON c.id = rd.change_id
                 JOIN repositories r ON r.id = c.repo_id
                 JOIN installations i ON i.id = r.installation_id
                 WHERE c.state IN ('pending', 'ready')
                   AND rd.next_attempt_at <= now()
                   AND (rd.synced_version < rd.desired_version OR rd.recheck_at <= now())
                   AND i.suspended_at IS NULL AND i.deleted_at IS NULL
                 ORDER BY rd.next_attempt_at
                 LIMIT $1
                 FOR UPDATE OF rd SKIP LOCKED
             ), claimed AS (
                 UPDATE readiness rd
                 SET next_attempt_at = now() + make_interval(secs => $2),
                     attempts = rd.attempts + 1
                 FROM due
                 WHERE rd.change_id = due.change_id
                 RETURNING rd.change_id, rd.desired_version, rd.attempts
             )
             SELECT claimed.*, c.repo_id, c.pr_number, c.head_sha, r.full_name, r.installation_id
             FROM claimed
             JOIN changes c ON c.id = claimed.change_id
             JOIN repositories r ON r.id = c.repo_id
             ORDER BY claimed.change_id",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    /// Records a readiness check and moves the change between `pending` and `ready`.
    /// Returns the change's state afterwards.
    pub async fn apply_readiness(
        &self,
        job: &ReadinessJob,
        r: Readiness,
    ) -> Result<ChangeState, StoreError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT c.state, c.base_ref, rp.default_branch,
                    EXISTS (SELECT 1 FROM footprint_reports f
                            WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha) AS has_footprint
             FROM changes c JOIN repositories rp ON rp.id = c.repo_id
             WHERE c.id = $1
             FOR UPDATE OF c",
        )
        .bind(job.change_db_id)
        .fetch_one(&mut *tx)
        .await?;
        let state: String = row.try_get("state")?;
        let state: ChangeState = state
            .parse()
            .map_err(|e: sheltie_core::UnknownVariant| StoreError::InvalidRow(e.to_string()))?;
        let targets_default = {
            let base: Option<String> = row.try_get("base_ref")?;
            let default: Option<String> = row.try_get("default_branch")?;
            base.is_some() && base == default
        };
        let has_footprint: bool = row.try_get("has_footprint")?;
        let ready = r.head_matches
            && r.approved
            && r.checks_green
            && !r.draft
            && has_footprint
            && targets_default;

        let next = match state {
            ChangeState::Pending if ready => ChangeState::Ready,
            ChangeState::Ready if !ready && r.head_matches => ChangeState::Pending,
            other => other,
        };
        if next != state {
            sqlx::query(
                "UPDATE changes
                 SET state = $2,
                     ready_at = CASE WHEN $2 = 'ready' THEN now() ELSE NULL END,
                     updated_at = now()
                 WHERE id = $1",
            )
            .bind(job.change_db_id)
            .bind(next.as_str())
            .execute(&mut *tx)
            .await?;
        }
        let recheck = if next == ChangeState::Ready {
            RECHECK_READY
        } else {
            RECHECK_PENDING
        };
        sqlx::query(
            "UPDATE readiness
             SET synced_version = GREATEST(synced_version, $2),
                 attempts = 0, last_error = NULL, next_attempt_at = now(),
                 recheck_at = now() + make_interval(secs => $3),
                 approved = $4, checks_green = $5, draft = $6, checked_at = now()
             WHERE change_id = $1",
        )
        .bind(job.change_db_id)
        .bind(job.version)
        .bind(recheck.as_secs_f64())
        .bind(r.approved)
        .bind(r.checks_green)
        .bind(r.draft)
        .execute(&mut *tx)
        .await?;
        // The check run explains what the change is waiting for, so refresh it.
        crate::intake::request_check_run(&mut tx, job.change_db_id).await?;
        tx.commit().await?;
        Ok(next)
    }

    /// Records a failed readiness check; it is retried after `retry_in`.
    pub async fn readiness_failed(
        &self,
        change_db_id: i64,
        retry_in: Duration,
        error: &str,
    ) -> Result<(), StoreError> {
        let error: String = error.chars().take(500).collect();
        sqlx::query(
            "UPDATE readiness
             SET next_attempt_at = now() + make_interval(secs => $2), last_error = $3
             WHERE change_id = $1",
        )
        .bind(change_db_id)
        .bind(retry_in.as_secs_f64())
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn job_from_row(row: &PgRow) -> Result<ReadinessJob, StoreError> {
    let invalid = |e: String| StoreError::InvalidRow(e);
    let head: String = row.try_get("head_sha")?;
    let full_name: String = row.try_get("full_name")?;
    Ok(ReadinessJob {
        change_db_id: row.try_get("change_id")?,
        change: ChangeId {
            repo: RepoId(from_db_id(row.try_get("repo_id")?, "repository id")?),
            pr: PrNumber(from_db_id(
                row.try_get("pr_number")?,
                "pull request number",
            )?),
            head: CommitSha::parse(head).map_err(|e| invalid(e.to_string()))?,
        },
        repo: RepoFullName::parse(full_name).map_err(|e| invalid(e.to_string()))?,
        installation: InstallationId(from_db_id(
            row.try_get("installation_id")?,
            "installation id",
        )?),
        version: row.try_get("desired_version")?,
        attempts: row.try_get("attempts")?,
    })
}
