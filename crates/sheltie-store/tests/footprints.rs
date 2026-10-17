//! Uploaded footprints, against a throwaway database per test.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use sheltie_core::intake::{InstallationEvent, Intake, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{
    ChangeId, CommitSha, DeliveryId, DiffHash, Footprint, FootprintReport, InstallationId, Login,
    PrNumber, RepoFullName, RepoId, TargetId, TreeHash,
};
use sheltie_store::{FootprintSummary, Store};
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

fn repo() -> RepoRef {
    RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
    }
}

fn report(head: char, writes: &[&str], reads: &[&str]) -> FootprintReport {
    let set = |v: &[&str]| v.iter().map(|t| TargetId::new(*t)).collect::<BTreeSet<_>>();
    FootprintReport {
        adapter: "sheltie-cargo/test".into(),
        base: sha('0'),
        head: sha(head),
        base_tree: TreeHash::parse("e".repeat(40)).unwrap(),
        diff_hash: DiffHash::new(format!("sha256:{head}")),
        footprint: Footprint {
            writes: set(writes),
            reads: set(reads),
            tests: BTreeSet::new(),
            leaky: false,
        },
        leak_reasons: vec![],
    }
}

async fn install(store: &Store) {
    let intake = Intake::Installation(InstallationEvent::Created {
        installation: InstallationId(77),
        account: Login::new("acme"),
        repos: vec![repo()],
    });
    store
        .ingest(&DeliveryId::new("install"), "installation", &intake)
        .await
        .unwrap();
}

async fn open_pr(store: &Store, head: char) {
    let intake = Intake::Pr(PrEvent {
        installation: InstallationId(77),
        repo: repo(),
        pr: PrNumber(5),
        head: sha(head),
        base: sha('0'),
        actor: Login::new("agent"),
        kind: PrEventKind::Opened,
        at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
        base_ref: "main".into(),
        default_branch: Some("main".into()),
        merge_commit: None,
        intent: sheltie_core::intake::IntentField::Unknown,
        closes: None,
    });
    store
        .ingest(
            &DeliveryId::new(format!("open-{head}")),
            "pull_request",
            &intake,
        )
        .await
        .unwrap();
}

fn change(head: char) -> ChangeId {
    ChangeId {
        repo: REPO,
        pr: PrNumber(5),
        head: sha(head),
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn upload_before_the_webhook_is_kept_for_the_change(pool: PgPool) {
    let store = Store::from_pool(pool);
    install(&store).await;
    let applied = store
        .save_footprint(REPO, &report('a', &["x"], &["x", "y"]), "run-1")
        .await
        .unwrap();
    assert_eq!(applied, 0, "no change at that head yet");
    open_pr(&store, 'a').await;
    let stored = store.footprint(&change('a')).await.unwrap().unwrap();
    assert_eq!(stored.footprint.reads.len(), 2);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn upload_after_the_webhook_refreshes_the_check_run(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    install(&store).await;
    open_pr(&store, 'a').await;
    // Sync the initial check run.
    for job in store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap()
    {
        assert_eq!(job.footprint, None);
        store
            .check_run_synced(job.change_db_id, job.version, Some(1))
            .await
            .unwrap();
    }

    let applied = store
        .save_footprint(REPO, &report('a', &["x"], &["x", "y"]), "run-1")
        .await
        .unwrap();
    assert_eq!(applied, 1);
    let diff: Option<String> = sqlx::query_scalar("SELECT diff_hash FROM changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(diff.as_deref(), Some("sha256:a"));

    let jobs = store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(jobs.len(), 1, "the check run shows the footprint");
    assert_eq!(
        jobs[0].footprint,
        Some(FootprintSummary {
            writes: 1,
            reads: 2,
            tests: 0,
            leaky: false,
            write_names: vec!["x".into()],
            read_names: vec!["x".into(), "y".into()],
            test_names: vec![],
            leak_reasons: vec![],
        })
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn reupload_replaces(pool: PgPool) {
    let store = Store::from_pool(pool);
    install(&store).await;
    store
        .save_footprint(REPO, &report('a', &["x"], &["x"]), "run-1")
        .await
        .unwrap();
    store
        .save_footprint(REPO, &report('a', &["x"], &["x", "y", "z"]), "run-2")
        .await
        .unwrap();
    open_pr(&store, 'a').await;
    let stored = store.footprint(&change('a')).await.unwrap().unwrap();
    assert_eq!(stored.footprint.reads.len(), 3);
}
