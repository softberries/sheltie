//! Inputs and outputs of scheduler epochs.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use sheltie_core::{ChangeId, CommitSha, PrNumber, RepoId, TargetId};
use sqlx::Row;

use crate::footprints::StoredFootprint;
use crate::{Store, StoreError, from_db_id, to_db_id};

/// Merges whose push has not been seen yet count as landed for this long.
const UNSEQUENCED_GRACE_MINUTES: i64 = 10;

/// A ready change with what the scheduler needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpochCandidate {
    /// Database id of the change.
    pub change_db_id: i64,
    /// The change.
    pub change: ChangeId,
    /// Scheduling priority.
    pub priority: i64,
    /// When it became ready.
    pub ready_at: DateTime<Utc>,
    /// Its latest footprint.
    pub footprint: StoredFootprint,
    /// Union of the write sets of changes that landed on the default branch after this
    /// change's base; `None` when that is unknown (base not tracked, or a landed change
    /// has no footprint).
    pub landed_writes: Option<BTreeSet<TargetId>>,
}

/// One change's prediction for an epoch.
#[derive(Debug, Clone, PartialEq)]
pub struct PredictionRow {
    /// Database id of the change.
    pub change_db_id: i64,
    /// Decision label (see `sheltie_scheduler::DECISION_LABELS`).
    pub decision: String,
    /// The decision as JSON.
    pub detail: serde_json::Value,
    /// One line for the check run.
    pub summary: String,
}

struct Landed {
    seq: Option<i64>,
    merged_at: Option<DateTime<Utc>>,
    writes: Option<BTreeSet<TargetId>>,
}

impl Store {
    /// Repositories that have ready changes.
    pub async fn repos_with_ready_changes(&self) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT DISTINCT repo_id FROM changes WHERE state = 'ready' ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await?;
        ids.into_iter()
            .map(|i| from_db_id(i, "repository id").map(RepoId))
            .collect()
    }

    /// The ready changes of `repo` with their footprints and landed write sets.
    pub async fn epoch_candidates(&self, repo: RepoId) -> Result<Vec<EpochCandidate>, StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        let ready = sqlx::query(
            "SELECT c.id, c.pr_number, c.head_sha, c.priority, c.ready_at
             FROM changes c
             WHERE c.repo_id = $1 AND c.state = 'ready'
             ORDER BY c.id",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await?;
        if ready.is_empty() {
            return Ok(Vec::new());
        }

        // Changes that landed on the default branch, with their position on it.
        let landed: Vec<Landed> = sqlx::query(
            "SELECT bc.seq, c.merged_at, fp.writes
             FROM changes c
             JOIN repositories r ON r.id = c.repo_id
             LEFT JOIN branch_commits bc ON bc.repo_id = c.repo_id AND bc.sha = c.merge_commit_sha
             LEFT JOIN LATERAL (
                 SELECT writes FROM footprint_reports f
                 WHERE f.repo_id = c.repo_id AND f.head_sha = c.head_sha
                 ORDER BY f.uploaded_at DESC LIMIT 1
             ) fp ON true
             WHERE c.repo_id = $1 AND c.merge_commit_sha IS NOT NULL
               AND c.base_ref = r.default_branch",
        )
        .bind(repo_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| -> Result<Landed, StoreError> {
            let writes: Option<Vec<String>> = r.try_get("writes")?;
            Ok(Landed {
                seq: r.try_get("seq")?,
                merged_at: r.try_get("merged_at")?,
                writes: writes.map(|w| w.into_iter().map(TargetId::new).collect()),
            })
        })
        .collect::<Result<_, _>>()?;

        let mut out = Vec::new();
        for row in &ready {
            let change = ChangeId {
                repo,
                pr: PrNumber(from_db_id(
                    row.try_get("pr_number")?,
                    "pull request number",
                )?),
                head: CommitSha::parse(row.try_get::<String, _>("head_sha")?)
                    .map_err(|e| StoreError::InvalidRow(e.to_string()))?,
            };
            let Some(footprint) = self.footprint(&change).await? else {
                continue; // readiness requires a footprint; it may have raced with a re-upload
            };
            let base_seq: Option<i64> = sqlx::query_scalar(
                "SELECT seq FROM branch_commits WHERE repo_id = $1 AND sha = $2",
            )
            .bind(repo_id)
            .bind(footprint.base.as_str())
            .fetch_optional(&self.pool)
            .await?;
            out.push(EpochCandidate {
                change_db_id: row.try_get("id")?,
                change,
                priority: row.try_get("priority")?,
                ready_at: row
                    .try_get::<Option<DateTime<Utc>>, _>("ready_at")?
                    .unwrap_or_else(Utc::now),
                landed_writes: landed_since(base_seq, &landed),
                footprint,
            });
        }
        Ok(out)
    }

    /// Stores an epoch's predictions for `repo`. Changes whose prediction changed get a log
    /// entry and a check-run refresh; predictions of changes no longer ready are removed.
    pub async fn record_predictions(
        &self,
        repo: RepoId,
        epoch_at: DateTime<Utc>,
        rows: &[PredictionRow],
    ) -> Result<usize, StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        let mut tx = self.pool.begin().await?;
        let current: BTreeMap<i64, (String, String)> = sqlx::query(
            "SELECT p.change_id, p.decision, p.summary
             FROM predictions p JOIN changes c ON c.id = p.change_id
             WHERE c.repo_id = $1",
        )
        .bind(repo_id)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(|r| {
            Ok((
                r.try_get("change_id")?,
                (r.try_get("decision")?, r.try_get("summary")?),
            ))
        })
        .collect::<Result<_, StoreError>>()?;

        let mut changed = 0;
        for row in rows {
            let same = current
                .get(&row.change_db_id)
                .is_some_and(|(d, s)| *d == row.decision && *s == row.summary);
            sqlx::query(
                "INSERT INTO predictions (change_id, epoch_at, decision, detail, summary)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (change_id) DO UPDATE SET
                     epoch_at = EXCLUDED.epoch_at, decision = EXCLUDED.decision,
                     detail = EXCLUDED.detail, summary = EXCLUDED.summary",
            )
            .bind(row.change_db_id)
            .bind(epoch_at)
            .bind(&row.decision)
            .bind(&row.detail)
            .bind(&row.summary)
            .execute(&mut *tx)
            .await?;
            if !same {
                changed += 1;
                sqlx::query(
                    "INSERT INTO prediction_log (change_id, epoch_at, decision, detail, summary)
                     VALUES ($1, $2, $3, $4, $5)",
                )
                .bind(row.change_db_id)
                .bind(epoch_at)
                .bind(&row.decision)
                .bind(&row.detail)
                .bind(&row.summary)
                .execute(&mut *tx)
                .await?;
                crate::intake::request_check_run(&mut tx, row.change_db_id).await?;
            }
        }
        let keep: Vec<i64> = rows.iter().map(|r| r.change_db_id).collect();
        sqlx::query(
            "DELETE FROM predictions p USING changes c
             WHERE c.id = p.change_id AND c.repo_id = $1 AND NOT (p.change_id = ANY($2))",
        )
        .bind(repo_id)
        .bind(&keep)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(changed)
    }
}

/// Write sets of changes that landed after `base_seq`. Recent merges whose push has not been
/// seen yet count as landed after any base.
fn landed_since(base_seq: Option<i64>, landed: &[Landed]) -> Option<BTreeSet<TargetId>> {
    let base_seq = base_seq?;
    let recent = Utc::now() - chrono::Duration::minutes(UNSEQUENCED_GRACE_MINUTES);
    let mut writes = BTreeSet::new();
    for l in landed {
        let after_base = match l.seq {
            Some(seq) => seq > base_seq,
            None => l.merged_at.is_some_and(|m| m > recent),
        };
        if after_base {
            writes.extend(l.writes.clone()?);
        }
    }
    Some(writes)
}
