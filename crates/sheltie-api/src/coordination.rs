//! Coordination before code is written (M7): predicted footprints, leases and safe issues.
//!
//! Shared by the REST endpoints, the MCP server and the background worker. A lease reserves an
//! issue's predicted footprint for one agent; overlapping issues are serialized: a second claim
//! is refused (MCP, REST) or waits (an agent assigned on GitHub) until the first lease ends.

use std::collections::BTreeSet;
use std::time::Duration;

use serde::Serialize;
use sheltie_core::coordination::safe_set;
use sheltie_core::{CommitSha, Footprint, IssueNumber, RepoId, RepoPath, TargetId};
use sheltie_graph::GraphSnapshot;
use sheltie_graph::predict::{Touches, predict, touches_in_text};
use sheltie_store::{Claim, IssueNotice, IssueRow, Lease, Store, StoreError, Waiter};

/// Label on an issue whose assignee waits for an overlapping lease.
pub const BLOCKED_LABEL: &str = "sheltie: blocked";

/// Most issues considered by [`Coordinator::safe_issues`].
const MAX_ISSUES: i64 = 500;
/// Most paths in one prediction request.
pub const MAX_PATHS: usize = 1_000;

/// Why a coordination request failed.
#[derive(Debug, thiserror::Error)]
pub enum CoordinationError {
    /// No such open issue is known.
    #[error("unknown issue")]
    UnknownIssue,
    /// The request is malformed.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A predicted footprint, as returned to agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Prediction {
    /// The default-branch commit whose graph was used; `None` if no graph was uploaded yet.
    pub graph_commit: Option<String>,
    /// Whether anything mapped to the graph; an unknown footprint is never reported as safe.
    pub known: bool,
    /// Targets the task is expected to write.
    pub writes: Vec<String>,
    /// Targets it is expected to affect, including the written ones.
    pub reads: Vec<String>,
    /// Whether a non-hermetic target widened the read set.
    pub leaky: bool,
}

impl Prediction {
    fn footprint(&self) -> Option<Footprint> {
        self.known.then(|| Footprint {
            writes: self.writes.iter().map(TargetId::new).collect(),
            reads: self.reads.iter().map(TargetId::new).collect(),
            ..Footprint::default()
        })
    }
}

/// A lease, as returned to agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct LeaseView {
    /// The issue.
    pub issue: u64,
    /// Who holds it.
    pub holder: String,
    /// When it lapses unless renewed (RFC 3339).
    pub expires_at: String,
}

impl From<&Lease> for LeaseView {
    fn from(l: &Lease) -> Self {
        Self {
            issue: l.issue.0,
            holder: l.holder.clone(),
            expires_at: l.expires_at.to_rfc3339(),
        }
    }
}

/// The lease that stands in the way of a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct Blocker {
    /// The lease.
    pub lease: LeaseView,
    /// Targets both footprints touch (one writes, the other reads or writes).
    pub overlap: Vec<String>,
}

/// The answer to a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct ClaimReply {
    /// `granted`, `renewed`, `taken` (someone else holds the issue) or `blocked` (an
    /// overlapping issue is leased).
    pub status: String,
    /// The lease, when granted or renewed; the other holder's, when taken.
    pub lease: Option<LeaseView>,
    /// What blocks it, when blocked.
    pub blocked_by: Option<Blocker>,
    /// The footprint the claim reserves.
    pub prediction: Prediction,
}

impl ClaimReply {
    /// Whether the caller holds the issue now.
    pub fn holds(&self) -> bool {
        matches!(self.status.as_str(), "granted" | "renewed")
    }
}

/// An issue that can be started now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
pub struct SafeIssue {
    /// The issue.
    pub issue: u64,
    /// Its title.
    pub title: String,
    /// Its predicted footprint.
    pub prediction: Prediction,
}

/// Coordinates agents of all repositories through the store.
#[derive(Debug, Clone)]
pub struct Coordinator {
    store: Store,
    ttl: Duration,
}

fn overlap(a: &Footprint, b: &Footprint) -> Vec<String> {
    let mut shared: BTreeSet<&TargetId> = a.writes.intersection(&b.reads).collect();
    shared.extend(b.writes.intersection(&a.reads));
    shared.extend(a.writes.intersection(&b.writes));
    shared
        .into_iter()
        .take(20)
        .map(|t| t.as_str().to_owned())
        .collect()
}

fn code_list(items: &[String]) -> String {
    let mut shown: Vec<String> = items.iter().take(8).map(|t| format!("`{t}`")).collect();
    if items.len() > 8 {
        shown.push(format!("and {} more", items.len() - 8));
    }
    shown.join(", ")
}

impl Coordinator {
    /// Creates a coordinator whose leases last `ttl` unless renewed.
    pub fn new(store: Store, ttl: Duration) -> Self {
        Self { store, ttl }
    }

    async fn snapshot(
        &self,
        repo: RepoId,
    ) -> Result<Option<(CommitSha, GraphSnapshot)>, CoordinationError> {
        let Some((commit, json)) = self.store.graph(repo).await? else {
            return Ok(None);
        };
        let snapshot =
            tokio::task::spawn_blocking(move || serde_json::from_value::<GraphSnapshot>(json))
                .await
                .map_err(|e| CoordinationError::Invalid(e.to_string()))?
                .map_err(|e| StoreError::InvalidRow(format!("stored graph: {e}")))?;
        Ok(Some((commit, snapshot)))
    }

    fn prediction(
        graph: Option<&(CommitSha, GraphSnapshot)>,
        touches: impl FnOnce(&GraphSnapshot) -> Touches,
    ) -> Prediction {
        let Some((commit, snapshot)) = graph else {
            return Prediction {
                graph_commit: None,
                known: false,
                writes: vec![],
                reads: vec![],
                leaky: false,
            };
        };
        let found = predict(snapshot, &touches(snapshot));
        let names = |s: &BTreeSet<TargetId>| s.iter().map(|t| t.as_str().to_owned()).collect();
        Prediction {
            graph_commit: Some(commit.as_str().to_owned()),
            known: found.is_some(),
            writes: found.as_ref().map_or_else(Vec::new, |f| names(&f.writes)),
            reads: found.as_ref().map_or_else(Vec::new, |f| names(&f.reads)),
            leaky: found.is_some_and(|f| f.leaky),
        }
    }

    fn issue_prediction(
        graph: Option<&(CommitSha, GraphSnapshot)>,
        issue: &IssueRow,
    ) -> Prediction {
        Self::prediction(graph, |s| {
            touches_in_text(s, &format!("{}\n{}", issue.title, issue.body))
        })
    }

    /// Predicts the footprint of a task that touches `paths`.
    pub async fn predict_paths(
        &self,
        repo: RepoId,
        paths: &[String],
    ) -> Result<Prediction, CoordinationError> {
        if paths.is_empty() || paths.len() > MAX_PATHS {
            return Err(CoordinationError::Invalid(format!(
                "give 1 to {MAX_PATHS} paths"
            )));
        }
        let paths: BTreeSet<RepoPath> = paths
            .iter()
            .map(|p| p.trim().trim_start_matches("./"))
            .filter(|p| !p.is_empty() && p.len() <= 1_000 && !p.split('/').any(|c| c == ".."))
            .map(RepoPath::new)
            .collect();
        let graph = self.snapshot(repo).await?;
        Ok(Self::prediction(graph.as_ref(), |_| Touches {
            paths,
            targets: BTreeSet::new(),
        }))
    }

    /// Predicts the footprint of an issue from its title and description.
    pub async fn predict_issue(
        &self,
        repo: RepoId,
        issue: IssueNumber,
    ) -> Result<Prediction, CoordinationError> {
        let row = self.open_issue(repo, issue).await?;
        let graph = self.snapshot(repo).await?;
        Ok(Self::issue_prediction(graph.as_ref(), &row))
    }

    async fn open_issue(
        &self,
        repo: RepoId,
        issue: IssueNumber,
    ) -> Result<IssueRow, CoordinationError> {
        match self.store.issue(repo, issue).await? {
            Some(row) if row.open => Ok(row),
            _ => Err(CoordinationError::UnknownIssue),
        }
    }

    /// Claims an issue for `holder`: granted unless another lease overlaps its prediction.
    pub async fn claim(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        holder: &str,
    ) -> Result<ClaimReply, CoordinationError> {
        let prediction = self.predict_issue(repo, issue).await?;
        let wanted = prediction.footprint().unwrap_or_default();
        let claim = self
            .store
            .claim_lease(repo, issue, holder, &wanted, self.ttl)
            .await?;
        let reply = |status: &str, lease: Option<&Lease>, blocked_by: Option<Blocker>| ClaimReply {
            status: status.into(),
            lease: lease.map(LeaseView::from),
            blocked_by,
            prediction: prediction.clone(),
        };
        Ok(match &claim {
            Claim::Granted(l) => reply("granted", Some(l), None),
            Claim::Renewed(l) => reply("renewed", Some(l), None),
            Claim::Taken(l) => reply("taken", Some(l), None),
            Claim::Blocked(l) => reply(
                "blocked",
                None,
                Some(Blocker {
                    lease: LeaseView::from(l),
                    overlap: overlap(&wanted, &l.footprint),
                }),
            ),
        })
    }

    /// Releases `holder`'s lease on an issue; `false` if they did not hold it.
    pub async fn release(
        &self,
        repo: RepoId,
        issue: IssueNumber,
        holder: &str,
    ) -> Result<bool, CoordinationError> {
        Ok(self
            .store
            .release_lease(repo, issue, Some(holder), "released")
            .await?
            .is_some())
    }

    /// Open issues that conflict neither with active leases nor with each other, oldest first.
    /// Issues already leased and issues whose footprint is unknown are left out.
    pub async fn safe_issues(
        &self,
        repo: RepoId,
        limit: usize,
    ) -> Result<Vec<SafeIssue>, CoordinationError> {
        let graph = self.snapshot(repo).await?;
        let leases = self.store.active_leases(repo).await?;
        let leased: BTreeSet<IssueNumber> = leases.iter().map(|l| l.issue).collect();
        let held: Vec<(u64, Footprint)> = leases
            .iter()
            .map(|l| (l.issue.0, l.footprint.clone()))
            .collect();
        let issues: Vec<(IssueRow, Prediction)> = self
            .store
            .open_issues(repo, MAX_ISSUES)
            .await?
            .into_iter()
            .filter(|i| !leased.contains(&i.number))
            .map(|i| {
                let p = Self::issue_prediction(graph.as_ref(), &i);
                (i, p)
            })
            .collect();
        let candidates: Vec<(u64, Option<Footprint>)> = issues
            .iter()
            .map(|(i, p)| (i.number.0, p.footprint()))
            .collect();
        let safe: BTreeSet<u64> = safe_set(&held, &candidates).into_iter().collect();
        Ok(issues
            .into_iter()
            .filter(|(i, _)| safe.contains(&i.number.0))
            .take(limit)
            .map(|(i, prediction)| SafeIssue {
                issue: i.number.0,
                title: i.title,
                prediction,
            })
            .collect())
    }

    /// Background work: ends finished leases, processes claims by assignment (granting them or
    /// marking the issue blocked) and notes predicted footprints on new issues.
    pub async fn tick(&self) -> Result<(), CoordinationError> {
        for (lease, reason) in self.store.release_finished_leases().await? {
            tracing::info!(repo = %lease.repo, issue = %lease.issue, holder = %lease.holder, reason, "lease ended");
        }
        for waiter in self.store.waiters().await? {
            if let Err(error) = self.process_waiter(&waiter).await {
                tracing::warn!(repo = %waiter.repo, issue = %waiter.issue, error = %error, "claim by assignment failed");
            }
        }
        for (repo, issue) in self.store.issues_to_note(50).await? {
            let graph = self.snapshot(repo).await?;
            let p = Self::issue_prediction(graph.as_ref(), &issue);
            let notice = p.known.then(|| IssueNotice {
                comment: Some(format!(
                    "Sheltie predicts this issue writes {} and affects {}{}. Agents assigned to issues that overlap it are serialized.",
                    code_list(&p.writes),
                    code_list(&p.reads),
                    if p.leaky { " (a non-hermetic target widens this)" } else { "" }
                )),
                ..IssueNotice::default()
            });
            self.store
                .issue_noted(repo, issue.number, notice.as_ref())
                .await?;
        }
        Ok(())
    }

    async fn process_waiter(&self, w: &Waiter) -> Result<(), CoordinationError> {
        let reply = match self.claim(w.repo, w.issue, &w.holder).await {
            Err(CoordinationError::UnknownIssue) => {
                return Ok(self.store.remove_waiter(w, None).await?);
            }
            other => other?,
        };
        match reply.status.as_str() {
            "granted" | "renewed" => {
                let notice = w.notified.then(|| IssueNotice {
                    comment: Some(format!(
                        "The overlapping work is done; this issue is now claimed for `{}`.",
                        w.holder
                    )),
                    remove_label: Some(BLOCKED_LABEL.into()),
                    ..IssueNotice::default()
                });
                self.store.remove_waiter(w, notice.as_ref()).await?;
                tracing::info!(repo = %w.repo, issue = %w.issue, holder = %w.holder, "claimed by assignment");
            }
            "taken" => {
                let other = reply.lease.map(|l| l.holder).unwrap_or_default();
                let notice = IssueNotice {
                    comment: Some(format!("This issue is already claimed by `{other}`.")),
                    ..IssueNotice::default()
                };
                self.store.remove_waiter(w, Some(&notice)).await?;
            }
            _ => {
                if let Some(b) = &reply.blocked_by {
                    let notice = IssueNotice {
                        comment: Some(format!(
                            "Sheltie is holding this issue for `{}`: it overlaps #{}, which `{}` is working on (shared: {}). It is claimed automatically when that work lands or its lease ends.",
                            w.holder,
                            b.lease.issue,
                            b.lease.holder,
                            code_list(&b.overlap)
                        )),
                        add_label: Some(BLOCKED_LABEL.into()),
                        ..IssueNotice::default()
                    };
                    self.store.waiter_blocked(w, &notice).await?;
                }
            }
        }
        Ok(())
    }
}
