//! Sweeps: the verified prefix, culprits and reverts.

use std::collections::HashMap;

use sheltie_core::{CommitSha, GuaranteeTier, PrNumber, RepoId};
use sqlx::Row;

use crate::reality::CiState;
use crate::{Store, StoreError, from_db_id, to_db_id};

/// A default-branch tip with its CI state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tip {
    /// The commit.
    pub sha: CommitSha,
    /// Its position on the branch.
    pub seq: i64,
    /// Its CI state, if known.
    pub ci: Option<CiState>,
}

/// A blamed commit and how its revert is going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Culprit {
    /// The blamed commit.
    pub culprit: CommitSha,
    /// The red tip that was analysed.
    pub red: CommitSha,
    /// `reverting`, `reverted` or `conflict`.
    pub state: String,
    /// The revert pull request, once opened.
    pub revert_pr: Option<PrNumber>,
}

/// The change a default-branch commit landed, if Sheltie knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandedChange {
    /// Database id.
    pub change_db_id: i64,
    /// Pull request.
    pub pr: PrNumber,
    /// The guarantee it landed with, if Sheltie landed it.
    pub tier: Option<GuaranteeTier>,
}

fn sha(s: String) -> Result<CommitSha, StoreError> {
    CommitSha::parse(s).map_err(|e| StoreError::InvalidRow(e.to_string()))
}

fn ci(s: Option<String>) -> Option<CiState> {
    match s.as_deref() {
        Some("success") => Some(CiState::Success),
        Some("failure") => Some(CiState::Failure),
        Some("error") => Some(CiState::Error),
        Some("pending") => Some(CiState::Pending),
        Some("none") => Some(CiState::None),
        _ => None,
    }
}

impl Store {
    /// Repositories in merge mode.
    pub async fn repos_in_merge_mode(&self) -> Result<Vec<RepoId>, StoreError> {
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT id FROM repositories WHERE mode = 'merge' ORDER BY id")
                .fetch_all(&self.pool)
                .await?;
        ids.into_iter()
            .map(|i| from_db_id(i, "repository id").map(RepoId))
            .collect()
    }

    /// The newest pushed tip of the default branch.
    pub async fn newest_tip(&self, repo: RepoId) -> Result<Option<Tip>, StoreError> {
        self.tip(repo, "SELECT sha, seq, ci_state FROM branch_commits WHERE repo_id = $1 AND is_tip ORDER BY seq DESC LIMIT 1").await
    }

    /// The newest tip whose CI passed.
    pub async fn newest_green_tip(&self, repo: RepoId) -> Result<Option<Tip>, StoreError> {
        self.tip(
            repo,
            "SELECT sha, seq, ci_state FROM branch_commits
             WHERE repo_id = $1 AND is_tip AND ci_state = 'success' ORDER BY seq DESC LIMIT 1",
        )
        .await
    }

    async fn tip(&self, repo: RepoId, sql: &'static str) -> Result<Option<Tip>, StoreError> {
        let row = sqlx::query(sql)
            .bind(to_db_id(repo.0, "repository id")?)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| -> Result<Tip, StoreError> {
            Ok(Tip {
                sha: sha(r.try_get("sha")?)?,
                seq: r.try_get("seq")?,
                ci: ci(r.try_get("ci_state")?),
            })
        })
        .transpose()
    }

    /// Known CI states of default-branch commits.
    pub async fn ci_states(
        &self,
        repo: RepoId,
        commits: &[CommitSha],
    ) -> Result<HashMap<CommitSha, CiState>, StoreError> {
        let shas: Vec<&str> = commits.iter().map(CommitSha::as_str).collect();
        let rows = sqlx::query("SELECT sha, ci_state FROM branch_commits WHERE repo_id = $1 AND sha = ANY($2) AND ci_state IS NOT NULL")
            .bind(to_db_id(repo.0, "repository id")?)
            .bind(&shas)
            .fetch_all(&self.pool)
            .await?;
        let mut out = HashMap::new();
        for r in rows {
            if let Some(state) = ci(r.try_get("ci_state")?) {
                out.insert(sha(r.try_get("sha")?)?, state);
            }
        }
        Ok(out)
    }

    /// Records the verified prefix; returns whether it changed. It only moves forward along the
    /// branch: a re-run of CI on a newer commit must not send it back to an older one.
    pub async fn set_verified(&self, repo: RepoId, commit: &CommitSha) -> Result<bool, StoreError> {
        let n = sqlx::query(
            "UPDATE repositories r SET verified_sha = $2, updated_at = now()
             WHERE r.id = $1 AND r.verified_sha IS DISTINCT FROM $2
               AND NOT EXISTS (
                   SELECT 1 FROM branch_commits old, branch_commits new
                   WHERE old.repo_id = r.id AND old.sha = r.verified_sha
                     AND new.repo_id = r.id AND new.sha = $2 AND new.seq < old.seq)",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(commit.as_str())
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// The most recently blamed commit of a repository.
    pub async fn latest_culprit(&self, repo: RepoId) -> Result<Option<(Culprit, i64)>, StoreError> {
        let row = sqlx::query(
            "SELECT c.culprit_sha, c.red_sha, c.state, c.revert_pr, coalesce(bc.seq, 0) AS red_seq
             FROM culprits c
             LEFT JOIN branch_commits bc ON bc.repo_id = c.repo_id AND bc.sha = c.red_sha
             WHERE c.repo_id = $1 ORDER BY c.created_at DESC LIMIT 1",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<(Culprit, i64), StoreError> {
            let pr: Option<i64> = r.try_get("revert_pr")?;
            Ok((
                Culprit {
                    culprit: sha(r.try_get("culprit_sha")?)?,
                    red: sha(r.try_get("red_sha")?)?,
                    state: r.try_get("state")?,
                    revert_pr: pr
                        .map(|p| from_db_id(p, "pull request number").map(PrNumber))
                        .transpose()?,
                },
                r.try_get("red_seq")?,
            ))
        })
        .transpose()
    }

    /// Records or updates a culprit.
    pub async fn record_culprit(
        &self,
        repo: RepoId,
        culprit: &CommitSha,
        red: &CommitSha,
        state: &str,
        revert_sha: Option<&CommitSha>,
        revert_pr: Option<PrNumber>,
    ) -> Result<(), StoreError> {
        let repo_id = to_db_id(repo.0, "repository id")?;
        let change: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM changes WHERE repo_id = $1 AND merge_commit_sha = $2 LIMIT 1",
        )
        .bind(repo_id)
        .bind(culprit.as_str())
        .fetch_optional(&self.pool)
        .await?;
        sqlx::query(
            "INSERT INTO culprits (repo_id, culprit_sha, red_sha, change_id, state, revert_sha, revert_pr)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (repo_id, culprit_sha) DO UPDATE SET
                 red_sha = EXCLUDED.red_sha, state = EXCLUDED.state,
                 revert_sha = coalesce(EXCLUDED.revert_sha, culprits.revert_sha),
                 revert_pr = coalesce(EXCLUDED.revert_pr, culprits.revert_pr),
                 updated_at = now()",
        )
        .bind(repo_id)
        .bind(culprit.as_str())
        .bind(red.as_str())
        .bind(change)
        .bind(state)
        .bind(revert_sha.map(CommitSha::as_str))
        .bind(revert_pr.map(|p| to_db_id(p.0, "pull request number")).transpose()?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The change a default-branch commit landed, with its guarantee tier if Sheltie landed it.
    pub async fn change_by_merge_commit(
        &self,
        repo: RepoId,
        commit: &CommitSha,
    ) -> Result<Option<LandedChange>, StoreError> {
        let row = sqlx::query(
            "SELECT c.id, c.pr_number, h.tier
             FROM changes c LEFT JOIN head_events h ON h.change_id = c.id
             WHERE c.repo_id = $1 AND c.merge_commit_sha = $2 LIMIT 1",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(commit.as_str())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| -> Result<LandedChange, StoreError> {
            let tier: Option<String> = r.try_get("tier")?;
            Ok(LandedChange {
                change_db_id: r.try_get("id")?,
                pr: PrNumber(from_db_id(r.try_get("pr_number")?, "pull request number")?),
                tier: tier
                    .map(|t| {
                        t.parse().map_err(|e: sheltie_core::UnknownVariant| {
                            StoreError::InvalidRow(e.to_string())
                        })
                    })
                    .transpose()?,
            })
        })
        .transpose()
    }

    /// Marks the suspected misses involving a blamed change as confirmed; returns how many.
    pub async fn confirm_misses(&self, repo: RepoId, change_db_id: i64) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE footprint_misses SET confirmed = true
             WHERE repo_id = $1 AND (change_a = $2 OR change_b = $2) AND NOT confirmed",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(change_db_id)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }
}
