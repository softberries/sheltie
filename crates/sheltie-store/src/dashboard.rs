//! Read models for the dashboard, and its sessions.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sheltie_core::{
    ChangeState, CommitSha, GuaranteeTier, PrNumber, RepoFullName, RepoId, ResolutionTier,
};
use sqlx::Row;
use sqlx::postgres::PgRow;

use crate::{RepoMode, Store, StoreError, from_db_id, to_db_id};

/// A signed-in dashboard user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardSession {
    /// GitHub login.
    pub login: String,
    /// Repositories they could access when signing in.
    pub repos: Vec<RepoId>,
}

/// One repository on the overview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoOverview {
    /// Its id.
    pub id: RepoId,
    /// `owner/name`.
    pub name: RepoFullName,
    /// Shadow or merge.
    pub mode: RepoMode,
    /// Whether the merge engine is on.
    pub merge_engine: bool,
    /// Open changes waiting for readiness.
    pub pending: i64,
    /// Ready changes.
    pub ready: i64,
    /// Changes being verified, merged or replayed.
    pub in_flight: i64,
    /// Landed in the last 24 hours.
    pub landed_today: i64,
    /// Returned in the last 24 hours.
    pub returned_today: i64,
    /// When something last landed.
    pub last_landed: Option<DateTime<Utc>>,
}

/// A change in the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueItem {
    /// The pull request.
    pub pr: PrNumber,
    /// Its title, if known.
    pub title: Option<String>,
    /// Its state.
    pub state: ChangeState,
    /// Targets written, if the footprint is known.
    pub writes: Option<i64>,
    /// Targets affected.
    pub reads: Option<i64>,
    /// The scheduler's latest plan for it.
    pub plan: Option<String>,
    /// When it last changed.
    pub since: DateTime<Utc>,
}

/// A recent landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandingItem {
    /// The pull request.
    pub pr: PrNumber,
    /// Its title, if known.
    pub title: Option<String>,
    /// The commit it landed as.
    pub commit: CommitSha,
    /// How it was proven.
    pub guarantee: GuaranteeTier,
    /// How a conflict was resolved, if there was one.
    pub resolution: Option<ResolutionTier>,
    /// When.
    pub at: DateTime<Utc>,
}

/// A returned change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnedItem {
    /// The pull request.
    pub pr: PrNumber,
    /// Its title, if known.
    pub title: Option<String>,
    /// Why.
    pub reason: String,
    /// When.
    pub at: DateTime<Utc>,
}

/// An agent's claim on an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseItem {
    /// The issue number.
    pub issue: u64,
    /// The issue's title, if known.
    pub title: Option<String>,
    /// Who holds it.
    pub holder: String,
    /// When it lapses.
    pub expires_at: DateTime<Utc>,
}

/// Everything the repository page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoDetail {
    /// The headline numbers.
    pub overview: RepoOverview,
    /// Open changes, newest activity first.
    pub queue: Vec<QueueItem>,
    /// Recent landings, newest first.
    pub landed: Vec<LandingItem>,
    /// Recently returned changes, newest first.
    pub returned: Vec<ReturnedItem>,
    /// Active agent leases.
    pub leases: Vec<LeaseItem>,
}

fn parse_err(e: impl std::fmt::Display) -> StoreError {
    StoreError::InvalidRow(e.to_string())
}

fn overview_row(r: &PgRow) -> Result<RepoOverview, StoreError> {
    let mode: String = r.try_get("mode")?;
    Ok(RepoOverview {
        id: RepoId(from_db_id(r.try_get("id")?, "repository id")?),
        name: RepoFullName::parse(r.try_get::<String, _>("full_name")?).map_err(parse_err)?,
        mode: if mode == "merge" {
            RepoMode::Merge
        } else {
            RepoMode::Shadow
        },
        merge_engine: r.try_get("merge_engine")?,
        pending: r.try_get("pending")?,
        ready: r.try_get("ready")?,
        in_flight: r.try_get("in_flight")?,
        landed_today: r.try_get("landed_today")?,
        returned_today: r.try_get("returned_today")?,
        last_landed: r.try_get("last_landed")?,
    })
}

const OVERVIEW: &str = "
    SELECT r.id, r.full_name, r.mode, r.merge_engine,
        count(*) FILTER (WHERE c.state = 'pending') AS pending,
        count(*) FILTER (WHERE c.state = 'ready') AS ready,
        count(*) FILTER (WHERE c.state IN ('verifying', 'merging', 'committable')) AS in_flight,
        count(*) FILTER (WHERE c.state = 'landed' AND c.updated_at > now() - interval '1 day') AS landed_today,
        count(*) FILTER (WHERE c.state = 'returned' AND c.updated_at > now() - interval '1 day') AS returned_today,
        max(c.updated_at) FILTER (WHERE c.state = 'landed') AS last_landed
    FROM repositories r
    JOIN installations i ON i.id = r.installation_id
    LEFT JOIN changes c ON c.repo_id = r.id
    WHERE r.id = ANY($1) AND i.deleted_at IS NULL
    GROUP BY r.id
    ORDER BY r.full_name";

impl Store {
    /// Starts a session for `id_hash` (SHA-256 of the cookie value), lasting `ttl`.
    pub async fn create_dashboard_session(
        &self,
        id_hash: &[u8; 32],
        login: &str,
        repos: &[RepoId],
        ttl: Duration,
    ) -> Result<(), StoreError> {
        let repos: Vec<i64> = repos
            .iter()
            .map(|r| to_db_id(r.0, "repository id"))
            .collect::<Result<_, _>>()?;
        let ttl = chrono::Duration::from_std(ttl).map_err(parse_err)?;
        sqlx::query("DELETE FROM dashboard_sessions WHERE expires_at < now()")
            .execute(&self.pool)
            .await?;
        sqlx::query(
            "INSERT INTO dashboard_sessions (id_hash, login, repo_ids, expires_at) VALUES ($1, $2, $3, now() + $4)",
        )
        .bind(id_hash.as_slice())
        .bind(login)
        .bind(repos)
        .bind(ttl)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The live session for `id_hash`, if any.
    pub async fn dashboard_session(
        &self,
        id_hash: &[u8; 32],
    ) -> Result<Option<DashboardSession>, StoreError> {
        let row = sqlx::query(
            "SELECT login, repo_ids FROM dashboard_sessions WHERE id_hash = $1 AND expires_at > now()",
        )
        .bind(id_hash.as_slice())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<DashboardSession, StoreError> {
            let ids: Vec<i64> = r.try_get("repo_ids")?;
            Ok(DashboardSession {
                login: r.try_get("login")?,
                repos: ids
                    .into_iter()
                    .map(|i| from_db_id(i, "repository id").map(RepoId))
                    .collect::<Result<_, _>>()?,
            })
        })
        .transpose()
    }

    /// Ends a session.
    pub async fn end_dashboard_session(&self, id_hash: &[u8; 32]) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM dashboard_sessions WHERE id_hash = $1")
            .bind(id_hash.as_slice())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The installed repositories among `repos`, with their headline numbers.
    pub async fn repo_overviews(&self, repos: &[RepoId]) -> Result<Vec<RepoOverview>, StoreError> {
        let ids: Vec<i64> = repos
            .iter()
            .map(|r| to_db_id(r.0, "repository id"))
            .collect::<Result<_, _>>()?;
        let rows = sqlx::query(OVERVIEW)
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(overview_row).collect()
    }

    /// The repository page, if `repo` is installed.
    pub async fn repo_detail(&self, repo: RepoId) -> Result<Option<RepoDetail>, StoreError> {
        let Some(overview) = self.repo_overviews(&[repo]).await?.into_iter().next() else {
            return Ok(None);
        };
        let id = to_db_id(repo.0, "repository id")?;
        let state = |r: &PgRow| -> Result<ChangeState, StoreError> {
            r.try_get::<String, _>("state")?.parse().map_err(parse_err)
        };
        let pr = |r: &PgRow| -> Result<PrNumber, StoreError> {
            Ok(PrNumber(from_db_id(
                r.try_get("pr_number")?,
                "pull request number",
            )?))
        };
        let queue = sqlx::query(
            "SELECT c.pr_number, c.title, c.state, c.updated_at,
                    cardinality(fp.writes)::bigint AS writes_n, cardinality(fp.reads)::bigint AS reads_n,
                    p.summary
             FROM changes c
             LEFT JOIN predictions p ON p.change_id = c.id
             LEFT JOIN LATERAL (
                 SELECT writes, reads FROM footprint_reports f
                 WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                 ORDER BY f.uploaded_at DESC LIMIT 1) fp ON true
             WHERE c.repo_id = $1 AND c.state IN ('pending', 'ready', 'verifying', 'merging', 'committable')
             ORDER BY c.updated_at DESC LIMIT 100",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(QueueItem {
                pr: pr(r)?,
                title: r.try_get("title")?,
                state: state(r)?,
                writes: r.try_get("writes_n")?,
                reads: r.try_get("reads_n")?,
                plan: r.try_get("summary")?,
                since: r.try_get("updated_at")?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
        let landed = sqlx::query(
            "SELECT c.pr_number, c.title, h.commit_sha, h.tier, h.created_at,
                    (SELECT rs.tier FROM resolutions rs JOIN resolution_changes rc ON rc.resolution_id = rs.id
                     WHERE rc.change_id = c.id ORDER BY rs.id DESC LIMIT 1) AS resolution
             FROM head_events h JOIN changes c ON c.id = h.change_id
             WHERE h.repo_id = $1 ORDER BY h.seq DESC LIMIT 20",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(LandingItem {
                pr: pr(r)?,
                title: r.try_get("title")?,
                commit: CommitSha::parse(r.try_get::<String, _>("commit_sha")?).map_err(parse_err)?,
                guarantee: r.try_get::<String, _>("tier")?.parse().map_err(parse_err)?,
                resolution: r
                    .try_get::<Option<String>, _>("resolution")?
                    .map(|t| t.parse().map_err(parse_err))
                    .transpose()?,
                at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
        let returned = sqlx::query(
            "SELECT pr_number, title, returned_reason, updated_at FROM changes
             WHERE repo_id = $1 AND state = 'returned' ORDER BY updated_at DESC LIMIT 10",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(ReturnedItem {
                pr: pr(r)?,
                title: r.try_get("title")?,
                reason: r
                    .try_get::<Option<String>, _>("returned_reason")?
                    .unwrap_or_default(),
                at: r.try_get("updated_at")?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
        let leases = sqlx::query(
            "SELECT l.issue_number, i.title, l.holder, l.expires_at FROM leases l
             LEFT JOIN issues i ON i.repo_id = l.repo_id AND i.number = l.issue_number
             WHERE l.repo_id = $1 AND l.released_at IS NULL AND l.expires_at > now()
             ORDER BY l.issue_number",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(LeaseItem {
                issue: from_db_id(r.try_get("issue_number")?, "issue number")?,
                title: r.try_get("title")?,
                holder: r.try_get("holder")?,
                expires_at: r.try_get("expires_at")?,
            })
        })
        .collect::<Result<_, StoreError>>()?;
        Ok(Some(RepoDetail {
            overview,
            queue,
            landed,
            returned,
            leases,
        }))
    }
}
