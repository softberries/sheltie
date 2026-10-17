//! M3 "done when" (automated part): two changes judged independent land, the default branch
//! turns red, and the shadow report names the pair as a suspected footprint miss.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use sheltie_core::intake::{
    ChecksEvent, InstallationEvent, Intake, PrEvent, PrEventKind, PushEvent, RepoRef,
};
use sheltie_core::{
    CommitSha, DeliveryId, DiffHash, Footprint, FootprintReport, InstallationId, Login, PrNumber,
    RepoFullName, RepoId, TargetId, TreeHash,
};
use sheltie_scheduler::report::{LandedInWindow, build_report, suspected_misses};
use sheltie_store::{CiState, Store};
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

async fn send(store: &Store, n: &mut u32, intake: Intake) {
    *n += 1;
    store
        .ingest(&DeliveryId::new(format!("d{n}")), "test", &intake)
        .await
        .unwrap();
}

fn pr(number: u64, head: char, kind: PrEventKind, merge: Option<char>, secs: i64) -> Intake {
    Intake::Pr(PrEvent {
        installation: InstallationId(77),
        repo: repo(),
        pr: PrNumber(number),
        head: sha(head),
        base: sha('0'),
        actor: Login::new("agent"),
        kind,
        at: Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap(),
        base_ref: "main".into(),
        default_branch: Some("main".into()),
        merge_commit: merge.map(sha),
        intent: sheltie_core::intake::IntentField::Unknown,
        closes: None,
        title: None,
    })
}

fn push(commits: &[char]) -> Intake {
    Intake::Push(PushEvent {
        installation: InstallationId(77),
        repo: repo(),
        branch: "main".into(),
        default_branch: "main".into(),
        after: sha(*commits.last().unwrap()),
        commits: commits.iter().map(|c| sha(*c)).collect(),
    })
}

async fn footprint(store: &Store, head: char, writes: &[&str]) {
    let set: BTreeSet<TargetId> = writes.iter().map(|w| TargetId::new(*w)).collect();
    let report = FootprintReport {
        adapter: "test".into(),
        base: sha('0'),
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
    store.save_footprint(REPO, &report, "ci").await.unwrap();
}

/// Runs every due CI lookup, answering from `states`; returns red windows' suspected misses.
async fn poll_ci(store: &Store, states: &[(char, CiState)]) -> usize {
    let mut misses = 0;
    for job in store.claim_branch_ci(50, Duration::ZERO).await.unwrap() {
        let c = job.sha.as_str().chars().next().unwrap();
        let state = states
            .iter()
            .find(|(s, _)| *s == c)
            .map_or(CiState::Pending, |(_, st)| *st);
        if let Some(window) = store.apply_branch_ci(&job, state).await.unwrap() {
            let landed: Vec<_> = window
                .landed
                .iter()
                .map(|(k, f)| LandedInWindow {
                    key: *k,
                    footprint: f.clone(),
                })
                .collect();
            let found = suspected_misses(&landed);
            misses += found.len();
            store
                .record_misses(job.repo, &window, &found)
                .await
                .unwrap();
        }
    }
    misses
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn red_main_after_two_independent_merges_is_a_suspected_miss(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let mut n = 0;
    send(
        &store,
        &mut n,
        Intake::Installation(InstallationEvent::Created {
            installation: InstallationId(77),
            account: Login::new("acme"),
            repos: vec![repo()],
        }),
    )
    .await;
    send(&store, &mut n, push(&['0'])).await;
    assert_eq!(poll_ci(&store, &[('0', CiState::Success)]).await, 0);

    // #1 writes billing, #2 writes auth: independent. Both merge, then main is red.
    send(&store, &mut n, pr(1, 'a', PrEventKind::Opened, None, 1)).await;
    footprint(&store, 'a', &["billing"]).await;
    send(&store, &mut n, pr(2, 'b', PrEventKind::Opened, None, 2)).await;
    footprint(&store, 'b', &["auth"]).await;
    send(
        &store,
        &mut n,
        pr(1, 'a', PrEventKind::Merged, Some('5'), 3),
    )
    .await;
    send(
        &store,
        &mut n,
        pr(2, 'b', PrEventKind::Merged, Some('6'), 4),
    )
    .await;
    send(&store, &mut n, push(&['5', '6'])).await;
    assert_eq!(
        poll_ci(&store, &[('5', CiState::Pending), ('6', CiState::Failure)]).await,
        1
    );

    // Seeing the same red again does not record the pair twice.
    send(
        &store,
        &mut n,
        Intake::Checks(ChecksEvent {
            installation: InstallationId(77),
            repo: repo(),
            head: sha('6'),
        }),
    )
    .await;
    poll_ci(&store, &[('6', CiState::Failure)]).await;
    let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM footprint_misses")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 1);

    let since = Utc.timestamp_opt(1_700_000_000, 0).unwrap();
    let input = store
        .shadow_report_input(&RepoFullName::parse("acme/widgets").unwrap(), since)
        .await
        .unwrap()
        .unwrap();
    let report = build_report(&input);
    assert_eq!(report.merged, 2);
    assert_eq!(report.red_commits, 1);
    assert_eq!(report.suspected_misses, 1);
    let md = report.to_markdown();
    assert!(md.contains("- #1 (`billing`) and #2 (`auth`)"), "{md}");
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn single_culprit_is_not_a_footprint_miss(pool: PgPool) {
    let store = Store::from_pool(pool);
    let mut n = 0;
    send(
        &store,
        &mut n,
        Intake::Installation(InstallationEvent::Created {
            installation: InstallationId(77),
            account: Login::new("acme"),
            repos: vec![repo()],
        }),
    )
    .await;
    send(&store, &mut n, push(&['0'])).await;
    poll_ci(&store, &[('0', CiState::Success)]).await;
    send(&store, &mut n, pr(1, 'a', PrEventKind::Opened, None, 1)).await;
    footprint(&store, 'a', &["billing"]).await;
    send(
        &store,
        &mut n,
        pr(1, 'a', PrEventKind::Merged, Some('5'), 2),
    )
    .await;
    send(&store, &mut n, push(&['5'])).await;
    assert_eq!(poll_ci(&store, &[('5', CiState::Failure)]).await, 0);
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn unknown_repository_has_no_report(pool: PgPool) {
    let store = Store::from_pool(pool);
    let since = Utc::now();
    assert!(
        store
            .shadow_report_input(&RepoFullName::parse("x/y").unwrap(), since)
            .await
            .unwrap()
            .is_none()
    );
}
