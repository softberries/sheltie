//! Sweep storage: tips, verified prefix, culprits, confirmed misses.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use sheltie_core::intake::{InstallationEvent, Intake, PushEvent, RepoRef};
use sheltie_core::{CommitSha, DeliveryId, InstallationId, Login, PrNumber, RepoFullName, RepoId};
use sheltie_store::{CiState, Store};
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

async fn setup(pool: &PgPool) -> Store {
    let store = Store::from_pool(pool.clone());
    let repo = RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
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
    for (n, commits) in [vec!['0'], vec!['1', '2'], vec!['3']]
        .into_iter()
        .enumerate()
    {
        store
            .ingest(
                &DeliveryId::new(format!("p{n}")),
                "push",
                &Intake::Push(PushEvent {
                    installation: InstallationId(77),
                    repo: repo.clone(),
                    branch: "main".into(),
                    default_branch: "main".into(),
                    after: sha(*commits.last().unwrap()),
                    commits: commits.iter().map(|c| sha(*c)).collect(),
                }),
            )
            .await
            .unwrap();
    }
    store
}

async fn set_ci(store: &Store, states: &[(char, CiState)]) {
    for job in store.claim_branch_ci(50, Duration::ZERO).await.unwrap() {
        let c = job.sha.as_str().chars().next().unwrap();
        if let Some((_, s)) = states.iter().find(|(x, _)| *x == c) {
            store.apply_branch_ci(&job, *s).await.unwrap();
        }
    }
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn tips_and_the_verified_prefix(pool: PgPool) {
    let store = setup(&pool).await;
    // '1' is inside a push, not a tip.
    set_ci(
        &store,
        &[
            ('0', CiState::Success),
            ('2', CiState::Success),
            ('3', CiState::Failure),
        ],
    )
    .await;
    assert_eq!(store.newest_tip(REPO).await.unwrap().unwrap().sha, sha('3'));
    let green = store.newest_green_tip(REPO).await.unwrap().unwrap();
    assert_eq!(green.sha, sha('2'));
    assert!(store.set_verified(REPO, &green.sha).await.unwrap());
    assert!(
        !store.set_verified(REPO, &green.sha).await.unwrap(),
        "unchanged"
    );
    assert!(
        !store.set_verified(REPO, &sha('0')).await.unwrap(),
        "never moves back to an older commit"
    );
    let states = store
        .ci_states(REPO, &[sha('2'), sha('3'), sha('9')])
        .await
        .unwrap();
    assert_eq!(states.get(&sha('3')), Some(&CiState::Failure));
    assert!(!states.contains_key(&sha('9')));
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn culprits_are_recorded_and_updated(pool: PgPool) {
    let store = setup(&pool).await;
    assert!(store.latest_culprit(REPO).await.unwrap().is_none());
    store
        .record_culprit(
            REPO,
            &sha('2'),
            &sha('3'),
            "reverting",
            Some(&sha('7')),
            Some(PrNumber(1000)),
        )
        .await
        .unwrap();
    store
        .record_culprit(REPO, &sha('2'), &sha('3'), "reverted", None, None)
        .await
        .unwrap();
    let (culprit, red_seq) = store.latest_culprit(REPO).await.unwrap().unwrap();
    assert_eq!(
        (culprit.state.as_str(), culprit.revert_pr),
        ("reverted", Some(PrNumber(1000)))
    );
    assert_eq!(culprit.red, sha('3'));
    assert!(red_seq > 0);
    assert!(
        store
            .change_by_merge_commit(REPO, &sha('2'))
            .await
            .unwrap()
            .is_none(),
        "no change landed it"
    );
}
