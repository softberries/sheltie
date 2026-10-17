//! M7 "done when": two agents given overlapping issues are serialized automatically.
//! Real server, throwaway database, stateful fake GitHub; agents over MCP and by assignment.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use rmcp::ServiceExt as _;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use serde_json::{Value, json};
use sheltie_core::{IssueNumber, RepoFullName, RepoId};
use sheltie_github::WebhookSecret;
use sheltie_github::fake::FakeGitHub;
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig};
use sheltie_github::testing::{OidcSigner, actions_claims, push_claims};
use sheltie_server::{AgentsConfig, Config, GitHubConfig, Server};
use sheltie_store::{PoolConfig, Store};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::sync::oneshot;

const SECRET: &str = "coordination-e2e-secret";
const REPO: RepoId = RepoId(1001);

struct Env {
    fake: FakeGitHub,
    signer: OidcSigner,
    store: Store,
    addr: std::net::SocketAddr,
    stop: Option<oneshot::Sender<()>>,
    handle: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
    deliveries: u32,
}

impl Env {
    async fn start(database: PgConnectOptions) -> Self {
        let signer = OidcSigner::new("k1");
        let fake = FakeGitHub::start(&signer).await;
        let key = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("coordination-key-{}.pem", std::process::id()));
        std::fs::write(&key, sheltie_github::testing::rsa_pem()).unwrap();
        let config = Config {
            database: database.clone(),
            bind: "127.0.0.1:0".parse().unwrap(),
            pool: PoolConfig::default(),
            agents: AgentsConfig::default(),
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

    async fn issue(
        &mut self,
        action: &str,
        n: u64,
        title: &str,
        body: &str,
        assignee: Option<&str>,
    ) {
        let mut payload = json!({
            "action": action,
            "issue": { "number": n, "title": title, "body": body,
                       "updated_at": format!("2026-10-21T10:{:02}:00Z", self.deliveries % 60) },
            "repository": { "id": 1001, "full_name": "acme/widgets", "default_branch": "main" },
            "installation": { "id": 77 }
        });
        if let Some(who) = assignee {
            payload["assignee"] = json!({ "login": who });
        }
        self.webhook("issues", payload).await;
    }

    /// shop::cart reads shop::pricing; shop::catalog stands alone.
    async fn upload_graph(&self) {
        // A pull request's run cannot replace the default branch's graph.
        assert_eq!(
            self.post_graph(&actions_claims("acme/widgets", 1001)).await,
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            self.post_graph(&push_claims("acme/widgets", 1001, "main"))
                .await,
            reqwest::StatusCode::ACCEPTED
        );
    }

    async fn post_graph(&self, claims: &Value) -> reqwest::StatusCode {
        let target = |reads: Value| {
            json!({
                "zone": { "group": "shop" }, "iface": "i", "public_iface": "p",
                "reads": reads, "tests": [], "hermetic": true
            })
        };
        let snapshot = json!({
            "targets": {
                "shop::cart": target(json!({ "shop::pricing": "local" })),
                "shop::pricing": target(json!({})),
                "shop::catalog": target(json!({}))
            },
            "owners": {
                "src/cart.rs": ["shop::cart"],
                "src/pricing.rs": ["shop::pricing"],
                "src/catalog.rs": ["shop::catalog"]
            }
        });
        let r = reqwest::Client::new()
            .post(format!("http://{}/v1/graphs", self.addr))
            .bearer_auth(self.signer.sign(claims))
            .json(&json!({ "commit": "c".repeat(40), "snapshot": snapshot }))
            .send()
            .await
            .unwrap();
        r.status()
    }

    /// Three issues: #1 pricing, #2 cart (reads pricing: overlaps #1), #3 catalog.
    async fn issues(&mut self) {
        self.issue(
            "opened",
            1,
            "Round prices to cents",
            "Change the rounding in src/pricing.rs.",
            None,
        )
        .await;
        self.issue(
            "opened",
            2,
            "Show cart discounts",
            "Edit src/cart.rs to show discounts.",
            None,
        )
        .await;
        self.issue(
            "opened",
            3,
            "Sort the catalog",
            "Sort products in src/catalog.rs.",
            None,
        )
        .await;
    }

    async fn token(&self, name: &str) -> String {
        let token = format!(
            "{}{name}-0123456789abcdef",
            sheltie_api::agents::TOKEN_PREFIX
        );
        let repo = RepoFullName::parse("acme/widgets").unwrap();
        let hash = sheltie_api::agents::token_hash(&token);
        assert!(
            self.store
                .create_token(&repo, name, &hash)
                .await
                .unwrap()
                .is_some()
        );
        token
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

    async fn wait_lease(&self, issue: u64, holder: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let leases = self.store.active_leases(REPO).await.unwrap();
            if leases
                .iter()
                .any(|l| l.issue == IssueNumber(issue) && l.holder == holder)
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "no lease on #{issue} for {holder}: {leases:?}"
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

struct Mcp(rmcp::service::RunningService<rmcp::RoleClient, ()>);

impl Mcp {
    async fn connect(addr: std::net::SocketAddr, token: &str) -> Self {
        let config = StreamableHttpClientTransportConfig::with_uri(format!("http://{addr}/mcp"))
            .auth_header(token.to_owned());
        Self(
            ().serve(StreamableHttpClientTransport::from_config(config))
                .await
                .unwrap(),
        )
    }

    async fn call(&self, tool: &'static str, args: Value) -> Value {
        let args = serde_json::from_value(args).unwrap();
        let result = self
            .0
            .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
            .await
            .unwrap();
        result.structured_content.unwrap()
    }
}

fn numbers(safe: &Value) -> Vec<u64> {
    safe.as_array()
        .map(|a| a.iter().map(|i| i["issue"].as_u64().unwrap()).collect())
        .unwrap_or_else(|| {
            safe["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["issue"].as_u64().unwrap())
                .collect()
        })
}

#[sqlx::test(migrations = false)]
async fn agents_over_mcp_are_serialized(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    env.issues().await;
    env.upload_graph().await;
    let a = Mcp::connect(env.addr, &env.token("agent-a").await).await;
    let b = Mcp::connect(env.addr, &env.token("agent-b").await).await;

    let tools: Vec<String> =
        a.0.list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
    for t in ["predict_footprint", "safe_issues", "claim_issue", "release"] {
        assert!(tools.contains(&t.to_owned()), "{tools:?}");
    }
    let p = a
        .call("predict_footprint", json!({ "paths": ["src/pricing.rs"] }))
        .await;
    assert_eq!(p["writes"], json!(["shop::pricing"]));
    assert_eq!(p["reads"], json!(["shop::cart", "shop::pricing"]));

    // #1 and #2 overlap; #3 is independent of both.
    assert_eq!(numbers(&a.call("safe_issues", json!({})).await), [1, 3]);
    let granted = a.call("claim_issue", json!({ "issue": 1 })).await;
    assert_eq!(granted["status"], "granted", "{granted}");

    let blocked = b.call("claim_issue", json!({ "issue": 2 })).await;
    assert_eq!(blocked["status"], "blocked", "{blocked}");
    assert_eq!(blocked["blocked_by"]["lease"]["issue"], 1);
    assert_eq!(blocked["blocked_by"]["lease"]["holder"], "agent-a");
    assert_eq!(
        numbers(&b.call("safe_issues", json!({})).await),
        [3],
        "#2 waits for #1"
    );
    assert_eq!(
        b.call("claim_issue", json!({ "issue": 1 })).await["status"],
        "taken"
    );

    // Agent A's pull request fixes #1 and lands: the lease ends and #2 can be claimed.
    let head = { env.fake.state().refs["main"].clone() };
    for (action, merged) in [("opened", false), ("closed", true)] {
        env.webhook("pull_request", json!({
            "action": action, "number": 10,
            "pull_request": {
                "number": 10, "merged": merged, "updated_at": format!("2026-10-21T11:00:0{}Z", u8::from(merged)),
                "head": { "sha": head, "ref": "agent-a/1" }, "base": { "sha": head, "ref": "main" },
                "body": "Rounds prices.\n\nFixes #1", "merge_commit_sha": if merged { json!(head) } else { json!(null) }
            },
            "repository": { "id": 1001, "full_name": "acme/widgets", "default_branch": "main" },
            "installation": { "id": 77 }, "sender": { "login": "agent-a" }
        }))
        .await;
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let reply = b.call("claim_issue", json!({ "issue": 2 })).await;
        if reply["status"] == "granted" {
            break;
        }
        assert!(Instant::now() < deadline, "still {reply}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    env.wait_lease(2, "agent-b").await;

    // Without a token, nothing is served.
    let r = reqwest::Client::new()
        .get(format!("http://{}/v1/issues/safe", env.addr))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::UNAUTHORIZED);
    env.stop().await;
}

#[sqlx::test(migrations = false)]
async fn agents_by_assignment_are_serialized(_: PgPoolOptions, db: PgConnectOptions) {
    let mut env = Env::start(db).await;
    env.issues().await;
    env.upload_graph().await;
    env.wait_fake("a footprint note on #1", |st| {
        st.comments
            .iter()
            .any(|(n, b)| *n == 1 && b.contains("`shop::pricing`"))
    })
    .await;

    env.issue(
        "assigned",
        1,
        "Round prices to cents",
        "Change the rounding in src/pricing.rs.",
        Some("agent-a"),
    )
    .await;
    env.wait_lease(1, "agent-a").await;
    env.issue(
        "assigned",
        2,
        "Show cart discounts",
        "Edit src/cart.rs to show discounts.",
        Some("agent-b"),
    )
    .await;
    env.wait_fake("#2 blocked", |st| {
        st.labels
            .contains(&(2, sheltie_api::coordination::BLOCKED_LABEL.into()))
            && st
                .comments
                .iter()
                .any(|(n, b)| *n == 2 && b.contains("overlaps #1"))
    })
    .await;
    assert!(
        !env.store
            .active_leases(REPO)
            .await
            .unwrap()
            .iter()
            .any(|l| l.issue == IssueNumber(2)),
        "not claimed while blocked"
    );
    // An independent issue is claimed right away.
    env.issue(
        "assigned",
        3,
        "Sort the catalog",
        "Sort products in src/catalog.rs.",
        Some("agent-c"),
    )
    .await;
    env.wait_lease(3, "agent-c").await;

    // #1 is closed: #2 is claimed for agent-b and unblocked.
    env.issue(
        "closed",
        1,
        "Round prices to cents",
        "Change the rounding in src/pricing.rs.",
        None,
    )
    .await;
    env.wait_lease(2, "agent-b").await;
    env.wait_fake("#2 unblocked", |st| {
        !st.labels.iter().any(|(n, _)| *n == 2)
            && st
                .comments
                .iter()
                .any(|(n, b)| *n == 2 && b.contains("now claimed for `agent-b`"))
    })
    .await;
    let blocked_comments = env
        .fake
        .state()
        .comments
        .iter()
        .filter(|(n, b)| *n == 2 && b.contains("overlaps"))
        .count();
    assert_eq!(blocked_comments, 1, "blocked is announced once");
    env.stop().await;
}
