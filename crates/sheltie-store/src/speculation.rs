//! Speculation groups: changes built as prefixes on one head.

use sheltie_core::{CommitSha, RepoId};
use sqlx::Row;

use crate::{Store, StoreError, to_db_id};

/// A group being built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecGroup {
    /// Group id.
    pub id: i64,
    /// Member changes (database ids), in build order.
    pub members: Vec<i64>,
    /// The head the group is built on.
    pub onto: CommitSha,
    /// The merge commit after each member, in order.
    pub member_shas: Vec<CommitSha>,
    /// The prefix builds, by ascending length.
    pub builds: Vec<SpecBuild>,
}

/// One prefix build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecBuild {
    /// How many members it contains.
    pub length: usize,
    /// The branch the repository's CI runs on.
    pub branch: String,
    /// The prefix's head commit.
    pub sha: CommitSha,
}

fn sha(s: String) -> Result<CommitSha, StoreError> {
    CommitSha::parse(s).map_err(|e| StoreError::InvalidRow(e.to_string()))
}

impl Store {
    /// Creates a group and moves its members to `verifying`.
    pub async fn create_spec_group(
        &self,
        repo: RepoId,
        members: &[i64],
        onto: &CommitSha,
    ) -> Result<i64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO spec_groups (repo_id, members, onto_sha) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(to_db_id(repo.0, "repository id")?)
        .bind(members)
        .bind(onto.as_str())
        .fetch_one(&mut *tx)
        .await?;
        for m in members {
            sqlx::query("UPDATE changes SET state = 'verifying', updated_at = now() WHERE id = $1 AND state = 'ready'")
                .bind(m)
                .execute(&mut *tx)
                .await?;
            crate::intake::request_check_run(&mut tx, *m).await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    /// Records the merge commits of a group and its prefix builds.
    pub async fn record_spec_builds(
        &self,
        group: i64,
        member_shas: &[CommitSha],
        builds: &[SpecBuild],
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let shas: Vec<&str> = member_shas.iter().map(CommitSha::as_str).collect();
        sqlx::query("UPDATE spec_groups SET member_shas = $2 WHERE id = $1")
            .bind(group)
            .bind(&shas)
            .execute(&mut *tx)
            .await?;
        for b in builds {
            sqlx::query(
                "INSERT INTO spec_builds (group_id, length, branch, sha) VALUES ($1, $2, $3, $4)",
            )
            .bind(group)
            .bind(
                i32::try_from(b.length)
                    .map_err(|_| StoreError::InvalidRow("prefix length".into()))?,
            )
            .bind(&b.branch)
            .bind(b.sha.as_str())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The groups of a repository being built.
    pub async fn spec_groups(&self, repo: RepoId) -> Result<Vec<SpecGroup>, StoreError> {
        let rows = sqlx::query("SELECT id, members, onto_sha, member_shas FROM spec_groups WHERE repo_id = $1 ORDER BY id")
            .bind(to_db_id(repo.0, "repository id")?)
            .fetch_all(&self.pool)
            .await?;
        let mut out = Vec::new();
        for r in rows {
            let id: i64 = r.try_get("id")?;
            let builds = sqlx::query(
                "SELECT length, branch, sha FROM spec_builds WHERE group_id = $1 ORDER BY length",
            )
            .bind(id)
            .fetch_all(&self.pool)
            .await?
            .into_iter()
            .map(|b| -> Result<SpecBuild, StoreError> {
                let length: i32 = b.try_get("length")?;
                Ok(SpecBuild {
                    length: usize::try_from(length)
                        .map_err(|_| StoreError::InvalidRow("prefix length".into()))?,
                    branch: b.try_get("branch")?,
                    sha: sha(b.try_get("sha")?)?,
                })
            })
            .collect::<Result<_, _>>()?;
            let member_shas: Vec<String> = r.try_get("member_shas")?;
            out.push(SpecGroup {
                id,
                members: r.try_get("members")?,
                onto: sha(r.try_get("onto_sha")?)?,
                member_shas: member_shas.into_iter().map(sha).collect::<Result<_, _>>()?,
                builds,
            });
        }
        Ok(out)
    }

    /// Ends a group: members still `verifying` go back to `ready` (landed and returned members
    /// were already recorded), and the group is deleted.
    pub async fn end_spec_group(&self, group: i64) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let members: Vec<i64> =
            sqlx::query_scalar("SELECT unnest(members) FROM spec_groups WHERE id = $1")
                .bind(group)
                .fetch_all(&mut *tx)
                .await?;
        for m in members {
            let n = sqlx::query("UPDATE changes SET state = 'ready', updated_at = now() WHERE id = $1 AND state = 'verifying'")
                .bind(m)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if n > 0 {
                crate::intake::request_check_run(&mut tx, m).await?;
            }
        }
        sqlx::query("DELETE FROM spec_groups WHERE id = $1")
            .bind(group)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}
