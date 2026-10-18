# Gradle

`sheltie-gradle` computes footprints for Gradle repositories, with Groovy or Kotlin DSL build
scripts and Java or Kotlin sources. It reads the scripts and sources straight from git: it
needs no Gradle, no JDK and no network, and it never evaluates a script. It has the same
commands and output as `sheltie-cargo`.

```sh
cargo install --git https://github.com/softberries/sheltie sheltie-gradle
```

## Units

Every project in `settings.gradle` or `settings.gradle.kts` is a unit:

- **Projects** come from `include` statements in their common forms: `include ':a', ':b:c'`,
  `include(":a", ":b")`, `include("a")`, spread over several lines. `:a:b` lives in `a/b`,
  unless the settings set `project(":a:b").projectDir = file("dir")` (or
  `new File(settingsDir, "dir")`). Parents of nested projects (`:a` for `:a:b`) are projects
  too. The root project `:` is a unit if it has a `src/` directory.
- **Identity**: the Gradle path, for example `:core` or `:libs:io`.
- **Files**: the project's `build.gradle(.kts)`, its `gradle.properties` and everything under
  its `src/`.
- **Dependencies**: a project reads the projects its build script names: `project(":x")`,
  `project(':x')`, `project(path: ":x")`, `project(path = ":x")`, and type-safe accessors such
  as `projects.core` or `projects.libs.sharedUtils` (kebab- and snake-case names become
  camelCase). The whole script is searched, not only `dependencies { }`.
- **Workspace configuration**: the settings script, the root build script,
  `gradle.properties`, version catalogs (`gradle/*.versions.toml`), the wrapper
  (`gradle/wrapper/**`, `gradlew`, `gradlew.bat`), convention plugins in `buildSrc/` and
  `build-logic/`, and builds included with `includeBuild`. A change to them affects every
  project.
- **Lockfiles**: `gradle.lockfile` (in any project) and `gradle/verification-metadata.xml`.
  A change to them affects every project.

Plain files (`.editorconfig`, `detekt.yml` anywhere) are tracked, but do not make a footprint
leaky. Anything else outside a project is assumed to affect the whole build; declare data
files with `--plain` (see [Workflows](workflows.md)).

## Interfaces

A change that does not alter a project's interface affects only that project. The interface
is:

- the public surface of the main sources: `src/main/{java,kotlin}`, Kotlin Multiplatform
  `src/<x>Main/{java,kotlin}` and `src/testFixtures/{java,kotlin}` (consumed with
  `testFixtures(project(":x"))`);
- the project's build script and `gradle.properties`, without comments and with whitespace
  normalised. Any change to them (a new dependency, a plugin setting) therefore reaches the
  project's dependents: pessimistic, because `api` dependencies are transitive;
- the contents of `src/main/resources/META-INF/` (service registrations and similar files
  loaded by dependents at run time).

The public surface of a Kotlin file is its `package` line (with file annotations such as
`@file:JvmName`) and every declaration that is not `private` or `internal`
(`@PublishedApi internal` counts). Classes, interfaces and objects count by their header and
the surface of their body. Functions with a block body count by signature. Expression-bodied
and `inline` functions, properties with initialisers and type aliases count whole: an
inferred type, a constant or an inlined body depends on them.

For Java, the `package` line and every `public` or `protected` type with its header count;
in their bodies, `public` and `protected` fields (whole, with initialisers), method and
constructor signatures, enum constants, every non-private member of interfaces and
annotation types, and nested types by the same rules. Annotations count; method bodies,
comments, formatting, imports, and private and package-private declarations do not.

When in doubt the adapter includes more: an extra declaration only makes footprints wider,
a missed one would hide a conflict.

| Change | Footprint |
| --- | --- |
| Block body, private member, test, resource | the project |
| Public signature, expression-bodied public function, public constant | the project and the projects that depend on it |
| The project's build script | the project and its dependents |
| Settings, root build script, version catalog, `buildSrc/` | every project |
| `gradle.lockfile` | every project |
| `README.md`, `docs/**` | nothing |

## Leaks

Projects are hermetic by default. Files a build script reads from outside its project
(`apply from: "../shared.gradle"`, `file("../data")`) are outside every unit, so changes to
them make footprints leaky unless they are under a project's `src/` or workspace
configuration.

## What it does not claim

- **Script evaluation**: projects included by computed names (`include(":gen:$name")`, loops
  over directories), dependencies added by convention plugins or `subprojects { }` blocks,
  and custom `buildFileName`s are not seen. Convention plugins and the root build script are
  workspace configuration, so changing them still affects every project.
- **Reflection, dependency injection, service loading by name**: code that finds classes or
  members at run time can depend on a private or internal declaration. Such changes look
  local.
- **Annotation processors and compiler plugins**: code generated by kapt, KSP or Java
  annotation processors (Room, Dagger, Moshi, Lombok) is not seen, so a change to an
  annotated private declaration can change a generated public API unnoticed.
- **Android**: build variants, flavours and source sets other than `main` (such as
  `src/debug`), `res/` and `AndroidManifest.xml` are owned by the project but do not feed its
  interface; resource IDs shared between modules are not compared.
- **Imports**: changing which class a simple name in a signature refers to only through an
  import is not seen.
- **Resolution**: dependency versions and configurations are not resolved. Every build
  script change counts as an interface change.

## Command line

```sh
# The footprint of HEAD against main, as JSON
sheltie-gradle footprint --base origin/main

# The build graph of a revision, as JSON
sheltie-gradle graph --rev HEAD

# What Sheltie would have saved over the last 90 days (see the ROI report)
sheltie-gradle roi --ci-minutes 15
```

`footprint` takes `--head`, `--repo`, `--out` and repeatable `--ignore` and `--plain` globs.
`roi` takes the options in [ROI report](roi.md); with `--exact` it computes each landing's
footprint with the adapter, which needs only git (no Gradle).
