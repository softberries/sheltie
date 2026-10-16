//! Parsing recorded webhook payloads from `tests/fixtures`.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sheltie_core::intake::{InstallationEvent, Intake, PrEventKind};
use sheltie_core::{InstallationId, PrNumber, RepoId};
use sheltie_github::payload::{PayloadError, parse};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

fn pr(event: &str, file: &str) -> sheltie_core::intake::PrEvent {
    match parse(event, &fixture(file)).unwrap() {
        Some(Intake::Pr(e)) => e,
        other => panic!("expected a PR event, got {other:?}"),
    }
}

#[test]
fn opened() {
    let e = pr("pull_request", "pull_request.opened.json");
    assert_eq!(e.kind, PrEventKind::Opened);
    assert_eq!(e.installation, InstallationId(77));
    assert_eq!(e.repo.id, RepoId(1001));
    assert_eq!(e.repo.full_name.as_str(), "acme/widgets");
    assert_eq!(e.pr, PrNumber(7));
    assert_eq!(e.head.as_str(), "1".repeat(40));
    assert_eq!(e.base.as_str(), "0".repeat(40));
    assert_eq!(e.actor.as_str(), "agent-bot[bot]");
    assert_eq!(e.at.to_rfc3339(), "2026-10-16T10:00:00+00:00");
}

#[test]
fn synchronize_moves_the_head() {
    let e = pr("pull_request", "pull_request.synchronize.json");
    assert_eq!(e.kind, PrEventKind::Synchronized);
    assert_eq!(e.head.as_str(), "2".repeat(40));
}

#[test]
fn closed_and_merged_is_merged() {
    assert_eq!(
        pr("pull_request", "pull_request.closed.json").kind,
        PrEventKind::Merged
    );
}

#[test]
fn approval() {
    let e = pr("pull_request_review", "pull_request_review.submitted.json");
    assert_eq!(e.kind, PrEventKind::Approved);
    assert_eq!(e.actor.as_str(), "maintainer");
}

#[test]
fn installation_created_lists_repositories() {
    let Some(Intake::Installation(InstallationEvent::Created {
        installation,
        account,
        repos,
    })) = parse("installation", &fixture("installation.created.json")).unwrap()
    else {
        panic!("expected installation created");
    };
    assert_eq!(installation, InstallationId(77));
    assert_eq!(account.as_str(), "acme");
    let names: Vec<_> = repos.iter().map(|r| r.full_name.as_str()).collect();
    assert_eq!(names, ["acme/widgets", "acme/gadgets"]);
}

#[test]
fn repositories_added() {
    let Some(Intake::Installation(InstallationEvent::ReposChanged { added, removed, .. })) = parse(
        "installation_repositories",
        &fixture("installation_repositories.added.json"),
    )
    .unwrap() else {
        panic!("expected repositories changed");
    };
    assert_eq!(added.len(), 1);
    assert!(removed.is_empty());
}

#[test]
fn invalid_values_are_rejected() {
    let mut body: serde_json::Value =
        serde_json::from_slice(&fixture("pull_request.opened.json")).unwrap();
    body["pull_request"]["head"]["sha"] = "not-a-sha".into();
    let err = parse("pull_request", &serde_json::to_vec(&body).unwrap()).unwrap_err();
    assert!(matches!(err, PayloadError::Invalid { .. }), "{err}");

    let mut body: serde_json::Value =
        serde_json::from_slice(&fixture("pull_request.opened.json")).unwrap();
    body["repository"]["full_name"] = "acme/../../admin".into();
    assert!(parse("pull_request", &serde_json::to_vec(&body).unwrap()).is_err());

    let mut body: serde_json::Value =
        serde_json::from_slice(&fixture("pull_request.opened.json")).unwrap();
    body.as_object_mut().unwrap().remove("installation");
    assert!(parse("pull_request", &serde_json::to_vec(&body).unwrap()).is_err());
}

#[test]
fn pull_request_target_and_merge_commit() {
    let opened = pr("pull_request", "pull_request.opened.json");
    assert_eq!(opened.base_ref, "main");
    assert_eq!(opened.default_branch.as_deref(), Some("main"));
    assert_eq!(opened.merge_commit, None);
    let merged = pr("pull_request", "pull_request.closed.json");
    assert_eq!(
        merged.merge_commit.map(|c| c.to_string()),
        Some("3".repeat(40))
    );
}

#[test]
fn finished_checks_and_statuses_request_readiness() {
    for (event, file) in [
        ("check_suite", "check_suite.completed.json"),
        ("status", "status.json"),
    ] {
        let Some(Intake::Checks(e)) = parse(event, &fixture(file)).unwrap() else {
            panic!("{event}: expected a checks event");
        };
        assert_eq!(e.head.as_str(), "2".repeat(40));
        assert_eq!(e.repo.id, RepoId(1001));
    }
}

#[test]
fn sheltie_own_check_run_is_ignored() {
    assert!(
        parse("check_run", &fixture("check_run.completed.sheltie.json"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn push_to_default_branch() {
    let Some(Intake::Push(e)) = parse("push", &fixture("push.main.json")).unwrap() else {
        panic!("expected a push");
    };
    assert_eq!(
        (e.branch.as_str(), e.default_branch.as_str()),
        ("main", "main")
    );
    let commits: Vec<String> = e.commits.iter().map(ToString::to_string).collect();
    assert_eq!(commits, ["4".repeat(40), "3".repeat(40)]);
}
