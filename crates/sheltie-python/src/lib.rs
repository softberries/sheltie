//! Python build-graph adapter for Sheltie.
//!
//! Reads a Python repository straight from git, without a Python interpreter, and describes
//! it as a [`Workspace`] for the shared adapter pipeline ([`sheltie_adapter::Generic`]):
//!
//! - **Projects** are directories with `pyproject.toml`, `setup.py` or `setup.cfg`, plus the
//!   members of a uv workspace. A project's source root is `src/` if it has one (or the
//!   setuptools `package-dir` / `packages.find.where`), else the project directory; a
//!   repository without projects is one source root.
//! - **Units** are import packages (directories with `__init__.py`, named by their dotted
//!   path), single modules at a source root, and test directories that are not packages
//!   (`tests:<dir>`).
//! - **Reads** come from every `import` and `from … import …` in a unit's files, resolved
//!   to the deepest unit they name; relative imports are resolved against the file's package.
//! - **Interfaces** hash the public signatures, classes, constants, `__all__` and
//!   `__init__.py` re-exports of a unit's modules ([`iface`]); function bodies and private
//!   names do not count.
//! - `$workspace` owns the project manifests and test configuration; `$lockfile` owns
//!   `uv.lock`, `poetry.lock`, `requirements*.txt` and the like.
//!
//! The text handling lives in [`scanner`] (logical statements) and [`imports`].

use sheltie_adapter::{Error, Git, Tool, TreeFiles, Workspace};

pub mod iface;
pub mod imports;
mod layout;
pub mod scanner;

/// The Python adapter (see the crate documentation).
#[derive(Debug, Clone, Copy, Default)]
pub struct Python;

impl Tool for Python {
    const NAME: &'static str = "sheltie-python";
    const VERSION: &'static str = concat!("sheltie-python/", env!("CARGO_PKG_VERSION"));
    const PLAIN: &'static [&'static str] = &[
        ".flake8",
        "ruff.toml",
        ".ruff.toml",
        "mypy.ini",
        ".pylintrc",
        ".pre-commit-config.yaml",
    ];

    fn workspace(git: &Git, files: &TreeFiles) -> Result<Workspace, Error> {
        let read = |path: &str| -> Result<String, Error> {
            let oid = files
                .oid(path)
                .ok_or_else(|| Error::Io(format!("{path} is not in the tree")))?;
            git.text(oid)
        };
        layout::workspace(files, &read)
    }
}
