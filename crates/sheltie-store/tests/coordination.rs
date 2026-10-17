//! Coordination storage: issues, tokens, leases and notices.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use sheltie_core::intake::{Intake, IssueEvent, IssueEventKind, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{
    CommitSha, DeliveryId, Footprint, GuaranteeTier, InstallationId, IssueNumber, Login, PrNumber,
    RepoFullName, RepoId, TargetId,
};
use sheltie_store::{Claim, IssueNotice, Store};
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);
const HOUR: Duration = Duration::from_secs(3600);

fn repo() -> RepoRef {
    RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
    }
}

fn fp(writes: &[&str], reads: &[&str]) -> Footprint {
    let set = |xs: &[&str]| {
        xs.iter()
            .map(|x| TargetId::new(*x))
            .collect::<BTreeSet<_>>()
    };
    let mut r = set(reads);
    r.extend(set(writes));
    Footprint {
        writes: set(writes),
        reads: r,
        ..Footprint::default()
    }
}

struct H {
    store: Store,
    n: std::cell::Cell<u32>,
}

impl H {
    fn new(pool: PgPool) -> Self {
        Self {
            store: Store::from_pool(pool),
            n: std::cell::Cell::new(0),
        }
    }

    async fn issue(&self, number: u64, kind: IssueEventKind, secs: i64) {
        self.n.set(self.n.get() + 1);
        let e = IssueEvent {
            installation: InstallationId(77),
            repo: repo(),
            issue: IssueNumber(number),
            kind,
            title: format!("Issue {number}"),
            body: format!("body at {secs}"),
            at: Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap(),
            default_branch: Some("main".into()),
        };
        self.store
            .ingest(
                &DeliveryId::new(format!("d{}", self.n.get())),
                "issues",
                &Intake::Issue(e),
            )
            .await
            .unwrap();
    }

    async fn claim(&self, issue: u64, holder: &str, f: &Footprint) -> Claim {
        self.store
            .claim_lease(REPO, IssueNumber(issue), holder, f, HOUR)
            .await
            .unwrap()
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn issues_keep_the_newest_text(pool: PgPool) {
    let h = H::new(pool);
    h.issue(1, IssueEventKind::Opened, 10).await;
    h.issue(1, IssueEventKind::Edited, 5).await; // older, out of order
    let i = h.store.issue(REPO, IssueNumber(1)).await.unwrap().unwrap();
    assert_eq!((i.body.as_str(), i.open), ("body at 10", true));
    h.issue(1, IssueEventKind::Closed, 20).await;
    assert!(
        !h.store
            .issue(REPO, IssueNumber(1))
            .await
            .unwrap()
            .unwrap()
            .open
    );
    h.issue(2, IssueEventKind::Opened, 1).await;
    let open: Vec<u64> = h
        .store
        .open_issues(REPO, 10)
        .await
        .unwrap()
        .iter()
        .map(|i| i.number.0)
        .collect();
    assert_eq!(open, [2]);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn tokens(pool: PgPool) {
    let h = H::new(pool);
    h.issue(1, IssueEventKind::Opened, 0).await; // makes the repository known
    let name = RepoFullName::parse("Acme/Widgets").unwrap();
    assert_eq!(
        h.store
            .create_token(&name, "agent-a", &[7; 32])
            .await
            .unwrap(),
        Some(REPO)
    );
    assert_eq!(
        h.store
            .create_token(&RepoFullName::parse("x/y").unwrap(), "a", &[8; 32])
            .await
            .unwrap(),
        None
    );
    let (repo, full, who) = h.store.token(&[7; 32]).await.unwrap().unwrap();
    assert_eq!(
        (repo, full.as_str(), who.as_str()),
        (REPO, "acme/widgets", "agent-a")
    );
    assert_eq!(h.store.revoke_token(&name, "agent-a").await.unwrap(), 1);
    assert_eq!(h.store.token(&[7; 32]).await.unwrap(), None);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn conflicting_claims_are_serialized(pool: PgPool) {
    let h = H::new(pool);
    h.issue(1, IssueEventKind::Opened, 0).await;
    let pricing = fp(&["pricing"], &["cart"]);
    let Claim::Granted(a) = h.claim(1, "agent-a", &pricing).await else {
        panic!()
    };
    assert!(matches!(h.claim(1, "agent-a", &pricing).await, Claim::Renewed(l) if l.id == a.id));
    assert!(matches!(h.claim(1, "agent-b", &pricing).await, Claim::Taken(l) if l.id == a.id));
    assert!(
        matches!(h.claim(2, "agent-b", &fp(&["cart"], &[])).await, Claim::Blocked(l) if l.id == a.id)
    );
    assert!(matches!(
        h.claim(3, "agent-b", &fp(&["catalog"], &[])).await,
        Claim::Granted(_)
    ));

    // Only the holder can release; then the blocked claim goes through.
    assert_eq!(
        h.store
            .release_lease(REPO, IssueNumber(1), Some("agent-b"), "x")
            .await
            .unwrap(),
        None
    );
    assert!(
        h.store
            .release_lease(REPO, IssueNumber(1), Some("agent-a"), "done")
            .await
            .unwrap()
            .is_some()
    );
    assert!(matches!(
        h.claim(2, "agent-b", &fp(&["cart"], &[])).await,
        Claim::Granted(_)
    ));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn concurrent_claims_never_both_win(pool: PgPool) {
    let h = H::new(pool.clone());
    h.issue(1, IssueEventKind::Opened, 0).await;
    let store = Store::from_pool(pool);
    let claims = (1..=8).map(|i| {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .claim_lease(
                    REPO,
                    IssueNumber(i),
                    &format!("agent-{i}"),
                    &fp(&["shared"], &[]),
                    HOUR,
                )
                .await
                .unwrap()
        })
    });
    let mut granted = 0;
    for c in claims {
        if matches!(c.await.unwrap(), Claim::Granted(_)) {
            granted += 1;
        }
    }
    assert_eq!(granted, 1);
    assert_eq!(store.active_leases(REPO).await.unwrap().len(), 1);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn finished_leases_are_released(pool: PgPool) {
    let h = H::new(pool.clone());
    for i in 1..=3 {
        h.issue(i, IssueEventKind::Opened, 0).await;
    }
    let Claim::Granted(_) = h.claim(1, "a", &fp(&["one"], &[])).await else {
        panic!()
    };
    let Claim::Granted(_) = h.claim(2, "b", &fp(&["two"], &[])).await else {
        panic!()
    };
    let Claim::Granted(_) = h.claim(3, "c", &fp(&["three"], &[])).await else {
        panic!()
    };
    assert!(h.store.release_finished_leases().await.unwrap().is_empty());

    // #1: its issue is closed. #2: a PR that fixes it lands. #3: it expires.
    h.issue(1, IssueEventKind::Closed, 30).await;
    let sha = |c: char| CommitSha::parse(c.to_string().repeat(40)).unwrap();
    h.store
        .ingest(
            &DeliveryId::new("pr"),
            "pull_request",
            &Intake::Pr(PrEvent {
                installation: InstallationId(77),
                repo: repo(),
                pr: PrNumber(9),
                head: sha('a'),
                base: sha('0'),
                actor: Login::new("b"),
                kind: PrEventKind::Opened,
                at: Utc.timestamp_opt(1_800_000_000, 0).unwrap(),
                base_ref: "main".into(),
                default_branch: Some("main".into()),
                merge_commit: None,
                intent: sheltie_core::intake::IntentField::Missing,
                closes: Some(BTreeSet::from([IssueNumber(2)])),
            }),
        )
        .await
        .unwrap();
    let change: i64 = sqlx::query_scalar("SELECT id FROM changes")
        .fetch_one(&pool)
        .await
        .unwrap();
    h.store
        .mark_landed(REPO, change, &sha('5'), GuaranteeTier::FootprintProven)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE leases SET expires_at = now() - interval '1 second' WHERE issue_number = 3",
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut released: Vec<(u64, String)> = h
        .store
        .release_finished_leases()
        .await
        .unwrap()
        .into_iter()
        .map(|(l, why)| (l.issue.0, why))
        .collect();
    released.sort();
    assert_eq!(
        released,
        [
            (1, "issue closed".into()),
            (2, "pull request landed".into()),
            (3, "expired".into())
        ]
    );
    assert!(h.store.active_leases(REPO).await.unwrap().is_empty());
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn waiters_and_notices(pool: PgPool) {
    let h = H::new(pool);
    h.issue(4, IssueEventKind::Opened, 0).await;
    h.issue(4, IssueEventKind::Assigned(Login::new("agent-b")), 1)
        .await;
    h.store
        .add_waiter(REPO, IssueNumber(4), "agent-b")
        .await
        .unwrap();
    let waiters = h.store.waiters().await.unwrap();
    assert_eq!(waiters.len(), 1, "assignment is a waiting claim");
    let blocked = IssueNotice {
        comment: Some("blocked".into()),
        ..IssueNotice::default()
    };
    h.store.waiter_blocked(&waiters[0], &blocked).await.unwrap();
    h.store.waiter_blocked(&waiters[0], &blocked).await.unwrap();
    assert!(h.store.waiters().await.unwrap()[0].notified);
    let once = h
        .store
        .claim_notices(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(once.len(), 1, "notified once");
    h.store.notice_sent(once[0].id).await.unwrap();
    // Unassigning drops the claim and releases the assignee's lease.
    let Claim::Granted(_) = h.claim(4, "agent-b", &fp(&["x"], &[])).await else {
        panic!()
    };
    h.issue(4, IssueEventKind::Assigned(Login::new("agent-b")), 4)
        .await;
    h.issue(4, IssueEventKind::Unassigned(Login::new("agent-b")), 5)
        .await;
    assert!(h.store.waiters().await.unwrap().is_empty());
    assert!(h.store.active_leases(REPO).await.unwrap().is_empty());

    let notice = IssueNotice {
        comment: Some("blocked".into()),
        add_label: Some("sheltie: blocked".into()),
        remove_label: None,
    };
    h.store
        .queue_notice(REPO, IssueNumber(4), &notice)
        .await
        .unwrap();
    let claimed = h
        .store
        .claim_notices(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].notice, notice);
    assert!(
        h.store
            .claim_notices(10, Duration::from_secs(60))
            .await
            .unwrap()
            .is_empty(),
        "leased"
    );
    h.store
        .notice_failed(claimed[0].id, Duration::ZERO)
        .await
        .unwrap();
    let again = h
        .store
        .claim_notices(10, Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(again[0].attempts, 2);
    h.store.notice_sent(again[0].id).await.unwrap();
    h.store
        .notice_failed(again[0].id, Duration::ZERO)
        .await
        .unwrap();
    assert!(
        h.store
            .claim_notices(10, Duration::from_secs(60))
            .await
            .unwrap()
            .is_empty(),
        "sent"
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn stats_count_without_names(pool: PgPool) {
    let h = H::new(pool);
    h.issue(1, IssueEventKind::Opened, 0).await;
    let Claim::Granted(_) = h.claim(1, "agent-a", &fp(&["x"], &[])).await else {
        panic!()
    };
    let stats = h
        .store
        .stats(Utc::now() - chrono::Duration::days(1))
        .await
        .unwrap();
    assert_eq!(stats.repositories.get("shadow"), Some(&1));
    assert_eq!((stats.leases, stats.replays, stats.returned), (1, 0, 0));
    let json = serde_json::to_string(&stats).unwrap();
    assert!(
        !json.contains("acme") && !json.contains("agent-a"),
        "{json}"
    );
}
