//! `sheltie-merge`: merges a pull request onto the default branch with Sheltie's tiers 1–3.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use sheltie_core::{CommitSha, PrNumber};
use sheltie_merge::engine::{MergeRequest, merge};

#[derive(Parser)]
#[command(
    version,
    about = "Merges a pull request onto the default branch (Sheltie merge engine, tiers 1-3)"
)]
struct Cli {
    /// Pull request number.
    #[arg(long)]
    pr: u64,
    /// Default-branch commit to merge onto.
    #[arg(long)]
    onto: String,
    /// The pull request's head commit.
    #[arg(long)]
    head: String,
    /// File with the merge commit message.
    #[arg(long)]
    message_file: PathBuf,
    /// Repository checkout.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Where to write the JSON report.
    #[arg(long)]
    out: PathBuf,
    /// The mergiraf program; found on PATH when omitted. Use `--no-mergiraf` to disable tier 1.
    #[arg(long)]
    mergiraf: Option<PathBuf>,
    /// Do not use mergiraf.
    #[arg(long)]
    no_mergiraf: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mergiraf = if cli.no_mergiraf {
        None
    } else {
        cli.mergiraf.or_else(|| which("mergiraf"))
    };
    let request = MergeRequest {
        repo: cli.repo,
        pr: PrNumber(cli.pr),
        onto: CommitSha::parse(cli.onto)?,
        head: CommitSha::parse(cli.head)?,
        message: std::fs::read_to_string(&cli.message_file)
            .with_context(|| format!("reading {}", cli.message_file.display()))?,
        mergiraf,
    };
    let report = merge(&request).context("merging")?;
    std::fs::write(&cli.out, serde_json::to_string_pretty(&report)? + "\n")
        .with_context(|| format!("writing {}", cli.out.display()))?;
    Ok(())
}

/// Finds a program on PATH.
fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| PathBuf::from(dir).join(program))
        .find(|p| p.is_file())
}
