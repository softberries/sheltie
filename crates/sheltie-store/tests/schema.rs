//! Schema tests against a throwaway database per test (`#[sqlx::test]`, needs `DATABASE_URL`).

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sheltie_core::{ChangeState, GuaranteeTier, ResolutionTier};
use sheltie_store::Store;
use sqlx::PgPool;

const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const CHECK_VIOLATION: &str = "23514";
const UNIQUE_VIOLATION: &str = "23505";

fn sql_state(err: &sqlx::Error) -> Option<String> {
    err.as_database_error()
        .and_then(|e| e.code())
        .map(|c| c.into_owned())
}

async fn insert_repo(pool: &PgPool) {
    sqlx::query("INSERT INTO repositories (id, full_name) VALUES (1, 'acme/widgets')")
        .execute(pool)
        .await
        .unwrap();
}

async fn insert_change(
    pool: &PgPool,
    pr: i64,
    head: &str,
    state: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "INSERT INTO changes (repo_id, pr_number, head_sha, base_sha, diff_hash, state)
         VALUES (1, $1, $2, $3, 'diff', $4) RETURNING id",
    )
    .bind(pr)
    .bind(head)
    .bind(SHA_B)
    .bind(state)
    .fetch_one(pool)
    .await
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn store_pings(pool: PgPool) {
    Store::from_pool(pool).ping().await.unwrap();
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn migrations_are_idempotent(pool: PgPool) {
    Store::from_pool(pool).migrate().await.unwrap();
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn every_change_state_is_accepted(pool: PgPool) {
    insert_repo(&pool).await;
    for (pr, state) in (1..).zip(ChangeState::ALL) {
        insert_change(&pool, pr, SHA_A, state.as_str())
            .await
            .unwrap_or_else(|e| panic!("state {state} rejected: {e}"));
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn unknown_change_state_is_rejected(pool: PgPool) {
    insert_repo(&pool).await;
    let err = insert_change(&pool, 1, SHA_A, "Landed").await.unwrap_err();
    assert_eq!(sql_state(&err).as_deref(), Some(CHECK_VIOLATION));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn invalid_sha_is_rejected(pool: PgPool) {
    insert_repo(&pool).await;
    for bad in ["abc", &SHA_A.to_uppercase()] {
        let err = insert_change(&pool, 1, bad, "pending").await.unwrap_err();
        assert_eq!(sql_state(&err).as_deref(), Some(CHECK_VIOLATION), "{bad}");
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn change_is_unique_per_pr_and_head(pool: PgPool) {
    insert_repo(&pool).await;
    insert_change(&pool, 1, SHA_A, "pending").await.unwrap();
    let err = insert_change(&pool, 1, SHA_A, "ready").await.unwrap_err();
    assert_eq!(sql_state(&err).as_deref(), Some(UNIQUE_VIOLATION));
    // A new head on the same PR is a new change.
    insert_change(&pool, 1, SHA_B, "pending").await.unwrap();
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn every_resolution_tier_is_accepted(pool: PgPool) {
    for tier in ResolutionTier::ALL {
        let similarity = tier.is_replay().then_some(0.9_f32);
        sqlx::query("INSERT INTO resolutions (tier, files, similarity) VALUES ($1, '{}', $2)")
            .bind(tier.as_str())
            .bind(similarity)
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("tier {tier} rejected: {e}"));
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn similarity_only_for_replay_tiers(pool: PgPool) {
    let err = sqlx::query(
        "INSERT INTO resolutions (tier, files, similarity) VALUES ('structural', '{}', 0.5)",
    )
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(sql_state(&err).as_deref(), Some(CHECK_VIOLATION));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn failed_tests_only_on_failure(pool: PgPool) {
    let insert = |outcome: &'static str| {
        sqlx::query(
            "INSERT INTO verifications (tree, targets, outcome, failed_tests, runner)
             VALUES ($1, '{core}', $2, '{core::t}', 'gha')",
        )
        .bind(SHA_A)
        .bind(outcome)
        .execute(&pool)
    };
    insert("fail").await.unwrap();
    let err = insert("pass").await.unwrap_err();
    assert_eq!(sql_state(&err).as_deref(), Some(CHECK_VIOLATION));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn head_event_seq_is_the_cas(pool: PgPool) {
    insert_repo(&pool).await;
    let change = insert_change(&pool, 1, SHA_A, "landed").await.unwrap();
    let cas = |seq: i64, tier: GuaranteeTier| {
        sqlx::query("INSERT INTO head_events (repo_id, seq, commit_sha, change_id, tier) VALUES (1, $1, $2, $3, $4)")
            .bind(seq)
            .bind(SHA_A)
            .bind(change)
            .bind(tier.as_str())
            .execute(&pool)
    };
    for (seq, &tier) in (0..).zip(GuaranteeTier::ALL) {
        cas(seq, tier)
            .await
            .unwrap_or_else(|e| panic!("tier {tier} rejected: {e}"));
    }
    // A second committer that read the same head loses.
    let err = cas(0, GuaranteeTier::FootprintProven).await.unwrap_err();
    assert_eq!(sql_state(&err).as_deref(), Some(UNIQUE_VIOLATION));
}
