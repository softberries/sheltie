//! M4 "done when" (automated part): independent PRs land in parallel through the
//! compare-and-swap, conflicting ones are verified or returned. Real server, throwaway
//! database, stateful fake GitHub.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sheltie_core::{ChangeState, CommitSha, PrNumber, RepoFullName, RepoId};
use sheltie_github::WebhookSecret;
use sheltie_github::fake::{FakeGitHub, FakePull};
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig};
use sheltie_github::testing::{OidcSigner, actions_claims};
use sheltie_server::{Config, GitHubConfig, Server};
use sheltie_store::{PoolConfig, RepoMode, Store};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;

const SECRET: &str = "merge-e2e-secret";

struct Env {
    fake: FakeGitHub,
    signer: OidcSigner,
    store: Store,
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    deliveries: u32,
    root: String,
}

impl Env {
    async fn start(database: PgConnectOptions) -> Self {
        let signer = OidcSigner::new("k1");
        let fake = FakeGitHub::start(&signer).await;
        let root = fake.state().refs["main"].clone();
        let key = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("merge-key-{}.pem", std::process::id()));
        std::fs::write(&key, sheltie_github::testing::rsa_pem()).unwrap();
        let config = Config {
            database: database.clone(),
            bind: "127.0.0.1:0".parse().unwrap(),
            pool: PoolConfig::default(),
            epoch: Duration::from_millis(200),
            github: Some(GitHubConfig {
                app_id: 1,
                private_key_path: key,
                webhook_secret: WebhookSecret::new(SECRET).unwrap(),
                api_url: Some(fake.uri().to_owned()),
                oidc: OidcConfig {
                    issuer: GITHUB_ACTIONS_ISSUER.into(),
                    audience: "sheltie".into(),
                    jwks_url: format!("{}/.well-known/jwks", fake.uri()),
                },
            }),
        };
        let server = Server::start(config).await.unwrap();
        let addr = server.local_addr().unwrap();
        let (stop, stopped) = oneshot::channel::<()>();
        let handle = tokio::spawn(server.serve(async {
            stopped.await.ok();
        }));
        let store = Store::connect(database, &PoolConfig::default())
            .await
            .unwrap();
        Self {
            fake,
            signer,
            store,
            addr,
            stop: Some(stop),
            handle: Some(handle),
            deliveries: 0,
            root,
        }
    }

    async fn webhook(&mut self, event: &str, body: Value) {
        self.deliveries += 1;
        let body = serde_json::to_vec(&body).unwrap();
        let r = reqwest::Client::new()
            .post(format!("http://{}/webhooks/github", self.addr))
            .header("x-github-event", event)
            .header("x-github-delivery", format!("d{}", self.deliveries))
            .header(
                "x-hub-signature-256",
                WebhookSecret::new(SECRET).unwrap().sign(&body),
            )
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "{event}: {}", r.status());
    }

    fn repository() -> Value {
        json!({ "id": 1001, "full_name": "acme/widgets", "default_branch": "main" })
    }

    async fn push_main(&mut self) {
        let head = self.fake.state().refs["main"].clone();
        self.webhook(
            "push",
            json!({
                "ref": "refs/heads/main", "after": head, "commits": [{ "id": head }],
                "repository": Self::repository(), "installation": { "id": 77 }
            }),
        )
        .await;
    }

    /// Opens PR `n` with a head commit on top of the root.
    async fn open_pr(&mut self, n: u64, title: &str) -> String {
        let head = {
            let mut st = self.fake.state();
            let root = self.root.clone();
            let head = st.commit(&[&root], title);
            st.pulls.insert(
                n,
                FakePull {
                    title: title.into(),
                    head: head.clone(),
                    ready: true,
                },
            );
            head
        };
        self.webhook("pull_request", json!({
            "action": "opened", "number": n,
            "pull_request": {
                "number": n, "merged": false, "updated_at": format!("2026-10-18T10:00:{n:02}Z"),
                "head": { "sha": head, "ref": format!("agent/{n}") },
                "base": { "sha": self.root, "ref": "main" }
            },
            "repository": Self::repository(), "installation": { "id": 77 }, "sender": { "login": "agent" }
        }))
        .await;
        head
    }

    async fn footprint(&self, head: &str, writes: &[&str]) {
        let report = json!({
            "adapter": "test", "base": self.root, "head": head, "base_tree": "e".repeat(40),
            "diff_hash": format!("sha256:{head}"),
            "footprint": { "writes": writes, "reads": writes, "tests": [], "leaky": false },
            "leak_reasons": []
        });
        let r = reqwest::Client::new()
            .post(format!("http://{}/v1/footprints", self.addr))
            .bearer_auth(self.signer.sign(&actions_claims("acme/widgets", 1001)))
            .json(&report)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::ACCEPTED);
    }

    async fn state(&self, pr: u64, head: &str) -> Option<ChangeState> {
        self.store
            .change_state(RepoId(1001), PrNumber(pr), &CommitSha::parse(head).unwrap())
            .await
            .unwrap()
    }

    async fn wait_state(&self, pr: u64, head: &str, want: ChangeState) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let got = self.state(pr, head).await;
            if got == Some(want) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "#{pr} is {got:?}, expected {want}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn stop(mut self) {
        self.stop.take().unwrap().send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.handle.take().unwrap())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[sqlx::test(migrations = false)]
async fn independent_prs_land_and_conflicting_ones_are_verified_or_returned(
    _: PgPoolOptions,
    db: PgConnectOptions,
) {
    let mut env = Env::start(db).await;
    env.push_main().await;
    let h1 = env.open_pr(1, "Billing retries").await;
    let h2 = env.open_pr(2, "Auth scopes").await;
    let h3 = env.open_pr(3, "Billing rounding").await;
    env.footprint(&h1, &["billing"]).await;
    env.footprint(&h2, &["auth"]).await;
    env.footprint(&h3, &["billing"]).await;
    // All three become ready in shadow mode, so the first merge-mode epoch sees them together.
    for (n, h) in [(1, &h1), (2, &h2), (3, &h3)] {
        env.wait_state(n, h, ChangeState::Ready).await;
    }
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );

    env.wait_state(2, &h2, ChangeState::Landed).await;
    env.wait_state(1, &h1, ChangeState::Landed).await;
    env.wait_state(3, &h3, ChangeState::Returned).await;
    env.wait_comment(3, "billing").await;

    {
        let st = env.fake.state();
        let chain = st.first_parents("main");
        assert_eq!(chain.len(), 3, "two merge commits on top of the root");
        let (m1, m2) = (&st.commits[&chain[0]], &st.commits[&chain[1]]);
        assert_eq!(m2.parents, [env.root.clone(), h2.clone()]);
        assert!(
            m2.message.contains("Merge #2: Auth scopes")
                && m2.message.contains("Sheltie-Verified: footprint"),
            "{}",
            m2.message
        );
        assert_eq!(m1.parents, [chain[1].clone(), h1.clone()]);
        assert!(
            m1.message.contains("Sheltie-Verified: speculation"),
            "{}",
            m1.message
        );
        assert!(
            st.comments
                .iter()
                .any(|(n, body)| *n == 3 && body.contains("billing")),
            "{:?}",
            st.comments
        );
        assert!(
            st.refs.keys().all(|b| !b.starts_with("sheltie/")),
            "work branches cleaned up: {:?}",
            st.refs.keys()
        );
    }
    env.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_lost_race_goes_to_verification_and_still_lands(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    env.push_main().await;
    let h1 = env.open_pr(1, "Billing retries").await;
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );
    // Someone pushes to main while Sheltie is landing #1: the CAS loses, and the unknown
    // commit means #1 must be verified on the new head before it lands.
    env.fake.state().race_on = Some("main".into());
    env.footprint(&h1, &["billing"]).await;
    env.wait_state(1, &h1, ChangeState::Landed).await;

    {
        let st = env.fake.state();
        let chain = st.first_parents("main");
        assert_eq!(chain.len(), 3);
        assert_eq!(st.commits[&chain[1]].message, "outside push");
        assert!(
            st.commits[&chain[0]]
                .message
                .contains("Sheltie-Verified: speculation")
        );
    }
    env.stop().await;
}

#[sqlx::test(migrations = false)]
async fn failed_verification_returns_the_change(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    // The base is not on the tracked history, so the change must be verified first.
    let h1 = env.open_pr(1, "Billing retries").await;
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );
    env.fake.state().verify_ci = Some("FAILURE".into());
    env.footprint(&h1, &["billing"]).await;
    env.wait_state(1, &h1, ChangeState::Returned).await;
    env.wait_comment(1, "CI failed").await;

    {
        let st = env.fake.state();
        assert_eq!(st.first_parents("main").len(), 1, "nothing landed");
        assert!(
            st.comments
                .iter()
                .any(|(n, b)| *n == 1 && b.contains("CI failed")),
            "{:?}",
            st.comments
        );
        assert!(st.refs.keys().all(|b| !b.starts_with("sheltie/")));
    }
    env.stop().await;
}

impl Env {
    fn main_head(&self) -> String {
        self.fake.state().refs["main"].clone()
    }

    /// Waits for a comment on PR `n` containing `needle`. Sheltie records a state change before
    /// it comments, so tests wait for the comment itself.
    async fn wait_comment(&self, n: u64, needle: &str) {
        self.wait_fake(&format!("a comment on #{n} with {needle:?}"), |st| {
            st.comments
                .iter()
                .any(|(pr, body)| *pr == n && body.contains(needle))
        })
        .await;
    }

    async fn wait_fake(&self, what: &str, pred: impl Fn(&sheltie_github::fake::FakeState) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if pred(&self.fake.state()) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// M4 "done when", second half: a seeded bad commit is found and reverted automatically.
#[sqlx::test(migrations = false)]
async fn a_bad_landing_is_blamed_and_reverted(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    let root = env.root.clone();
    env.fake.state().ci.insert(root.clone(), "SUCCESS".into());
    env.push_main().await;

    // #1 lands: the seeded bad commit. CI on main turns red.
    let h1 = env.open_pr(1, "Billing retries").await;
    env.footprint(&h1, &["billing"]).await;
    env.wait_state(1, &h1, ChangeState::Ready).await;
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );
    env.wait_state(1, &h1, ChangeState::Landed).await;
    let m1 = env.main_head();
    env.fake.state().ci.insert(m1.clone(), "FAILURE".into());
    env.push_main().await;

    // #2 (independent) lands on top; main stays red, so the failure is not a flake.
    let h2 = env.open_pr(2, "Auth scopes").await;
    env.footprint(&h2, &["auth"]).await;
    env.wait_state(2, &h2, ChangeState::Landed).await;
    let m2 = env.main_head();
    env.fake.state().ci.insert(m2.clone(), "FAILURE".into());
    env.push_main().await;

    env.wait_fake("the revert to land", |st| {
        st.refs
            .get("main")
            .and_then(|h| st.commits.get(h))
            .is_some_and(|c| c.message.contains("Sheltie-Revert"))
    })
    .await;
    env.wait_comment(1, "reverted it in #1000").await;
    {
        let st = env.fake.state();
        let head = &st.commits[&st.refs["main"]];
        assert!(
            head.message.contains(&format!("Sheltie-Revert: {m1}")),
            "{}",
            head.message
        );
        assert!(
            head.message.starts_with("Revert #1: Billing retries"),
            "{}",
            head.message
        );
        assert_eq!(
            head.parents[0], m2,
            "the revert lands on top of the red head"
        );
        assert_eq!(
            st.refs.get(sheltie_server::VERIFIED_BRANCH),
            Some(&root),
            "verified prefix stays at the last green commit"
        );
        assert!(
            st.opened.iter().any(|(_, title, _)| title == "Revert #1"),
            "{:?}",
            st.opened
        );
        assert!(
            st.comments
                .iter()
                .any(|(n, b)| *n == 1 && b.contains("reverted it in #1000")),
            "{:?}",
            st.comments
        );
        assert!(
            st.refs.keys().all(|b| !b.starts_with("sheltie/bisect/")),
            "probe branches cleaned up"
        );
    }
    env.stop().await;
}

#[sqlx::test(migrations = false)]
async fn a_single_red_commit_is_not_reverted(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    let root = env.root.clone();
    env.fake.state().ci.insert(root, "SUCCESS".into());
    env.push_main().await;
    let h1 = env.open_pr(1, "Billing retries").await;
    env.footprint(&h1, &["billing"]).await;
    env.wait_state(1, &h1, ChangeState::Ready).await;
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );
    env.wait_state(1, &h1, ChangeState::Landed).await;
    let m1 = env.main_head();
    env.fake.state().ci.insert(m1.clone(), "FAILURE".into());
    env.push_main().await;
    // Give the sweeper several rounds: a lone red may be flaky, so nothing is reverted.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(env.main_head(), m1);
    assert!(env.fake.state().opened.is_empty());
    env.stop().await;
}

impl Env {
    async fn footprint_rw(&self, head: &str, writes: &[&str], reads: &[&str]) {
        let mut all: Vec<&str> = writes.to_vec();
        all.extend_from_slice(reads);
        let report = json!({
            "adapter": "test", "base": self.root, "head": head, "base_tree": "e".repeat(40),
            "diff_hash": format!("sha256:{head}"),
            "footprint": { "writes": writes, "reads": all, "tests": [], "leaky": false },
            "leak_reasons": []
        });
        let r = reqwest::Client::new()
            .post(format!("http://{}/v1/footprints", self.addr))
            .bearer_auth(self.signer.sign(&actions_claims("acme/widgets", 1001)))
            .json(&report)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), reqwest::StatusCode::ACCEPTED);
    }
}

/// M5a: a semantic chain is built as prefixes; the longest green prefix lands, the change
/// that broke the build is returned, and the rest lands in a later round.
#[sqlx::test(migrations = false)]
async fn a_speculative_chain_lands_its_green_prefix(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    env.push_main().await;
    // A writes core; B reads core and writes api; C reads api and writes web: one chain.
    let a = env.open_pr(1, "Core").await;
    let b = env.open_pr(2, "Api").await;
    let c = env.open_pr(3, "Web").await;
    env.fake.state().failing_heads.insert(b.clone());
    env.footprint_rw(&a, &["core"], &[]).await;
    env.footprint_rw(&b, &["api"], &["core"]).await;
    env.footprint_rw(&c, &["web"], &["api"]).await;
    for (n, h) in [(1, &a), (2, &b), (3, &c)] {
        env.wait_state(n, h, ChangeState::Ready).await;
    }
    assert!(
        env.store
            .set_repo_mode(
                &RepoFullName::parse("acme/widgets").unwrap(),
                RepoMode::Merge
            )
            .await
            .unwrap()
    );
    env.wait_state(1, &a, ChangeState::Landed).await;
    env.wait_state(2, &b, ChangeState::Returned).await;
    env.wait_state(3, &c, ChangeState::Landed).await;
    env.wait_comment(2, "together with #1").await;
    {
        let st = env.fake.state();
        let chain = st.first_parents("main");
        assert_eq!(chain.len(), 3, "A and C landed on top of the root");
        assert_eq!(st.commits[&chain[1]].parents[1], a);
        assert_eq!(st.commits[&chain[0]].parents[1], c);
        assert!(
            st.comments
                .iter()
                .any(|(n, body)| *n == 2 && body.contains("together with #1")),
            "{:?}",
            st.comments
        );
        assert!(
            st.refs.keys().all(|r| !r.starts_with("sheltie/spec/")),
            "{:?}",
            st.refs.keys()
        );
    }
    env.stop().await;
}

impl Env {
    async fn merge_report(&self, report: Value) -> reqwest::StatusCode {
        reqwest::Client::new()
            .post(format!("http://{}/v1/merges", self.addr))
            .bearer_auth(self.signer.sign(&actions_claims("acme/widgets", 1001)))
            .json(&report)
            .send()
            .await
            .unwrap()
            .status()
    }

    /// Two ready PRs writing `billing`; merge mode and the merge engine on. Returns the heads.
    async fn conflicting_pair(&mut self) -> (String, String) {
        self.push_main().await;
        let h1 = self.open_pr(1, "Billing retries").await;
        let h2 = self.open_pr(2, "Billing rounding").await;
        self.footprint(&h1, &["billing"]).await;
        self.footprint(&h2, &["billing"]).await;
        self.wait_state(1, &h1, ChangeState::Ready).await;
        self.wait_state(2, &h2, ChangeState::Ready).await;
        let repo = RepoFullName::parse("acme/widgets").unwrap();
        assert!(self.store.set_merge_engine(&repo, true).await.unwrap());
        assert!(
            self.store
                .set_repo_mode(&repo, RepoMode::Merge)
                .await
                .unwrap()
        );
        self.wait_state(1, &h1, ChangeState::Landed).await;
        self.wait_fake("a merge dispatch", |st| !st.dispatches.is_empty())
            .await;
        (h1, h2)
    }
}

/// M5 "done when": a conflict is resolved by the merge engine (here tier 2) and lands.
#[sqlx::test(migrations = false)]
async fn merge_engine_resolution_is_verified_and_lands(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    let (_, h2) = env.conflicting_pair().await;
    let (event, payload) = env.fake.state().dispatches[0].clone();
    assert_eq!(event, sheltie_server::MERGE_EVENT);
    assert_eq!(payload["pr"], 2);
    assert_eq!(payload["head"], h2);
    let onto = payload["onto"].as_str().unwrap().to_owned();
    assert_eq!(onto, env.main_head(), "merged onto the current head");

    // The runner merges and reports: Cargo.lock was regenerated.
    let result = {
        let mut st = env.fake.state();
        let sha = st.commit(&[&onto, &h2], "Merge #2: Billing rounding");
        st.ci.insert(sha.clone(), "SUCCESS".into());
        sha
    };
    let status = env
        .merge_report(json!({
            "pr": 2, "head": h2, "onto": onto, "result": result,
            "files": [{ "path": "Cargo.lock", "tier": "regenerate" }], "unresolved": []
        }))
        .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);

    env.wait_state(2, &h2, ChangeState::Landed).await;
    assert_eq!(env.main_head(), result, "the engine's merge commit landed");
    let id: i64 = sqlx::query_scalar("SELECT id FROM changes WHERE pr_number = 2")
        .fetch_one(env.store.pool())
        .await
        .unwrap();
    assert_eq!(
        env.store.resolution_tier(id).await.unwrap(),
        Some(sheltie_core::ResolutionTier::Regenerate)
    );
    env.stop().await;
}

#[sqlx::test(migrations = false)]
async fn unresolved_merge_returns_the_change(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    let (_, h2) = env.conflicting_pair().await;
    let onto = env.fake.state().dispatches[0].1["onto"]
        .as_str()
        .unwrap()
        .to_owned();
    // A report for an unrelated head is refused.
    let wrong = env
        .merge_report(json!({ "pr": 2, "head": "9".repeat(40), "onto": onto, "result": null, "files": [], "unresolved": ["x"] }))
        .await;
    assert_eq!(wrong, reqwest::StatusCode::CONFLICT);
    let status = env
        .merge_report(json!({ "pr": 2, "head": h2, "onto": onto, "result": null, "files": [], "unresolved": ["src/billing.rs"] }))
        .await;
    assert_eq!(status, reqwest::StatusCode::ACCEPTED);
    env.wait_state(2, &h2, ChangeState::Returned).await;
    env.wait_comment(2, "`src/billing.rs`").await;
    env.stop().await;
}
