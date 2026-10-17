//! Merging a pull request onto the head in a checkout, tier by tier.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use sheltie_core::{CommitSha, FileResolution, MergeReport, PrNumber, RepoPath, ResolutionTier};

use crate::lines::{merge_set, renumber};
use crate::rules::{Kind, Rules};

/// Errors from the engine (not merge conflicts, which are reported, not errors).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// A git command failed unexpectedly.
    #[error("git {args} failed: {stderr}")]
    Git {
        /// The arguments.
        args: String,
        /// What git printed.
        stderr: String,
    },
    /// A regenerate command failed.
    #[error("`{command}` failed: {stderr}")]
    Regenerate {
        /// The command.
        command: String,
        /// Its error output (truncated).
        stderr: String,
    },
    /// Any other I/O problem.
    #[error("{0}")]
    Io(String),
}

/// One merge to perform.
#[derive(Debug, Clone)]
pub struct MergeRequest {
    /// The checkout (any commit; the engine checks out `onto` itself).
    pub repo: PathBuf,
    /// The pull request.
    pub pr: PrNumber,
    /// Default-branch commit to merge onto.
    pub onto: CommitSha,
    /// The pull request's head.
    pub head: CommitSha,
    /// Message of the merge commit.
    pub message: String,
    /// The mergiraf program, if available (tier 1).
    pub mergiraf: Option<PathBuf>,
}

struct Git<'a>(&'a Path);

impl Git<'_> {
    fn output(&self, args: &[&str]) -> Result<std::process::Output, EngineError> {
        Command::new("git")
            .arg("-C")
            .arg(self.0)
            .args([
                "-c",
                "user.name=Sheltie",
                "-c",
                "user.email=sheltie@users.noreply.github.com",
            ])
            .args(args)
            .output()
            .map_err(|e| EngineError::Io(format!("running git: {e}")))
    }

    fn run(&self, args: &[&str]) -> Result<String, EngineError> {
        let out = self.output(args)?;
        if !out.status.success() {
            return Err(EngineError::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// File contents at a stage (`:1:path`) or revision (`rev:path`); `None` if absent.
    fn show(&self, spec: &str) -> Result<Option<Vec<u8>>, EngineError> {
        let out = self.output(&["show", spec])?;
        Ok(out.status.success().then_some(out.stdout))
    }

    fn paths(&self, args: &[&str]) -> Result<Vec<String>, EngineError> {
        Ok(self
            .run(args)?
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

/// Merges `request.head` onto `request.onto` in the checkout and commits the result if
/// every conflict was resolved. Rules are read from `onto` (the default branch), never from
/// the pull request.
pub fn merge(request: &MergeRequest) -> Result<MergeReport, EngineError> {
    let git = Git(&request.repo);
    let (onto, head) = (request.onto.as_str(), request.head.as_str());
    let rules = match git.show(&format!("{onto}:.sheltie/merge.toml"))? {
        Some(bytes) => Rules::parse(&String::from_utf8_lossy(&bytes))
            .map_err(|e| EngineError::Io(e.to_string()))?,
        None => Rules::default(),
    };
    let base = git.run(&["merge-base", onto, head])?.trim().to_owned();

    git.run(&["checkout", "-q", "--detach", onto])?;
    // Conflicts make `git merge` exit non-zero; they are handled below.
    git.output(&[
        "-c",
        "merge.conflictStyle=diff3",
        "merge",
        "--no-ff",
        "--no-commit",
        "-q",
        head,
    ])?;
    if !git
        .output(&["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
        .status
        .success()
    {
        // Nothing to merge: head is already in onto, which is then the result.
        return Ok(report(
            request,
            Some(request.onto.clone()),
            Vec::new(),
            Vec::new(),
        ));
    }

    let conflicted = git.paths(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
    let changed_ours: BTreeSet<String> = git
        .paths(&["diff", "--name-only", "-z", &base, onto])?
        .into_iter()
        .collect();
    let changed_theirs: BTreeSet<String> = git
        .paths(&["diff", "--name-only", "-z", &base, head])?
        .into_iter()
        .collect();

    let mut files = Vec::new();
    let mut unresolved = Vec::new();
    let mut regenerate: Vec<String> = Vec::new();

    for path in &conflicted {
        match rules.kind(path) {
            Some(Kind::Derived(d)) => {
                git.run(&["checkout", "--ours", "--", path])?;
                git.run(&["add", "--", path])?;
                if !regenerate.contains(&d.regenerate) {
                    regenerate.push(d.regenerate.clone());
                }
            }
            Some(Kind::Set) => {
                let stage = |n: u8| -> Result<String, EngineError> {
                    Ok(git
                        .show(&format!(":{n}:{path}"))?
                        .map(|b| String::from_utf8_lossy(&b).into_owned())
                        .unwrap_or_default())
                };
                let merged = merge_set(&stage(1)?, &stage(2)?, &stage(3)?);
                std::fs::write(request.repo.join(path), merged)
                    .map_err(|e| EngineError::Io(e.to_string()))?;
                git.run(&["add", "--", path])?;
                files.push(resolved(path, ResolutionTier::Declared));
            }
            Some(Kind::Sequence(_)) | None => {
                if structural(&git, request, path)? {
                    files.push(resolved(path, ResolutionTier::Structural));
                } else {
                    unresolved.push(RepoPath::new(path.as_str()));
                }
            }
        }
    }
    if !unresolved.is_empty() {
        git.output(&["merge", "--abort"])?;
        return Ok(report(request, None, files, unresolved));
    }

    // Tier 3: numbered files both sides added with the same number.
    for seq in &rules.sequences {
        let added = |from: &str, to: &str| -> Result<Vec<String>, EngineError> {
            Ok(git
                .paths(&["diff", "--name-only", "--diff-filter=A", "-z", from, to])?
                .into_iter()
                .filter(|p| matches!(rules.kind(p), Some(Kind::Sequence(s)) if s == seq))
                .collect())
        };
        let ours_added = added(&base, onto)?;
        let theirs_added = added(&base, head)?;
        let existing: Vec<String> = git
            .paths(&["ls-tree", "-r", "-z", "--name-only", &base])?
            .into_iter()
            .filter(|p| matches!(rules.kind(p), Some(Kind::Sequence(s)) if s == seq))
            .collect();
        for (from, to) in renumber(&existing, &ours_added, &theirs_added, &seq.key) {
            git.run(&["mv", "--", &from, &to])?;
            files.push(resolved(&to, ResolutionTier::Declared));
        }
    }

    // Tier 2: derived files changed on both sides are regenerated, even without a textual
    // conflict, since a clean line merge of a lockfile can still be wrong.
    for path in changed_ours.intersection(&changed_theirs) {
        if let Some(Kind::Derived(d)) = rules.kind(path)
            && !regenerate.contains(&d.regenerate)
        {
            regenerate.push(d.regenerate.clone());
        }
    }
    for command in &regenerate {
        let out = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&request.repo)
            .output()
            .map_err(|e| EngineError::Io(format!("running `{command}`: {e}")))?;
        if !out.status.success() {
            git.output(&["merge", "--abort"])?;
            let stderr: String = String::from_utf8_lossy(&out.stderr)
                .chars()
                .take(500)
                .collect();
            return Err(EngineError::Regenerate {
                command: command.clone(),
                stderr,
            });
        }
    }
    if !regenerate.is_empty() {
        for path in git.paths(&[
            "ls-files",
            "-z",
            "--modified",
            "--others",
            "--exclude-standard",
        ])? {
            if rules.is_derived(&path) {
                git.run(&["add", "--", &path])?;
                files.push(resolved(&path, ResolutionTier::Regenerate));
            }
        }
        for path in changed_ours
            .intersection(&changed_theirs)
            .chain(&conflicted)
        {
            if rules.is_derived(path) && !files.iter().any(|f| f.path.as_str() == path.as_str()) {
                files.push(resolved(path, ResolutionTier::Regenerate));
            }
        }
    }

    git.run(&["commit", "-q", "--no-verify", "-m", &request.message])?;
    let result = CommitSha::parse(git.run(&["rev-parse", "HEAD"])?.trim())
        .map_err(|e| EngineError::Io(e.to_string()))?;
    Ok(report(request, Some(result), files, Vec::new()))
}

/// Tier 1: runs mergiraf on the three stages of a conflicted file.
fn structural(git: &Git<'_>, request: &MergeRequest, path: &str) -> Result<bool, EngineError> {
    let Some(mergiraf) = &request.mergiraf else {
        return Ok(false);
    };
    let (Some(base), Some(ours), Some(theirs)) = (
        git.show(&format!(":1:{path}"))?,
        git.show(&format!(":2:{path}"))?,
        git.show(&format!(":3:{path}"))?,
    ) else {
        return Ok(false); // added or deleted on one side: not a content merge
    };
    let dir = std::env::temp_dir().join(format!("sheltie-merge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Io(e.to_string()))?;
    let write = |name: &str, data: &[u8]| -> Result<PathBuf, EngineError> {
        let p = dir.join(name);
        std::fs::write(&p, data).map_err(|e| EngineError::Io(e.to_string()))?;
        Ok(p)
    };
    let (b, o, t, out) = (
        write("base", &base)?,
        write("ours", &ours)?,
        write("theirs", &theirs)?,
        dir.join("out"),
    );
    let status = Command::new(mergiraf)
        .arg("merge")
        .args([&b, &o, &t])
        .arg("-o")
        .arg(&out)
        .args(["-p", path])
        .output()
        .map_err(|e| EngineError::Io(format!("running mergiraf: {e}")))?;
    if !status.status.success() {
        return Ok(false);
    }
    let merged = std::fs::read(&out).map_err(|e| EngineError::Io(e.to_string()))?;
    std::fs::write(request.repo.join(path), merged).map_err(|e| EngineError::Io(e.to_string()))?;
    git.run(&["add", "--", path])?;
    Ok(true)
}

fn resolved(path: &str, tier: ResolutionTier) -> FileResolution {
    FileResolution {
        path: RepoPath::new(path),
        tier,
    }
}

fn report(
    request: &MergeRequest,
    result: Option<CommitSha>,
    files: Vec<FileResolution>,
    unresolved: Vec<RepoPath>,
) -> MergeReport {
    MergeReport {
        pr: request.pr,
        head: request.head.clone(),
        onto: request.onto.clone(),
        result,
        files,
        unresolved,
    }
}
