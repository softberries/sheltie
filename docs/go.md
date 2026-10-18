# Go

`sheltie-go` computes footprints for Go repositories. It reads everything straight from git:
it does not need the `go` toolchain, a module cache or network access, and it never runs your
code.

## Units

Sheltie schedules Go code by **package**:

- Every `go.mod` is a module; its path comes from the `module` directive. A repository may hold
  several modules, and a nested module's directories do not belong to the outer one.
- Every directory with `.go` files under a module root is a package, identified by its import
  path, e.g. `example.com/shop/pricing`. Directories with only test files are packages too.
- A package owns the `.go` files directly in its directory, test files included.
  Subdirectories are their own packages.
- Like the `go` command, Sheltie skips `testdata` directories and directories whose name starts
  with `.` or `_`.

A package **reads** every package of the repository it imports, from any of its files, test
files included. Imports of other modules (the standard library, dependencies) are covered by
`go.mod` and `go.sum` below.

Two changes to the same package always conflict, even in different files. A change to a
package's body never reaches the packages that import it; a change to its interface does.

## The interface

A package's interface is what importers can see. Sheltie hashes, from its non-test files:

- the package name and `//go:build` constraints;
- exported functions and methods, signature only (the body does not count). Methods count
  whatever their receiver, since an unexported type's methods are reachable through
  interfaces and embedding;
- exported types, whole: every field (unexported ones too), method and type parameter;
- exported constants and variables, with their values. A `const ( … )` group with an exported
  name counts as a whole, because `iota` and implicit repetition tie its lines together;
- unexported types and constants that any of the above mention, such as an embedded
  unexported struct or a constant an exported one is computed from.

Comments and formatting do not count (`gofmt`, rewrapping a parameter list, rewording a doc
comment). Moving a declaration between files of the package does not count either.

A package's interface also folds in the interfaces of the packages it imports, because an
exported signature may use their types: an interface change in `pricing` reaches `cart`,
which imports it, and `cmd/shop`, which imports `cart`. A change that adds an import reads the
imported package.

The rule throughout: when unsure, include more. An extra interface change only widens a
footprint and costs some parallelism; a missed one would hide a real conflict.

## Workspace files

| Files | Target | Effect of a change |
| --- | --- | --- |
| `go.mod`, `go.work` | `$workspace` | Every package is affected |
| `go.sum`, `go.work.sum`, `vendor/**` | `$lockfile` | Every package is affected |
| `.golangci.yml`, `.golangci.yaml` | the file itself | Tracked, not leaky (see below) |

When the repository root is itself a package (a `main.go` next to `go.mod`), files outside
every other package that are neither ignored nor plain (`.github/**`, lint configuration)
count as that package's data, and make footprints leaky.

## Leaks

A package is not hermetic, and footprints that read it are marked leaky, when:

- it uses cgo (`import "C"`): C sources and headers are outside the package graph;
- it has a `//go:embed` directive: the embedded files are read at build time.

A changed file that no package owns is also leaky: inside a package's directory (data files,
`testdata/`, assembly or C files) it becomes `<package>/files`, which the package reads;
elsewhere it is assumed to affect the whole build unless declared plain. Markdown, `docs/` and
licence files are ignored.

## What it does not claim

- No type checking. Sheltie reads declarations textually; it does not resolve names across
  packages, so it cannot tell which part of an interface an importer actually uses. Any
  interface change reaches every importer.
- Build constraints are not evaluated: all files of a package count, whatever `GOOS`, `GOARCH`
  and tags they target.
- Unexported functions are never part of the interface, even when an exported variable is
  initialised by calling one.
- `replace` directives pointing outside the repository, and versions of external modules, are
  only seen as a change to `go.mod` or `go.sum`.
- Generated code counts as source: regenerate it in the same change.

## Command line

```sh
# The footprint of HEAD against the merge base with origin/main, as JSON.
sheltie-go footprint --base origin/main --head HEAD

# Write it to a file, declare data files that do not affect the build.
sheltie-go footprint --base origin/main --out footprint.json --plain 'config/**'

# The build graph Sheltie sees at one revision.
sheltie-go graph --rev HEAD

# What Sheltie would have saved on this repository (see ROI report).
sheltie-go roi --ci-minutes 12
```

`--ignore <glob>` adds files that never affect the build. `roi` takes the same options as
`sheltie-cargo roi` (see [ROI report](roi.md)).

Build it with `cargo install --git https://github.com/softberries/sheltie sheltie-go`.
