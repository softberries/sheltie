//! What intent replays (tier 4) cost: the agent's own estimates and token counts, per
//! repository, for the dashboard.

use sheltie_core::{RepoId, TokenUsage};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

/// Agent spend of one repository over a period.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AgentSpend {
    /// Replays requested.
    pub replays: u32,
    /// Replays whose result passed the checks.
    pub passed: u32,
    /// Estimated spend, in US dollars.
    pub cost_usd: f64,
    /// Estimated spend per day (UTC), oldest first.
    pub daily_usd: Vec<f64>,
    /// Estimated spend over the last 24 hours, which the daily budget limits.
    pub last_day_usd: f64,
    /// `[replay] daily_budget_usd`, when replay is enabled.
    pub daily_budget_usd: Option<f64>,
    /// Tokens of the runs that reported them; `None` when none did.
    pub tokens: Option<TokenUsage>,
    /// How many runs reported tokens.
    pub runs_with_tokens: u32,
}

fn count(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn tokens(n: Option<i64>) -> u64 {
    n.and_then(|n| u64::try_from(n).ok()).unwrap_or(0)
}

impl Store {
    /// Stores the repository's replay budget (`None`: replay is off).
    pub async fn set_replay_budget(
        &self,
        repo: RepoId,
        daily_budget_usd: Option<f64>,
    ) -> Result<(), StoreError> {
        let budget = daily_budget_usd.filter(|b| b.is_finite() && *b > 0.0);
        sqlx::query(
            "UPDATE repositories SET replay_daily_budget_usd = $2
             WHERE id = $1 AND replay_daily_budget_usd IS DISTINCT FROM $2",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(budget)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replay spend of `repo` over the last `days` days (UTC).
    pub async fn agent_spend(&self, repo: RepoId, days: u32) -> Result<AgentSpend, StoreError> {
        let id = to_db_id(repo.0, "repository id")?;
        let days = i32::try_from(days.clamp(1, 366)).unwrap_or(30);
        let row = sqlx::query(
            "SELECT count(*) AS replays,
                    count(*) FILTER (WHERE checks_passed) AS passed,
                    coalesce(sum(cost_usd), 0) AS cost,
                    count(*) FILTER (WHERE input_tokens IS NOT NULL) AS with_tokens,
                    sum(input_tokens)::bigint AS input,
                    sum(output_tokens)::bigint AS output,
                    sum(cache_read_tokens)::bigint AS cache_read,
                    sum(cache_write_tokens)::bigint AS cache_write,
                    (SELECT coalesce(sum(cost_usd), 0) FROM replay_runs
                     WHERE repo_id = $1 AND requested_at > now() - interval '1 day') AS last_day,
                    (SELECT replay_daily_budget_usd FROM repositories WHERE id = $1) AS budget
             FROM replay_runs
             WHERE repo_id = $1 AND requested_at >= current_date - ($2::int - 1)",
        )
        .bind(id)
        .bind(days)
        .fetch_one(&self.pool)
        .await?;
        let daily_usd: Vec<f64> = sqlx::query_scalar(
            "SELECT coalesce(sum(r.cost_usd), 0)
             FROM generate_series(current_date - ($2::int - 1), current_date, interval '1 day') d
             LEFT JOIN replay_runs r ON r.repo_id = $1 AND r.requested_at >= d
                                     AND r.requested_at < d + interval '1 day'
             GROUP BY d ORDER BY d",
        )
        .bind(id)
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        let runs_with_tokens = count(row.try_get("with_tokens")?);
        Ok(AgentSpend {
            replays: count(row.try_get("replays")?),
            passed: count(row.try_get("passed")?),
            cost_usd: row.try_get("cost")?,
            daily_usd,
            last_day_usd: row.try_get("last_day")?,
            daily_budget_usd: row.try_get("budget")?,
            tokens: (runs_with_tokens > 0)
                .then(|| -> Result<TokenUsage, StoreError> {
                    Ok(TokenUsage {
                        input: tokens(row.try_get("input")?),
                        output: tokens(row.try_get("output")?),
                        cache_read: tokens(row.try_get("cache_read")?),
                        cache_write: tokens(row.try_get("cache_write")?),
                    })
                })
                .transpose()?,
            runs_with_tokens,
        })
    }
}
