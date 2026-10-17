//! What actually happened on the default branch: CI results, suspected misses, report data.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sheltie_core::{CommitSha, Footprint, InstallationId, RepoFullName, RepoId, TargetId, TestId};
use sheltie_scheduler::report::{MergedChange, ReportInput, SuspectedMiss};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{Store, StoreError, from_db_id, to_db_id};

/// Pending CI is polled this often.
const CI_REPOLL: Duration = Duration::from_secs(60);
/// Only commits pushed this recently are polled.
const CI_HORIZON_DAYS: i32 = 7;
/// When no green commit is known, a red window reaches back at most this many commits.
const MAX_WINDOW: i64 = 50;

/// CI result of a commit, from GitHub's status rollup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CiState {
    /// Still running.
    Pending,
    /// All passed.
    Success,
    /// Something failed.
    Failure,
    /// Something errored.
    Error,
    /// The commit has no checks.
    None,
}

impl CiState {
    fn as_str(self) -> &'static str {
        match self {
            CiState::Pending => "pending",
            CiState::Success => "success",
            CiState::Failure => "failure",
            CiState::Error => "error",
            CiState::None => "none",
        }
    }

    fn is_red(self) -> bool {
        matches!(self, CiState::Failure | CiState::Error)
    }
}

/// A default-branch commit whose CI result is wanted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCiJob {
    /// Repository.
    pub repo: RepoId,
    /// Its name.
    pub full_name: RepoFullName,
    /// Installation whose token to use.
    pub installation: InstallationId,
    /// The commit.
    pub sha: CommitSha,
    /// Attempts so far, including this one.
    pub attempts: i32,
}

/// The changes that landed between the last green commit and a red one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedWindow {
    /// The red commit.
    pub red: CommitSha,
    /// The last green commit before it, if known.
    pub green: Option<CommitSha>,
    /// Landed changes in the window: database id and footprint, if known.
    pub landed: Vec<(i64, Option<Footprint>)>,
}

impl Store {
    /// Claims recent default-branch commits whose CI result is unknown or pending.
    pub async fn claim_branch_ci(
        &self,
        limit: i64,
        lease: Duration,
    ) -> Result<Vec<BranchCiJob>, StoreError> {
        let rows = sqlx::query(
            "WITH due AS (
                 SELECT bc.repo_id, bc.sha
                 FROM branch_commits bc
                 JOIN repositories r ON r.id = bc.repo_id
                 JOIN installations i ON i.id = r.installation_id
                 WHERE (bc.ci_state IS NULL OR bc.ci_state = 'pending')
                   AND bc.ci_next_at <= now()
                   AND bc.pushed_at > now() - make_interval(days => $3)
                   AND i.suspended_at IS NULL AND i.deleted_at IS NULL
                 ORDER BY bc.ci_next_at
                 LIMIT $1
                 FOR UPDATE OF bc SKIP LOCKED
             )
             UPDATE branch_commits bc
             SET ci_next_at = now() + make_interval(secs => $2), ci_attempts = bc.ci_attempts + 1
             FROM due, repositories r
             WHERE bc.repo_id = due.repo_id AND bc.sha = due.sha AND r.id = bc.repo_id
             RETURNING bc.repo_id, bc.sha, bc.ci_attempts, r.full_name, r.installation_id",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .bind(CI_HORIZON_DAYS)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(ci_job_from_row).collect()
    }

    /// Records how long a default-branch commit's CI took (for the time-saved estimate).
    pub async fn record_ci_seconds(
        &self,
        repo: RepoId,
        sha: &CommitSha,
        seconds: i64,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE branch_commits SET ci_seconds = $3 WHERE repo_id = $1 AND sha = $2")
            .bind(to_db_id(repo.0, "repository id")?)
            .bind(sha.as_str())
            .bind(i32::try_from(seconds.clamp(0, 7 * 86_400)).unwrap_or(i32::MAX))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a CI result. When a commit turns red, returns the window of changes that
    /// landed since the last green commit, for miss detection.
    pub async fn apply_branch_ci(
        &self,
        job: &BranchCiJob,
        state: CiState,
    ) -> Result<Option<RedWindow>, StoreError> {
        let repo_id = to_db_id(job.repo.0, "repository id")?;
        let mut tx = self.pool.begin().await?;
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT ci_state FROM branch_commits WHERE repo_id = $1 AND sha = $2 FOR UPDATE",
        )
        .bind(repo_id)
        .bind(job.sha.as_str())
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE branch_commits SET
                 ci_state = $3, ci_checked_at = now(), ci_attempts = 0,
                 ci_next_at = now() + make_interval(secs => $4)
             WHERE repo_id = $1 AND sha = $2",
        )
        .bind(repo_id)
        .bind(job.sha.as_str())
        .bind(state.as_str())
        .bind(CI_REPOLL.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        let was_red = matches!(previous.as_deref(), Some("failure" | "error"));
        if !state.is_red() || was_red {
            tx.commit().await?;
            return Ok(None);
        }
        let seq: i64 =
            sqlx::query_scalar("SELECT seq FROM branch_commits WHERE repo_id = $1 AND sha = $2")
                .bind(repo_id)
                .bind(job.sha.as_str())
                .fetch_one(&mut *tx)
                .await?;
        let green: Option<(String, i64)> = sqlx::query_as(
            "SELECT sha, seq FROM branch_commits
             WHERE repo_id = $1 AND seq < $2 AND ci_state = 'success'
             ORDER BY seq DESC LIMIT 1",
        )
        .bind(repo_id)
        .bind(seq)
        .fetch_optional(&mut *tx)
        .await?;
        let from = green.as_ref().map_or(seq - MAX_WINDOW, |(_, s)| *s);
        let landed = sqlx::query(
            "SELECT c.id, fp.writes, fp.reads, fp.tests, fp.leaky
             FROM changes c
             JOIN branch_commits bc ON bc.repo_id = c.repo_id AND bc.sha = c.merge_commit_sha
             LEFT JOIN LATERAL (
                 SELECT writes, reads, tests, leaky FROM footprint_reports f
                 WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                 ORDER BY f.uploaded_at DESC LIMIT 1
             ) fp ON true
             WHERE c.repo_id = $1 AND bc.seq > $2 AND bc.seq <= $3
             ORDER BY bc.seq",
        )
        .bind(repo_id)
        .bind(from)
        .bind(seq)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(|r| -> Result<(i64, Option<Footprint>), StoreError> {
            let writes: Option<Vec<String>> = r.try_get("writes")?;
            let fp = match writes {
                None => None,
                Some(w) => {
                    let reads: Vec<String> = r.try_get("reads")?;
                    let tests: Vec<String> = r.try_get("tests")?;
                    Some(Footprint {
                        writes: w.into_iter().map(TargetId::new).collect(),
                        reads: reads.into_iter().map(TargetId::new).collect(),
                        tests: tests.into_iter().map(TestId::new).collect(),
                        leaky: r.try_get("leaky")?,
                    })
                }
            };
            Ok((r.try_get("id")?, fp))
        })
        .collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(Some(RedWindow {
            red: job.sha.clone(),
            green: green
                .map(|(s, _)| {
                    CommitSha::parse(s).map_err(|e| StoreError::InvalidRow(e.to_string()))
                })
                .transpose()?,
            landed,
        }))
    }

    /// Records a failed CI lookup; it is retried after `retry_in`.
    pub async fn branch_ci_failed(
        &self,
        job: &BranchCiJob,
        retry_in: Duration,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE branch_commits SET ci_next_at = now() + make_interval(secs => $3) WHERE repo_id = $1 AND sha = $2")
            .bind(to_db_id(job.repo.0, "repository id")?)
            .bind(job.sha.as_str())
            .bind(retry_in.as_secs_f64())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Stores suspected misses of a red window (keys are change database ids).
    pub async fn record_misses(
        &self,
        repo: RepoId,
        window: &RedWindow,
        misses: &[SuspectedMiss<i64>],
    ) -> Result<(), StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        for m in misses {
            let names = |s: &BTreeSet<TargetId>| {
                s.iter().map(|t| t.as_str().to_owned()).collect::<Vec<_>>()
            };
            sqlx::query(
                "INSERT INTO footprint_misses (repo_id, red_sha, green_sha, change_a, change_b, writes_a, writes_b)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)
                 ON CONFLICT (repo_id, red_sha, change_a, change_b) DO NOTHING",
            )
            .bind(repo_id)
            .bind(window.red.as_str())
            .bind(window.green.as_ref().map(CommitSha::as_str))
            .bind(m.a)
            .bind(m.b)
            .bind(names(&m.writes_a))
            .bind(names(&m.writes_b))
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Collects the shadow report's input for `repo` since `since`.
    pub async fn shadow_report_input(
        &self,
        repo: &RepoFullName,
        since: DateTime<Utc>,
    ) -> Result<Option<ReportInput>, StoreError> {
        let Some(repo_id): Option<i64> =
            sqlx::query_scalar("SELECT id FROM repositories WHERE full_name = $1")
                .bind(repo.as_str())
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let merged = sqlx::query(
            "SELECT c.pr_number, c.ready_at, c.merged_at,
                    (SELECT decision FROM prediction_log pl
                     WHERE pl.change_id = c.id AND pl.epoch_at <= c.merged_at
                     ORDER BY pl.id DESC LIMIT 1) AS predicted,
                    (SELECT min(epoch_at) FROM prediction_log pl
                     WHERE pl.change_id = c.id AND pl.decision = 'land_now' AND pl.epoch_at <= c.merged_at)
                        AS first_land_now,
                    (SELECT writes FROM footprint_reports f
                     WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                     ORDER BY f.uploaded_at DESC LIMIT 1) AS writes
             FROM changes c JOIN repositories r ON r.id = c.repo_id
             WHERE c.repo_id = $1 AND c.merged_at >= $2 AND c.base_ref = r.default_branch
             ORDER BY c.merged_at",
        )
        .bind(repo_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| -> Result<MergedChange, StoreError> {
            let writes: Option<Vec<String>> = r.try_get("writes")?;
            Ok(MergedChange {
                pr: from_db_id(r.try_get("pr_number")?, "pull request number")?,
                ready_at: r.try_get("ready_at")?,
                merged_at: r.try_get("merged_at")?,
                predicted: r.try_get("predicted")?,
                first_land_now: r.try_get("first_land_now")?,
                writes: writes.map(|w| w.into_iter().map(TargetId::new).collect()),
            })
        })
        .collect::<Result<_, _>>()?;
        let land_now_batches: Vec<i32> = sqlx::query_scalar(
            "SELECT jsonb_array_length(pl.detail -> 'batch')
             FROM prediction_log pl JOIN changes c ON c.id = pl.change_id
             WHERE c.repo_id = $1 AND pl.epoch_at >= $2 AND pl.decision = 'land_now'",
        )
        .bind(repo_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?;
        let red_commits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM branch_commits
             WHERE repo_id = $1 AND pushed_at >= $2 AND ci_state IN ('failure', 'error')",
        )
        .bind(repo_id)
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        let misses = sqlx::query(
            "SELECT a.pr_number AS pr_a, b.pr_number AS pr_b, m.writes_a, m.writes_b, m.confirmed
             FROM footprint_misses m
             JOIN changes a ON a.id = m.change_a JOIN changes b ON b.id = m.change_b
             WHERE m.repo_id = $1 AND m.detected_at >= $2
             ORDER BY m.id",
        )
        .bind(repo_id)
        .bind(since)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| -> Result<SuspectedMiss<u64>, StoreError> {
            let set = |v: Vec<String>| v.into_iter().map(TargetId::new).collect::<BTreeSet<_>>();
            Ok(SuspectedMiss {
                a: from_db_id(r.try_get("pr_a")?, "pull request number")?,
                b: from_db_id(r.try_get("pr_b")?, "pull request number")?,
                writes_a: set(r.try_get("writes_a")?),
                writes_b: set(r.try_get("writes_b")?),
                confirmed: r.try_get("confirmed")?,
            })
        })
        .collect::<Result<_, _>>()?;
        Ok(Some(ReportInput {
            repository: repo.as_str().to_owned(),
            since,
            merged,
            land_now_batches: land_now_batches
                .into_iter()
                .map(|n| usize::try_from(n).unwrap_or(0))
                .collect(),
            red_commits: usize::try_from(red_commits).unwrap_or(0),
            misses,
        }))
    }
}

fn ci_job_from_row(row: &PgRow) -> Result<BranchCiJob, StoreError> {
    let invalid = |e: String| StoreError::InvalidRow(e);
    Ok(BranchCiJob {
        repo: RepoId(from_db_id(row.try_get("repo_id")?, "repository id")?),
        full_name: RepoFullName::parse(row.try_get::<String, _>("full_name")?)
            .map_err(|e| invalid(e.to_string()))?,
        installation: InstallationId(from_db_id(
            row.try_get("installation_id")?,
            "installation id",
        )?),
        sha: CommitSha::parse(row.try_get::<String, _>("sha")?)
            .map_err(|e| invalid(e.to_string()))?,
        attempts: row.try_get("ci_attempts")?,
    })
}
