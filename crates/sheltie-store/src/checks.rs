//! The check-run outbox: which changes need their "Sheltie" check run posted or updated.

use std::time::Duration;

use sheltie_core::{
    ChangeId, ChangeState, CommitSha, GuaranteeTier, InstallationId, PrNumber, RepoFullName,
    RepoId, ResolutionTier,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{Store, StoreError, from_db_id, to_db_id};

/// Longest error text kept in `check_runs.last_error`.
const MAX_ERROR_LEN: usize = 500;

/// A claimed check-run sync: post the change's current state to GitHub.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckRunJob {
    /// Database id of the change; pass it back to [`Store::check_run_synced`].
    pub change_db_id: i64,
    /// The change.
    pub change: ChangeId,
    /// Its state at claim time.
    pub state: ChangeState,
    /// Repository to post to.
    pub repo: RepoFullName,
    /// Installation whose token to use.
    pub installation: InstallationId,
    /// The check run already created on GitHub, if any.
    pub github_id: Option<u64>,
    /// The version this sync covers; a newer request during the sync triggers another one.
    pub version: i64,
    /// How many times this sync has been attempted, including this one.
    pub attempts: i32,
    /// The latest footprint uploaded for the change's head, if any.
    pub footprint: Option<FootprintSummary>,
    /// The last readiness check, if any.
    pub readiness: Option<ReadinessFlags>,
    /// The current shadow-mode prediction, for ready changes.
    pub prediction: Option<String>,
    /// Why the change was returned, if it was.
    pub returned_reason: Option<String>,
    /// Whether Sheltie lands changes in the repository (otherwise it only predicts).
    pub merge_mode: bool,
    /// The repository's default branch, when known.
    pub default_branch: Option<String>,
    /// How it landed, once landed.
    pub landing: Option<Landing>,
    /// The tier that resolved its latest conflict, if any.
    pub resolution: Option<ResolutionTier>,
    /// The replay tier in progress, while an agent re-derives it.
    pub replaying: Option<ResolutionTier>,
    /// What the latest reported replay was built from and tested with, if there was one.
    pub replay: Option<crate::ReplayDetails>,
}

/// The parts of readiness a check run explains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadinessFlags {
    /// Approved per branch protection.
    pub approved: bool,
    /// Other checks passed.
    pub checks_green: bool,
    /// The pull request is a draft.
    pub draft: bool,
}

/// A footprint, for display: sizes, and the first names of each set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintSummary {
    /// Targets written.
    pub writes: usize,
    /// Targets read.
    pub reads: usize,
    /// Test groups to run.
    pub tests: usize,
    /// Whether a non-hermetic target widened it.
    pub leaky: bool,
    /// Targets written, at most [`MAX_LISTED`].
    pub write_names: Vec<String>,
    /// Targets read, at most [`MAX_LISTED`].
    pub read_names: Vec<String>,
    /// Test groups, at most [`MAX_LISTED`].
    pub test_names: Vec<String>,
    /// Why it is leaky, at most [`MAX_LISTED`].
    pub leak_reasons: Vec<String>,
}

/// Most names of each footprint set shown on a check run.
pub const MAX_LISTED: usize = 30;

/// How a landed change landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landing {
    /// The default-branch commit it landed as.
    pub commit: CommitSha,
    /// How it was proven: `footprint`, `speculation` or `sweep`.
    pub tier: GuaranteeTier,
}

impl Store {
    /// Claims up to `limit` due check-run syncs, leasing each for `lease`.
    ///
    /// A claimed job is not handed out again until the lease expires, so several workers
    /// can poll concurrently. Repositories of suspended or deleted installations are skipped.
    pub async fn claim_check_runs(
        &self,
        limit: i64,
        lease: Duration,
    ) -> Result<Vec<CheckRunJob>, StoreError> {
        let rows = sqlx::query(
            "WITH due AS (
                 SELECT cr.change_id
                 FROM check_runs cr
                 JOIN changes c ON c.id = cr.change_id
                 JOIN repositories r ON r.id = c.repo_id
                 JOIN installations i ON i.id = r.installation_id
                 WHERE cr.synced_version < cr.desired_version
                   AND cr.next_attempt_at <= now()
                   AND i.suspended_at IS NULL
                   AND i.deleted_at IS NULL
                 ORDER BY cr.next_attempt_at
                 LIMIT $1
                 FOR UPDATE OF cr SKIP LOCKED
             ), claimed AS (
                 UPDATE check_runs cr
                 SET next_attempt_at = now() + make_interval(secs => $2),
                     attempts = cr.attempts + 1,
                     updated_at = now()
                 FROM due
                 WHERE cr.change_id = due.change_id
                 RETURNING cr.change_id, cr.github_id, cr.desired_version, cr.attempts
             )
             SELECT claimed.change_id, claimed.github_id, claimed.desired_version,
                    claimed.attempts, c.repo_id, c.pr_number, c.head_sha, c.state,
                    r.full_name, r.installation_id,
                    fp.writes_n, fp.reads_n, fp.tests_n, fp.leaky,
                    fp.write_names, fp.read_names, fp.test_names, fp.leak_reasons,
                    rd.approved, rd.checks_green, rd.draft, p.summary AS prediction, c.returned_reason, r.mode = 'merge' AS merge_mode,
                    r.default_branch, he.commit_sha AS landed_sha, he.tier AS landed_tier,
                    (SELECT rs.tier FROM resolutions rs JOIN resolution_changes rc ON rc.resolution_id = rs.id
                     WHERE rc.change_id = c.id ORDER BY rs.id DESC LIMIT 1) AS resolution_tier,
                    (SELECT rr.tier FROM replay_runs rr
                     WHERE rr.change_id = c.id AND NOT rr.handled ORDER BY rr.id DESC LIMIT 1) AS replay_tier,
                    (SELECT rr.details FROM replay_runs rr
                     WHERE rr.change_id = c.id AND rr.reported_at IS NOT NULL
                     ORDER BY rr.id DESC LIMIT 1) AS replay_details
             FROM claimed
             JOIN changes c ON c.id = claimed.change_id
             JOIN repositories r ON r.id = c.repo_id
             LEFT JOIN readiness rd ON rd.change_id = c.id
             LEFT JOIN predictions p ON p.change_id = c.id
             LEFT JOIN LATERAL (
                 SELECT cardinality(writes) AS writes_n, cardinality(reads) AS reads_n,
                        cardinality(tests) AS tests_n, leaky,
                        writes[1:30] AS write_names, reads[1:30] AS read_names,
                        tests[1:30] AS test_names, leak_reasons[1:30] AS leak_reasons
                 FROM footprint_reports f
                 WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                 ORDER BY f.uploaded_at DESC
                 LIMIT 1
             ) fp ON true
             LEFT JOIN LATERAL (
                 SELECT h.commit_sha, h.tier FROM head_events h
                 WHERE h.change_id = c.id ORDER BY h.seq DESC LIMIT 1
             ) he ON true
             ORDER BY claimed.change_id",
        )
        .bind(limit)
        .bind(lease.as_secs_f64())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(job_from_row).collect()
    }

    /// Records a successful sync of `version`, and the GitHub check run id if one was created.
    pub async fn check_run_synced(
        &self,
        change_db_id: i64,
        version: i64,
        github_id: Option<u64>,
    ) -> Result<(), StoreError> {
        let github_id = github_id
            .map(|id| to_db_id(id, "check run id"))
            .transpose()?;
        sqlx::query(
            "UPDATE check_runs
             SET synced_version = GREATEST(synced_version, $2),
                 github_id = COALESCE($3, github_id),
                 attempts = 0,
                 last_error = NULL,
                 next_attempt_at = now(),
                 updated_at = now()
             WHERE change_id = $1",
        )
        .bind(change_db_id)
        .bind(version)
        .bind(github_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Records a failed sync; it is retried after `retry_in`.
    pub async fn check_run_failed(
        &self,
        change_db_id: i64,
        retry_in: Duration,
        error: &str,
    ) -> Result<(), StoreError> {
        let error: String = error.chars().take(MAX_ERROR_LEN).collect();
        sqlx::query(
            "UPDATE check_runs
             SET next_attempt_at = now() + make_interval(secs => $2),
                 last_error = $3,
                 updated_at = now()
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

fn job_from_row(row: &PgRow) -> Result<CheckRunJob, StoreError> {
    let invalid = |e: String| StoreError::InvalidRow(e);
    let state: String = row.try_get("state")?;
    let head: String = row.try_get("head_sha")?;
    let full_name: String = row.try_get("full_name")?;
    let installation: i64 = row.try_get("installation_id")?;
    let github_id: Option<i64> = row.try_get("github_id")?;
    let count = |name: &str| -> Result<Option<usize>, StoreError> {
        let n: Option<i32> = row.try_get(name)?;
        Ok(n.map(|n| usize::try_from(n).unwrap_or(0)))
    };
    let footprint = match (count("writes_n")?, count("reads_n")?, count("tests_n")?) {
        (Some(writes), Some(reads), Some(tests)) => Some(FootprintSummary {
            writes,
            reads,
            tests,
            leaky: row.try_get::<Option<bool>, _>("leaky")?.unwrap_or(false),
            write_names: row
                .try_get::<Option<Vec<String>>, _>("write_names")?
                .unwrap_or_default(),
            read_names: row
                .try_get::<Option<Vec<String>>, _>("read_names")?
                .unwrap_or_default(),
            test_names: row
                .try_get::<Option<Vec<String>>, _>("test_names")?
                .unwrap_or_default(),
            leak_reasons: row
                .try_get::<Option<Vec<String>>, _>("leak_reasons")?
                .unwrap_or_default(),
        }),
        _ => None,
    };
    Ok(CheckRunJob {
        change_db_id: row.try_get("change_id")?,
        change: ChangeId {
            repo: RepoId(from_db_id(row.try_get("repo_id")?, "repository id")?),
            pr: PrNumber(from_db_id(
                row.try_get("pr_number")?,
                "pull request number",
            )?),
            head: CommitSha::parse(head).map_err(|e| invalid(e.to_string()))?,
        },
        state: state
            .parse()
            .map_err(|e: sheltie_core::UnknownVariant| invalid(e.to_string()))?,
        repo: RepoFullName::parse(full_name).map_err(|e| invalid(e.to_string()))?,
        installation: InstallationId(from_db_id(installation, "installation id")?),
        github_id: github_id
            .map(|id| from_db_id(id, "check run id"))
            .transpose()?,
        version: row.try_get("desired_version")?,
        attempts: row.try_get("attempts")?,
        footprint,
        readiness: match (
            row.try_get::<Option<bool>, _>("approved")?,
            row.try_get::<Option<bool>, _>("checks_green")?,
            row.try_get::<Option<bool>, _>("draft")?,
        ) {
            (Some(approved), Some(checks_green), Some(draft)) => Some(ReadinessFlags {
                approved,
                checks_green,
                draft,
            }),
            _ => None,
        },
        prediction: row.try_get("prediction")?,
        returned_reason: row.try_get("returned_reason")?,
        merge_mode: row.try_get("merge_mode")?,
        default_branch: row.try_get("default_branch")?,
        landing: match (
            row.try_get::<Option<String>, _>("landed_sha")?,
            row.try_get::<Option<String>, _>("landed_tier")?,
        ) {
            (Some(sha), Some(tier)) => Some(Landing {
                commit: CommitSha::parse(sha).map_err(|e| invalid(e.to_string()))?,
                tier: tier
                    .parse()
                    .map_err(|e: sheltie_core::UnknownVariant| invalid(e.to_string()))?,
            }),
            _ => None,
        },
        resolution: tier_column(row, "resolution_tier")?,
        replaying: tier_column(row, "replay_tier")?,
        replay: row
            .try_get::<Option<serde_json::Value>, _>("replay_details")?
            .map(|v| crate::ReplayDetails::from_column(Some(v)))
            .transpose()?,
    })
}

fn tier_column(row: &PgRow, name: &str) -> Result<Option<ResolutionTier>, StoreError> {
    row.try_get::<Option<String>, _>(name)?
        .map(|t| {
            t.parse()
                .map_err(|e: sheltie_core::UnknownVariant| StoreError::InvalidRow(e.to_string()))
        })
        .transpose()
}
