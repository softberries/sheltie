# JavaScript and TypeScript

`sheltie-node` computes footprints for JavaScript and TypeScript repositories: npm, Yarn,
pnpm and Bun workspaces, Lerna and Nx. It reads everything from git, so the runner needs
`git` but not Node.js, a package manager or `node_modules`.

## Supported layouts

`sheltie-node` finds the packages of a revision, in this order:

| Source | What it reads |
| --- | --- |
| Root `package.json` | `workspaces`, as an array or as `{ "packages": [...] }` (npm, Yarn, Bun) |
| `pnpm-workspace.yaml` | the `packages:` list, quoted or not, with `!` exclusions |
| `lerna.json` | `packages` (default `packages/*`) |
| Nx | every directory with a `project.json` |

A directory matching a workspace glob that has a `package.json` is a package. Globs work as
in the package managers: `packages/*` matches one level, `packages/**` any depth, and
`!pattern` excludes. Directories under `node_modules/` never count.

With none of these, a root `package.json` makes the whole repository one package.

## Packages

Each package (a *unit*) is one target in the build graph:

- **Name**: the `name` in its `package.json`, else the Nx project name in `project.json`,
  else its directory.
- **Files**: everything under its directory except `node_modules/`, `dist/`, `build/`,
  `out/`, `coverage/`, `.next/`, `.turbo/`, nested packages, documentation (`*.md`) and lint
  or format configuration.
- **Dependencies**: workspace packages named in `dependencies`, `devDependencies`,
  `peerDependencies` or `optionalDependencies` (any version, including `workspace:*`); Nx
  `implicitDependencies` (except `!excluded`); and workspace packages its sources import, by
  package name (`@acme/ui`, `@acme/ui/button`), by a `paths` alias from the root
  `tsconfig.base.json` or `tsconfig.json`, or by a relative path into another package's
  directory.
- **Tests**: a change to a package runs its tests, and those of every package it affects.

## The public interface

A change to a package affects the packages that depend on it only if it changes the
package's public interface. The interface is:

1. the `package.json` fields `name`, `version`, `main`, `module`, `types`, `typings`,
   `exports`, `bin`, `type`, `dependencies` and `peerDependencies`;
2. from its sources (`.ts`, `.tsx`, `.mts`, `.cts`, `.js`, `.jsx`, `.mjs`, `.cjs`), except
   tests (`*.test.*`, `*.spec.*`, and anything under `__tests__/`, `test/` or `tests/`):
   - every `.d.ts` file;
   - every top-level `export` statement, including `export * from`, `export { a } from`
     and `export default`;
   - every top-level `type`, `interface`, `enum`, `declare` and `class`, exported or not,
     because they can appear in exported signatures;
   - declarations exported by name (`export { format }`, `module.exports = { format }`) and
     CommonJS `module.exports` / `exports.x` statements.

Comments and formatting do not count. Of functions, arrow functions assigned to a `const`,
and class methods, only the signature counts, when the body can be told apart from it: a
change inside a function body does not reach dependents. When it cannot (for example
`export const f = memo((props) => { … })`), the whole statement counts. Objects of functions
(`export const api = { get() { … } }`) also count whole.

A package's interface also folds in the interfaces of the workspace packages it depends on,
because its exports may use their types: an interface change in `@acme/utils` reaches
`@acme/ui`, which depends on it, and the apps that depend on `@acme/ui`. A change that adds a
dependency reads the new dependency.

The rule throughout is to include more when unsure. An extra line in the interface only
makes a footprint wider; a missing one would hide a real conflict.

## Workspace files

| Target | Files | Effect of a change |
| --- | --- | --- |
| `$workspace` | root `package.json`, `tsconfig.json`, `tsconfig.base.json`, `nx.json`, `pnpm-workspace.yaml`, `lerna.json`, `turbo.json`, `.npmrc`, `.nvmrc`, `.node-version`, `.yarnrc`, `.yarnrc.yml`, `.pnpmfile.cjs` | every package is affected |
| `$lockfile` | `package-lock.json`, `npm-shrinkwrap.json`, `yarn.lock`, `pnpm-lock.yaml`, `bun.lock`, `bun.lockb` | every package is affected |

Lint and format configuration (`.eslintrc*`, `eslint.config.*`, `.prettierrc*`,
`prettier.config.*`, `.prettierignore`, `.eslintignore`, `.editorconfig`, at any depth) is
tracked, so two changes to the same file conflict, but it does not affect any package.
Documentation (`*.md`, `docs/`) is ignored.

Any other file outside every package, such as `scripts/release.mjs` or a root
`jest.config.js`, is assumed to affect the whole build: the footprint is *leaky* and the
change conflicts with everything. Declare data files that do not affect the build with
`--plain` (or `plain:` in the [footprint workflow](workflows.md)).

## Leaks

A footprint is leaky when the build may depend on more than the files Sheltie sees:

- a package whose `package.json` has a `preinstall`, `install`, `postinstall` or `prepare`
  script (`install script`): it can download or generate anything;
- a package whose `package.json` is not valid JSON;
- a changed file outside every package (see above).

## What it does not claim

- It does not run TypeScript. The interface is read line by line; an unusual construct is
  counted whole rather than missed, and a file whose brackets do not balance (for example
  because of a regular expression containing `{`) counts whole.
- Data files inside a package (`src/fixtures/orders.json`) belong to the package but are not
  part of its interface, even when a module re-exports them.
- A package's own `tsconfig.json`, bundler and test configuration belong to the package but
  are not part of its interface.
- In a workspace, install scripts in the root `package.json` (such as `prepare: husky`) do
  not make footprints leaky; changes to the root `package.json` affect every package anyway.
  (Without workspaces the root `package.json` is the package's own, and its install scripts
  count.)
- The order of conditions in `exports` is not compared, only their content.
- Lockfiles count as a whole: any lockfile change affects every package.

## Command line

`sheltie-node` has the same commands as `sheltie-cargo`. Run them in a checkout with
history:

```sh
# The footprint of HEAD against main, as JSON
sheltie-node footprint --base origin/main

# The build graph of a revision, as JSON
sheltie-node graph --rev HEAD

# What Sheltie would have saved over the last 90 days (see the ROI report)
sheltie-node roi --ci-minutes 12
```

`footprint` takes `--head` (default `HEAD`), `--repo`, `--out`, and repeatable `--ignore` and
`--plain` globs. `roi` takes the options listed in [ROI report](roi.md).
