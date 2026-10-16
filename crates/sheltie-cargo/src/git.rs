//! Reading trees and blobs from a git repository without checking them out.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;

use sheltie_core::{CommitSha, RepoPath, TreeHash};

use crate::Error;

/// A git object id as printed by git.
pub(crate) type Oid = String;

/// The files of one tree: path → blob id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TreeFiles(pub(crate) BTreeMap<RepoPath, Oid>);

impl TreeFiles {
    pub(crate) fn contains(&self, path: &str) -> bool {
        self.0.contains_key(&RepoPath::new(path))
    }

    pub(crate) fn oid(&self, path: &str) -> Option<&Oid> {
        self.0.get(&RepoPath::new(path))
    }
}

/// A repository accessed through the `git` command line.
pub struct Git {
    dir: PathBuf,
    batch: RefCell<Option<Batch>>,
    blobs: RefCell<HashMap<Oid, Rc<Vec<u8>>>>,
}

struct Batch {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Drop for Batch {
    fn drop(&mut self) {
        // Closing stdin ends `git cat-file --batch`; reap it so no zombie is left.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Git {
    /// Opens the repository containing `dir`.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, Error> {
        let dir = dir.into();
        let git = Self {
            dir,
            batch: RefCell::new(None),
            blobs: RefCell::new(HashMap::new()),
        };
        let top = git.run(&["rev-parse", "--show-toplevel"])?;
        Ok(Self {
            dir: PathBuf::from(top.trim()),
            ..git
        })
    }

    /// The repository's top-level directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<String, Error> {
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .map_err(|e| Error::Io(format!("running git: {e}")))?;
        if !out.status.success() {
            return Err(Error::Git {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        String::from_utf8(out.stdout).map_err(|_| Error::Io("git printed non-UTF-8 output".into()))
    }

    /// Resolves a revision to a commit.
    pub fn commit(&self, rev: &str) -> Result<CommitSha, Error> {
        let sha = self.run(&[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{rev}^{{commit}}"),
        ])?;
        CommitSha::parse(sha.trim()).map_err(|e| Error::Io(e.to_string()))
    }

    /// The tree of a commit.
    pub fn tree(&self, commit: &CommitSha) -> Result<TreeHash, Error> {
        let sha = self.run(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?;
        TreeHash::parse(sha.trim()).map_err(|e| Error::Io(e.to_string()))
    }

    /// The best common ancestor of two commits.
    pub fn merge_base(&self, a: &CommitSha, b: &CommitSha) -> Result<CommitSha, Error> {
        let sha = self.run(&["merge-base", a.as_str(), b.as_str()])?;
        CommitSha::parse(sha.trim()).map_err(|e| Error::Io(e.to_string()))
    }

    /// Every file in a commit's tree (submodules and symlinks excluded).
    pub(crate) fn files(&self, commit: &CommitSha) -> Result<TreeFiles, Error> {
        let out = self.run(&["ls-tree", "-r", "-z", "--full-tree", commit.as_str()])?;
        let mut files = BTreeMap::new();
        for entry in out.split('\0').filter(|e| !e.is_empty()) {
            let (meta, path) = entry
                .split_once('\t')
                .ok_or_else(|| Error::Io(format!("unexpected ls-tree line {entry:?}")))?;
            let mut parts = meta.split(' ');
            let (mode, kind, oid) = (parts.next(), parts.next(), parts.next());
            if kind == Some("blob")
                && mode != Some("120000")
                && let Some(oid) = oid
            {
                files.insert(RepoPath::new(path), oid.to_owned());
            }
        }
        Ok(TreeFiles(files))
    }

    /// Paths that differ between two commits (renames appear as delete plus add).
    pub(crate) fn changed_paths(
        &self,
        a: &CommitSha,
        b: &CommitSha,
    ) -> Result<Vec<RepoPath>, Error> {
        let out = self.run(&[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-only",
            a.as_str(),
            b.as_str(),
        ])?;
        Ok(out
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(RepoPath::new)
            .collect())
    }

    /// The raw diff between two commits: one line per changed path with both blob ids.
    /// Stable across git versions, so its hash identifies the diff.
    pub(crate) fn raw_diff(&self, a: &CommitSha, b: &CommitSha) -> Result<String, Error> {
        self.run(&[
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--full-index",
            a.as_str(),
            b.as_str(),
        ])
    }

    /// The contents of a blob, cached by id.
    pub(crate) fn blob(&self, oid: &str) -> Result<Rc<Vec<u8>>, Error> {
        if let Some(b) = self.blobs.borrow().get(oid) {
            return Ok(Rc::clone(b));
        }
        let data = Rc::new(self.read_blob(oid)?);
        self.blobs
            .borrow_mut()
            .insert(oid.to_owned(), Rc::clone(&data));
        Ok(data)
    }

    fn read_blob(&self, oid: &str) -> Result<Vec<u8>, Error> {
        let io = |e: std::io::Error| Error::Io(format!("git cat-file: {e}"));
        let mut slot = self.batch.borrow_mut();
        if slot.is_none() {
            let mut child = Command::new("git")
                .arg("-C")
                .arg(&self.dir)
                .args(["cat-file", "--batch"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(io)?;
            let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
                return Err(Error::Io("git cat-file: no pipes".into()));
            };
            *slot = Some(Batch {
                child,
                stdin,
                stdout: BufReader::new(stdout),
            });
        }
        let Some(batch) = slot.as_mut() else {
            return Err(Error::Io("git cat-file: not running".into()));
        };
        writeln!(batch.stdin, "{oid}").map_err(io)?;
        batch.stdin.flush().map_err(io)?;
        let mut header = String::new();
        batch.stdout.read_line(&mut header).map_err(io)?;
        // "<oid> blob <size>" or "<oid> missing"
        let mut parts = header.split_whitespace();
        let (Some(_), Some("blob"), Some(size)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Error::Io(format!(
                "git cat-file: unexpected header {:?}",
                header.trim()
            )));
        };
        let size: usize = size
            .parse()
            .map_err(|_| Error::Io(format!("git cat-file: bad size {size:?}")))?;
        let mut data = vec![0; size + 1]; // contents plus the trailing newline
        batch.stdout.read_exact(&mut data).map_err(io)?;
        data.truncate(size);
        Ok(data)
    }
}

impl std::fmt::Debug for Git {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Git")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

/// A temporary checkout of one commit, removed on drop. Used to run `cargo metadata`.
pub(crate) struct Worktree<'a> {
    git: &'a Git,
    path: PathBuf,
}

impl<'a> Worktree<'a> {
    pub(crate) fn add(git: &'a Git, commit: &CommitSha, scratch: &Path) -> Result<Self, Error> {
        let path = scratch.join(format!(
            "sheltie-worktree-{}-{}",
            std::process::id(),
            &commit.as_str()[..12]
        ));
        let path_str = path
            .to_str()
            .ok_or_else(|| Error::Io("non-UTF-8 scratch path".into()))?;
        git.run(&[
            "worktree",
            "add",
            "--detach",
            "--quiet",
            path_str,
            commit.as_str(),
        ])?;
        Ok(Self { git, path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree<'_> {
    fn drop(&mut self) {
        if let Some(p) = self.path.to_str() {
            let _ = self.git.run(&["worktree", "remove", "--force", p]);
        }
    }
}
