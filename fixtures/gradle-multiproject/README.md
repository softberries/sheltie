# shop (Gradle)

A three-project Gradle build (Kotlin DSL) used by Sheltie's tests: `:core` (Kotlin, money and
prices), `:api` (Java, depends on `:core` through `projects.core`) and `:app` (Java, depends on
`project(":api")`), with a version catalog in `gradle/libs.versions.toml`.
