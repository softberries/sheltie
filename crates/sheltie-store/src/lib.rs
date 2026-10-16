//! Postgres persistence (sqlx) and migrations.
//!
//! Sheltie's control plane is stateless; all durable state lives here. The schema is in
//! `migrations/` and is embedded in the binary, so [`Store::migrate`] needs no files at runtime.
//! Enum-like columns store the text forms of the `sheltie-core` enums.

use std::time::Duration;

use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// The embedded schema migrations.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Could not open the connection pool.
    #[error("cannot connect to Postgres")]
    Connect(#[source] sqlx::Error),
    /// A migration failed to apply.
    #[error("database migration failed")]
    Migrate(#[from] MigrateError),
    /// A query failed.
    #[error("database query failed")]
    Query(#[from] sqlx::Error),
}

/// Pool settings for [`Store::connect`].
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of open connections.
    pub max_connections: u32,
    /// How long to wait for a free connection before failing.
    pub acquire_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            acquire_timeout: Duration::from_secs(5),
        }
    }
}

/// Handle to the Sheltie database. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Opens a connection pool and checks that the database is reachable.
    pub async fn connect(
        options: PgConnectOptions,
        config: &PoolConfig,
    ) -> Result<Self, StoreError> {
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .connect_with(options)
            .await
            .map_err(StoreError::Connect)?;
        Ok(Self { pool })
    }

    /// Wraps an existing pool, e.g. one provided by `#[sqlx::test]`.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Applies pending migrations. Safe to run from several replicas at once: sqlx serializes
    /// them with an advisory lock.
    pub async fn migrate(&self) -> Result<(), StoreError> {
        MIGRATOR.run(&self.pool).await?;
        Ok(())
    }

    /// Round-trips a trivial query; used by health checks.
    pub async fn ping(&self) -> Result<(), StoreError> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    /// The underlying pool, for crates that run their own queries.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}
