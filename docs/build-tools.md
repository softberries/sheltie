# Build tools

Sheltie learns what a pull request touches from an **adapter** for your build tool. The
adapter runs on your CI runner (the [footprint workflow](workflows.md) installs and runs it),
reads the base and head trees straight from git, and describes each as a graph of **units**
(crates, packages, modules) with their dependencies and a hash of their **public interface**.
Two changes conflict only when one writes a unit the other writes or affects, where a change
affects the dependents of a unit only if it changes that unit's interface.

| Build tool | Adapter | Unit | Interface | Page |
| --- | --- | --- | --- | --- |
| Cargo | `sheltie-cargo` | module | parsed with `syn` | [Workflows](workflows.md) |
| Go | `sheltie-go` | package | exported declarations | [Go](go.md) |
| JavaScript / TypeScript: npm, yarn, pnpm workspaces, Nx | `sheltie-node` | workspace package | exports and types | [JavaScript and TypeScript](node.md) |
| Python: uv, Poetry, setuptools | `sheltie-python` | import package | public signatures | [Python](python.md) |
| Maven | `sheltie-maven` | module | public Java and Kotlin API | [Maven](maven.md) |
| Gradle | `sheltie-gradle` | project | public Java and Kotlin API | [Gradle](gradle.md) |

Bazel, sbt and Mill are planned.

## How accurate it is

The Cargo adapter parses Rust and compares module interfaces exactly. The others read the
build files and the sources without running the build tool or a compiler, and extract the
public surface from declarations line by line. They are deliberately pessimistic:

- When a declaration is ambiguous, more of it counts as interface.
- A unit's interface folds in the interfaces of the units it depends on, because its own API
  may expose their types. An interface change therefore reaches dependents of dependents.
- A change that adds a dependency reads the new dependency, so it meets a concurrent change
  to that dependency.

The cost is that some changes look like interface changes when they are not, so a few more
pull requests are built together than strictly needed. Body-only changes still stay inside
their unit.

What no adapter sees: code loaded at run time by name (reflection, dynamic imports, plugin
systems), and files read by tests or the build that are not part of a unit's sources. Files
inside a unit's directory that are not its sources make the footprint *leaky*: Sheltie then
widens it to everything that reads the unit.

Workspace-wide files (root manifests, toolchain versions, shared build logic) and dependency
lockfiles affect every unit.

## The command line

Every adapter has the same commands:

```sh
sheltie-go footprint --base origin/main --head HEAD   # the footprint, as JSON
sheltie-go graph --rev HEAD                           # the build graph, as JSON
sheltie-go roi --ci-minutes 12                        # the ROI report from history
```

`--ignore` and `--plain` add globs for files outside the build (see
[Workflows](workflows.md)); `roi` is described in [ROI report](roi.md). All adapters come with
every [release](https://github.com/softberries/sheltie/releases).
