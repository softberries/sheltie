//! Footprints uploaded by CI adapters.

use sheltie_core::{
    ChangeId, CommitSha, DiffHash, Footprint, FootprintReport, RepoId, TargetId, TestId, TreeHash,
};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

/// A stored footprint, as the scheduler reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFootprint {
    /// Merge base the footprint was computed against.
    pub base: CommitSha,
    /// Tree of `base`.
    pub base_tree: TreeHash,
    /// Hash of the diff.
    pub diff_hash: DiffHash,
    /// The sets.
    pub footprint: Footprint,
}

impl Store {
    /// Stores an uploaded footprint for `repo`; returns how many open changes it applies to.
    ///
    /// Changes at the report's head get its diff hash, and their check runs are refreshed.
    /// A later upload for the same head and base tree replaces the earlier one.
    pub async fn save_footprint(
        &self,
        repo: RepoId,
        report: &FootprintReport,
        uploaded_by: &str,
    ) -> Result<u64, StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        let names =
            |v: &mut dyn Iterator<Item = &str>| v.map(str::to_owned).collect::<Vec<String>>();
        let fp = &report.footprint;
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO footprint_reports
                 (repo_id, head_sha, base_sha, base_tree, diff_hash, adapter,
                  writes, reads, tests, leaky, leak_reasons, uploaded_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT (repo_id, head_sha, base_tree) DO UPDATE SET
                 base_sha = EXCLUDED.base_sha, diff_hash = EXCLUDED.diff_hash,
                 adapter = EXCLUDED.adapter, writes = EXCLUDED.writes, reads = EXCLUDED.reads,
                 tests = EXCLUDED.tests, leaky = EXCLUDED.leaky,
                 leak_reasons = EXCLUDED.leak_reasons, uploaded_by = EXCLUDED.uploaded_by,
                 uploaded_at = now()",
        )
        .bind(repo_id)
        .bind(report.head.as_str())
        .bind(report.base.as_str())
        .bind(report.base_tree.as_str())
        .bind(report.diff_hash.as_str())
        .bind(&report.adapter)
        .bind(names(&mut fp.writes.iter().map(TargetId::as_str)))
        .bind(names(&mut fp.reads.iter().map(TargetId::as_str)))
        .bind(names(&mut fp.tests.iter().map(TestId::as_str)))
        .bind(fp.leaky)
        .bind(&report.leak_reasons)
        .bind(uploaded_by)
        .execute(&mut *tx)
        .await?;

        let changes: Vec<i64> = sqlx::query_scalar(
            "UPDATE changes SET diff_hash = $3, updated_at = now()
             WHERE repo_id = $1 AND head_sha = $2 AND state NOT IN ('landed', 'closed')
             RETURNING id",
        )
        .bind(repo_id)
        .bind(report.head.as_str())
        .bind(report.diff_hash.as_str())
        .fetch_all(&mut *tx)
        .await?;
        for change in &changes {
            crate::intake::request_check_run(&mut tx, *change).await?;
            crate::intake::request_readiness(&mut tx, *change).await?;
        }
        tx.commit().await?;
        Ok(changes.len() as u64)
    }

    /// Whether the App is installed (and not suspended) on `repo`.
    pub async fn repository_installed(&self, repo: RepoId) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM repositories r JOIN installations i ON i.id = r.installation_id
                 WHERE r.id = $1 AND i.deleted_at IS NULL AND i.suspended_at IS NULL
             )",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_one(&self.pool)
        .await?)
    }

    /// The most recently uploaded footprint for a change's head, if any.
    pub async fn footprint(
        &self,
        change: &ChangeId,
    ) -> Result<Option<StoredFootprint>, StoreError> {
        let row = sqlx::query(
            "SELECT base_sha, base_tree, diff_hash, writes, reads, tests, leaky
             FROM footprint_reports
             WHERE repo_id = $1 AND head_sha = $2
             ORDER BY uploaded_at DESC
             LIMIT 1",
        )
        .bind(to_db_id(change.repo.0, "repository id")?)
        .bind(change.head.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| footprint_from_row(&r)).transpose()
    }
}

fn footprint_from_row(r: &sqlx::postgres::PgRow) -> Result<StoredFootprint, StoreError> {
    let invalid = |e: String| StoreError::InvalidRow(e);
    let base: String = r.try_get("base_sha")?;
    let tree: String = r.try_get("base_tree")?;
    let writes: Vec<String> = r.try_get("writes")?;
    let reads: Vec<String> = r.try_get("reads")?;
    let tests: Vec<String> = r.try_get("tests")?;
    Ok(StoredFootprint {
        base: CommitSha::parse(base).map_err(|e| invalid(e.to_string()))?,
        base_tree: TreeHash::parse(tree).map_err(|e| invalid(e.to_string()))?,
        diff_hash: DiffHash::new(r.try_get::<String, _>("diff_hash")?),
        footprint: Footprint {
            writes: writes.into_iter().map(TargetId::new).collect(),
            reads: reads.into_iter().map(TargetId::new).collect(),
            tests: tests.into_iter().map(TestId::new).collect(),
            leaky: r.try_get("leaky")?,
        },
    })
}
