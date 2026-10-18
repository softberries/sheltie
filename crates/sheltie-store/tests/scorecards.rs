//! Agent scorecards from stored pull request events.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use chrono::Utc;
use sheltie_core::agents::{AgentRule, PEOPLE, PrOrigin, UNKNOWN};
use sheltie_core::intake::{InstallationEvent, Intake, IntentField, PrEvent, PrEventKind, RepoRef};
use sheltie_core::{CommitSha, DeliveryId, InstallationId, Login, PrNumber, RepoFullName, RepoId};
use sheltie_store::Store;
use sqlx::PgPool;

const REPO: RepoId = RepoId(1001);

fn repo() -> RepoRef {
    RepoRef {
        id: REPO,
        full_name: RepoFullName::parse("acme/widgets").unwrap(),
    }
}

fn sha(c: char) -> CommitSha {
    CommitSha::parse(c.to_string().repeat(40)).unwrap()
}

fn origin(author: &str, bot: bool, branch: &str) -> Option<Box<PrOrigin>> {
    Some(Box::new(PrOrigin {
        author: Login::new(author),
        bot,
        head_ref: branch.into(),
        labels: vec!["agent".into()],
    }))
}

fn pr(n: u64, head: char, kind: PrEventKind, secs: i64, origin: Option<Box<PrOrigin>>) -> Intake {
    Intake::Pr(PrEvent {
        installation: InstallationId(77),
        repo: repo(),
        pr: PrNumber(n),
        head: sha(head),
        base: sha('0'),
        actor: Login::new("someone"),
        kind,
        at: Utc::now() + chrono::Duration::seconds(secs),
        base_ref: "main".into(),
        default_branch: Some("main".into()),
        merge_commit: (kind == PrEventKind::Merged).then(|| sha('f')),
        intent: IntentField::Unknown,
        closes: None,
        title: None,
        origin,
    })
}

async fn send(store: &Store, n: &mut u32, intake: Intake) {
    *n += 1;
    store
        .ingest(&DeliveryId::new(format!("d{n}")), "test", &intake)
        .await
        .unwrap();
}

#[sqlx::test(migrator = "sheltie_store::MIGRATOR")]
async fn scorecards_group_pull_requests_by_agent(pool: PgPool) {
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
    // #1: a bot, pushed twice, merged.
    send(
        &store,
        &mut n,
        pr(
            1,
            'a',
            PrEventKind::Opened,
            1,
            origin("claude[bot]", true, "claude/a"),
        ),
    )
    .await;
    send(
        &store,
        &mut n,
        pr(
            1,
            'b',
            PrEventKind::Synchronized,
            2,
            origin("claude[bot]", true, "claude/a"),
        ),
    )
    .await;
    send(
        &store,
        &mut n,
        pr(
            1,
            'b',
            PrEventKind::Merged,
            3,
            origin("claude[bot]", true, "claude/a"),
        ),
    )
    .await;
    // #2: a person on a codex/ branch, changes requested, closed unmerged.
    send(
        &store,
        &mut n,
        pr(
            2,
            'c',
            PrEventKind::Opened,
            1,
            origin("kris", false, "codex/b"),
        ),
    )
    .await;
    send(
        &store,
        &mut n,
        pr(2, 'c', PrEventKind::ChangesRequested, 2, None),
    )
    .await;
    send(
        &store,
        &mut n,
        pr(
            2,
            'c',
            PrEventKind::Closed,
            3,
            origin("kris", false, "codex/b"),
        ),
    )
    .await;
    // #3: a person, still open; #4: seen before authors were recorded.
    send(
        &store,
        &mut n,
        pr(3, 'd', PrEventKind::Opened, 1, origin("ana", false, "fix")),
    )
    .await;
    send(&store, &mut n, pr(4, 'e', PrEventKind::Opened, 1, None)).await;

    let codex = AgentRule {
        name: "codex".into(),
        model: Some("gpt-5-codex".into()),
        author: None,
        branch: Some("codex/".into()),
        label: None,
    };
    store
        .set_agent_rules(REPO, std::slice::from_ref(&codex))
        .await
        .unwrap();
    assert_eq!(store.agent_rules(REPO).await.unwrap(), vec![codex.clone()]);

    let cards = store.agent_scorecards(REPO, 30).await.unwrap();
    let find = |agent: &str| cards.iter().find(|c| c.key.agent == agent).unwrap();
    let claude = find("claude");
    assert_eq!((claude.prs, claude.merged, claude.closed), (1, 1, 0));
    assert_eq!(claude.heads_per_pr, 2.0);
    assert_eq!(claude.merge_rate, Some(1.0));
    let codex_card = find("codex");
    assert_eq!(codex_card.key.model.as_deref(), Some("gpt-5-codex"));
    assert_eq!(
        (codex_card.prs, codex_card.merged, codex_card.closed),
        (1, 0, 1)
    );
    assert_eq!(codex_card.merge_rate, Some(0.0));
    assert_eq!(find(PEOPLE).prs, 1);
    assert_eq!(find(UNKNOWN).prs, 1);

    let outcomes = store.pr_outcomes(REPO, 30).await.unwrap();
    assert_eq!(outcomes.iter().map(|o| o.changes_requested).sum::<u32>(), 1);
    assert!(
        outcomes
            .iter()
            .flat_map(|o| &o.origin)
            .all(|o| o.labels == ["agent"])
    );

    // Clearing the rules; invalid rules are refused.
    store.set_agent_rules(REPO, &[]).await.unwrap();
    assert!(store.agent_rules(REPO).await.unwrap().is_empty());
    let bad = AgentRule {
        branch: None,
        ..codex
    };
    assert!(store.set_agent_rules(REPO, &[bad]).await.is_err());
}
