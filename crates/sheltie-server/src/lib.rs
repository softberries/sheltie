//! Wiring for the Sheltie control plane: configuration, database, HTTP listener.
//!
//! Kept as a library so integration tests can start the real server; `main.rs` only adds
//! logging and signal handling.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use sheltie_api::{Footprints, Webhooks};
use sheltie_github::oidc::{DEFAULT_AUDIENCE, OidcConfig, OidcVerifier};
use sheltie_github::{CheckRunSync, GitHubApp, ReadinessSync, WebhookSecret};
use sheltie_store::{PoolConfig, Store};
use sqlx::postgres::PgConnectOptions;
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};

mod committer;
mod shadow;
mod sweeper;

pub use committer::Committer;
pub use shadow::{BranchCiWorker, ShadowScheduler};
pub use sweeper::{Sweeper, VERIFIED_BRANCH};

/// Default time between scheduler epochs.
pub const DEFAULT_EPOCH_SECONDS: u64 = 10;

/// Default listen address when `SHELTIE_BIND` is unset.
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";

/// Server configuration.
///
/// `Debug` omits the database password.
#[derive(Clone)]
pub struct Config {
    /// Postgres connection options.
    pub database: PgConnectOptions,
    /// Address the HTTP server listens on.
    pub bind: SocketAddr,
    /// Connection pool settings.
    pub pool: PoolConfig,
    /// Time between scheduler epochs.
    pub epoch: Duration,
    /// GitHub App settings; without them the server serves only `/healthz`.
    pub github: Option<GitHubConfig>,
}

/// GitHub App settings.
#[derive(Debug, Clone)]
pub struct GitHubConfig {
    /// The App's numeric id.
    pub app_id: u64,
    /// Path to the App's private key (PEM), read at startup.
    pub private_key_path: PathBuf,
    /// The App's webhook secret.
    pub webhook_secret: WebhookSecret,
    /// API base URL; `None` means `https://api.github.com`.
    pub api_url: Option<String>,
    /// How footprint uploads from GitHub Actions are authenticated.
    pub oidc: OidcConfig,
}

/// The variables that configure the GitHub App; all or none must be set.
const GITHUB_VARS: [&str; 3] = [
    "SHELTIE_GITHUB_APP_ID",
    "SHELTIE_GITHUB_PRIVATE_KEY_PATH",
    "SHELTIE_GITHUB_WEBHOOK_SECRET",
];

impl Config {
    /// Reads `DATABASE_URL` (required), `SHELTIE_BIND` (default [`DEFAULT_BIND`]) and the
    /// optional GitHub App settings: `SHELTIE_GITHUB_APP_ID`, `SHELTIE_GITHUB_PRIVATE_KEY_PATH`,
    /// `SHELTIE_GITHUB_WEBHOOK_SECRET` (all or none), `SHELTIE_GITHUB_API_URL`, and for
    /// footprint uploads `SHELTIE_OIDC_AUDIENCE` (default `sheltie`) and `SHELTIE_OIDC_ISSUER`
    /// (default GitHub Actions on github.com).
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Builds the configuration from a variable lookup; [`Config::from_env`] uses the process
    /// environment.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let url = lookup("DATABASE_URL").context("DATABASE_URL is not set")?;
        // The URL carries the password, so it never goes into an error message.
        let invalid = || anyhow::anyhow!("DATABASE_URL is not a valid postgres:// URL");
        if !(url.starts_with("postgres://") || url.starts_with("postgresql://")) {
            return Err(invalid());
        }
        let database = url.parse::<PgConnectOptions>().map_err(|_| invalid())?;
        let bind = lookup("SHELTIE_BIND").unwrap_or_else(|| DEFAULT_BIND.to_owned());
        let bind = bind
            .parse()
            .with_context(|| format!("SHELTIE_BIND {bind:?} is not a socket address"))?;
        Ok(Self {
            database,
            bind,
            pool: PoolConfig::default(),
            epoch: epoch(&lookup)?,
            github: github_config(&lookup)?,
        })
    }
}

/// `SHELTIE_EPOCH_SECONDS`, 1–3600, default [`DEFAULT_EPOCH_SECONDS`].
fn epoch(lookup: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Duration> {
    let Some(value) = lookup("SHELTIE_EPOCH_SECONDS").filter(|v| !v.is_empty()) else {
        return Ok(Duration::from_secs(DEFAULT_EPOCH_SECONDS));
    };
    match value.parse::<u64>() {
        Ok(secs @ 1..=3600) => Ok(Duration::from_secs(secs)),
        _ => anyhow::bail!(
            "SHELTIE_EPOCH_SECONDS {value:?} must be a number of seconds from 1 to 3600"
        ),
    }
}

fn github_config(lookup: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<GitHubConfig>> {
    let values = GITHUB_VARS.map(|key| lookup(key).filter(|v| !v.is_empty()));
    let [Some(app_id), Some(key_path), Some(secret)] = values else {
        if values.iter().any(Option::is_some) {
            anyhow::bail!("set all of {} or none of them", GITHUB_VARS.join(", "));
        }
        return Ok(None);
    };
    let app_id = app_id
        .parse()
        .with_context(|| format!("SHELTIE_GITHUB_APP_ID {app_id:?} is not a number"))?;
    let webhook_secret =
        WebhookSecret::new(secret).context("SHELTIE_GITHUB_WEBHOOK_SECRET is empty")?;
    Ok(Some(GitHubConfig {
        app_id,
        private_key_path: PathBuf::from(key_path),
        webhook_secret,
        api_url: lookup("SHELTIE_GITHUB_API_URL").filter(|v| !v.is_empty()),
        oidc: oidc_config(lookup),
    }))
}

fn oidc_config(lookup: &impl Fn(&str) -> Option<String>) -> OidcConfig {
    let var = |k: &str| lookup(k).filter(|v| !v.is_empty());
    let mut oidc = OidcConfig::github_actions(
        var("SHELTIE_OIDC_AUDIENCE").unwrap_or_else(|| DEFAULT_AUDIENCE.to_owned()),
    );
    if let Some(issuer) = var("SHELTIE_OIDC_ISSUER") {
        let issuer = issuer.trim_end_matches('/').to_owned();
        oidc.jwks_url = format!("{issuer}/.well-known/jwks");
        oidc.issuer = issuer;
    }
    oidc
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_host", &self.database.get_host())
            .field("database_port", &self.database.get_port())
            .field("database_name", &self.database.get_database())
            .field("database_user", &self.database.get_username())
            .field("bind", &self.bind)
            .field("pool", &self.pool)
            .field("epoch", &self.epoch)
            .field("github", &self.github)
            .finish_non_exhaustive()
    }
}

/// A server that has connected to the database, applied migrations and bound its listener.
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    store: Store,
    github: Option<GitHub>,
}

/// The running GitHub integration: webhook intake, footprint uploads and the workers.
#[derive(Debug)]
struct GitHub {
    webhooks: Webhooks,
    footprints: Footprints,
    check_runs: CheckRunSync,
    readiness: ReadinessSync,
    shadow: ShadowScheduler,
    branch_ci: BranchCiWorker,
    sweeper: Sweeper,
}

impl Server {
    /// Connects to Postgres, applies pending migrations, loads the GitHub App key and binds
    /// the listen address.
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        // Load the key first: a misconfigured App should fail fast, before touching the database.
        let github_app = match &config.github {
            Some(github) => Some(load_github_app(github).await?),
            None => {
                tracing::warn!("GitHub App not configured; webhooks are disabled");
                None
            }
        };
        let store = Store::connect(config.database, &config.pool)
            .await
            .context("connecting to Postgres")?;
        store.migrate().await.context("applying migrations")?;
        let listener = TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("binding {}", config.bind))?;
        let github = match config.github.zip(github_app) {
            Some((github, app)) => {
                // Intake (webhooks, uploads) signals this; `serve` forwards it to each worker.
                let intake = Arc::new(Notify::new());
                let verifier =
                    OidcVerifier::new(github.oidc).context("creating the OIDC verifier")?;
                let app = Arc::new(app);
                Some(GitHub {
                    webhooks: Webhooks {
                        secret: github.webhook_secret,
                        wake: Arc::clone(&intake),
                    },
                    footprints: Footprints {
                        verifier: Arc::new(verifier),
                        wake: intake,
                    },
                    check_runs: CheckRunSync::new(store.clone(), Arc::clone(&app)),
                    readiness: ReadinessSync::new(store.clone(), Arc::clone(&app)),
                    branch_ci: BranchCiWorker::new(store.clone(), Arc::clone(&app)),
                    sweeper: Sweeper::new(store.clone(), Arc::clone(&app), config.epoch),
                    shadow: ShadowScheduler::new(
                        store.clone(),
                        config.epoch,
                        Arc::new(Notify::new()),
                    )
                    .with_committer(Committer::new(store.clone(), app)),
                })
            }
            None => None,
        };
        Ok(Self {
            listener,
            store,
            github,
        })
    }

    /// The address actually bound; differs from the configured one when the port was 0.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves HTTP and runs the check-run worker until `shutdown` resolves, then drains
    /// in-flight requests and stops the worker.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let addr = self.local_addr()?;
        let (stop_tx, stop_rx) = watch::channel(false);
        tokio::spawn(async move {
            shutdown.await;
            // Receivers may already be gone; nothing to do then.
            let _ = stop_tx.send(true);
        });
        let stopped = |mut rx: watch::Receiver<bool>| async move {
            // An error means the sender dropped, which also means stop.
            let _ = rx.wait_for(|stop| *stop).await;
        };

        let mut workers = Vec::new();
        let (webhooks, footprints) = match self.github {
            Some(GitHub {
                webhooks,
                footprints,
                check_runs,
                readiness,
                shadow,
                branch_ci,
                sweeper,
            }) => {
                workers.push(tokio::spawn(sweeper.run(stopped(stop_rx.clone()))));
                let check_runs_wake = Arc::clone(&shadow.changed);
                let readiness_wake = Arc::new(Notify::new());
                let branch_ci_wake = Arc::new(Notify::new());
                workers.push(tokio::spawn(fan_out(
                    Arc::clone(&webhooks.wake),
                    vec![
                        Arc::clone(&check_runs_wake),
                        Arc::clone(&readiness_wake),
                        Arc::clone(&branch_ci_wake),
                    ],
                    stopped(stop_rx.clone()),
                )));
                workers.push(tokio::spawn(
                    branch_ci.run(branch_ci_wake, stopped(stop_rx.clone())),
                ));
                workers.push(tokio::spawn(
                    check_runs.run(check_runs_wake, stopped(stop_rx.clone())),
                ));
                workers.push(tokio::spawn(
                    readiness.run(readiness_wake, stopped(stop_rx.clone())),
                ));
                workers.push(tokio::spawn(shadow.run(stopped(stop_rx.clone()))));
                (Some(webhooks), Some(footprints))
            }
            None => (None, None),
        };

        tracing::info!(addr = %addr, github = webhooks.is_some(), "sheltie-server listening");
        let router = sheltie_api::router(self.store, webhooks, footprints);
        let served = axum::serve(self.listener, router)
            .with_graceful_shutdown(stopped(stop_rx))
            .await
            .context("HTTP server failed");
        for worker in workers {
            worker.await.context("a background worker panicked")?;
        }
        served
    }
}

/// Forwards each notification on `source` to every target, until `shutdown`.
async fn fan_out(
    source: Arc<Notify>,
    targets: Vec<Arc<Notify>>,
    shutdown: impl Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => return,
            () = source.notified() => targets.iter().for_each(|t| t.notify_one()),
        }
    }
}

async fn load_github_app(config: &GitHubConfig) -> anyhow::Result<GitHubApp> {
    let path = &config.private_key_path;
    let key = tokio::fs::read(path)
        .await
        .with_context(|| format!("reading GitHub App private key {}", path.display()))?;
    GitHubApp::new(config.app_id, &key, config.api_url.as_deref())
        .with_context(|| format!("loading GitHub App private key {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    const URL: &str = "postgres://sheltie:s3cret@db.internal:5432/sheltie";

    fn config(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        let vars: HashMap<_, _> = vars.iter().copied().collect();
        Config::from_lookup(|key| vars.get(key).map(|v| (*v).to_owned()))
    }

    #[test]
    fn defaults_bind_address() {
        let config = config(&[("DATABASE_URL", URL)]).unwrap();
        assert_eq!(config.bind, DEFAULT_BIND.parse().unwrap());
        assert_eq!(config.database.get_host(), "db.internal");
    }

    #[test]
    fn reads_bind_address() {
        let config = config(&[("DATABASE_URL", URL), ("SHELTIE_BIND", "0.0.0.0:9000")]).unwrap();
        assert_eq!(config.bind, "0.0.0.0:9000".parse().unwrap());
    }

    #[test]
    fn requires_database_url() {
        let err = config(&[]).unwrap_err();
        assert!(err.to_string().contains("DATABASE_URL is not set"));
    }

    #[test]
    fn invalid_url_error_does_not_leak_it() {
        for bad in [
            "mysql://user:s3cret@host/db",
            "postgres://user:s3cret@host:port/db",
        ] {
            let err = config(&[("DATABASE_URL", bad)]).unwrap_err();
            assert!(!format!("{err:#} {err:?}").contains("s3cret"), "{err:#}");
        }
    }

    #[test]
    fn debug_does_not_leak_password() {
        let config = config(&[("DATABASE_URL", URL)]).unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
        assert!(debug.contains("db.internal"), "{debug}");
    }

    #[test]
    fn github_is_optional() {
        assert!(config(&[("DATABASE_URL", URL)]).unwrap().github.is_none());
    }

    #[test]
    fn reads_github_settings() {
        let config = config(&[
            ("DATABASE_URL", URL),
            ("SHELTIE_GITHUB_APP_ID", "123456"),
            ("SHELTIE_GITHUB_PRIVATE_KEY_PATH", "/run/secrets/app.pem"),
            ("SHELTIE_GITHUB_WEBHOOK_SECRET", "hush-hush"),
        ])
        .unwrap();
        let github = config.github.as_ref().unwrap();
        assert_eq!(github.app_id, 123456);
        assert_eq!(github.api_url, None);
        assert_eq!(github.oidc, OidcConfig::github_actions("sheltie"));
        assert!(
            !format!("{config:?}").contains("hush-hush"),
            "secret leaked into Debug"
        );
    }

    #[test]
    fn partial_github_settings_are_an_error() {
        let err = config(&[("DATABASE_URL", URL), ("SHELTIE_GITHUB_APP_ID", "1")]).unwrap_err();
        assert!(err.to_string().contains("set all of"), "{err}");
    }

    #[test]
    fn epoch_defaults_and_bounds() {
        assert_eq!(
            config(&[("DATABASE_URL", URL)]).unwrap().epoch,
            Duration::from_secs(10)
        );
        let custom = config(&[("DATABASE_URL", URL), ("SHELTIE_EPOCH_SECONDS", "30")]).unwrap();
        assert_eq!(custom.epoch, Duration::from_secs(30));
        for bad in ["0", "3601", "fast"] {
            assert!(
                config(&[("DATABASE_URL", URL), ("SHELTIE_EPOCH_SECONDS", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn rejects_bad_bind_address() {
        assert!(config(&[("DATABASE_URL", URL), ("SHELTIE_BIND", "localhost")]).is_err());
    }
}
