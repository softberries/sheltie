# Maven

`sheltie-maven` computes footprints for Maven repositories. It reads the POMs and the Java and
Kotlin sources straight from git: it needs no `mvn`, no JDK and no network, and it never runs
your build. It has the same commands and output as `sheltie-cargo`.

```sh
cargo install --git https://github.com/softberries/sheltie sheltie-maven
```

## Units

Every module of every reactor in the repository is a unit: the root `pom.xml`, and any other
`pom.xml` outside `src/` and `target/` (a second reactor such as guava's `android/`):

- **Modules** are found through `<modules>` (including those in `<profiles>`), recursively. A
  `<module>` can name a directory or a POM file. A `<parent>` found by its `relativePath`
  (default `../pom.xml`) is a unit too, even if no `<modules>` lists it.
- **Identity**: `groupId:artifactId`, for example `com.acme.shop:core`. A missing `groupId`
  is inherited from `<parent>`. When two reactors have a module with the same coordinates,
  both ids get the directory (`com.google.guava:guava@android/guava`), and a dependency
  resolves to the module of its own reactor.
- **Files**: the module's POM and everything under its `src/` (main and test sources,
  resources). Nested modules own their own files.
- **Dependencies**: a module reads the modules named by its `<dependencies>`, its build
  plugins, their `<dependencies>` and `annotationProcessorPaths`, including those of profiles,
  and its parent. `<dependencyManagement>` and `<pluginManagement>` only declare versions and
  create no edge. `${project.groupId}`, `${project.version}` and properties from
  `<properties>` (the parent's included) are resolved.
- **Workspace configuration**: `.mvn/**`, `mvnw` and `mvnw.cmd`. A change to them affects
  every module.

Plain files (`checkstyle*.xml` anywhere, `.editorconfig`) are tracked, but do not make a
footprint leaky. Anything else outside a module is assumed to affect the whole build; declare
data files with `--plain` (see [Workflows](workflows.md)).

## Interfaces

A change that does not alter a module's interface affects only that module. The interface
is:

- the public surface of `src/main/java` and `src/main/kotlin`;
- the whole POM, without comments and with whitespace normalised. Any POM change (a new
  dependency, a plugin setting) therefore reaches the module's dependents: pessimistic,
  because dependencies are transitive;
- the contents of `src/main/resources/META-INF/` (service registrations and similar files
  loaded by dependents at run time);
- the test sources too, when another module depends on its `test-jar`.

The public surface of a Java file is its `package` line and every `public` or `protected` type
with its header (type parameters, `extends`, `implements`, record components). In their
bodies: `public` and `protected` fields (whole, with initialisers, because `javac` inlines
constants), method and constructor signatures, enum constants, every non-private member of
interfaces and annotation types, and nested types by the same rules. Annotations count;
method bodies, comments, formatting, imports, and private and package-private declarations
do not. A `module-info.java` counts whole.

For Kotlin, everything that is not `private` or `internal` counts (`@PublishedApi internal`
does). Functions with a block body count by signature. Expression-bodied and `inline`
functions, properties with initialisers and type aliases count whole: an inferred type, a
constant or an inlined body depends on them.

When in doubt the adapter includes more: an extra declaration only makes footprints wider,
a missed one would hide a conflict.

| Change | Footprint |
| --- | --- |
| Method body, private member, test, resource | the module |
| Public signature, public constant, record component | the module and the modules that depend on it |
| The module's POM | the module and its dependents |
| Parent POM | every module inheriting from it |
| `.mvn/**`, `mvnw` | every module |
| `README.md`, `docs/**` | nothing |

A module's interface also folds in the interfaces of the modules it depends on, because its
public signatures may use their types: an interface change in one module reaches its
dependents and theirs. A change that adds a dependency reads the new dependency.

## Leaks

A module is not hermetic when its build runs code Sheltie cannot see: `exec-maven-plugin`,
`maven-antrun-plugin` or `build-helper-maven-plugin` in its `<build><plugins>` or its
parent's (inherited). Footprints that read such a module are leaky and give the reason, for
example `com.acme.shop:api: build plugin runs code (exec-maven-plugin)`.

## What it does not claim

- **Reflection, dependency injection, service loading by name**: code that finds classes or
  members at run time (Spring, Jackson, `Class.forName`) can depend on a private or
  package-private declaration. Such changes look local.
- **Annotation processors and compiler plugins**: generated code (Lombok, MapStruct, Dagger,
  Immutables) is not seen, so a change to an annotated private field can change a generated
  public API unnoticed. A processor built in the same repository is a dependency of the
  modules that use it.
- **Split packages**: package-private declarations are ignored, though another module (often
  its tests) can use them if it declares the same package.
- **Imports**: changing which class a simple name in a signature refers to only through an
  import is not seen.
- **Resolution**: dependency versions, BOMs, `<scope>` and `<optional>` are not resolved.
  Every POM change counts as an interface change, and changes to external dependencies are
  not seen beyond the POM text.
- **Other layouts**: `src/main/{java,kotlin}`, Kotlin Multiplatform `src/<x>Main` and a
  `<sourceDirectory>` / `<testSourceDirectory>` set in the module or a parent feed the
  interface. Generated sources and Groovy or Scala sources are not read.
- **Other plugins**: only the three plugins above are flagged as leaky. Any plugin can read
  files outside the module; if yours do, declare the files or treat them as workspace
  configuration.

## Command line

```sh
# The footprint of HEAD against main, as JSON
sheltie-maven footprint --base origin/main

# The build graph of a revision, as JSON
sheltie-maven graph --rev HEAD

# What Sheltie would have saved over the last 90 days (see the ROI report)
sheltie-maven roi --ci-minutes 15
```

`footprint` takes `--head`, `--repo`, `--out` and repeatable `--ignore` and `--plain` globs.
`roi` takes the options in [ROI report](roi.md); with `--exact` it computes each landing's
footprint with the adapter, which needs only git (no Maven).
