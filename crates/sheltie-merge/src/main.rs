//! `sheltie-merge`: Sheltie's merge engine on the runner.
//!
//! - `sheltie-merge merge`: tiers 1–3 (structural merge, regenerated files, declared rules).
//! - `sheltie-merge replay`: tier 4, re-derive the change from its intent with Claude Code.

use std::path::PathBuf;
use std::process::Command;

use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use sheltie_core::{CommitSha, Intent, PrNumber};
use sheltie_merge::engine::{MergeRequest, merge};
use sheltie_merge::replay::{Mode, ReplayRequest, claude_command, replay};
use sheltie_merge::rules::Rules;

#[derive(Parser)]
#[command(version, about = "Sheltie merge engine (tiers 1-4) for the CI runner")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(clap::Args)]
struct Common {
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
}

#[derive(Clone, Copy, ValueEnum)]
enum ReplayMode {
    /// 4a: resolve the conflicted hunks.
    Hunks,
    /// 4b: re-implement the change on the new head.
    Full,
}

#[derive(Subcommand)]
enum Cmd {
    /// Merges with tiers 1-3.
    Merge {
        #[command(flatten)]
        common: Common,
        /// The mergiraf program; found on PATH when omitted.
        #[arg(long)]
        mergiraf: Option<PathBuf>,
        /// Do not use mergiraf.
        #[arg(long)]
        no_mergiraf: bool,
    },
    /// Re-derives the change from its intent (tier 4).
    Replay {
        #[command(flatten)]
        common: Common,
        /// 4a (hunks) or 4b (full).
        #[arg(long, value_enum)]
        mode: ReplayMode,
        /// File with the pull request description, which holds the intent block.
        #[arg(long)]
        pr_body_file: PathBuf,
        /// Agent command as a JSON array, `{prompt}` replaced by the prompt. Defaults to Claude
        /// Code with the limits from `[replay]` in `.sheltie/merge.toml`.
        #[arg(long)]
        agent_command: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Merge {
            common,
            mergiraf,
            no_mergiraf,
        } => {
            let request = MergeRequest {
                repo: common.repo.clone(),
                pr: PrNumber(common.pr),
                onto: CommitSha::parse(&common.onto)?,
                head: CommitSha::parse(&common.head)?,
                message: read(&common.message_file)?,
                mergiraf: if no_mergiraf {
                    None
                } else {
                    mergiraf.or_else(|| which("mergiraf"))
                },
            };
            let report = merge(&request).context("merging")?;
            write(&common.out, &serde_json::to_string_pretty(&report)?)
        }
        Cmd::Replay {
            common,
            mode,
            pr_body_file,
            agent_command,
        } => {
            let onto = CommitSha::parse(&common.onto)?;
            // Settings come from the default branch, never from the pull request.
            let rules_text = Command::new("git")
                .arg("-C")
                .arg(&common.repo)
                .args(["show", &format!("{onto}:.sheltie/merge.toml")])
                .output()
                .context("reading .sheltie/merge.toml")?;
            let rules = if rules_text.status.success() {
                Rules::parse(&String::from_utf8_lossy(&rules_text.stdout))?
            } else {
                Rules::default()
            };
            anyhow::ensure!(
                rules.replay.enabled,
                "replay is not enabled in .sheltie/merge.toml"
            );
            let intent = Intent::from_description(&read(&pr_body_file)?)?
                .context("the pull request has no sheltie-intent block")?;
            let agent = match agent_command {
                Some(json) => serde_json::from_str(&json)
                    .context("--agent-command must be a JSON array of strings")?,
                None => claude_command(&rules.replay),
            };
            let request = ReplayRequest {
                repo: common.repo.clone(),
                pr: PrNumber(common.pr),
                onto,
                head: CommitSha::parse(&common.head)?,
                mode: match mode {
                    ReplayMode::Hunks => Mode::Hunks,
                    ReplayMode::Full => Mode::Full,
                },
                intent,
                message: read(&common.message_file)?,
                agent,
                test_command: rules.replay.test_command.clone(),
            };
            let report = replay(&request).context("replaying")?;
            write(&common.out, &serde_json::to_string_pretty(&report)?)
        }
    }
}

fn read(path: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

fn write(path: &PathBuf, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))
}

/// Finds a program on PATH.
fn which(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|dir| PathBuf::from(dir).join(program))
        .find(|p| p.is_file())
}
