//! Coordination (M7): default-branch graphs, issues, agent tokens, leases and issue notices.

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sheltie_core::coordination::first_conflict;
use sheltie_core::intake::{IssueEvent, IssueEventKind};
use sheltie_core::{CommitSha, Footprint, IssueNumber, RepoFullName, RepoId, TargetId};
use sqlx::{PgConnection, Row};

use crate::{Store, StoreError, from_db_id, to_db_id};

/// An issue as last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRow {
    /// Its number.
    pub number: IssueNumber,
    /// Title.
    pub title: String,
    /// Description.
    pub body: String,
    /// Whether it is open.
    pub open: bool,
}

/// An active or released lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// Lease id.
    pub id: i64,
    /// The repository.
    pub repo: RepoId,
    /// The issue it reserves.
    pub issue: IssueNumber,
    /// Who holds it: a token name or a GitHub login.
    pub holder: String,
    /// The reserved footprint (writes and reads).
    pub footprint: Footprint,
    /// When it lapses unless renewed.
    pub expires_at: DateTime<Utc>,
}

/// Result of [`Store::claim_lease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Claim {
    /// A new lease.
    Granted(Lease),
    /// The holder already had it; its expiry moved.
    Renewed(Lease),
    /// Someone else holds this issue.
    Taken(Lease),
    /// Another issue's lease overlaps this footprint.
    Blocked(Lease),
}

/// A comment and label change to post on an issue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueNotice {
    /// Comment body.
    pub comment: Option<String>,
    /// Label to add.
    pub add_label: Option<String>,
    /// Label to remove.
    pub remove_label: Option<String>,
}

/// A notice claimed for sending.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingNotice {
    /// Notice id.
    pub id: i64,
    /// The repository.
    pub repo: RepoId,
    /// The issue.
    pub issue: IssueNumber,
    /// What to post.
    pub notice: IssueNotice,
    /// Attempts so far.
    pub attempts: i32,
}

/// Who waits for an issue (claimed by assignment while blocked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Waiter {
    /// The repository.
    pub repo: RepoId,
    /// The issue.
    pub issue: IssueNumber,
    /// Who was assigned.
    pub holder: String,
    /// Whether the issue was already marked as blocked for them.
    pub notified: bool,
}

fn targets(v: Vec<String>) -> BTreeSet<TargetId> {
    v.into_iter().map(TargetId::new).collect()
}

fn names(set: &BTreeSet<TargetId>) -> Vec<&str> {
    set.iter().map(TargetId::as_str).collect()
}

fn lease_row(r: &sqlx::postgres::PgRow) -> Result<Lease, StoreError> {
    Ok(Lease {
        id: r.try_get("id")?,
        repo: RepoId(from_db_id(r.try_get("repo_id")?, "repository id")?),
        issue: IssueNumber(from_db_id(r.try_get("issue_number")?, "issue number")?),
        holder: r.try_get("holder")?,
        footprint: Footprint {
            writes: targets(r.try_get("writes")?),
            reads: targets(r.try_get("reads")?),
            ..Footprint::default()
        },
        expires_at: r.try_get("expires_at")?,
    })
}

async fn active_leases_in(conn: &mut PgConnection, repo: i64) -> Result<Vec<Lease>, StoreError> {
    let rows = sqlx::query(
        "SELECT id, repo_id, issue_number, holder, writes, reads, expires_at FROM leases
         WHERE repo_id = $1 AND released_at IS NULL AND expires_at > now() ORDER BY id",
    )
    .bind(repo)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(lease_row).collect()
}

/// Queues a notice in the caller's transaction.
pub(crate) async fn enqueue_notice(
    conn: &mut PgConnection,
    repo: i64,
    issue: i64,
    notice: &IssueNotice,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO issue_notices (repo_id, issue_number, comment, add_label, remove_label)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(repo)
    .bind(issue)
    .bind(notice.comment.as_deref())
    .bind(notice.add_label.as_deref())
    .bind(notice.remove_label.as_deref())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Applies an issue event in the intake transaction: keeps the newest title, description and
/// open state; an assignee leaving drops their waiting claim.
pub(crate) async fn apply_issue_event(
    conn: &mut PgConnection,
    e: &IssueEvent,
) -> Result<(), StoreError> {
    let repo = to_db_id(e.repo.id.0, "repository id")?;
    let issue = to_db_id(e.issue.0, "issue number")?;
    let open = !matches!(e.kind, IssueEventKind::Closed);
    sqlx::query(
        "INSERT INTO issues (repo_id, number, title, body, open, event_at) VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (repo_id, number) DO UPDATE
         SET title = EXCLUDED.title, body = EXCLUDED.body, open = EXCLUDED.open, event_at = EXCLUDED.event_at
         WHERE issues.event_at <= EXCLUDED.event_at",
    )
    .bind(repo)
    .bind(issue)
    .bind(&e.title)
    .bind(&e.body)
    .bind(open)
    .bind(e.at)
    .execute(&mut *conn)
    .await?;
    match &e.kind {
        IssueEventKind::Assigned(who) => {
            sqlx::query(
                "INSERT INTO lease_waits (repo_id, issue_number, holder) VALUES ($1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(repo)
            .bind(issue)
            .bind(who.as_str())
            .execute(&mut *conn)
            .await?;
        }
        IssueEventKind::Unassigned(who) => {
            sqlx::query(
                "DELETE FROM lease_waits WHERE repo_id = $1 AND issue_number = $2 AND holder = $3",
            )
            .bind(repo)
            .bind(issue)
            .bind(who.as_str())
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "UPDATE leases SET released_at = now(), released_reason = 'unassigned'
                 WHERE repo_id = $1 AND issue_number = $2 AND holder = $3 AND released_at IS NULL",
            )
            .bind(repo)
            .bind(issue)
            .bind(who.as_str())
            .execute(&mut *conn)
            .await?;
        }
        IssueEventKind::Closed => {
            sqlx::query("DELETE FROM lease_waits WHERE repo_id = $1 AND issue_number = $2")
                .bind(repo)
                .bind(issue)
                .execute(&mut *conn)
                .await?;
        }
        IssueEventKind::Opened | IssueEventKind::Edited | IssueEventKind::Reopened => {}
    }
    Ok(())
}

/// Replaces the issues a pull request closes, in the intake transaction.
pub(crate) async fn set_pr_closes(
    conn: &mut PgConnection,
    repo: i64,
    pr: i64,
    issues: &BTreeSet<IssueNumber>,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM pr_closes WHERE repo_id = $1 AND pr_number = $2")
        .bind(repo)
        .bind(pr)
        .execute(&mut *conn)
        .await?;
    for issue in issues.iter().take(50) {
        sqlx::query("INSERT INTO pr_closes (repo_id, pr_number, issue_number) VALUES ($1, $2, $3)")
            .bind(repo)
            .bind(pr)
            .bind(to_db_id(issue.0, "issue number")?)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

impl Store {
    /// Stores the default branch's build graph (as uploaded JSON), replacing the previous one.
    pub async fn save_graph(
        &self,
        repo: RepoId,
        commit: &CommitSha,
        snapshot: &serde_json::Value,
        uploaded_by: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO graph_snapshots (repo_id, commit_sha, snapshot, uploaded_by) VALUES ($1, $2, $3, $4)
             ON CONFLICT (repo_id) DO UPDATE SET commit_sha = EXCLUDED.commit_sha,
                 snapshot = EXCLUDED.snapshot, uploaded_by = EXCLUDED.uploaded_by, uploaded_at = now()",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(commit.as_str())
        .bind(snapshot)
        .bind(uploaded_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The latest default-branch graph of a repository.
    pub async fn graph(
        &self,
        repo: RepoId,
    ) -> Result<Option<(CommitSha, serde_json::Value)>, StoreError> {
        let row =
            sqlx::query("SELECT commit_sha, snapshot FROM graph_snapshots WHERE repo_id = $1")
                .bind(to_db_id(repo.0, "repository id")?)
                .fetch_optional(&self.pool)
                .await?;
        row.map(|r| -> Result<_, StoreError> {
            let sha: String = r.try_get("commit_sha")?;
            Ok((
                CommitSha::parse(sha).map_err(|e| StoreError::InvalidRow(e.to_string()))?,
                r.try_get("snapshot")?,
            ))
        })
        .transpose()
    }

    /// One issue.
    pub async fn issue(
        &self,
        repo: RepoId,
        issue: IssueNumber,
    ) -> Result<Option<IssueRow>, StoreError> {
        let row = sqlx::query(
            "SELECT number, title, body, open FROM issues WHERE repo_id = $1 AND number = $2",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(issue.0, "issue number")?)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(issue_row).transpose()
    }

    /// The lowest-numbered issue a pull request's description closes, as Sheltie last saw it
    /// (only issues Sheltie has received events for).
    pub async fn closed_issue(
        &self,
        repo: RepoId,
        pr: sheltie_core::PrNumber,
    ) -> Result<Option<IssueRow>, StoreError> {
        let row = sqlx::query(
            "SELECT i.number, i.title, i.body, i.open FROM pr_closes p
             JOIN issues i ON i.repo_id = p.repo_id AND i.number = p.issue_number
             WHERE p.repo_id = $1 AND p.pr_number = $2
             ORDER BY p.issue_number LIMIT 1",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(pr.0, "pull request number")?)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(issue_row).transpose()
    }

    /// Open issues, oldest first.
    pub async fn open_issues(&self, repo: RepoId, limit: i64) -> Result<Vec<IssueRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT number, title, body, open FROM issues WHERE repo_id = $1 AND open
             ORDER BY number LIMIT $2",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(issue_row).collect()
    }

    /// Creates a token for an installed repository; `None` if the repository is unknown.
    pub async fn create_token(
        &self,
        repo: &RepoFullName,
        name: &str,
        token_hash: &[u8; 32],
    ) -> Result<Option<RepoId>, StoreError> {
        let id: Option<i64> = sqlx::query_scalar(
            "INSERT INTO api_tokens (repo_id, name, token_hash)
             SELECT id, $2, $3 FROM repositories WHERE lower(full_name) = lower($1)
             RETURNING repo_id",
        )
        .bind(repo.as_str())
        .bind(name)
        .bind(token_hash.as_slice())
        .fetch_optional(&self.pool)
        .await?;
        id.map(|i| from_db_id(i, "repository id").map(RepoId))
            .transpose()
    }

    /// Revokes a repository's tokens with this name; returns how many.
    pub async fn revoke_token(&self, repo: &RepoFullName, name: &str) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE api_tokens SET revoked_at = now()
             WHERE name = $2 AND revoked_at IS NULL
               AND repo_id = (SELECT id FROM repositories WHERE lower(full_name) = lower($1))",
        )
        .bind(repo.as_str())
        .bind(name)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// The repository and name of a live token.
    pub async fn token(
        &self,
        token_hash: &[u8; 32],
    ) -> Result<Option<(RepoId, RepoFullName, String)>, StoreError> {
        let row = sqlx::query(
            "SELECT t.repo_id, r.full_name, t.name FROM api_tokens t JOIN repositories r ON r.id = t.repo_id
             WHERE t.token_hash = $1 AND t.revoked_at IS NULL",
        )
        .bind(token_hash.as_slice())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<_, StoreError> {
            Ok((
                RepoId(from_db_id(r.try_get("repo_id")?, "repository id")?),
                RepoFullName::parse(r.try_get::<String, _>("full_name")?)
                    .map_err(|e| StoreError::InvalidRow(e.to_string()))?,
                r.try_get("name")?,
            ))
        })
        .transpose()
    }

    /// Claims an issue for `holder` with its predicted footprint, atomically per repository:
    /// granted only if no other active lease conflicts with it.
    pub async fn claim_lease(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        holder: &str,
        footprint: &Footprint,
        ttl: Duration,
    ) -> Result<Claim, StoreError> {
        let repo_db = to_db_id(repo.0, "repository id")?;
        let issue_db = to_db_id(issue.0, "issue number")?;
        let ttl =
            chrono::Duration::from_std(ttl).map_err(|e| StoreError::InvalidRow(e.to_string()))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("leases:{repo_db}"))
            .execute(&mut *tx)
            .await?;
        let active = active_leases_in(&mut tx, repo_db).await?;
        if let Some(mine) = active.iter().find(|l| l.issue == issue) {
            if mine.holder != holder {
                return Ok(Claim::Taken(mine.clone()));
            }
            let row = sqlx::query("UPDATE leases SET expires_at = now() + $2 WHERE id = $1 RETURNING id, repo_id, issue_number, holder, writes, reads, expires_at")
            .bind(mine.id)
            .bind(ttl)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(Claim::Renewed(lease_row(&row)?));
        }
        let held: Vec<(usize, Footprint)> = active
            .iter()
            .enumerate()
            .map(|(i, l)| (i, l.footprint.clone()))
            .collect();
        if let Some(&i) = first_conflict(footprint, &held) {
            return Ok(Claim::Blocked(active[i].clone()));
        }
        // An expired lease on this issue is closed before the new one takes its place.
        sqlx::query(
            "UPDATE leases SET released_at = now(), released_reason = 'expired'
             WHERE repo_id = $1 AND issue_number = $2 AND released_at IS NULL",
        )
        .bind(repo_db)
        .bind(issue_db)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query("INSERT INTO leases (repo_id, issue_number, holder, writes, reads, expires_at)
             VALUES ($1, $2, $3, $4, $5, now() + $6) RETURNING id, repo_id, issue_number, holder, writes, reads, expires_at")
        .bind(repo_db)
        .bind(issue_db)
        .bind(holder)
        .bind(names(&footprint.writes))
        .bind(names(&footprint.reads))
        .bind(ttl)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM lease_waits WHERE repo_id = $1 AND issue_number = $2")
            .bind(repo_db)
            .bind(issue_db)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Claim::Granted(lease_row(&row)?))
    }

    /// Releases the active lease on an issue; with `holder`, only if they hold it.
    pub async fn release_lease(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        holder: Option<&str>,
        reason: &str,
    ) -> Result<Option<Lease>, StoreError> {
        let row = sqlx::query(
            "UPDATE leases SET released_at = now(), released_reason = $4
             WHERE repo_id = $1 AND issue_number = $2 AND released_at IS NULL
               AND ($3::text IS NULL OR holder = $3)
             RETURNING id, repo_id, issue_number, holder, writes, reads, expires_at",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(issue.0, "issue number")?)
        .bind(holder)
        .bind(reason)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(lease_row).transpose()
    }

    /// Active leases of a repository.
    pub async fn active_leases(&self, repo: RepoId) -> Result<Vec<Lease>, StoreError> {
        let mut conn = self.pool.acquire().await?;
        active_leases_in(&mut conn, to_db_id(repo.0, "repository id")?).await
    }

    /// Releases leases that are done: expired, their issue closed, or a pull request that
    /// closes the issue landed or was merged. Returns them.
    pub async fn release_finished_leases(&self) -> Result<Vec<(Lease, String)>, StoreError> {
        let rows = sqlx::query("WITH done AS (
                SELECT l.id,
                    CASE
                        WHEN l.expires_at <= now() THEN 'expired'
                        WHEN EXISTS (SELECT 1 FROM issues i WHERE i.repo_id = l.repo_id
                                     AND i.number = l.issue_number AND NOT i.open) THEN 'issue closed'
                        ELSE 'pull request landed'
                    END AS reason
                FROM leases l
                WHERE l.released_at IS NULL AND (
                    l.expires_at <= now()
                    OR EXISTS (SELECT 1 FROM issues i WHERE i.repo_id = l.repo_id
                               AND i.number = l.issue_number AND NOT i.open)
                    OR EXISTS (SELECT 1 FROM pr_closes p JOIN changes c
                                 ON c.repo_id = p.repo_id AND c.pr_number = p.pr_number
                               WHERE p.repo_id = l.repo_id AND p.issue_number = l.issue_number
                                 AND (c.state = 'landed' OR c.merged_at IS NOT NULL)))
            )
            UPDATE leases l SET released_at = now(), released_reason = done.reason
            FROM done WHERE l.id = done.id
            RETURNING l.id, l.repo_id, l.issue_number, l.holder, l.writes, l.reads, l.expires_at, done.reason")
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((lease_row(r)?, r.try_get::<String, _>("reason")?)))
            .collect()
    }

    /// Records a claim by assignment for the coordinator to process.
    pub async fn add_waiter(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        holder: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO lease_waits (repo_id, issue_number, holder) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(to_db_id(issue.0, "issue number")?)
        .bind(holder)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Waiting claims across repositories, oldest first.
    pub async fn waiters(&self) -> Result<Vec<Waiter>, StoreError> {
        let rows = sqlx::query(
            "SELECT repo_id, issue_number, holder, notified FROM lease_waits
             ORDER BY created_at, repo_id, issue_number, holder",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Waiter {
                    repo: RepoId(from_db_id(r.try_get("repo_id")?, "repository id")?),
                    issue: IssueNumber(from_db_id(r.try_get("issue_number")?, "issue number")?),
                    holder: r.try_get("holder")?,
                    notified: r.try_get("notified")?,
                })
            })
            .collect()
    }

    /// Ends a waiting claim; with `notice`, posts it in the same transaction.
    pub async fn remove_waiter(
        &self,
        w: &Waiter,
        notice: Option<&IssueNotice>,
    ) -> Result<(), StoreError> {
        let (repo, issue) = (
            to_db_id(w.repo.0, "repository id")?,
            to_db_id(w.issue.0, "issue number")?,
        );
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM lease_waits WHERE repo_id = $1 AND issue_number = $2 AND holder = $3",
        )
        .bind(repo)
        .bind(issue)
        .bind(&w.holder)
        .execute(&mut *tx)
        .await?;
        if let Some(notice) = notice {
            enqueue_notice(&mut tx, repo, issue, notice).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Marks a waiting claim as blocked and posts `notice`, once.
    pub async fn waiter_blocked(&self, w: &Waiter, notice: &IssueNotice) -> Result<(), StoreError> {
        let (repo, issue) = (
            to_db_id(w.repo.0, "repository id")?,
            to_db_id(w.issue.0, "issue number")?,
        );
        let mut tx = self.pool.begin().await?;
        let first = sqlx::query(
            "UPDATE lease_waits SET notified = true
             WHERE repo_id = $1 AND issue_number = $2 AND holder = $3 AND NOT notified",
        )
        .bind(repo)
        .bind(issue)
        .bind(&w.holder)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if first > 0 {
            enqueue_notice(&mut tx, repo, issue, notice).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Open issues not yet given a footprint note, in repositories that have a graph.
    pub async fn issues_to_note(&self, limit: i64) -> Result<Vec<(RepoId, IssueRow)>, StoreError> {
        let rows = sqlx::query(
            "SELECT i.repo_id, i.number, i.title, i.body, i.open FROM issues i
             JOIN graph_snapshots g ON g.repo_id = i.repo_id
             WHERE i.open AND NOT i.noted ORDER BY i.repo_id, i.number LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    RepoId(from_db_id(r.try_get("repo_id")?, "repository id")?),
                    issue_row(r)?,
                ))
            })
            .collect()
    }

    /// Marks an issue as noted and posts `notice`, if any.
    pub async fn issue_noted(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        notice: Option<&IssueNotice>,
    ) -> Result<(), StoreError> {
        let (repo, issue) = (
            to_db_id(repo.0, "repository id")?,
            to_db_id(issue.0, "issue number")?,
        );
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE issues SET noted = true WHERE repo_id = $1 AND number = $2")
            .bind(repo)
            .bind(issue)
            .execute(&mut *tx)
            .await?;
        if let Some(notice) = notice {
            enqueue_notice(&mut tx, repo, issue, notice).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Queues a comment or label change on an issue.
    pub async fn queue_notice(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        notice: &IssueNotice,
    ) -> Result<(), StoreError> {
        let mut conn = self.pool.acquire().await?;
        enqueue_notice(
            &mut conn,
            to_db_id(repo.0, "repository id")?,
            to_db_id(issue.0, "issue number")?,
            notice,
        )
        .await
    }

    /// Claims due notices for `lease`, oldest first.
    pub async fn claim_notices(
        &self,
        limit: i64,
        lease: Duration,
    ) -> Result<Vec<PendingNotice>, StoreError> {
        let lease =
            chrono::Duration::from_std(lease).map_err(|e| StoreError::InvalidRow(e.to_string()))?;
        let rows = sqlx::query(
            "UPDATE issue_notices SET leased_until = now() + $2, attempts = attempts + 1
             WHERE id IN (
                SELECT id FROM issue_notices
                WHERE sent_at IS NULL AND next_attempt_at <= now()
                  AND (leased_until IS NULL OR leased_until < now())
                ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
             RETURNING id, repo_id, issue_number, comment, add_label, remove_label, attempts",
        )
        .bind(limit)
        .bind(lease)
        .fetch_all(&self.pool)
        .await?;
        let mut notices: Vec<PendingNotice> = rows
            .iter()
            .map(|r| {
                Ok(PendingNotice {
                    id: r.try_get("id")?,
                    repo: RepoId(from_db_id(r.try_get("repo_id")?, "repository id")?),
                    issue: IssueNumber(from_db_id(r.try_get("issue_number")?, "issue number")?),
                    notice: IssueNotice {
                        comment: r.try_get("comment")?,
                        add_label: r.try_get("add_label")?,
                        remove_label: r.try_get("remove_label")?,
                    },
                    attempts: r.try_get("attempts")?,
                })
            })
            .collect::<Result<_, StoreError>>()?;
        notices.sort_by_key(|n| n.id);
        Ok(notices)
    }

    /// Marks a notice as sent.
    pub async fn notice_sent(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE issue_notices SET sent_at = now(), leased_until = NULL WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Schedules a failed notice for another attempt after `backoff`.
    pub async fn notice_failed(&self, id: i64, backoff: Duration) -> Result<(), StoreError> {
        let backoff = chrono::Duration::from_std(backoff)
            .map_err(|e| StoreError::InvalidRow(e.to_string()))?;
        sqlx::query(
            "UPDATE issue_notices SET leased_until = NULL, next_attempt_at = now() + $2 WHERE id = $1",
        )
        .bind(id)
        .bind(backoff)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn issue_row(r: &sqlx::postgres::PgRow) -> Result<IssueRow, StoreError> {
    Ok(IssueRow {
        number: IssueNumber(from_db_id(r.try_get("number")?, "issue number")?),
        title: r.try_get("title")?,
        body: r.try_get("body")?,
        open: r.try_get("open")?,
    })
}
