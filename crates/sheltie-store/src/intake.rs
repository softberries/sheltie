//! Applying forge events: one transaction per webhook delivery.

use chrono::{DateTime, Utc};
use sheltie_core::intake::{InstallationEvent, Intake, Lifecycle, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{ChangeState, DeliveryId, InstallationId, Login};
use sqlx::{PgConnection, Row};

use crate::{Store, StoreError, to_db_id};

/// Result of [`Store::ingest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingested {
    /// The delivery was new and has been applied.
    Applied,
    /// The delivery was seen before; nothing changed.
    Duplicate,
}

impl Store {
    /// Records a webhook delivery and applies its event, atomically.
    ///
    /// Redeliveries of the same delivery id are detected and ignored. Pull request lifecycle
    /// events older than one already applied to the PR are recorded with `applied = false`
    /// and change nothing, since GitHub does not guarantee delivery order.
    pub async fn ingest(
        &self,
        delivery: &DeliveryId,
        event: &str,
        intake: &Intake,
    ) -> Result<Ingested, StoreError> {
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO webhook_deliveries (delivery_id, event) VALUES ($1, $2)
             ON CONFLICT (delivery_id) DO NOTHING",
        )
        .bind(delivery.as_str())
        .bind(event)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            return Ok(Ingested::Duplicate);
        }
        match intake {
            Intake::Pr(e) => apply_pr_event(&mut tx, delivery, e).await?,
            Intake::Installation(e) => apply_installation_event(&mut tx, e).await?,
        }
        tx.commit().await?;
        Ok(Ingested::Applied)
    }
}

async fn apply_pr_event(
    conn: &mut PgConnection,
    delivery: &DeliveryId,
    e: &PrEvent,
) -> Result<(), StoreError> {
    let repo_id = to_db_id(e.repo.id.0, "repository id")?;
    let pr = to_db_id(e.pr.0, "pull request number")?;
    upsert_installation(conn, e.installation, &Login::new(e.repo.full_name.owner())).await?;
    upsert_repo(conn, &e.repo, Some(e.installation)).await?;

    // Serialize events for the same PR so supersession is decided on a consistent view.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("pr:{repo_id}:{pr}"))
        .execute(&mut *conn)
        .await?;

    let applied = match e.kind.lifecycle() {
        Lifecycle::Record => true,
        lifecycle => {
            let newest: Option<DateTime<Utc>> = sqlx::query_scalar(
                "SELECT max(event_at) FROM changes WHERE repo_id = $1 AND pr_number = $2",
            )
            .bind(repo_id)
            .bind(pr)
            .fetch_one(&mut *conn)
            .await?;
            let stale = match newest {
                None => false,
                Some(newest) => {
                    // `opened` is always a PR's first event, so on a tie anything else wins.
                    e.at < newest || (e.at == newest && e.kind == PrEventKind::Opened)
                }
            };
            if !stale {
                match lifecycle {
                    Lifecycle::Open => open_head(conn, repo_id, pr, e).await?,
                    _ => close_live(conn, repo_id, pr, None, e.at).await?,
                }
            }
            !stale
        }
    };

    sqlx::query(
        "INSERT INTO pr_events
             (delivery_id, repo_id, pr_number, head_sha, base_sha, kind, actor, event_at, applied)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(delivery.as_str())
    .bind(repo_id)
    .bind(pr)
    .bind(e.head.as_str())
    .bind(e.base.as_str())
    .bind(e.kind.as_str())
    .bind(e.actor.as_str())
    .bind(e.at)
    .bind(applied)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Tracks the PR at the event's head: supersedes other heads, then creates or reopens the change.
async fn open_head(
    conn: &mut PgConnection,
    repo_id: i64,
    pr: i64,
    e: &PrEvent,
) -> Result<(), StoreError> {
    close_live(conn, repo_id, pr, Some(e.head.as_str()), e.at).await?;
    let change_id: i64 = sqlx::query_scalar(
        "INSERT INTO changes (repo_id, pr_number, head_sha, base_sha, state, event_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (repo_id, pr_number, head_sha) DO UPDATE
             SET base_sha = EXCLUDED.base_sha,
                 state = CASE WHEN changes.state = $7 THEN $5 ELSE changes.state END,
                 event_at = EXCLUDED.event_at,
                 updated_at = now()
         RETURNING id",
    )
    .bind(repo_id)
    .bind(pr)
    .bind(e.head.as_str())
    .bind(e.base.as_str())
    .bind(ChangeState::Pending.as_str())
    .bind(e.at)
    .bind(ChangeState::Closed.as_str())
    .fetch_one(&mut *conn)
    .await?;
    request_check_run(conn, change_id).await
}

/// Closes the PR's live changes, except the one at `keep_head`.
async fn close_live(
    conn: &mut PgConnection,
    repo_id: i64,
    pr: i64,
    keep_head: Option<&str>,
    at: DateTime<Utc>,
) -> Result<(), StoreError> {
    let live: Vec<&str> = ChangeState::ALL
        .iter()
        .filter(|s| s.is_live())
        .map(|s| s.as_str())
        .collect();
    let closed: Vec<i64> = sqlx::query_scalar(
        "UPDATE changes SET state = $1, event_at = $2, updated_at = now()
         WHERE repo_id = $3 AND pr_number = $4 AND state = ANY($5)
           AND head_sha IS DISTINCT FROM $6
         RETURNING id",
    )
    .bind(ChangeState::Closed.as_str())
    .bind(at)
    .bind(repo_id)
    .bind(pr)
    .bind(&live)
    .bind(keep_head)
    .fetch_all(&mut *conn)
    .await?;
    for change_id in closed {
        request_check_run(conn, change_id).await?;
    }
    Ok(())
}

/// Marks the change's check run as needing a sync with GitHub.
async fn request_check_run(conn: &mut PgConnection, change_id: i64) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO check_runs (change_id) VALUES ($1)
         ON CONFLICT (change_id) DO UPDATE
             SET desired_version = check_runs.desired_version + 1,
                 attempts = 0,
                 next_attempt_at = now(),
                 updated_at = now()",
    )
    .bind(change_id)
    .execute(conn)
    .await?;
    Ok(())
}

async fn apply_installation_event(
    conn: &mut PgConnection,
    e: &InstallationEvent,
) -> Result<(), StoreError> {
    match e {
        InstallationEvent::Created {
            installation,
            account,
            repos,
        } => {
            upsert_installation(conn, *installation, account).await?;
            set_installation_flag(conn, *installation, InstallationFlag::Restored).await?;
            for repo in repos {
                upsert_repo(conn, repo, Some(*installation)).await?;
            }
        }
        InstallationEvent::Deleted { installation } => {
            set_installation_flag(conn, *installation, InstallationFlag::Deleted).await?;
            sqlx::query("UPDATE repositories SET installation_id = NULL, updated_at = now() WHERE installation_id = $1")
                .bind(to_db_id(installation.0, "installation id")?)
                .execute(&mut *conn)
                .await?;
        }
        InstallationEvent::Suspended { installation } => {
            set_installation_flag(conn, *installation, InstallationFlag::Suspended).await?;
        }
        InstallationEvent::Unsuspended { installation } => {
            set_installation_flag(conn, *installation, InstallationFlag::Unsuspended).await?;
        }
        InstallationEvent::ReposChanged {
            installation,
            account,
            added,
            removed,
        } => {
            upsert_installation(conn, *installation, account).await?;
            for repo in added {
                upsert_repo(conn, repo, Some(*installation)).await?;
            }
            for repo in removed {
                sqlx::query(
                    "UPDATE repositories SET installation_id = NULL, updated_at = now()
                     WHERE id = $1 AND installation_id = $2",
                )
                .bind(to_db_id(repo.0, "repository id")?)
                .bind(to_db_id(installation.0, "installation id")?)
                .execute(&mut *conn)
                .await?;
            }
        }
    }
    Ok(())
}

async fn upsert_installation(
    conn: &mut PgConnection,
    installation: InstallationId,
    account: &Login,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO installations (id, account_login) VALUES ($1, $2)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(to_db_id(installation.0, "installation id")?)
    .bind(account.as_str())
    .execute(conn)
    .await?;
    Ok(())
}

/// A change to an installation's lifecycle columns.
#[derive(Debug, Clone, Copy)]
enum InstallationFlag {
    Restored,
    Deleted,
    Suspended,
    Unsuspended,
}

async fn set_installation_flag(
    conn: &mut PgConnection,
    installation: InstallationId,
    flag: InstallationFlag,
) -> Result<(), StoreError> {
    let sql = match flag {
        InstallationFlag::Restored => {
            "UPDATE installations SET deleted_at = NULL, suspended_at = NULL, updated_at = now()
             WHERE id = $1"
        }
        InstallationFlag::Deleted => {
            "UPDATE installations SET deleted_at = now(), updated_at = now() WHERE id = $1"
        }
        InstallationFlag::Suspended => {
            "UPDATE installations SET suspended_at = now(), updated_at = now() WHERE id = $1"
        }
        InstallationFlag::Unsuspended => {
            "UPDATE installations SET suspended_at = NULL, updated_at = now() WHERE id = $1"
        }
    };
    sqlx::query(sql)
        .bind(to_db_id(installation.0, "installation id")?)
        .execute(conn)
        .await?;
    Ok(())
}

async fn upsert_repo(
    conn: &mut PgConnection,
    repo: &RepoRef,
    installation: Option<InstallationId>,
) -> Result<(), StoreError> {
    let installation = installation
        .map(|i| to_db_id(i.0, "installation id"))
        .transpose()?;
    sqlx::query(
        "INSERT INTO repositories (id, full_name, installation_id) VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE
             SET full_name = EXCLUDED.full_name,
                 installation_id = EXCLUDED.installation_id,
                 updated_at = now()",
    )
    .bind(to_db_id(repo.id.0, "repository id")?)
    .bind(repo.full_name.as_str())
    .bind(installation)
    .execute(conn)
    .await?;
    Ok(())
}

impl Store {
    /// The state of the change for `repo`/`pr` at `head`, if it exists.
    pub async fn change_state(
        &self,
        repo: sheltie_core::RepoId,
        pr: sheltie_core::PrNumber,
        head: &sheltie_core::CommitSha,
    ) -> Result<Option<ChangeState>, StoreError> {
        let row = sqlx::query(
            "SELECT state FROM changes WHERE repo_id = $1 AND pr_number = $2 AND head_sha = $3",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(pr.0, "pull request number")?)
        .bind(head.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| {
            let state: String = r.try_get("state")?;
            state
                .parse()
                .map_err(|e: sheltie_core::UnknownVariant| StoreError::InvalidRow(e.to_string()))
        })
        .transpose()
    }
}
