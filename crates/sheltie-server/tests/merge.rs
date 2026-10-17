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
