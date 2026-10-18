//! What the adapter reads from Gradle scripts (Groovy or Kotlin DSL), without running them:
//! `include` statements in settings, project dependencies in build files.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;
use sheltie_adapter::scan::strip_c_comments;

fn re(pattern: &str) -> Regex {
    // Every pattern is a constant, covered by the tests below.
    Regex::new(pattern).unwrap_or_else(|e| unreachable!("constant regex {pattern}: {e}"))
}

static INCLUDE: LazyLock<Regex> = LazyLock::new(|| re(r"\binclude\b"));
static STRING: LazyLock<Regex> = LazyLock::new(|| re(r#""([^"\\\n]*)"|'([^'\\\n]*)'"#));
static INCLUDE_BUILD: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\bincludeBuild\s*\(?\s*["']([^"'\n]+)["']"#));
static PROJECT_DIR: LazyLock<Regex> = LazyLock::new(|| {
    re(concat!(
        r#"\bproject\s*\(\s*["']([^"'\n]+)["']\s*\)\s*\.projectDir\s*=\s*"#,
        r#"(?:file\s*\(\s*["']([^"'\n]+)["']\s*\)"#,
        r#"|(?:new\s+)?File\s*\(\s*(?:settingsDir|rootDir)\s*,\s*["']([^"'\n]+)["']\s*\))"#,
    ))
});
static PROJECT: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\bproject\s*\(\s*(?:path\s*[:=]\s*)?["'](:[^"'\n]*)["']"#));
static ACCESSOR: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bprojects((?:\s*\.\s*[A-Za-z_][A-Za-z0-9_]*)+)"));

/// What `settings.gradle(.kts)` declares.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    /// Included project paths (`:a:b`), in order.
    pub(crate) includes: Vec<String>,
    /// Project directories set with `project(":x").projectDir = …`.
    pub(crate) dirs: BTreeMap<String, String>,
    /// Directories of included builds (`includeBuild`).
    pub(crate) builds: Vec<String>,
}

/// A project path in canonical form: leading `:`, no trailing one.
pub(crate) fn canonical(path: &str) -> String {
    let p = path.trim().trim_end_matches(':');
    if p.starts_with(':') {
        p.to_owned()
    } else {
        format!(":{p}")
    }
}

/// The default directory of a project path: `:a:b` is `a/b`.
pub(crate) fn default_dir(path: &str) -> String {
    path.trim_start_matches(':').replace(':', "/")
}

/// Reads a settings script. Includes built from variables (`"$x"`) are skipped.
pub(crate) fn settings(text: &str) -> Settings {
    let code = strip_c_comments(text);
    let mut s = Settings::default();
    for m in INCLUDE.find_iter(&code) {
        for lit in STRING.captures_iter(include_args(&code[m.end()..])) {
            let Some(v) = lit.get(1).or_else(|| lit.get(2)).map(|v| v.as_str()) else {
                continue;
            };
            if !v.contains('$') && !v.trim().is_empty() {
                s.includes.push(canonical(v));
            }
        }
    }
    for c in PROJECT_DIR.captures_iter(&code) {
        let dir = c.get(2).or_else(|| c.get(3)).map(|d| d.as_str());
        if let (Some(path), Some(dir)) = (c.get(1), dir) {
            s.dirs.insert(canonical(path.as_str()), clean_dir(dir));
        }
    }
    s.builds = INCLUDE_BUILD
        .captures_iter(&code)
        .filter_map(|c| c.get(1))
        .map(|d| clean_dir(d.as_str()))
        .filter(|d| !d.is_empty())
        .collect();
    s
}

fn clean_dir(dir: &str) -> String {
    dir.trim()
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_owned()
}

/// The arguments of an `include` call starting at `rest`: up to the matching `)`, or to the
/// end of the line (continued while it ends with a comma).
fn include_args(rest: &str) -> &str {
    let trimmed = rest.trim_start_matches([' ', '\t']);
    let offset = rest.len() - trimmed.len();
    if trimmed.starts_with('(') {
        let mut depth = 0usize;
        for (i, c) in trimmed.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return &rest[..offset + i];
                    }
                }
                _ => {}
            }
        }
        return rest;
    }
    let mut end = 0;
    for line in rest.split_inclusive('\n') {
        end += line.len();
        if !line.trim_end().ends_with(',') {
            break;
        }
    }
    &rest[..end]
}

/// Project paths a build script refers to: `project(":x")`, `project(':x')`,
/// `project(path: ":x")`, `project(path = ":x")`.
pub(crate) fn project_refs(text: &str) -> BTreeSet<String> {
    let code = strip_c_comments(text);
    PROJECT
        .captures_iter(&code)
        .filter_map(|c| c.get(1))
        .map(|p| canonical(p.as_str()))
        .collect()
}

/// Type-safe project accessor chains a build script uses: `projects.a.bC` is `["a", "bC"]`.
pub(crate) fn accessor_refs(text: &str) -> BTreeSet<Vec<String>> {
    let code = strip_c_comments(text);
    ACCESSOR
        .captures_iter(&code)
        .filter_map(|c| c.get(1))
        .map(|chain| {
            chain
                .as_str()
                .split('.')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .collect()
}

/// The accessor chain of a project path: kebab- and snake-case names become camelCase
/// (`:shared-utils:io` is `["sharedUtils", "io"]`).
pub(crate) fn accessor(path: &str) -> Vec<String> {
    path.split(':')
        .filter(|s| !s.is_empty())
        .map(|name| {
            let mut out = String::new();
            let mut upper = false;
            for c in name.chars() {
                if c == '-' || c == '_' {
                    upper = true;
                } else if upper {
                    out.extend(c.to_uppercase());
                    upper = false;
                } else {
                    out.push(c);
                }
            }
            out
        })
        .collect()
}

/// A build script without comments, whitespace-normalised: what dependents see.
pub(crate) fn normalized(text: &str) -> String {
    sheltie_adapter::scan::normalize(&strip_c_comments(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn include_forms() {
        let groovy = "rootProject.name = 'shop'\n\
            include ':a', ':b:c'\n\
            include ':d',\n        ':e'\n\
            include 'f'\n\
            // include ':commented'\n\
            /* include ':block' */\n\
            includeBuild 'build-logic'\n\
            includeFlat 'sibling'\n\
            project(':b:c').projectDir = new File(settingsDir, 'modules/c')\n";
        let s = settings(groovy);
        assert_eq!(s.includes, [":a", ":b:c", ":d", ":e", ":f"]);
        assert_eq!(s.dirs[":b:c"], "modules/c");
        assert_eq!(s.builds, ["build-logic"]);

        let kotlin = "pluginManagement { includeBuild(\"./build-logic/\") }\n\
            include(\":core\", \":api\")\n\
            include(\"app\")\n\
            include(\n    \":libs:shared-utils\",\n    \":libs:io\",\n)\n\
            listOf(\"x\").forEach { include(\":gen:$it\") }\n\
            project(\":app\").projectDir = file(\"apps/app\")\n";
        let s = settings(kotlin);
        assert_eq!(
            s.includes,
            [":core", ":api", ":app", ":libs:shared-utils", ":libs:io"]
        );
        assert_eq!(s.dirs[":app"], "apps/app");
        assert_eq!(s.builds, ["build-logic"]);
    }

    #[test]
    fn project_dependencies() {
        let build = "dependencies {\n\
            implementation(project(\":core\"))\n\
            api project(':shared:io')\n\
            testImplementation(project(path: ':testing', configuration: 'fixtures'))\n\
            runtimeOnly(project(path = \":runtime\"))\n\
            implementation(projects.sharedUtils)\n\
            implementation(projects.libs.io)\n\
            // implementation(project(\":commented\"))\n\
            implementation(testFixtures(projects .core))\n\
        }\n";
        assert_eq!(
            project_refs(build).into_iter().collect::<Vec<_>>(),
            [":core", ":runtime", ":shared:io", ":testing"]
        );
        let chains: Vec<Vec<String>> = accessor_refs(build).into_iter().collect();
        assert_eq!(
            chains,
            [
                vec!["core".to_owned()],
                vec!["libs".into(), "io".into()],
                vec!["sharedUtils".into()]
            ]
        );
    }

    #[test]
    fn names_and_paths() {
        assert_eq!(accessor(":libs:shared-utils"), ["libs", "sharedUtils"]);
        assert_eq!(accessor(":snake_case"), ["snakeCase"]);
        assert_eq!(canonical("a:b"), ":a:b");
        assert_eq!(canonical(":a:"), ":a");
        assert_eq!(default_dir(":a:b"), "a/b");
        assert_eq!(normalized("a  // x\n  b"), "a b");
    }
}
