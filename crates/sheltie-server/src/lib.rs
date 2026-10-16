//! Wiring for the Sheltie control plane: configuration, database, HTTP listener.
//!
//! Kept as a library so integration tests can start the real server; `main.rs` only adds
//! logging and signal handling.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use sheltie_api::Webhooks;
use sheltie_github::{CheckRunSync, GitHubApp, WebhookSecret};
use sheltie_store::{PoolConfig, Store};
use sqlx::postgres::PgConnectOptions;
use tokio::net::TcpListener;
use tokio::sync::{Notify, watch};

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
    /// `SHELTIE_GITHUB_WEBHOOK_SECRET` (all or none) and `SHELTIE_GITHUB_API_URL`.
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
            github: github_config(&lookup)?,
        })
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
    }))
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

/// The running GitHub integration: webhook intake plus the check-run worker.
#[derive(Debug)]
struct GitHub {
    webhooks: Webhooks,
    sync: CheckRunSync,
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
        let github = config.github.zip(github_app).map(|(config, app)| GitHub {
            webhooks: Webhooks {
                secret: config.webhook_secret,
                wake: Arc::new(Notify::new()),
            },
            sync: CheckRunSync::new(store.clone(), Arc::new(app)),
        });
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

        let (webhooks, worker) = match self.github {
            Some(GitHub { webhooks, sync }) => {
                let worker =
                    tokio::spawn(sync.run(Arc::clone(&webhooks.wake), stopped(stop_rx.clone())));
                (Some(webhooks), Some(worker))
            }
            None => (None, None),
        };

        tracing::info!(addr = %addr, github = webhooks.is_some(), "sheltie-server listening");
        let served = axum::serve(self.listener, sheltie_api::router(self.store, webhooks))
            .with_graceful_shutdown(stopped(stop_rx))
            .await
            .context("HTTP server failed");
        if let Some(worker) = worker {
            worker.await.context("check-run worker panicked")?;
        }
        served
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
    fn rejects_bad_bind_address() {
        assert!(config(&[("DATABASE_URL", URL), ("SHELTIE_BIND", "localhost")]).is_err());
    }
}
