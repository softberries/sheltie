//! How changed files outside the module graph are treated.

use globset::{Glob, GlobSet, GlobSetBuilder};
use sheltie_core::RepoPath;

use crate::Error;

/// Files that never affect a build: dropped from footprints when no target owns them.
pub const DEFAULT_IGNORE: &[&str] = &[
    "**/*.md",
    "**/*.rst",
    "**/*.adoc",
    "CHANGES*",
    "CHANGELOG*",
    "AUTHORS*",
    "CONTRIBUTORS*",
    "NOTICE*",
    "docs/**",
    "LICENSE*",
    "**/LICENSE*",
    "**/.gitignore",
    ".editorconfig",
    ".gitattributes",
];

/// Files outside the build that are still worth tracking (two PRs editing one conflict),
/// but do not affect any target, whatever the build tool. Adapters add their own.
pub const COMMON_PLAIN: &[&str] = &[
    ".github/**",
    ".gitlab-ci.yml",
    ".circleci/**",
    ".pre-commit-config.yaml",
];

/// Classifies files that no module owns. Anything neither ignored nor plain is assumed to
/// be read at build or test time and makes the footprint leaky.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    ignore: GlobSet,
    plain: GlobSet,
}

impl PathPolicy {
    /// Builds a policy from glob patterns.
    pub fn new(ignore: &[&str], plain: &[&str]) -> Result<Self, Error> {
        Ok(Self {
            ignore: globs(ignore)?,
            plain: globs(plain)?,
        })
    }

    /// Whether no build reads the file.
    pub fn is_ignored(&self, path: &RepoPath) -> bool {
        self.ignore.is_match(path.as_str())
    }

    /// Whether the file is data outside the build: tracked, but not leaky.
    pub fn is_plain(&self, path: &RepoPath) -> bool {
        self.plain.is_match(path.as_str())
    }
}

impl Default for PathPolicy {
    fn default() -> Self {
        // The default patterns are constants and known to be valid.
        Self::new(DEFAULT_IGNORE, COMMON_PLAIN)
            .unwrap_or_else(|e| unreachable!("default globs: {e}"))
    }
}

fn globs(patterns: &[&str]) -> Result<GlobSet, Error> {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        builder.add(Glob::new(p).map_err(|e| Error::Config(format!("glob {p:?}: {e}")))?);
    }
    builder.build().map_err(|e| Error::Config(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let p = PathPolicy::default();
        assert!(p.is_ignored(&RepoPath::new("README.md")));
        assert!(p.is_ignored(&RepoPath::new("crates/x/README.md")));
        assert!(p.is_ignored(&RepoPath::new("docs/design.md")));
        assert!(p.is_ignored(&RepoPath::new("CHANGES.rst")));
        assert!(p.is_ignored(&RepoPath::new("docs/guide/index.adoc")));
        assert!(!p.is_ignored(&RepoPath::new("src/changes.py")));
        assert!(!p.is_ignored(&RepoPath::new("config/flags.txt")));
        assert!(p.is_plain(&RepoPath::new(".github/workflows/ci.yml")));
        assert!(!p.is_plain(&RepoPath::new("scripts/deploy.sh")));
    }
}
