//! Webhook intake and the check-run outbox, against a throwaway database per test.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use sheltie_core::intake::{InstallationEvent, Intake, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{
    ChangeState, CommitSha, DeliveryId, InstallationId, Login, PrNumber, RepoFullName, RepoId,
};
use sheltie_store::{Ingested, Store};
use sqlx::PgPool;

const INSTALLATION: InstallationId = InstallationId(77);
const REPO: RepoId = RepoId(1001);
const PR: PrNumber = PrNumber(5);

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
}

fn repo() -> RepoRef {
    RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
    }
}

fn pr_event(kind: PrEventKind, head: char, secs: i64) -> Intake {
    Intake::Pr(PrEvent {
        installation: INSTALLATION,
        repo: repo(),
        pr: PR,
        head: sha(head),
        base: sha('0'),
        actor: Login::new("agent-bot"),
        kind,
        at: at(secs),
        base_ref: "main".into(),
        default_branch: Some("main".into()),
        merge_commit: None,
        intent: sheltie_core::intake::IntentField::Unknown,
        closes: None,
        title: None,
    })
}

struct Harness {
    store: Store,
    next_delivery: std::cell::Cell<u32>,
}

impl Harness {
    fn new(pool: PgPool) -> Self {
        Self {
            store: Store::from_pool(pool),
            next_delivery: std::cell::Cell::new(0),
        }
    }

    async fn send(&self, intake: Intake) -> Ingested {
        let n = self.next_delivery.get() + 1;
        self.next_delivery.set(n);
        self.store
            .ingest(&DeliveryId::new(format!("delivery-{n}")), "test", &intake)
            .await
            .unwrap()
    }

    async fn state(&self, head: char) -> Option<ChangeState> {
        self.store.change_state(REPO, PR, &sha(head)).await.unwrap()
    }

    /// Claims every due job and returns (head, state) pairs, marking them synced.
    async fn drain(&self) -> Vec<(char, ChangeState)> {
        let jobs = self
            .store
            .claim_check_runs(100, Duration::from_secs(60))
            .await
            .unwrap();
        let mut out = Vec::new();
        for job in jobs {
            let head = job.change.head.as_str().chars().next().unwrap();
            out.push((head, job.state));
            self.store
                .check_run_synced(job.change_db_id, job.version, Some(9000))
                .await
                .unwrap();
        }
        out.sort_by_key(|(h, _)| *h);
        out
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn opened_pr_becomes_pending_with_a_check_run(pool: PgPool) {
    let h = Harness::new(pool);
    assert_eq!(
        h.send(pr_event(PrEventKind::Opened, 'a', 0)).await,
        Ingested::Applied
    );
    assert_eq!(h.state('a').await, Some(ChangeState::Pending));
    assert_eq!(h.drain().await, [('a', ChangeState::Pending)]);
    assert_eq!(h.drain().await, [], "synced jobs are not handed out again");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn redelivery_is_ignored(pool: PgPool) {
    let h = Harness::new(pool.clone());
    let delivery = DeliveryId::new("same");
    let event = pr_event(PrEventKind::Opened, 'a', 0);
    assert_eq!(
        h.store
            .ingest(&delivery, "pull_request", &event)
            .await
            .unwrap(),
        Ingested::Applied
    );
    assert_eq!(
        h.store
            .ingest(&delivery, "pull_request", &event)
            .await
            .unwrap(),
        Ingested::Duplicate
    );
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM pr_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(events, 1);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn push_supersedes_the_old_head(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    h.drain().await;
    h.send(pr_event(PrEventKind::Synchronized, 'b', 10)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Closed));
    assert_eq!(h.state('b').await, Some(ChangeState::Pending));
    // Both check runs need an update: the old one is completed, the new one created.
    assert_eq!(
        h.drain().await,
        [('a', ChangeState::Closed), ('b', ChangeState::Pending)]
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn out_of_order_delivery_does_not_resurrect_an_old_head(pool: PgPool) {
    let h = Harness::new(pool.clone());
    h.send(pr_event(PrEventKind::Synchronized, 'b', 10)).await;
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    assert_eq!(h.state('a').await, None);
    assert_eq!(h.state('b').await, Some(ChangeState::Pending));
    let applied: Vec<bool> = sqlx::query_scalar("SELECT applied FROM pr_events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(applied, [true, false]);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn opened_never_wins_a_timestamp_tie(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Synchronized, 'b', 0)).await;
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    assert_eq!(h.state('b').await, Some(ChangeState::Pending));
    assert_eq!(h.state('a').await, None);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn close_and_reopen(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    h.send(pr_event(PrEventKind::Closed, 'a', 10)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Closed));
    h.send(pr_event(PrEventKind::Reopened, 'a', 20)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Pending));
    // A late close from before the reopen is ignored.
    h.send(pr_event(PrEventKind::Closed, 'a', 15)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Pending));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn merged_elsewhere_closes(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    h.send(pr_event(PrEventKind::Merged, 'a', 10)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Closed));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn reviews_are_recorded_without_state_change(pool: PgPool) {
    let h = Harness::new(pool.clone());
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    h.drain().await;
    h.send(pr_event(PrEventKind::Approved, 'a', 5)).await;
    assert_eq!(h.state('a').await, Some(ChangeState::Pending));
    assert_eq!(h.drain().await, []);
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM pr_events ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(kinds, ["opened", "approved"]);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn suspended_and_deleted_installations_are_not_synced(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    let installation = INSTALLATION;
    h.send(Intake::Installation(InstallationEvent::Suspended {
        installation,
    }))
    .await;
    assert_eq!(h.drain().await, []);
    h.send(Intake::Installation(InstallationEvent::Unsuspended {
        installation,
    }))
    .await;
    h.send(Intake::Installation(InstallationEvent::Deleted {
        installation,
    }))
    .await;
    assert_eq!(h.drain().await, []);
    // Reinstalling restores access and the pending sync goes out.
    h.send(Intake::Installation(InstallationEvent::Created {
        installation,
        account: Login::new("acme"),
        repos: vec![repo()],
    }))
    .await;
    assert_eq!(h.drain().await, [('a', ChangeState::Pending)]);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn removed_repository_is_not_synced(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    h.send(Intake::Installation(InstallationEvent::ReposChanged {
        installation: INSTALLATION,
        account: Login::new("acme"),
        added: vec![],
        removed: vec![REPO],
    }))
    .await;
    assert_eq!(h.drain().await, []);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn claimed_jobs_are_leased(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    let first = h
        .store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].attempts, 1);
    assert_eq!(first[0].github_id, None);
    let second = h
        .store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert!(second.is_empty(), "a leased job was handed out twice");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn failed_sync_is_retried_after_backoff(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    let job = h
        .store
        .claim_check_runs(10, Duration::ZERO)
        .await
        .unwrap()
        .remove(0);
    h.store
        .check_run_failed(job.change_db_id, Duration::from_secs(3600), "boom")
        .await
        .unwrap();
    assert!(
        h.store
            .claim_check_runs(10, Duration::ZERO)
            .await
            .unwrap()
            .is_empty()
    );
    h.store
        .check_run_failed(job.change_db_id, Duration::ZERO, "boom")
        .await
        .unwrap();
    let retry = h.store.claim_check_runs(10, Duration::ZERO).await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempts, 2);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn update_during_sync_triggers_another_sync(pool: PgPool) {
    let h = Harness::new(pool);
    h.send(pr_event(PrEventKind::Opened, 'a', 0)).await;
    let job = h
        .store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap()
        .remove(0);
    // The PR closes while the first sync is in flight.
    h.send(pr_event(PrEventKind::Closed, 'a', 10)).await;
    h.store
        .check_run_synced(job.change_db_id, job.version, Some(42))
        .await
        .unwrap();
    let next = h
        .store
        .claim_check_runs(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].state, ChangeState::Closed);
    assert_eq!(
        next[0].github_id,
        Some(42),
        "the created check run is updated, not recreated"
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn the_intent_follows_the_description(pool: PgPool) {
    use sheltie_core::Intent;
    use sheltie_core::intake::IntentField;
    let h = Harness::new(pool.clone());
    let with = |kind, secs, intent: IntentField| match pr_event(kind, 'a', secs) {
        Intake::Pr(mut e) => {
            e.intent = intent;
            Intake::Pr(e)
        }
        _ => unreachable!(),
    };
    let intent = Intent {
        task: "Add retries".into(),
        plan: None,
        acceptance: vec!["t::a".into()],
    };
    h.send(with(
        PrEventKind::Opened,
        0,
        IntentField::Valid(intent.clone()),
    ))
    .await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        h.store.change_intent(id).await.unwrap(),
        Some(intent.clone())
    );
    // Reviews do not carry the description: the intent is kept.
    h.send(with(PrEventKind::Approved, 5, IntentField::Unknown))
        .await;
    assert_eq!(h.store.change_intent(id).await.unwrap(), Some(intent));
    // An edit that breaks the block clears it and records why.
    h.send(with(
        PrEventKind::Edited,
        10,
        IntentField::Invalid("bad json".into()),
    ))
    .await;
    assert_eq!(h.store.change_intent(id).await.unwrap(), None);
    let why: Option<String> = sqlx::query_scalar("SELECT intent_error FROM changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(why.as_deref(), Some("bad json"));
    assert_eq!(
        h.state('a').await,
        Some(ChangeState::Pending),
        "edits change no state"
    );
}
