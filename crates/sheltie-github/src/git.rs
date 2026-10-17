//! Git operations the committer needs: refs, merges, compare, PR titles and comments.
//!
//! Security-sensitive: these calls move the default branch. The only call that does is
//! [`GitHubApp::fast_forward`], which never forces: it fails if the branch moved.

use serde::Deserialize;
use serde_json::json;
use sheltie_core::{CommitSha, InstallationId, PrNumber, RepoFullName};

use crate::client::{GitHubApp, GitHubError};

/// Result of merging a commit into a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// A merge commit was created on the branch.
    Merged(CommitSha),
    /// The commit was already in the branch.
    AlreadyMerged,
    /// The merge has conflicts.
    Conflict,
}

/// Result of a fast-forward attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FastForward {
    /// The branch now points at the new commit.
    Done,
    /// The branch moved (or the commit is not a descendant): the compare-and-swap lost.
    Moved,
}

/// Up to this many commits are read from a compare; more counts as "unknown".
const MAX_COMPARE: usize = 250;

#[derive(Deserialize)]
struct RefObject {
    object: ShaObject,
}

#[derive(Deserialize)]
struct ShaObject {
    sha: String,
}

#[derive(Deserialize)]
struct Compare {
    total_commits: usize,
    commits: Vec<CompareCommit>,
}

#[derive(Deserialize)]
struct CompareCommit {
    sha: String,
    parents: Vec<ShaObject>,
}

#[derive(Deserialize)]
struct PullTitle {
    title: String,
}

#[derive(Deserialize)]
struct FileContent {
    content: String,
}

#[derive(Deserialize)]
struct PullNumber {
    number: u64,
}

#[derive(Deserialize)]
struct GitCommit {
    tree: ShaObject,
    parents: Vec<ShaObject>,
}

/// A commit's tree and parents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    /// Tree sha.
    pub tree: String,
    /// Parents, first parent first.
    pub parents: Vec<CommitSha>,
}

fn sha(s: String) -> Result<CommitSha, GitHubError> {
    CommitSha::parse(s).map_err(|e| GitHubError::Unexpected(e.to_string()))
}

impl GitHubApp {
    /// The commit a branch points at.
    pub async fn branch_head(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        branch: &str,
    ) -> Result<CommitSha, GitHubError> {
        let (status, body) = self
            .raw(
                inst,
                "GET",
                &format!("/repos/{repo}/git/ref/heads/{branch}"),
                None,
            )
            .await?;
        expect(status, &[200], &body)?;
        let r: RefObject = parse(&body)?;
        sha(r.object.sha)
    }

    /// The CI of `sha` from the runs triggered by pushing `branch` only, or `None` if none ran
    /// there yet. Used to run a commit's CI again (a probe branch) and read just that run.
    pub async fn branch_run_ci(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        sha: &CommitSha,
        branch: &str,
    ) -> Result<Option<sheltie_store::CiState>, GitHubError> {
        #[derive(Deserialize)]
        struct Suites {
            check_suites: Vec<crate::ci::Suite>,
        }
        let (status, body) = self
            .raw(
                inst,
                "GET",
                &format!(
                    "/repos/{repo}/commits/{}/check-suites?per_page=100",
                    sha.as_str()
                ),
                None,
            )
            .await?;
        expect(status, &[200], &body)?;
        let suites: Suites = parse(&body)?;
        Ok(crate::ci::branch_suites_state(
            &suites.check_suites,
            branch,
            self.app_id,
        ))
    }

    /// Creates (or resets) a Sheltie work branch at `at`.
    pub async fn reset_branch(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        branch: &str,
        at: &CommitSha,
    ) -> Result<(), GitHubError> {
        let body = json!({ "ref": format!("refs/heads/{branch}"), "sha": at.as_str() });
        let (status, text) = self
            .raw(inst, "POST", &format!("/repos/{repo}/git/refs"), Some(body))
            .await?;
        if status == 422 {
            // Exists already: move it (forcing is fine on Sheltie's own work branches).
            let body = json!({ "sha": at.as_str(), "force": true });
            let (status, text) = self
                .raw(
                    inst,
                    "PATCH",
                    &format!("/repos/{repo}/git/refs/heads/{branch}"),
                    Some(body),
                )
                .await?;
            return expect(status, &[200], &text);
        }
        expect(status, &[201], &text)
    }

    /// Deletes a Sheltie work branch; a missing branch is not an error.
    pub async fn delete_branch(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        branch: &str,
    ) -> Result<(), GitHubError> {
        let (status, text) = self
            .raw(
                inst,
                "DELETE",
                &format!("/repos/{repo}/git/refs/heads/{branch}"),
                None,
            )
            .await?;
        expect(status, &[204, 404, 422], &text)
    }

    /// Merges `head` into `branch` with `message`.
    pub async fn merge_into(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        branch: &str,
        head: &CommitSha,
        message: &str,
    ) -> Result<MergeResult, GitHubError> {
        let body = json!({ "base": branch, "head": head.as_str(), "commit_message": message });
        let (status, text) = self
            .raw(inst, "POST", &format!("/repos/{repo}/merges"), Some(body))
            .await?;
        match status {
            201 => {
                let c: ShaObject = parse(&text)?;
                Ok(MergeResult::Merged(sha(c.sha)?))
            }
            204 => Ok(MergeResult::AlreadyMerged),
            409 => Ok(MergeResult::Conflict),
            _ => Err(unexpected(status, &text)),
        }
    }

    /// Moves `branch` to `to` only if that is a fast-forward (never forces).
    pub async fn fast_forward(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        branch: &str,
        to: &CommitSha,
    ) -> Result<FastForward, GitHubError> {
        let body = json!({ "sha": to.as_str(), "force": false });
        let (status, text) = self
            .raw(
                inst,
                "PATCH",
                &format!("/repos/{repo}/git/refs/heads/{branch}"),
                Some(body),
            )
            .await?;
        match status {
            200 => Ok(FastForward::Done),
            // Verified on GitHub: "422 Update is not a fast forward".
            422 => Ok(FastForward::Moved),
            _ => Err(unexpected(status, &text)),
        }
    }

    /// The first-parent chain from `head` back to (excluding) `base`, oldest first: the
    /// commits that landed on the branch since `base`. `None` if `base` is not on that chain
    /// or there are too many commits to tell.
    pub async fn first_parent_chain(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        base: &CommitSha,
        head: &CommitSha,
    ) -> Result<Option<Vec<CommitSha>>, GitHubError> {
        if base == head {
            return Ok(Some(Vec::new()));
        }
        let path = format!("/repos/{repo}/compare/{base}...{head}?per_page={MAX_COMPARE}");
        let (status, text) = self.raw(inst, "GET", &path, None).await?;
        expect(status, &[200], &text)?;
        let c: Compare = parse(&text)?;
        if c.total_commits > c.commits.len() {
            return Ok(None);
        }
        let parents: std::collections::HashMap<&str, Option<&str>> = c
            .commits
            .iter()
            .map(|k| (k.sha.as_str(), k.parents.first().map(|p| p.sha.as_str())))
            .collect();
        let mut chain = Vec::new();
        let mut cur = head.as_str();
        loop {
            let Some(parent) = parents.get(cur) else {
                return Ok(None); // left the compared range without meeting `base`
            };
            chain.push(sha(cur.to_owned())?);
            match parent {
                Some(p) if *p == base.as_str() => break,
                Some(p) => cur = p,
                None => return Ok(None),
            }
        }
        chain.reverse();
        Ok(Some(chain))
    }

    /// A commit's tree and parents.
    pub async fn commit_info(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        commit: &CommitSha,
    ) -> Result<CommitInfo, GitHubError> {
        let (status, text) = self
            .raw(
                inst,
                "GET",
                &format!("/repos/{repo}/git/commits/{commit}"),
                None,
            )
            .await?;
        expect(status, &[200], &text)?;
        let c: GitCommit = parse(&text)?;
        Ok(CommitInfo {
            tree: c.tree.sha,
            parents: c
                .parents
                .into_iter()
                .map(|p| sha(p.sha))
                .collect::<Result<_, _>>()?,
        })
    }

    /// Creates a commit object (no branch moves).
    pub async fn create_commit(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        tree: &str,
        parents: &[CommitSha],
        message: &str,
    ) -> Result<CommitSha, GitHubError> {
        let body = json!({
            "message": message,
            "tree": tree,
            "parents": parents.iter().map(CommitSha::as_str).collect::<Vec<_>>(),
        });
        let (status, text) = self
            .raw(
                inst,
                "POST",
                &format!("/repos/{repo}/git/commits"),
                Some(body),
            )
            .await?;
        expect(status, &[201], &text)?;
        sha(parse::<ShaObject>(&text)?.sha)
    }

    /// Opens a pull request; returns its number.
    pub async fn open_pr(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrNumber, GitHubError> {
        let req = json!({ "title": title, "head": head, "base": base, "body": body });
        let (status, text) = self
            .raw(inst, "POST", &format!("/repos/{repo}/pulls"), Some(req))
            .await?;
        expect(status, &[201], &text)?;
        Ok(PrNumber(parse::<PullNumber>(&text)?.number))
    }

    /// Fires a `repository_dispatch` event, which starts workflows listening for `event_type`.
    pub async fn dispatch(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        event_type: &str,
        payload: serde_json::Value,
    ) -> Result<(), GitHubError> {
        let body = json!({ "event_type": event_type, "client_payload": payload });
        let (status, text) = self
            .raw(
                inst,
                "POST",
                &format!("/repos/{repo}/dispatches"),
                Some(body),
            )
            .await?;
        expect(status, &[204], &text)
    }

    /// A file's contents at a ref, or `None` if it does not exist there.
    pub async fn file_at(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        path: &str,
        git_ref: &str,
    ) -> Result<Option<String>, GitHubError> {
        let (status, text) = self
            .raw(
                inst,
                "GET",
                &format!("/repos/{repo}/contents/{path}?ref={git_ref}"),
                None,
            )
            .await?;
        if status == 404 {
            return Ok(None);
        }
        expect(status, &[200], &text)?;
        let file: FileContent = parse(&text)?;
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(file.content.replace('\n', ""))
            .map_err(|e| GitHubError::Unexpected(format!("file content: {e}")))?;
        Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// Adds labels to a pull request.
    pub async fn add_labels(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        pr: PrNumber,
        labels: &[&str],
    ) -> Result<(), GitHubError> {
        for name in labels {
            if let Some(label) = crate::labels::by_name(name) {
                self.ensure_label(inst, repo, label).await?;
            }
        }
        let (status, text) = self
            .raw(
                inst,
                "POST",
                &format!("/repos/{repo}/issues/{}/labels", pr.0),
                Some(json!({ "labels": labels })),
            )
            .await?;
        expect(status, &[200], &text)
    }

    /// A pull request's title.
    pub async fn pr_title(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        pr: PrNumber,
    ) -> Result<String, GitHubError> {
        let (status, text) = self
            .raw(inst, "GET", &format!("/repos/{repo}/pulls/{}", pr.0), None)
            .await?;
        expect(status, &[200], &text)?;
        Ok(parse::<PullTitle>(&text)?.title)
    }

    /// Creates one of Sheltie's labels with its color and description, once per repository
    /// and process; an existing label is left as the repository's owners set it.
    async fn ensure_label(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        label: crate::labels::Label,
    ) -> Result<(), GitHubError> {
        let key = format!("{repo}/{}", label.name);
        let known = |s: &Self| {
            s.labels_ready
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&key)
        };
        if known(self) {
            return Ok(());
        }
        let (status, text) = self
            .raw(
                inst,
                "POST",
                &format!("/repos/{repo}/labels"),
                Some(json!({ "name": label.name, "color": label.color, "description": label.description })),
            )
            .await?;
        // 422: it already exists.
        expect(status, &[201, 422], &text)?;
        self.labels_ready
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key);
        Ok(())
    }

    /// Removes a label from an issue or pull request; a label that is not there is fine.
    pub async fn remove_label(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        number: PrNumber,
        label: &str,
    ) -> Result<(), GitHubError> {
        let encoded: String = label
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        let (status, text) = self
            .raw(
                inst,
                "DELETE",
                &format!("/repos/{repo}/issues/{}/labels/{encoded}", number.0),
                None,
            )
            .await?;
        expect(status, &[200, 404], &text)
    }

    /// Comments on a pull request or issue (they share numbers).
    pub async fn comment(
        &self,
        inst: InstallationId,
        repo: &RepoFullName,
        pr: PrNumber,
        body: &str,
    ) -> Result<(), GitHubError> {
        let (status, text) = self
            .raw(
                inst,
                "POST",
                &format!("/repos/{repo}/issues/{}/comments", pr.0),
                Some(json!({ "body": body })),
            )
            .await?;
        expect(status, &[201], &text)
    }
}

fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, GitHubError> {
    serde_json::from_str(text)
        .map_err(|e| GitHubError::Unexpected(format!("unexpected response: {e}")))
}

fn expect(status: u16, ok: &[u16], text: &str) -> Result<(), GitHubError> {
    if ok.contains(&status) {
        Ok(())
    } else {
        Err(unexpected(status, text))
    }
}

fn unexpected(status: u16, text: &str) -> GitHubError {
    let message: String = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_owned))
        .unwrap_or_default();
    GitHubError::Unexpected(format!(
        "HTTP {status}: {}",
        message.chars().take(200).collect::<String>()
    ))
}
