//! Applying forge events: one transaction per webhook delivery.

use chrono::{DateTime, Utc};
use sheltie_core::intake::{
    ChecksEvent, InstallationEvent, Intake, IntentField, Lifecycle, PrEvent, PrEventKind,
    PushEvent, RepoRef,
};
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
            Intake::Checks(e) => apply_checks_event(&mut tx, e).await?,
            Intake::Push(e) => apply_push_event(&mut tx, e).await?,
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
    if let Some(branch) = &e.default_branch {
        set_default_branch(conn, repo_id, branch).await?;
    }

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
    if let Some(merge_commit) = &e.merge_commit {
        sqlx::query(
            "UPDATE changes SET merge_commit_sha = $4, merged_at = $5, updated_at = now()
             WHERE repo_id = $1 AND pr_number = $2 AND head_sha = $3",
        )
        .bind(repo_id)
        .bind(pr)
        .bind(e.head.as_str())
        .bind(merge_commit.as_str())
        .bind(e.at)
        .execute(&mut *conn)
        .await?;
    }
    // Keep the latest intent of the PR's live changes (the description belongs to the PR).
    let (intent, intent_error) = match &e.intent {
        IntentField::Unknown => (None, None),
        IntentField::Missing => (Some(serde_json::Value::Null), None),
        IntentField::Valid(i) => (
            Some(serde_json::to_value(i).map_err(|e| StoreError::InvalidRow(e.to_string()))?),
            None,
        ),
        IntentField::Invalid(why) => (
            Some(serde_json::Value::Null),
            Some(why.chars().take(500).collect::<String>()),
        ),
    };
    if let Some(intent) = intent {
        let intent = (!intent.is_null()).then_some(intent);
        sqlx::query(
            "UPDATE changes SET intent = $3, intent_error = $4, updated_at = now()
             WHERE repo_id = $1 AND pr_number = $2 AND state NOT IN ('landed', 'closed')",
        )
        .bind(repo_id)
        .bind(pr)
        .bind(intent)
        .bind(intent_error)
        .execute(&mut *conn)
        .await?;
    }
    if e.kind.lifecycle() == Lifecycle::Record {
        // Reviews and draft changes affect readiness.
        request_readiness_at(conn, repo_id, e.head.as_str()).await?;
    }

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
        "INSERT INTO changes (repo_id, pr_number, head_sha, base_sha, state, event_at, base_ref)
         VALUES ($1, $2, $3, $4, $5, $6, $8)
         ON CONFLICT (repo_id, pr_number, head_sha) DO UPDATE
             SET base_sha = EXCLUDED.base_sha,
                 base_ref = EXCLUDED.base_ref,
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
    .bind(&e.base_ref)
    .fetch_one(&mut *conn)
    .await?;
    request_check_run(conn, change_id).await?;
    request_readiness(conn, change_id).await
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

/// Marks a change's readiness as needing a fresh look at GitHub.
pub(crate) async fn request_readiness(
    conn: &mut PgConnection,
    change_id: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO readiness (change_id) VALUES ($1)
         ON CONFLICT (change_id) DO UPDATE
             SET desired_version = readiness.desired_version + 1,
                 attempts = 0,
                 next_attempt_at = now()",
    )
    .bind(change_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Requests readiness for every live change of `repo_id` at `head`.
pub(crate) async fn request_readiness_at(
    conn: &mut PgConnection,
    repo_id: i64,
    head: &str,
) -> Result<(), StoreError> {
    let changes: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM changes
         WHERE repo_id = $1 AND head_sha = $2 AND state NOT IN ('landed', 'closed')",
    )
    .bind(repo_id)
    .bind(head)
    .fetch_all(&mut *conn)
    .await?;
    for change in changes {
        request_readiness(conn, change).await?;
    }
    Ok(())
}

async fn set_default_branch(
    conn: &mut PgConnection,
    repo_id: i64,
    branch: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE repositories SET default_branch = $2, updated_at = now()
         WHERE id = $1 AND default_branch IS DISTINCT FROM $2",
    )
    .bind(repo_id)
    .bind(branch)
    .execute(conn)
    .await?;
    Ok(())
}

async fn apply_checks_event(conn: &mut PgConnection, e: &ChecksEvent) -> Result<(), StoreError> {
    let repo_id = to_db_id(e.repo.id.0, "repository id")?;
    upsert_installation(conn, e.installation, &Login::new(e.repo.full_name.owner())).await?;
    upsert_repo(conn, &e.repo, Some(e.installation)).await?;
    // A default-branch commit's CI changed: look again (a re-run may have turned it green).
    sqlx::query(
        "UPDATE branch_commits SET ci_state = NULL, ci_next_at = now()
         WHERE repo_id = $1 AND sha = $2",
    )
    .bind(repo_id)
    .bind(e.head.as_str())
    .execute(&mut *conn)
    .await?;
    request_readiness_at(conn, repo_id, e.head.as_str()).await
}

/// Records commits pushed to the default branch, in order, so the scheduler knows what
/// landed after a change's base. Pushes to other branches are ignored.
async fn apply_push_event(conn: &mut PgConnection, e: &PushEvent) -> Result<(), StoreError> {
    let repo_id = to_db_id(e.repo.id.0, "repository id")?;
    upsert_installation(conn, e.installation, &Login::new(e.repo.full_name.owner())).await?;
    upsert_repo(conn, &e.repo, Some(e.installation)).await?;
    set_default_branch(conn, repo_id, &e.default_branch).await?;
    if e.branch != e.default_branch {
        return Ok(());
    }
    let mut commits: Vec<&str> = e.commits.iter().map(|c| c.as_str()).collect();
    if commits.last() != Some(&e.after.as_str()) {
        commits.push(e.after.as_str());
    }
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("branch:{repo_id}"))
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO branch_commits (repo_id, sha, seq)
         SELECT $1, c.sha,
                (SELECT coalesce(max(seq), 0) FROM branch_commits WHERE repo_id = $1) + c.ord
         FROM unnest($2::text[]) WITH ORDINALITY AS c(sha, ord)
         WHERE NOT EXISTS (SELECT 1 FROM branch_commits b WHERE b.repo_id = $1 AND b.sha = c.sha)",
    )
    .bind(repo_id)
    .bind(&commits)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE branch_commits SET is_tip = true WHERE repo_id = $1 AND sha = $2")
        .bind(repo_id)
        .bind(e.after.as_str())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Marks the change's check run as needing a sync with GitHub.
pub(crate) async fn request_check_run(
    conn: &mut PgConnection,
    change_id: i64,
) -> Result<(), StoreError> {
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
