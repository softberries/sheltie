//! Readiness, target-branch tracking, epoch inputs and predictions.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde_json::json;
use sheltie_core::intake::{
    ChecksEvent, InstallationEvent, Intake, PrEvent, PrEventKind, PushEvent, RepoRef,
};
use sheltie_core::{
    ChangeState, CommitSha, DeliveryId, DiffHash, Footprint, FootprintReport, InstallationId,
    Login, PrNumber, RepoFullName, RepoId, TargetId, TreeHash,
};
use sheltie_store::{PredictionRow, Readiness, Store};
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);
const GREEN: Readiness = Readiness {
    head_matches: true,
    approved: true,
    checks_green: true,
    draft: false,
};

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

fn repo() -> RepoRef {
    RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
    }
}

struct H {
    store: Store,
    pool: PgPool,
    n: std::cell::Cell<u32>,
}

impl H {
    async fn new(pool: PgPool) -> Self {
        let h = Self {
            store: Store::from_pool(pool.clone()),
            pool,
            n: 0.into(),
        };
        h.send(Intake::Installation(InstallationEvent::Created {
            installation: InstallationId(77),
            account: Login::new("acme"),
            repos: vec![repo()],
        }))
        .await;
        h
    }

    async fn send(&self, intake: Intake) {
        self.n.set(self.n.get() + 1);
        self.store
            .ingest(
                &DeliveryId::new(format!("d{}", self.n.get())),
                "test",
                &intake,
            )
            .await
            .unwrap();
    }

    async fn pr(
        &self,
        pr: u64,
        head: char,
        kind: PrEventKind,
        base_ref: &str,
        merge: Option<char>,
    ) {
        self.send(Intake::Pr(PrEvent {
            installation: InstallationId(77),
            repo: repo(),
            pr: PrNumber(pr),
            head: sha(head),
            base: sha('0'),
            actor: Login::new("agent"),
            kind,
            at: Utc
                .timestamp_opt(1_800_000_000 + i64::from(self.n.get()), 0)
                .unwrap(),
            base_ref: base_ref.into(),
            default_branch: Some("main".into()),
            merge_commit: merge.map(sha),
            intent: sheltie_core::intake::IntentField::Unknown,
            closes: None,
            title: None,
        }))
        .await;
    }

    async fn push(&self, commits: &[char]) {
        self.send(Intake::Push(PushEvent {
            installation: InstallationId(77),
            repo: repo(),
            branch: "main".into(),
            default_branch: "main".into(),
            after: sha(*commits.last().unwrap()),
            commits: commits.iter().map(|c| sha(*c)).collect(),
        }))
        .await;
    }

    async fn footprint(&self, head: char, base: char, writes: &[&str]) {
        let set: BTreeSet<TargetId> = writes.iter().map(|w| TargetId::new(*w)).collect();
        let report = FootprintReport {
            adapter: "test".into(),
            base: sha(base),
            head: sha(head),
            base_tree: TreeHash::parse("e".repeat(40)).unwrap(),
            diff_hash: DiffHash::new(format!("d{head}")),
            footprint: Footprint {
                writes: set.clone(),
                reads: set,
                tests: BTreeSet::new(),
                leaky: false,
            },
            leak_reasons: vec![],
        };
        self.store
            .save_footprint(REPO, &report, "ci")
            .await
            .unwrap();
    }

    /// Runs every due readiness check with `r`; returns the resulting states.
    async fn check(&self, r: Readiness) -> Vec<ChangeState> {
        let mut out = Vec::new();
        for job in self
            .store
            .claim_readiness(10, Duration::ZERO)
            .await
            .unwrap()
        {
            out.push(self.store.apply_readiness(&job, r).await.unwrap());
        }
        out
    }

    async fn state(&self, pr: u64, head: char) -> ChangeState {
        self.store
            .change_state(REPO, PrNumber(pr), &sha(head))
            .await
            .unwrap()
            .unwrap()
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn ready_needs_approval_checks_and_a_footprint(pool: PgPool) {
    let h = H::new(pool).await;
    h.pr(1, 'a', PrEventKind::Opened, "main", None).await;
    assert_eq!(
        h.check(GREEN).await,
        [ChangeState::Pending],
        "no footprint yet"
    );
    h.footprint('a', '0', &["x"]).await;
    assert_eq!(h.check(GREEN).await, [ChangeState::Ready]);
    let ready_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT ready_at FROM changes")
            .fetch_one(&h.pool)
            .await
            .unwrap();
    assert!(ready_at.is_some());

    // A dismissed approval sends it back.
    h.pr(1, 'a', PrEventKind::ReviewDismissed, "main", None)
        .await;
    assert_eq!(
        h.check(Readiness {
            approved: false,
            ..GREEN
        })
        .await,
        [ChangeState::Pending]
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn drafts_red_checks_and_other_branches_stay_pending(pool: PgPool) {
    let h = H::new(pool).await;
    h.pr(1, 'a', PrEventKind::Opened, "release", None).await;
    h.footprint('a', '0', &["x"]).await;
    assert_eq!(
        h.check(GREEN).await,
        [ChangeState::Pending],
        "targets a non-default branch"
    );
    h.pr(2, 'b', PrEventKind::Opened, "main", None).await;
    h.footprint('b', '0', &["y"]).await;
    let states = h
        .check(Readiness {
            checks_green: false,
            ..GREEN
        })
        .await;
    assert!(states.iter().all(|s| *s == ChangeState::Pending));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn check_events_request_readiness(pool: PgPool) {
    let h = H::new(pool).await;
    h.pr(1, 'a', PrEventKind::Opened, "main", None).await;
    h.check(GREEN).await;
    assert!(
        h.store
            .claim_readiness(10, Duration::ZERO)
            .await
            .unwrap()
            .is_empty(),
        "nothing due"
    );
    h.send(Intake::Checks(ChecksEvent {
        installation: InstallationId(77),
        repo: repo(),
        head: sha('a'),
    }))
    .await;
    assert_eq!(
        h.store
            .claim_readiness(10, Duration::ZERO)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn pushes_record_the_default_branch_in_order(pool: PgPool) {
    let h = H::new(pool).await;
    h.push(&['0', '1']).await;
    h.push(&['1', '2']).await; // '1' again: ignored
    h.send(Intake::Push(PushEvent {
        installation: InstallationId(77),
        repo: repo(),
        branch: "feature".into(),
        default_branch: "main".into(),
        after: sha('9'),
        commits: vec![sha('9')],
    }))
    .await;
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT sha, seq FROM branch_commits ORDER BY seq")
            .fetch_all(&h.pool)
            .await
            .unwrap();
    let shas: Vec<char> = rows
        .iter()
        .map(|(s, _)| s.chars().next().unwrap())
        .collect();
    assert_eq!(shas, ['0', '1', '2']);
    assert!(rows.windows(2).all(|w| w[0].1 < w[1].1));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn landed_writes_are_what_merged_after_the_base(pool: PgPool) {
    let h = H::new(pool).await;
    h.push(&['0']).await;
    // PR 2 lands on top of '0' as merge commit '5', writing "x".
    h.pr(2, 'b', PrEventKind::Opened, "main", None).await;
    h.footprint('b', '0', &["x"]).await;
    h.pr(2, 'b', PrEventKind::Merged, "main", Some('5')).await;
    h.push(&['5']).await;
    // PR 1 (base '0') and PR 3 (base unknown) are ready.
    h.pr(1, 'a', PrEventKind::Opened, "main", None).await;
    h.footprint('a', '0', &["y"]).await;
    h.pr(3, 'c', PrEventKind::Opened, "main", None).await;
    h.footprint('c', '8', &["z"]).await;
    h.check(GREEN).await;

    let mut cands = h.store.epoch_candidates(REPO).await.unwrap();
    cands.sort_by_key(|c| c.change.pr);
    assert_eq!(cands.len(), 2);
    assert_eq!(
        cands[0].landed_writes,
        Some(BTreeSet::from([TargetId::new("x")]))
    );
    assert_eq!(
        cands[1].landed_writes, None,
        "base '8' is not on the tracked branch"
    );
    assert_eq!(h.state(2, 'b').await, ChangeState::Closed);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn predictions_are_logged_only_when_they_change(pool: PgPool) {
    let h = H::new(pool).await;
    h.pr(1, 'a', PrEventKind::Opened, "main", None).await;
    let id: i64 = sqlx::query_scalar("SELECT id FROM changes")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    let row = |decision: &str, summary: &str| PredictionRow {
        change_db_id: id,
        decision: decision.into(),
        detail: json!({ "decision": decision }),
        summary: summary.into(),
    };
    let now = Utc::now();
    assert_eq!(
        h.store
            .record_predictions(REPO, now, &[row("land_now", "would land now")])
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        h.store
            .record_predictions(REPO, now, &[row("land_now", "would land now")])
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        h.store
            .record_predictions(REPO, now, &[row("reverify", "would reverify")])
            .await
            .unwrap(),
        1
    );
    let logged: i64 = sqlx::query_scalar("SELECT count(*) FROM prediction_log")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(logged, 2);
    h.store.record_predictions(REPO, now, &[]).await.unwrap();
    let current: i64 = sqlx::query_scalar("SELECT count(*) FROM predictions")
        .fetch_one(&h.pool)
        .await
        .unwrap();
    assert_eq!(
        current, 0,
        "predictions of changes no longer ready are removed"
    );
}
