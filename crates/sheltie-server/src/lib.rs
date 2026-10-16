//! Wiring for the Sheltie control plane: configuration, database, HTTP listener.
//!
//! Kept as a library so integration tests can start the real server; `main.rs` only adds
//! logging and signal handling.

use std::fmt;
use std::future::Future;
use std::net::SocketAddr;

use anyhow::Context as _;
use sheltie_store::{PoolConfig, Store};
use sqlx::postgres::PgConnectOptions;
use tokio::net::TcpListener;

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
}

impl Config {
    /// Reads `DATABASE_URL` (required) and `SHELTIE_BIND` (default [`DEFAULT_BIND`]).
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
        })
    }
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
            .finish_non_exhaustive()
    }
}

/// A server that has connected to the database, applied migrations and bound its listener.
#[derive(Debug)]
pub struct Server {
    listener: TcpListener,
    store: Store,
}

impl Server {
    /// Connects to Postgres, applies pending migrations and binds the listen address.
    pub async fn start(config: Config) -> anyhow::Result<Self> {
        let store = Store::connect(config.database, &config.pool)
            .await
            .context("connecting to Postgres")?;
        store.migrate().await.context("applying migrations")?;
        let listener = TcpListener::bind(config.bind)
            .await
            .with_context(|| format!("binding {}", config.bind))?;
        Ok(Self { listener, store })
    }

    /// The address actually bound; differs from the configured one when the port was 0.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves HTTP until `shutdown` resolves, then drains in-flight requests.
    pub async fn serve(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        tracing::info!(addr = %self.local_addr()?, "sheltie-server listening");
        axum::serve(self.listener, sheltie_api::router(self.store))
            .with_graceful_shutdown(shutdown)
            .await
            .context("HTTP server failed")
    }
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
    fn rejects_bad_bind_address() {
        assert!(config(&[("DATABASE_URL", URL), ("SHELTIE_BIND", "localhost")]).is_err());
    }
}
