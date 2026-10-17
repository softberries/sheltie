//! The dashboard end to end: GitHub sign-in through the fake, what a user sees, and that a
//! user only sees repositories they can access.

// Integration-test helpers are not `#[test]` functions, so clippy's test exemption misses them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use serde_json::{Value, json};
use sheltie_github::WebhookSecret;
use sheltie_github::fake::FakeGitHub;
use sheltie_github::oidc::{GITHUB_ACTIONS_ISSUER, OidcConfig};
use sheltie_github::testing::OidcSigner;
use sheltie_server::{AgentsConfig, Config, DashboardConfig, GitHubConfig, Server};
use sheltie_store::PoolConfig;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};

const SECRET: &str = "dashboard-e2e-secret";

async fn start(
    db: PgConnectOptions,
    fake: &FakeGitHub,
) -> (std::net::SocketAddr, tokio::sync::oneshot::Sender<()>) {
    let key = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("dash-key-{}.pem", std::process::id()));
    std::fs::write(&key, sheltie_github::testing::rsa_pem()).unwrap();
    let config = Config {
        database: db,
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
            dashboard: Some(DashboardConfig {
                public_url: "http://127.0.0.1".into(),
                client_id: "Iv1.test".into(),
                client_secret: secrecy::SecretString::from("shh"),
                web_url: Some(fake.uri().to_owned()),
            }),
        }),
    };
    let server = Server::start(config).await.unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(server.serve(async {
        stopped.await.ok();
    }));
    (addr, stop)
}

async fn webhook(addr: std::net::SocketAddr, n: u32, event: &str, body: Value) {
    let body = serde_json::to_vec(&body).unwrap();
    let r = reqwest::Client::new()
        .post(format!("http://{addr}/webhooks/github"))
        .header("x-github-event", event)
        .header("x-github-delivery", format!("d{n}"))
        .header(
            "x-hub-signature-256",
            WebhookSecret::new(SECRET).unwrap().sign(&body),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
}

fn cookie_value(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| {
            v.strip_prefix(&format!("{name}="))
                .map(|rest| rest.split(';').next().unwrap_or_default().to_owned())
        })
}

#[sqlx::test(migrations = false)]
async fn sign_in_and_see_your_repositories(_: PgPoolOptions, db: PgConnectOptions) {
    let signer = OidcSigner::new("k1");
    let fake = FakeGitHub::start(&signer).await;
    fake.state().oauth_user = Some(("octo-dev".into(), vec![1001]));
    let (addr, stop) = start(db, &fake).await;

    // Two repositories are known; the user may only see acme/widgets.
    for (n, (id, name, pr, title)) in [
        (1001, "acme/widgets", 7, "Round prices <b>to cents</b>"),
        (2002, "acme/secret", 9, "Secret work"),
    ]
    .into_iter()
    .enumerate()
    {
        let head = format!("{:040x}", n + 10);
        webhook(addr, n as u32, "pull_request", json!({
            "action": "opened", "number": pr,
            "pull_request": {
                "number": pr, "merged": false, "updated_at": "2026-10-18T10:00:00Z", "title": title,
                "head": { "sha": head, "ref": "x" }, "base": { "sha": "0".repeat(40), "ref": "main" }
            },
            "repository": { "id": id, "full_name": name, "default_branch": "main" },
            "installation": { "id": 77 }, "sender": { "login": "agent" }
        }))
        .await;
    }

    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let base = format!("http://{addr}");

    // Signed out: the sign-in page.
    let page = http
        .get(format!("{base}/dashboard"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(page.contains("Sign in with GitHub"));

    // Sign in: GitHub gets a state, and the callback must bring it back.
    let login = http.get(format!("{base}/auth/login")).send().await.unwrap();
    assert_eq!(login.status(), reqwest::StatusCode::SEE_OTHER);
    let location = login.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        location.starts_with(&format!(
            "{}/login/oauth/authorize?client_id=Iv1.test",
            fake.uri()
        )),
        "{location}"
    );
    let state = cookie_value(&login, "sheltie_oauth_state").unwrap();
    assert!(location.ends_with(&format!("state={state}")));

    let forged = http
        .get(format!(
            "{base}/auth/callback?code=good&state={}",
            "f".repeat(64)
        ))
        .header("cookie", format!("sheltie_oauth_state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        forged.status(),
        reqwest::StatusCode::BAD_REQUEST,
        "state must match"
    );

    let callback = http
        .get(format!("{base}/auth/callback?code=good&state={state}"))
        .header("cookie", format!("sheltie_oauth_state={state}"))
        .send()
        .await
        .unwrap();
    assert_eq!(callback.status(), reqwest::StatusCode::SEE_OTHER);
    let session = cookie_value(&callback, "sheltie_session").unwrap();
    let raw_cookie = callback
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .find(|c| c.starts_with("sheltie_session="))
        .unwrap();
    assert!(
        raw_cookie.contains("HttpOnly") && raw_cookie.contains("SameSite=Lax"),
        "{raw_cookie}"
    );
    let with_session = |url: String| {
        http.get(url)
            .header("cookie", format!("sheltie_session={session}"))
    };

    let overview = with_session(format!("{base}/dashboard"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        overview.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'"),
        true
    );
    let overview = overview.text().await.unwrap();
    assert!(
        overview.contains("acme/widgets") && overview.contains("octo-dev"),
        "{overview}"
    );
    assert!(
        !overview.contains("acme/secret"),
        "not the user's repository"
    );

    let repo = with_session(format!("{base}/dashboard/acme/widgets"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        repo.contains("#7") && repo.contains("Round prices &lt;b&gt;to cents&lt;/b&gt;"),
        "{repo}"
    );
    assert!(repo.contains("shadow mode"));

    let hidden = with_session(format!("{base}/dashboard/acme/secret"))
        .send()
        .await
        .unwrap();
    assert_eq!(hidden.status(), reqwest::StatusCode::NOT_FOUND);

    let css = http
        .get(format!("{base}/dashboard/assets/dashboard.css"))
        .send()
        .await
        .unwrap();
    assert_eq!(css.headers()["content-type"], "text/css; charset=utf-8");

    // Signing out ends the session.
    let out = http
        .post(format!("{base}/auth/logout"))
        .header("cookie", format!("sheltie_session={session}"))
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), reqwest::StatusCode::SEE_OTHER);
    let after = with_session(format!("{base}/dashboard"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(after.contains("Sign in with GitHub"));
    stop.send(()).unwrap();
}
