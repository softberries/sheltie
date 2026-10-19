//! Intents and replay runs (tier 4).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sheltie_core::{
    AcceptanceTest, CommitSha, Intent, ReplayContext, ReplayReport, RepoId, ResolutionTier, Score,
};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

/// The latest replay attempt of a change in `merging`.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayRun {
    /// Run id.
    pub id: i64,
    /// Database id of the change.
    pub change_db_id: i64,
    /// 4a or 4b.
    pub tier: ResolutionTier,
    /// What it was replayed onto.
    pub onto: CommitSha,
    /// When the runner was asked.
    pub requested_at: DateTime<Utc>,
    /// The report, once received.
    pub report: Option<ReplayOutcome>,
}

/// What the runner reported.
#[derive(Debug, Clone, PartialEq)]
pub struct ReplayOutcome {
    /// The merge commit, when the checks passed.
    pub result: Option<CommitSha>,
    /// Scope and acceptance checks passed and the agent finished.
    pub checks_passed: bool,
    /// Similarity to the original diff.
    pub similarity: Option<Score>,
    /// Why it failed, if it did.
    pub error: Option<String>,
    /// What the replay was built from and tested with.
    pub details: ReplayDetails,
}

/// What a reported replay was built from and tested with (`replay_runs.details`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayDetails {
    /// What the prompt was built from; `None` from clients before 0.6.
    #[serde(default)]
    pub context: Option<ReplayContext>,
    /// Tests found in the pull request's diff and how they ran.
    #[serde(default)]
    pub acceptance: Vec<AcceptanceTest>,
    /// Whether any acceptance test (found, or listed in an intent block) checked the result.
    #[serde(default)]
    pub acceptance_checked: bool,
}

impl ReplayDetails {
    /// The details of a report.
    pub fn of(report: &ReplayReport) -> Self {
        Self {
            context: report.context.clone(),
            acceptance: report.acceptance.clone(),
            acceptance_checked: report
                .checks
                .as_ref()
                .is_some_and(|c| c.acceptance_passed.is_some()),
        }
    }

    pub(crate) fn from_column(v: Option<serde_json::Value>) -> Result<Self, StoreError> {
        v.map(|v| serde_json::from_value(v).map_err(|e| StoreError::InvalidRow(e.to_string())))
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

/// Result of [`Store::record_replay_report`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayReportOutcome {
    /// Stored.
    Stored,
    /// No replay of that change and head onto that commit is pending.
    UnknownChange,
}

/// A [`sheltie_core::TokenUsage`] as stored.
#[derive(Clone, Copy)]
struct TokenRow {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
}

impl Store {
    /// The change's intent, if its description has a valid one.
    pub async fn change_intent(&self, change_db_id: i64) -> Result<Option<Intent>, StoreError> {
        let v: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT intent FROM changes WHERE id = $1")
                .bind(change_db_id)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        v.map(|v| serde_json::from_value(v).map_err(|e| StoreError::InvalidRow(e.to_string())))
            .transpose()
    }

    /// How many replays a change has had.
    pub async fn replay_attempts(&self, change_db_id: i64) -> Result<u32, StoreError> {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM replay_runs WHERE change_id = $1")
            .bind(change_db_id)
            .fetch_one(&self.pool)
            .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }

    /// Estimated replay spend of a repository over the last 24 hours, in US dollars.
    pub async fn replay_cost_last_day(&self, repo: RepoId) -> Result<f64, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT coalesce(sum(cost_usd), 0) FROM replay_runs
             WHERE repo_id = $1 AND requested_at > now() - interval '1 day'",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Records a replay request and moves the change to `merging`.
    pub async fn start_replay(
        &self,
        repo: RepoId,
        change_db_id: i64,
        tier: ResolutionTier,
        onto: &CommitSha,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        // Earlier merge and verify runs are superseded: their reports must not be acted on again.
        sqlx::query("DELETE FROM merge_runs WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM verify_runs WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE replay_runs SET handled = true WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO replay_runs (change_id, repo_id, tier, onto_sha) VALUES ($1, $2, $3, $4)",
        )
        .bind(change_db_id)
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(tier.as_str())
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

    /// Stores a runner's replay report on the matching pending run.
    pub async fn record_replay_report(
        &self,
        repo: RepoId,
        report: &ReplayReport,
    ) -> Result<ReplayReportOutcome, StoreError> {
        let run: Option<i64> = sqlx::query_scalar(
            "SELECT r.id FROM replay_runs r JOIN changes c ON c.id = r.change_id
             WHERE c.repo_id = $1 AND c.pr_number = $2 AND c.head_sha = $3 AND c.state = 'merging'
               AND r.onto_sha = $4 AND r.tier = $5 AND r.reported_at IS NULL
             ORDER BY r.id DESC LIMIT 1",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(report.pr.0, "pull request number")?)
        .bind(report.head.as_str())
        .bind(report.onto.as_str())
        .bind(report.tier.as_str())
        .fetch_optional(&self.pool)
        .await?;
        let Some(run) = run else {
            return Ok(ReplayReportOutcome::UnknownChange);
        };
        let error: Option<String> = report.error.as_ref().map(|e| e.chars().take(500).collect());
        // Bounded by `TokenUsage::MAX`, so every count fits a BIGINT.
        let db = |n: u64| i64::try_from(n).unwrap_or(i64::MAX);
        let tokens = report.tokens.map(|t| TokenRow {
            input: db(t.input),
            output: db(t.output),
            cache_read: db(t.cache_read),
            cache_write: db(t.cache_write),
        });
        sqlx::query(
            "UPDATE replay_runs SET reported_at = now(), result_sha = $2, checks_passed = $3,
                 similarity = $4, cost_usd = $5, turns = $6, error = $7,
                 input_tokens = $8, output_tokens = $9, cache_read_tokens = $10,
                 cache_write_tokens = $11, model = $12, details = $13
             WHERE id = $1",
        )
        .bind(run)
        .bind(report.result.as_ref().map(CommitSha::as_str))
        .bind(report.checks_passed())
        .bind(report.checks.as_ref().map(|c| c.similarity.get()))
        .bind(report.cost_usd)
        .bind(i32::try_from(report.turns).unwrap_or(i32::MAX))
        .bind(error)
        .bind(tokens.map(|t| t.input))
        .bind(tokens.map(|t| t.output))
        .bind(tokens.map(|t| t.cache_read))
        .bind(tokens.map(|t| t.cache_write))
        .bind(report.model.as_deref())
        .bind(
            serde_json::to_value(ReplayDetails::of(report))
                .map_err(|e| StoreError::InvalidRow(e.to_string()))?,
        )
        .execute(&self.pool)
        .await?;
        Ok(ReplayReportOutcome::Stored)
    }

    /// The latest unhandled replay run of each change of `repo` in `merging`.
    pub async fn pending_replays(&self, repo: RepoId) -> Result<Vec<ReplayRun>, StoreError> {
        let rows = sqlx::query(
            "SELECT DISTINCT ON (r.change_id) r.id, r.change_id, r.tier, r.onto_sha, r.requested_at,
                    r.reported_at, r.result_sha, r.checks_passed, r.similarity, r.error, r.details
             FROM replay_runs r JOIN changes c ON c.id = r.change_id
             WHERE c.repo_id = $1 AND c.state = 'merging' AND NOT r.handled
             ORDER BY r.change_id, r.id DESC",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| -> Result<ReplayRun, StoreError> {
                let invalid = |e: String| StoreError::InvalidRow(e);
                let tier: String = r.try_get("tier")?;
                let reported: Option<DateTime<Utc>> = r.try_get("reported_at")?;
                let result: Option<String> = r.try_get("result_sha")?;
                let similarity: Option<f32> = r.try_get("similarity")?;
                Ok(ReplayRun {
                    id: r.try_get("id")?,
                    change_db_id: r.try_get("change_id")?,
                    tier: tier
                        .parse()
                        .map_err(|e: sheltie_core::UnknownVariant| invalid(e.to_string()))?,
                    onto: CommitSha::parse(r.try_get::<String, _>("onto_sha")?)
                        .map_err(|e| invalid(e.to_string()))?,
                    requested_at: r.try_get("requested_at")?,
                    report: match reported {
                        None => None,
                        Some(_) => Some(ReplayOutcome {
                            result: result
                                .map(CommitSha::parse)
                                .transpose()
                                .map_err(|e| invalid(e.to_string()))?,
                            checks_passed: r
                                .try_get::<Option<bool>, _>("checks_passed")?
                                .unwrap_or(false),
                            similarity: similarity.and_then(|s| Score::new(s).ok()),
                            error: r.try_get("error")?,
                            details: ReplayDetails::from_column(r.try_get("details")?)?,
                        }),
                    },
                })
            })
            .collect()
    }

    /// Marks a replay run as acted upon; records the resolution when it is going to land.
    pub async fn handle_replay(&self, run: &ReplayRun, landing: bool) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE replay_runs SET handled = true WHERE id = $1")
            .bind(run.id)
            .execute(&mut *tx)
            .await?;
        if landing
            && let Some(ReplayOutcome {
                result: Some(sha),
                similarity,
                ..
            }) = &run.report
        {
            let id: i64 = sqlx::query_scalar(
                "INSERT INTO resolutions (tier, files, similarity, merge_sha) VALUES ($1, '{}', $2, $3) RETURNING id",
            )
            .bind(run.tier.as_str())
            .bind(similarity.map(Score::get))
            .bind(sha.as_str())
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("INSERT INTO resolution_changes (resolution_id, position, change_id) VALUES ($1, 0, $2)")
                .bind(id)
                .bind(run.change_db_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
