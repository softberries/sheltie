# Python

`sheltie-python` computes footprints for Python repositories. It reads the repository straight
from git and parses the source itself: it needs no Python interpreter, no virtual environment
and no installed dependencies, so it runs in seconds on any CI runner.

## Supported layouts

- **uv workspaces**: the members listed under `[tool.uv.workspace] members` (globs, minus
  `exclude`) in a `pyproject.toml`.
- **Projects**: every directory with a `pyproject.toml`, `setup.py` or `setup.cfg`, at any
  depth (monorepos with several projects, Poetry, PDM, Hatch and setuptools alike).
- **src layout and flat layout**: a project's source root is its `src/` directory if it has
  one, else the project directory itself. A setuptools `package-dir = {"" = "lib"}` or
  `[tool.setuptools.packages.find] where = ["lib"]` in `pyproject.toml` overrides this.
- **No project files at all**: the repository root is the source root.

`.venv/`, `venv/`, `node_modules/`, `build/`, `dist/`, `__pycache__/`, `.tox/` and every other
dot-directory are skipped.

## Units

Sheltie's targets for Python are import packages, named the way you import them:

| Unit | Id | Files |
| --- | --- | --- |
| A directory with `__init__.py` under a source root | its dotted path, e.g. `shop.cart` | its `.py` and `.pyi` files, `py.typed`, and Python files in subdirectories without `__init__.py` |
| A module directly at a source root, e.g. `src/tasks.py` | `tasks` | the module (and its `.pyi` stub) |
| A test directory that is not a package (named `tests` or `test`, or holding `test_*.py` / `*_test.py`) | `tests:<dir>`, e.g. `tests:packages/core/tests` | its Python files, `conftest.py` included |

Test modules inside a package belong to the package. If two projects contain a package with
the same name, the ids get the directory appended (`utils@a/src/utils`), and imports of the
name read both.

A unit **reads** the units its files import: `import a.b.c`, `import a.b as x`,
`from a.b import c, d`, parenthesised multi-line imports, and relative imports (`from . import x`,
`from ..pkg import y`, resolved against the importing file's package). Each import resolves to
the deepest unit it names (`a.b.c`, else `a.b`, else `a`); `from a.b import c` also reads `a.b.c`
when that is a unit. Imports count wherever they are: inside functions, `try` blocks and
`if TYPE_CHECKING:`. A test directory also reads the test directories above it, whose
`conftest.py` fixtures it uses.

## Interfaces

A change only reaches the units that import a package when the package's **public interface**
changes. The interface is read from the package's modules (test modules excluded), with
comments removed and formatting ignored:

- public functions (`def` and `async def` whose name does not start with `_`): decorators and
  the signature with parameters, defaults and annotations, not the body;
- public classes: decorators, bases, every method signature except `_private` ones (dunders
  such as `__init__` count), class-level attributes and annotations, and nested public classes;
- module-level assignments to public names, type aliases (`type T = …`) and `__all__`;
- in `__init__.py`, every import (they are re-exports). A package that re-exports another
  unit, and any module with `from x import *`, includes that unit's interface in its own;
- statements inside module-level `if`, `try` and `with` blocks, and the block headers.

So editing a function body, a private helper (`_round`, even its signature), a docstring or a
comment stays inside the package; changing a public signature, a public constant, a class
member or a re-export reaches every unit importing the package, and the units importing
those: a unit's interface also folds in the interfaces of the units it imports, because its
signatures may use their types. A change that adds an import reads the imported unit. When
the adapter is unsure,
it counts a statement as interface: a wider footprint only costs parallelism, a missed
interface change could hide a conflict.

A unit's tests are its own id plus the test directories that import it, so a body change in
`shop_core` also lists `tests:packages/core/tests`.

## Configuration, lockfiles and leaks

- `$workspace` owns `pyproject.toml`, `setup.py` and `setup.cfg` of every project, `tox.ini`,
  `noxfile.py`, `pytest.ini`, `Pipfile`, `.python-version` and the root `conftest.py`. Every
  unit reads it, so changing any of them conflicts with every change.
- `$lockfile` owns `uv.lock`, `poetry.lock`, `Pipfile.lock`, `pdm.lock`, `requirements*.txt`
  and `constraints*.txt`.
- Linter and formatter settings (`.flake8`, `ruff.toml`, `.ruff.toml`, `mypy.ini`,
  `.pylintrc`, `.pre-commit-config.yaml`) are plain files: two changes to one conflict, but
  they affect no unit.
- A project whose `setup.py` mentions `ext_modules` or `cmdclass` builds extensions or runs
  custom build steps, so its units are not hermetic: footprints reading them are leaky
  (`setup.py builds extensions`).
- Other files in a package directory (templates, JSON, `.pyx` sources) are owned by no unit:
  a change to one is attributed to the package and is leaky. Files outside every unit are
  leaky unless they are [plain](workflows.md#footprints-required).

## What it does not claim

The adapter sees imports that are written down. It does not see:

- dynamic imports: `importlib.import_module(name)`, `__import__`, plugin entry points,
  Django `INSTALLED_APPS` strings and similar registries;
- names reached through `sys.path` manipulation, `.pth` files or namespace packages split
  across several source roots under different layouts;
- source roots configured in `setup.cfg` (`package_dir`), Poetry `packages = [{ from = … }]`
  or Hatch build targets (only `src/` and the setuptools keys above are recognised);
- the effect of a body change on behaviour that dependents observe (a function returning a
  different value): like for every adapter, the merge check and sweeps cover that by running
  tests;
- f-strings that reuse their own quote character inside `{…}` (allowed since Python 3.12),
  which can confuse statement splitting on that line.

Third-party packages are not units: a dependency upgrade shows up as a lockfile or
`pyproject.toml` change.

## Command line

`sheltie-python` has the same commands as every adapter:

```sh
# The footprint of HEAD against main, as JSON (what the footprint workflow uploads).
sheltie-python footprint --base origin/main

# The build graph Sheltie sees for one revision.
sheltie-python graph --rev HEAD

# Replay the last 90 days of merges and estimate what Sheltie would have saved.
sheltie-python roi --ci-minutes 12
```

`footprint` takes `--ignore <glob>` and `--plain <glob>` (repeatable) for files that never
affect the build and for data files outside it; `roi` takes the options described in
[ROI report](roi.md).
