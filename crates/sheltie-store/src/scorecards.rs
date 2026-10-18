//! Per-agent scorecards: what became of each agent's pull requests.

use chrono::{DateTime, Utc};
use sheltie_core::agents::{AgentRule, PrOrigin, PrOutcome, Scorecard, scorecards, validate_rules};
use sheltie_core::{Login, RepoFullName, RepoId};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

impl Store {
    /// The id of a repository Sheltie knows, by `owner/name` (case-insensitive).
    pub async fn repo_id_by_name(&self, repo: &RepoFullName) -> Result<Option<RepoId>, StoreError> {
        let id: Option<i64> =
            sqlx::query_scalar("SELECT id FROM repositories WHERE lower(full_name) = lower($1)")
                .bind(repo.as_str())
                .fetch_optional(&self.pool)
                .await?;
        id.map(|id| Ok(RepoId(crate::from_db_id(id, "repository id")?)))
            .transpose()
    }

    /// Stores the repository's `[[agents]]` rules (empty: none). Invalid rules are not stored.
    pub async fn set_agent_rules(
        &self,
        repo: RepoId,
        rules: &[AgentRule],
    ) -> Result<(), StoreError> {
        validate_rules(rules).map_err(|e| StoreError::InvalidRow(e.to_string()))?;
        let value = if rules.is_empty() {
            None
        } else {
            Some(serde_json::to_value(rules).map_err(|e| StoreError::InvalidRow(e.to_string()))?)
        };
        sqlx::query(
            "UPDATE repositories SET agent_rules = $2
             WHERE id = $1 AND agent_rules IS DISTINCT FROM $2",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The repository's `[[agents]]` rules; none when unset or unreadable.
    pub async fn agent_rules(&self, repo: RepoId) -> Result<Vec<AgentRule>, StoreError> {
        let value: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT agent_rules FROM repositories WHERE id = $1")
                .bind(to_db_id(repo.0, "repository id")?)
                .fetch_optional(&self.pool)
                .await?
                .flatten();
        Ok(value
            .and_then(|v| serde_json::from_value::<Vec<AgentRule>>(v).ok())
            .filter(|r| validate_rules(r).is_ok())
            .unwrap_or_default())
    }

    /// What became of the pull requests Sheltie first saw in the last `days` days.
    pub async fn pr_outcomes(&self, repo: RepoId, days: u32) -> Result<Vec<PrOutcome>, StoreError> {
        let rows = sqlx::query(
            "WITH prs AS (
                 SELECT c.pr_number, array_agg(c.id) AS ids, min(c.created_at) AS opened_at,
                        max(c.merged_at) AS merged_at,
                        (array_agg(c.state ORDER BY c.id DESC))[1] AS last_state,
                        count(*) AS heads,
                        bool_or(c.returned_reason IS NOT NULL OR c.state = 'returned') AS returned,
                        max(c.id) FILTER (WHERE c.author_login IS NOT NULL) AS origin_id
                 FROM changes c
                 WHERE c.repo_id = $1
                 GROUP BY c.pr_number
                 HAVING min(c.created_at) >= now() - make_interval(days => $2))
             SELECT p.opened_at, p.merged_at, p.last_state, p.heads, p.returned,
                    o.author_login, o.author_bot, o.head_ref, o.labels,
                    (SELECT min(h.created_at) FROM head_events h WHERE h.change_id = ANY(p.ids))
                        AS landed_at,
                    EXISTS (SELECT 1 FROM culprits cu
                            WHERE cu.change_id = ANY(p.ids) AND cu.state = 'reverted') AS reverted,
                    EXISTS (SELECT 1 FROM resolution_changes rc
                            JOIN resolutions r ON r.id = rc.resolution_id
                            WHERE rc.change_id = ANY(p.ids)
                              AND r.tier IN ('replay_hunks', 'replay_full')) AS replayed,
                    (SELECT count(*) FROM pr_events e
                     WHERE e.repo_id = $1 AND e.pr_number = p.pr_number
                       AND e.kind = 'changes_requested') AS changes_requested,
                    (SELECT coalesce(sum(r.cost_usd), 0) FROM replay_runs r
                     WHERE r.change_id = ANY(p.ids)) AS replay_cost
             FROM prs p LEFT JOIN changes o ON o.id = p.origin_id",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(i32::try_from(days.clamp(1, 366)).unwrap_or(30))
        .fetch_all(&self.pool)
        .await?;
        let count = |n: i64| u32::try_from(n).unwrap_or(u32::MAX);
        rows.iter()
            .map(|r| -> Result<PrOutcome, StoreError> {
                let author: Option<String> = r.try_get("author_login")?;
                let origin = author.map(|a| -> Result<PrOrigin, StoreError> {
                    Ok(PrOrigin {
                        author: Login::new(a),
                        bot: r.try_get::<Option<bool>, _>("author_bot")?.unwrap_or(false),
                        head_ref: r
                            .try_get::<Option<String>, _>("head_ref")?
                            .unwrap_or_default(),
                        labels: r
                            .try_get::<Option<Vec<String>>, _>("labels")?
                            .unwrap_or_default(),
                    })
                });
                let merged_at: Option<DateTime<Utc>> = r.try_get("merged_at")?;
                let landed_at: Option<DateTime<Utc>> = r.try_get("landed_at")?;
                let merged_at = merged_at.or(landed_at);
                Ok(PrOutcome {
                    origin: origin.transpose()?,
                    opened_at: r.try_get("opened_at")?,
                    merged_at,
                    closed_unmerged: merged_at.is_none()
                        && r.try_get::<String, _>("last_state")? == "closed",
                    heads: count(r.try_get("heads")?),
                    returned: r.try_get("returned")?,
                    reverted: r.try_get("reverted")?,
                    replayed: r.try_get("replayed")?,
                    changes_requested: count(r.try_get("changes_requested")?),
                    replay_cost_usd: r.try_get("replay_cost")?,
                })
            })
            .collect()
    }

    /// Scorecards for the agents behind the pull requests of the last `days` days.
    pub async fn agent_scorecards(
        &self,
        repo: RepoId,
        days: u32,
    ) -> Result<Vec<Scorecard>, StoreError> {
        let rules = self.agent_rules(repo).await?;
        Ok(scorecards(&rules, &self.pr_outcomes(repo, days).await?))
    }
}
