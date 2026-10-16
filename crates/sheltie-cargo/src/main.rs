//! `sheltie-cargo`: computes a change's footprint for Sheltie.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use sheltie_cargo::{DEFAULT_IGNORE, DEFAULT_PLAIN, Git, PathPolicy};

#[derive(Parser)]
#[command(
    version,
    about = "Cargo adapter for Sheltie: computes footprints of changes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Computes the footprint of HEAD against BASE and prints it as JSON.
    Footprint {
        /// Base revision (usually the pull request's base branch or commit).
        #[arg(long)]
        base: String,
        /// Head revision.
        #[arg(long, default_value = "HEAD")]
        head: String,
        /// Repository directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Write the JSON here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Extra glob for files that never affect the build (repeatable).
        #[arg(long = "ignore")]
        ignore: Vec<String>,
    },
    /// Prints the build graph Sheltie sees for one revision, as JSON.
    Graph {
        /// Revision to analyse.
        #[arg(long, default_value = "HEAD")]
        rev: String,
        /// Repository directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Footprint {
            base,
            head,
            repo,
            out,
            ignore,
        } => {
            let git = Git::open(&repo).context("opening repository")?;
            let patterns: Vec<&str> = DEFAULT_IGNORE
                .iter()
                .copied()
                .chain(ignore.iter().map(String::as_str))
                .collect();
            let policy = PathPolicy::new(&patterns, DEFAULT_PLAIN)?;
            let scratch = std::env::temp_dir();
            let report = sheltie_cargo::compute(&git, &base, &head, &policy, &scratch)
                .with_context(|| format!("computing the footprint of {head} against {base}"))?;
            let json = serde_json::to_string_pretty(&report)?;
            match out {
                Some(path) => std::fs::write(&path, json + "\n")
                    .with_context(|| format!("writing {}", path.display()))?,
                None => {
                    use std::io::Write as _;
                    writeln!(std::io::stdout(), "{json}")?;
                }
            }
            Ok(())
        }
        Command::Graph { rev, repo } => {
            let git = Git::open(&repo).context("opening repository")?;
            let graph =
                sheltie_cargo::graph(&git, &rev, &PathPolicy::default(), &std::env::temp_dir())
                    .with_context(|| format!("building the graph of {rev}"))?;
            use std::io::Write as _;
            writeln!(
                std::io::stdout(),
                "{}",
                serde_json::to_string_pretty(&graph)?
            )?;
            Ok(())
        }
    }
}
