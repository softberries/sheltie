//! Merge-mode storage: repository mode, commit lock, landed write sets, verify runs.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use chrono::{TimeZone, Utc};
use sheltie_core::intake::{InstallationEvent, Intake, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{
    ChangeState, CommitSha, DeliveryId, DiffHash, Footprint, FootprintReport, GuaranteeTier,
    InstallationId, Login, PrNumber, RepoFullName, RepoId, TargetId, TreeHash,
};
use sheltie_store::{RepoMode, Store, VerifyRun};
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

fn name() -> RepoFullName {
    RepoFullName::parse("acme/widgets").unwrap()
}

async fn setup(pool: &PgPool) -> (Store, i64) {
    let store = Store::from_pool(pool.clone());
    let repo = RepoRef {
        id: REPO,
        full_name: name(),
    };
    store
        .ingest(
            &DeliveryId::new("i"),
            "installation",
            &Intake::Installation(InstallationEvent::Created {
                installation: InstallationId(77),
                account: Login::new("acme"),
                repos: vec![repo.clone()],
            }),
        )
        .await
        .unwrap();
    store
        .ingest(
            &DeliveryId::new("o"),
            "pull_request",
            &Intake::Pr(PrEvent {
                installation: InstallationId(77),
                repo,
                pr: PrNumber(1),
                head: sha('a'),
                base: sha('0'),
                actor: Login::new("agent"),
                kind: PrEventKind::Opened,
                at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
                base_ref: "main".into(),
                default_branch: Some("main".into()),
                merge_commit: None,
                intent: sheltie_core::intake::IntentField::Unknown,
                closes: None,
            }),
        )
        .await
        .unwrap();
    let writes: BTreeSet<TargetId> = BTreeSet::from([TargetId::new("billing")]);
    store
        .save_footprint(
            REPO,
            &FootprintReport {
                adapter: "t".into(),
                base: sha('0'),
                head: sha('a'),
                base_tree: TreeHash::parse("e".repeat(40)).unwrap(),
                diff_hash: DiffHash::new("d"),
                footprint: Footprint {
                    writes: writes.clone(),
                    reads: writes,
                    tests: BTreeSet::new(),
                    leaky: false,
                },
                leak_reasons: vec![],
            },
            "ci",
        )
        .await
        .unwrap();
    let id: i64 = sqlx::query_scalar("SELECT id FROM changes")
        .fetch_one(pool)
        .await
        .unwrap();
    (store, id)
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn mode_defaults_to_shadow_and_can_be_switched(pool: PgPool) {
    let (store, _) = setup(&pool).await;
    assert_eq!(store.repo_mode(REPO).await.unwrap(), RepoMode::Shadow);
    assert!(store.set_repo_mode(&name(), RepoMode::Merge).await.unwrap());
    assert_eq!(store.repo_mode(REPO).await.unwrap(), RepoMode::Merge);
    assert!(
        !store
            .set_repo_mode(&RepoFullName::parse("x/y").unwrap(), RepoMode::Merge)
            .await
            .unwrap()
    );
    let access = store.repo_access(REPO).await.unwrap().unwrap();
    assert_eq!(
        (access.default_branch.as_str(), access.mode),
        ("main", RepoMode::Merge)
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn commit_lock_is_exclusive(pool: PgPool) {
    let (store, _) = setup(&pool).await;
    let first = store.try_lock_repo(REPO).await.unwrap();
    assert!(first.is_some());
    assert!(
        store.try_lock_repo(REPO).await.unwrap().is_none(),
        "second holder"
    );
    drop(first);
    assert!(
        store.try_lock_repo(REPO).await.unwrap().is_some(),
        "released on drop"
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn landing_records_tier_and_head_event_and_writes_become_known(pool: PgPool) {
    let (store, id) = setup(&pool).await;
    assert_eq!(
        store.merge_commit_writes(REPO, &[sha('5')]).await.unwrap(),
        None,
        "unknown commit"
    );
    assert_eq!(
        store.merge_commit_writes(REPO, &[]).await.unwrap(),
        Some(BTreeSet::new())
    );

    store
        .mark_landed(REPO, id, &sha('5'), GuaranteeTier::FootprintProven)
        .await
        .unwrap();
    assert_eq!(
        store
            .change_state(REPO, PrNumber(1), &sha('a'))
            .await
            .unwrap(),
        Some(ChangeState::Landed)
    );
    let (seq, tier): (i64, String) = sqlx::query_as("SELECT seq, tier FROM head_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!((seq, tier.as_str()), (0, "footprint"));
    assert_eq!(
        store.merge_commit_writes(REPO, &[sha('5')]).await.unwrap(),
        Some(BTreeSet::from([TargetId::new("billing")]))
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn verify_run_lifecycle(pool: PgPool) {
    let (store, id) = setup(&pool).await;
    let run = VerifyRun {
        branch: "sheltie/verify/1-aaaaaaaaaaaa".into(),
        verify_sha: sha('7'),
        onto: sha('0'),
    };
    store.start_verify_run(id, &run).await.unwrap();
    assert_eq!(
        store
            .change_state(REPO, PrNumber(1), &sha('a'))
            .await
            .unwrap(),
        Some(ChangeState::Verifying)
    );
    assert_eq!(store.verify_run(id).await.unwrap(), Some(run));
    assert_eq!(store.verifying_changes(REPO).await.unwrap(), [id]);
    assert!(store.verify_started_at(id).await.unwrap().is_some());
    store.abandon_verify_run(id).await.unwrap();
    assert_eq!(
        store
            .change_state(REPO, PrNumber(1), &sha('a'))
            .await
            .unwrap(),
        Some(ChangeState::Ready)
    );
    store.mark_returned(id, "CI failed").await.unwrap();
    let reason: Option<String> = sqlx::query_scalar("SELECT returned_reason FROM changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(reason.as_deref(), Some("CI failed"));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn speculation_group_lifecycle(pool: PgPool) {
    use sheltie_store::SpecBuild;
    let (store, id) = setup(&pool).await;
    // Only ready changes join a group.
    sqlx::query("UPDATE changes SET state = 'ready'")
        .execute(&pool)
        .await
        .unwrap();
    let group = store
        .create_spec_group(REPO, &[id], &sha('0'))
        .await
        .unwrap();
    assert_eq!(
        store
            .change_state(REPO, PrNumber(1), &sha('a'))
            .await
            .unwrap(),
        Some(ChangeState::Verifying)
    );
    let build = SpecBuild {
        length: 1,
        branch: "sheltie/spec/1-1".into(),
        sha: sha('7'),
    };
    store
        .record_spec_builds(group, &[sha('7')], std::slice::from_ref(&build))
        .await
        .unwrap();
    let groups = store.spec_groups(REPO).await.unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        (groups[0].members.as_slice(), groups[0].builds.as_slice()),
        ([id].as_slice(), [build].as_slice())
    );
    assert_eq!(groups[0].member_shas, [sha('7')]);
    store.end_spec_group(group).await.unwrap();
    assert!(store.spec_groups(REPO).await.unwrap().is_empty());
    assert_eq!(
        store
            .change_state(REPO, PrNumber(1), &sha('a'))
            .await
            .unwrap(),
        Some(ChangeState::Ready),
        "back in the queue"
    );
}
