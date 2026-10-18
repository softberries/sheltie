//! The Sheltie control-plane binary.

use anyhow::Context as _;
use chrono::Utc;
use clap::{Parser, Subcommand};
use sheltie_core::RepoFullName;
use sheltie_scheduler::report::build_report;
use sheltie_server::{Config, Server};
use sheltie_store::{RepoMode, Store};
use tracing_subscriber::EnvFilter;

/// The Sheltie control plane.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the server (the default).
    Serve,
    /// Prints the shadow-mode report for a repository: predictions versus what happened.
    ShadowReport {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// Report window in days.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Output format; defaults to `text` on a terminal and `markdown` otherwise.
        #[arg(long, value_parser = ["text", "markdown", "json"])]
        format: Option<String>,
    },
    /// Prints each agent's scorecard for a repository: merge rate, returns, reverts,
    /// replays, review rounds and time to merge of the pull requests it opened.
    Scorecard {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// Pull requests opened in the last this many days.
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=366))]
        days: u32,
        /// Output format; defaults to `text` on a terminal and `markdown` otherwise.
        #[arg(long, value_parser = ["text", "markdown", "json"])]
        format: Option<String>,
    },
    /// Administers repositories.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },
    /// Exits 0 if the local server answers `/healthz` (for container health checks).
    Healthcheck,
    /// Prints anonymous usage counts as JSON (no repository names or ids), to share by hand.
    Stats {
        /// Window in days.
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
    /// Manages agent tokens (REST and MCP).
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
}

#[derive(Subcommand)]
enum TokenCommand {
    /// Creates a token for an agent and prints it once.
    Create {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// Agent name; it holds the agent's leases.
        #[arg(long)]
        name: String,
    },
    /// Revokes an agent's tokens.
    Revoke {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// Agent name.
        #[arg(long)]
        name: String,
    },
}

#[derive(Subcommand)]
enum RepoCommand {
    /// Sets whether Sheltie only predicts (`shadow`) or lands changes (`merge`).
    SetMode {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// `shadow` or `merge`.
        #[arg(long, value_parser = ["shadow", "merge"])]
        mode: String,
    },
    /// Turns the merge engine on or off; it needs the `sheltie-merge` workflow in the repository.
    SetMergeEngine {
        /// Repository as `owner/name`.
        #[arg(long)]
        repo: String,
        /// `on` or `off`.
        #[arg(long, value_parser = ["on", "off"])]
        state: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv()?;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        // Colours only on a terminal: container log collectors get plain text.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();

    match Cli::parse().command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let server = Server::start(Config::from_env()?).await?;
            server.serve(shutdown_signal()).await?;
            tracing::info!("sheltie-server stopped");
        }
        Command::ShadowReport { repo, days, format } => {
            let repo = RepoFullName::parse(repo)?;
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            let since = Utc::now() - chrono::Duration::days(i64::from(days));
            let input = store
                .shadow_report_input(&repo, since)
                .await?
                .with_context(|| format!("Sheltie has no data for {repo}"))?;
            let report = build_report(&input);
            let terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
            let format =
                format.unwrap_or_else(|| if terminal { "text" } else { "markdown" }.into());
            let out = match format.as_str() {
                "json" => serde_json::to_string_pretty(&report)?,
                "markdown" => report.to_markdown(),
                _ => report.to_text(terminal),
            };
            use std::io::Write as _;
            writeln!(std::io::stdout(), "{out}")?;
        }
        Command::Scorecard { repo, days, format } => {
            let repo = RepoFullName::parse(repo)?;
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            let id = store
                .repo_id_by_name(&repo)
                .await?
                .with_context(|| format!("Sheltie has no data for {repo}"))?;
            let cards = store.agent_scorecards(id, days).await?;
            let terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
            let format =
                format.unwrap_or_else(|| if terminal { "text" } else { "markdown" }.into());
            let out = match format.as_str() {
                "json" => serde_json::to_string_pretty(&cards)?,
                "markdown" => sheltie_core::agents::to_markdown(&cards, days),
                _ => sheltie_core::agents::to_text(&cards, days),
            };
            use std::io::Write as _;
            write!(std::io::stdout(), "{out}")?;
        }
        Command::Repo {
            command: RepoCommand::SetMode { repo, mode },
        } => {
            let repo = RepoFullName::parse(repo)?;
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            let mode = if mode == "merge" {
                RepoMode::Merge
            } else {
                RepoMode::Shadow
            };
            if !store.set_repo_mode(&repo, mode).await? {
                anyhow::bail!("Sheltie does not know {repo}; install the App on it first");
            }
            tracing::info!(%repo, mode = mode.as_str(), "repository mode set");
        }
        Command::Repo {
            command: RepoCommand::SetMergeEngine { repo, state },
        } => {
            let repo = RepoFullName::parse(repo)?;
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            if !store.set_merge_engine(&repo, state == "on").await? {
                anyhow::bail!("Sheltie does not know {repo}; install the App on it first");
            }
            tracing::info!(%repo, merge_engine = %state, "merge engine set");
        }
        Command::Healthcheck => {
            let bind = std::env::var("SHELTIE_BIND").unwrap_or_else(|_| "127.0.0.1:8080".into());
            let port = bind.rsplit(':').next().unwrap_or("8080");
            match healthy(&format!("127.0.0.1:{port}")) {
                Ok(true) => {}
                Ok(false) => anyhow::bail!("/healthz did not return 200"),
                Err(e) => return Err(e).context("connecting to the server"),
            }
        }
        Command::Stats { days } => {
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            let since = Utc::now() - chrono::Duration::days(i64::from(days));
            let report = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "days": days,
                "stats": store.stats(since).await?,
            });
            use std::io::Write as _;
            writeln!(
                std::io::stdout(),
                "{}",
                serde_json::to_string_pretty(&report)?
            )?;
        }
        Command::Token { command } => {
            let config = Config::from_env()?;
            let store = Store::connect(config.database, &config.pool)
                .await
                .context("connecting to Postgres")?;
            match command {
                TokenCommand::Create { repo, name } => {
                    let repo = RepoFullName::parse(repo)?;
                    anyhow::ensure!(
                        (1..=100).contains(&name.len())
                            && name
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric()
                                    || matches!(b, b'-' | b'_' | b'.')),
                        "--name must be 1-100 letters, digits, '-', '_' or '.'"
                    );
                    let mut secret = [0u8; 32];
                    getrandom::fill(&mut secret)
                        .map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
                    let token = format!(
                        "{}{}",
                        sheltie_api::agents::TOKEN_PREFIX,
                        hex::encode(secret)
                    );
                    if store
                        .create_token(&repo, &name, &sheltie_api::agents::token_hash(&token))
                        .await?
                        .is_none()
                    {
                        anyhow::bail!("Sheltie does not know {repo}; install the App on it first");
                    }
                    use std::io::Write as _;
                    writeln!(std::io::stdout(), "{token}")?;
                    tracing::info!(%repo, name, "token created; it is shown only once");
                }
                TokenCommand::Revoke { repo, name } => {
                    let repo = RepoFullName::parse(repo)?;
                    let n = store.revoke_token(&repo, &name).await?;
                    tracing::info!(%repo, name, revoked = n, "tokens revoked");
                }
            }
        }
    }
    Ok(())
}

/// A plain HTTP/1.0 GET of `/healthz`; true on status 200.
fn healthy(addr: &str) -> std::io::Result<bool> {
    use std::io::{Read as _, Write as _};
    let timeout = Some(std::time::Duration::from_secs(3));
    let addr = addr
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let mut stream =
        std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_secs(3))?;
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    stream.write_all(b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
    let mut head = [0u8; 12];
    stream.read_exact(&mut head)?;
    Ok(head.starts_with(b"HTTP/1.") && &head[9..12] == b"200")
}

/// Loads `.env` when present; a missing file is fine, a malformed one is an error.
fn load_dotenv() -> anyhow::Result<()> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(e) if e.not_found() => Ok(()),
        Err(e) => Err(e).context("reading .env"),
    }
}

/// Resolves on Ctrl-C, or on SIGTERM on Unix (what container runtimes send).
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "cannot listen for Ctrl-C");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(error) => {
                tracing::error!(%error, "cannot listen for SIGTERM");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
