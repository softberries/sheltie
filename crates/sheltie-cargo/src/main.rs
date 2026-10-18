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

#[derive(Clone, Copy, clap::ValueEnum)]
enum Format {
    Text,
    Markdown,
    Json,
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
        /// Extra glob for data files outside the build: two changes to the same file conflict,
        /// but they do not make a footprint leaky (repeatable).
        #[arg(long = "plain")]
        plain: Vec<String>,
    },
    /// Replays the branch's recent merge history through Sheltie's scheduler and reports the
    /// waiting time and CI runs saved against a one-at-a-time merge queue.
    Roi {
        /// Repository directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Branch to replay (default: the remote's default branch, else HEAD).
        #[arg(long)]
        branch: Option<String>,
        /// Days of history.
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u32).range(1..=730))]
        days: u32,
        /// Median CI time on the branch, in minutes (for example from `gh run list`).
        #[arg(long)]
        ci_minutes: f64,
        /// Compute every landing's footprint with the full adapter instead of predicting it
        /// from the changed files. Slower; needs `cargo` like the footprint command.
        #[arg(long)]
        exact: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Name shown in the report (default: owner/name of the `origin` remote).
        #[arg(long)]
        name: Option<String>,
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
            plain,
        } => {
            let git = Git::open(&repo).context("opening repository")?;
            let patterns: Vec<&str> = DEFAULT_IGNORE
                .iter()
                .copied()
                .chain(ignore.iter().map(String::as_str))
                .collect();
            let plain: Vec<&str> = DEFAULT_PLAIN
                .iter()
                .copied()
                .chain(plain.iter().map(String::as_str))
                .collect();
            let policy = PathPolicy::new(&patterns, &plain)?;
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
        Command::Roi {
            repo,
            branch,
            days,
            ci_minutes,
            exact,
            format,
            name,
            ignore,
        } => roi(repo, branch, days, ci_minutes, exact, format, name, ignore),
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

/// The `roi` command.
#[allow(clippy::too_many_arguments)]
fn roi(
    repo: PathBuf,
    branch: Option<String>,
    days: u32,
    ci_minutes: f64,
    exact: bool,
    format: Format,
    name: Option<String>,
    ignore: Vec<String>,
) -> anyhow::Result<()> {
    use std::io::{IsTerminal as _, Write as _};

    use sheltie_cargo::history::{Footprints, history, landings};
    use sheltie_scheduler::capacity::{DEFAULT_CLEAN_SHARE, capacity};
    use sheltie_scheduler::roi::{MIN_CHANGES, RoiReport, replay};

    anyhow::ensure!(
        ci_minutes.is_finite() && ci_minutes > 0.0 && ci_minutes <= 24.0 * 60.0,
        "--ci-minutes must be between 0 and 1440"
    );
    let git = Git::open(&repo).context("opening repository")?;
    let branch = branch.unwrap_or_else(|| default_branch(&git));
    let patterns: Vec<&str> = DEFAULT_IGNORE
        .iter()
        .copied()
        .chain(ignore.iter().map(String::as_str))
        .collect();
    let policy = PathPolicy::new(&patterns, DEFAULT_PLAIN)?;
    let mode = if exact {
        Footprints::Exact
    } else {
        Footprints::Predicted
    };

    let found = landings(&git, &branch, days)
        .with_context(|| format!("reading the history of {branch}"))?;
    anyhow::ensure!(
        found.len() >= MIN_CHANGES,
        "{} landing(s) on {branch} in the last {days} days; the report needs at least {MIN_CHANGES}",
        found.len()
    );
    let show_progress = std::io::stderr().is_terminal();
    let history = history(
        &git,
        &branch,
        &found,
        mode,
        &policy,
        &std::env::temp_dir(),
        |done, total| {
            if show_progress {
                eprint!("\r\x1b[2Kfootprints {done}/{total}");
            }
        },
    )
    .context("computing footprints")?;
    if show_progress {
        eprint!("\r\x1b[2K");
    }

    let ci_seconds = (ci_minutes * 60.0).round() as i64;
    let replayed = replay(&history.changes, ci_seconds).with_context(|| {
        format!(
            "only {} landing(s) have a footprint; the report needs at least {MIN_CHANGES}{}",
            history.changes.len(),
            if exact {
                " (--exact runs `cargo metadata` on every landing; is cargo installed?)"
            } else {
                ""
            }
        )
    })?;
    let footprints: Vec<_> = history
        .changes
        .iter()
        .map(|c| c.footprint.clone())
        .collect();
    let report = RoiReport {
        repository: name.unwrap_or_else(|| repo_name(&git)),
        branch,
        from: found.first().map(|l| l.at).unwrap_or_default(),
        to: found.last().map(|l| l.at).unwrap_or_default(),
        footprints: mode.describe().to_owned(),
        skipped: history.skipped,
        replay: replayed,
        capacity: capacity(&footprints, DEFAULT_CLEAN_SHARE),
    };
    let out = match format {
        Format::Text => {
            let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            report.to_text(color)
        }
        Format::Markdown => report.to_markdown(),
        Format::Json => serde_json::to_string_pretty(&report)? + "\n",
    };
    write!(std::io::stdout(), "{out}")?;
    Ok(())
}

/// The remote's default branch (`origin/HEAD`), else `HEAD`.
fn default_branch(git: &Git) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(git.dir())
        .args([
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "HEAD".to_owned())
}

/// `owner/name` from the `origin` remote URL, else the directory name.
fn repo_name(git: &Git) -> String {
    let url = std::process::Command::new("git")
        .arg("-C")
        .arg(git.dir())
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    owner_and_name(url.trim()).unwrap_or_else(|| {
        git.dir().file_name().map_or_else(
            || "repository".to_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    })
}

/// `owner/name` from `https://github.com/owner/name(.git)` or `git@github.com:owner/name.git`.
fn owner_and_name(url: &str) -> Option<String> {
    let path = url
        .strip_suffix(".git")
        .unwrap_or(url)
        .trim_end_matches('/');
    let path = path.rsplit_once(':').map_or(path, |(_, p)| p);
    let mut parts = path.rsplit('/');
    let (name, owner) = (parts.next()?, parts.next()?);
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

#[cfg(test)]
mod tests {
    use super::owner_and_name;

    #[test]
    fn repository_names_from_remotes() {
        assert_eq!(
            owner_and_name("https://github.com/acme/widgets.git").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            owner_and_name("git@github.com:acme/widgets.git").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            owner_and_name("https://github.com/acme/widgets/").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(
            owner_and_name("ssh://git@host:2222/acme/widgets").as_deref(),
            Some("acme/widgets")
        );
        assert_eq!(owner_and_name(""), None);
    }
}
