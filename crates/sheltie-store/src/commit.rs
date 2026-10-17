//! Merge mode: repository mode, verify runs, landing and returning changes.

use std::collections::BTreeSet;

use sheltie_core::{
    ChangeId, ChangeState, CommitSha, GuaranteeTier, InstallationId, PrNumber, RepoFullName,
    RepoId, TargetId,
};
use sqlx::Row;

use crate::footprints::StoredFootprint;
use crate::{Store, StoreError, from_db_id, to_db_id};

/// Whether Sheltie lands changes in a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepoMode {
    /// Predict only (M3).
    Shadow,
    /// Land changes (M4).
    Merge,
}

impl RepoMode {
    /// The stored text form.
    pub fn as_str(self) -> &'static str {
        match self {
            RepoMode::Shadow => "shadow",
            RepoMode::Merge => "merge",
        }
    }
}

/// A change merged onto the head on a verify branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyRun {
    /// The verify branch.
    pub branch: String,
    /// The merge commit on it.
    pub verify_sha: CommitSha,
    /// The default-branch head it was merged onto.
    pub onto: CommitSha,
}

/// What the committer needs to reach a repository on GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoAccess {
    /// `owner/name`.
    pub full_name: RepoFullName,
    /// Installation whose token to use.
    pub installation: InstallationId,
    /// The branch changes land on.
    pub default_branch: String,
    /// Whether Sheltie lands changes there.
    pub mode: RepoMode,
}

/// A change as the committer sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitterChange {
    /// Database id.
    pub change_db_id: i64,
    /// The change.
    pub change: ChangeId,
    /// Its state.
    pub state: ChangeState,
    /// Its footprint, if uploaded.
    pub footprint: Option<StoredFootprint>,
}

/// Holds a repository's commit lock until dropped. Only one server replica commits to a
/// repository at a time.
pub struct RepoLock {
    _tx: sqlx::Transaction<'static, sqlx::Postgres>,
}

impl std::fmt::Debug for RepoLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RepoLock")
    }
}

impl Store {
    /// Takes the repository's commit lock, or returns `None` if another replica holds it.
    pub async fn try_lock_repo(&self, repo: RepoId) -> Result<Option<RepoLock>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let locked: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("commit:{}", repo.0))
                .fetch_one(&mut *tx)
                .await?;
        Ok(locked.then_some(RepoLock { _tx: tx }))
    }

    /// How to reach a repository, if it is installed and its default branch is known.
    pub async fn repo_access(&self, repo: RepoId) -> Result<Option<RepoAccess>, StoreError> {
        let row = sqlx::query(
            "SELECT r.full_name, r.installation_id, r.default_branch, r.mode
             FROM repositories r JOIN installations i ON i.id = r.installation_id
             WHERE r.id = $1 AND i.deleted_at IS NULL AND i.suspended_at IS NULL
               AND r.default_branch IS NOT NULL",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<RepoAccess, StoreError> {
            let mode: String = r.try_get("mode")?;
            Ok(RepoAccess {
                full_name: RepoFullName::parse(r.try_get::<String, _>("full_name")?)
                    .map_err(|e| StoreError::InvalidRow(e.to_string()))?,
                installation: InstallationId(from_db_id(
                    r.try_get("installation_id")?,
                    "installation id",
                )?),
                default_branch: r.try_get("default_branch")?,
                mode: if mode == "merge" {
                    RepoMode::Merge
                } else {
                    RepoMode::Shadow
                },
            })
        })
        .transpose()
    }

    /// A change by database id, with its footprint.
    pub async fn committer_change(
        &self,
        change_db_id: i64,
    ) -> Result<Option<CommitterChange>, StoreError> {
        let Some(row) =
            sqlx::query("SELECT repo_id, pr_number, head_sha, state FROM changes WHERE id = $1")
                .bind(change_db_id)
                .fetch_optional(&self.pool)
                .await?
        else {
            return Ok(None);
        };
        let invalid = |e: String| StoreError::InvalidRow(e);
        let change = ChangeId {
            repo: RepoId(from_db_id(row.try_get("repo_id")?, "repository id")?),
            pr: PrNumber(from_db_id(
                row.try_get("pr_number")?,
                "pull request number",
            )?),
            head: CommitSha::parse(row.try_get::<String, _>("head_sha")?)
                .map_err(|e| invalid(e.to_string()))?,
        };
        let state: String = row.try_get("state")?;
        Ok(Some(CommitterChange {
            change_db_id,
            state: state
                .parse()
                .map_err(|e: sheltie_core::UnknownVariant| invalid(e.to_string()))?,
            footprint: self.footprint(&change).await?,
            change,
        }))
    }

    /// Repositories in merge mode with changes being verified.
    pub async fn repos_with_verifying(&self) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT c.repo_id FROM changes c JOIN repositories r ON r.id = c.repo_id
             WHERE c.state = 'verifying' AND r.mode = 'merge' ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|i| from_db_id(i, "repository id").map(RepoId))
            .collect()
    }

    /// Sets a repository's mode by name; returns false if Sheltie does not know it.
    pub async fn set_repo_mode(
        &self,
        repo: &RepoFullName,
        mode: RepoMode,
    ) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "UPDATE repositories SET mode = $2, updated_at = now() WHERE full_name = $1",
        )
        .bind(repo.as_str())
        .bind(mode.as_str())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// A repository's mode.
    pub async fn repo_mode(&self, repo: RepoId) -> Result<RepoMode, StoreError> {
        let mode: Option<String> =
            sqlx::query_scalar("SELECT mode FROM repositories WHERE id = $1")
                .bind(to_db_id(repo.0, "repository id")?)
                .fetch_optional(&self.pool)
                .await?;
        Ok(if mode.as_deref() == Some("merge") {
            RepoMode::Merge
        } else {
            RepoMode::Shadow
        })
    }

    /// The default branch of a repository, if known.
    pub async fn default_branch(&self, repo: RepoId) -> Result<Option<String>, StoreError> {
        Ok(sqlx::query_scalar::<_, Option<String>>(
            "SELECT default_branch FROM repositories WHERE id = $1",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_optional(&self.pool)
        .await?
        .flatten())
    }

    /// The union of the write sets of the changes whose merge commits are `commits`, or
    /// `None` if any commit is not a known change's merge commit or its footprint is unknown.
    pub async fn merge_commit_writes(
        &self,
        repo: RepoId,
        commits: &[CommitSha],
    ) -> Result<Option<BTreeSet<TargetId>>, StoreError> {
        if commits.is_empty() {
            return Ok(Some(BTreeSet::new()));
        }
        let shas: Vec<&str> = commits.iter().map(CommitSha::as_str).collect();
        let rows = sqlx::query(
            "SELECT c.merge_commit_sha,
                    (SELECT writes FROM footprint_reports f
                     WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                     ORDER BY f.uploaded_at DESC LIMIT 1) AS writes
             FROM changes c
             WHERE c.repo_id = $1 AND c.merge_commit_sha = ANY($2)",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(&shas)
        .fetch_all(&self.pool)
        .await?;
        let mut found = BTreeSet::new();
        let mut writes = BTreeSet::new();
        for r in rows {
            let sha: String = r.try_get("merge_commit_sha")?;
            let Some(w): Option<Vec<String>> = r.try_get("writes")? else {
                return Ok(None);
            };
            found.insert(sha);
            writes.extend(w.into_iter().map(TargetId::new));
        }
        Ok((found.len() == shas.iter().collect::<BTreeSet<_>>().len()).then_some(writes))
    }

    /// The verify run of a change, if one is in progress.
    pub async fn verify_run(&self, change_db_id: i64) -> Result<Option<VerifyRun>, StoreError> {
        let row = sqlx::query(
            "SELECT branch, verify_sha, onto_sha FROM verify_runs WHERE change_id = $1",
        )
        .bind(change_db_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<VerifyRun, StoreError> {
            let parse =
                |s: String| CommitSha::parse(s).map_err(|e| StoreError::InvalidRow(e.to_string()));
            Ok(VerifyRun {
                branch: r.try_get("branch")?,
                verify_sha: parse(r.try_get("verify_sha")?)?,
                onto: parse(r.try_get("onto_sha")?)?,
            })
        })
        .transpose()
    }

    /// When the change's verify run started, if it has one.
    pub async fn verify_started_at(
        &self,
        change_db_id: i64,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT started_at FROM verify_runs WHERE change_id = $1")
                .bind(change_db_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Records a new verify run (replacing an older one) and moves the change to `verifying`.
    pub async fn start_verify_run(
        &self,
        change_db_id: i64,
        run: &VerifyRun,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO verify_runs (change_id, branch, verify_sha, onto_sha) VALUES ($1, $2, $3, $4)
             ON CONFLICT (change_id) DO UPDATE SET branch = EXCLUDED.branch,
                 verify_sha = EXCLUDED.verify_sha, onto_sha = EXCLUDED.onto_sha, started_at = now()",
        )
        .bind(change_db_id)
        .bind(&run.branch)
        .bind(run.verify_sha.as_str())
        .bind(run.onto.as_str())
        .execute(&mut *tx)
        .await?;
        set_state(&mut tx, change_db_id, "verifying", None).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records that a change landed as `merge_commit` with guarantee `tier`.
    pub async fn mark_landed(
        &self,
        repo: RepoId,
        change_db_id: i64,
        merge_commit: &CommitSha,
        tier: GuaranteeTier,
    ) -> Result<(), StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE changes SET merge_commit_sha = $2, merged_at = now() WHERE id = $1")
            .bind(change_db_id)
            .bind(merge_commit.as_str())
            .execute(&mut *tx)
            .await?;
        set_state(&mut tx, change_db_id, "landed", None).await?;
        sqlx::query(
            "INSERT INTO head_events (repo_id, seq, commit_sha, change_id, tier)
             SELECT $1, coalesce(max(seq), -1) + 1, $2, $3, $4 FROM head_events WHERE repo_id = $1",
        )
        .bind(repo_id)
        .bind(merge_commit.as_str())
        .bind(change_db_id)
        .bind(tier.as_str())
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM verify_runs WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Returns a change to its author with a reason.
    pub async fn mark_returned(&self, change_db_id: i64, reason: &str) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        set_state(&mut tx, change_db_id, "returned", Some(reason)).await?;
        sqlx::query("DELETE FROM verify_runs WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Ends a verify run that has to start over (the head moved too far); back to `ready`.
    pub async fn abandon_verify_run(&self, change_db_id: i64) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM verify_runs WHERE change_id = $1")
            .bind(change_db_id)
            .execute(&mut *tx)
            .await?;
        set_state(&mut tx, change_db_id, "ready", None).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Database ids of the changes of `repo` with a verify run in progress.
    pub async fn verifying_changes(&self, repo: RepoId) -> Result<Vec<i64>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT c.id FROM changes c JOIN verify_runs v ON v.change_id = c.id
             WHERE c.repo_id = $1 AND c.state = 'verifying' ORDER BY c.id",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_all(&self.pool)
        .await?)
    }
}

/// Moves a change to `state` (only from a live state) and refreshes its check run.
async fn set_state(
    conn: &mut sqlx::PgConnection,
    change_db_id: i64,
    state: &str,
    reason: Option<&str>,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE changes SET state = $2, returned_reason = $3, updated_at = now()
         WHERE id = $1 AND state NOT IN ('landed', 'closed')",
    )
    .bind(change_db_id)
    .bind(state)
    .bind(reason)
    .execute(&mut *conn)
    .await?;
    crate::intake::request_check_run(conn, change_db_id).await
}
