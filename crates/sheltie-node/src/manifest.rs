//! Reading `package.json`, `project.json`, `pnpm-workspace.yaml`, `lerna.json` and
//! `tsconfig.json`.

use std::collections::BTreeSet;

use serde_json::Value;
use sheltie_adapter::scan::strip_c_comments;

/// `package.json` fields that dependents see.
const IFACE_FIELDS: &[&str] = &[
    "name",
    "version",
    "main",
    "module",
    "types",
    "typings",
    "exports",
    "bin",
    "type",
    "dependencies",
    "peerDependencies",
];

/// Dependency fields whose names can be workspace packages.
const DEP_FIELDS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

/// Lifecycle scripts that run on install: the build is then not a function of the files.
const INSTALL_SCRIPTS: &[&str] = &["preinstall", "install", "postinstall", "prepare"];

/// Workspace globs from a root `package.json`: `workspaces` as an array or as
/// `{ "packages": [...] }`.
pub(crate) fn npm_workspaces(pkg: &Value) -> Vec<String> {
    let ws = pkg.get("workspaces");
    let list = ws
        .and_then(Value::as_array)
        .or_else(|| ws.and_then(|w| w.get("packages")).and_then(Value::as_array));
    strings(list)
}

/// Package globs from `lerna.json` (`packages`, default `packages/*`).
pub(crate) fn lerna_packages(lerna: &Value) -> Vec<String> {
    match lerna.get("packages").and_then(Value::as_array) {
        Some(list) => strings(Some(list)),
        None => vec!["packages/*".to_owned()],
    }
}

fn strings(list: Option<&Vec<Value>>) -> Vec<String> {
    list.into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The `packages:` list of a `pnpm-workspace.yaml`, as a block list (`- 'packages/*'`) or a
/// flow list (`['a', 'b']`). Quotes and trailing comments are removed; other keys are
/// ignored.
pub(crate) fn pnpm_packages(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_packages = false;
    for line in yaml.lines() {
        let content = uncomment(line);
        if content.trim().is_empty() {
            continue;
        }
        let indented = content.starts_with([' ', '\t']);
        if !indented {
            in_packages = false;
            if let Some(rest) = content.strip_prefix("packages:") {
                let rest = rest.trim();
                if let Some(flow) = rest.strip_prefix('[') {
                    let flow = flow.trim_end().trim_end_matches(']');
                    out.extend(flow.split(',').map(unquote).filter(|s| !s.is_empty()));
                } else {
                    in_packages = true;
                }
            }
            continue;
        }
        if in_packages && let Some(item) = content.trim().strip_prefix('-') {
            let item = unquote(item);
            if !item.is_empty() {
                out.push(item);
            }
        }
    }
    out
}

/// The line without a `#` comment outside quotes.
fn uncomment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev.is_whitespace() => return &line[..i],
            None => {}
        }
        prev = c;
    }
    line
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    for q in ['"', '\''] {
        if let Some(inner) = s.strip_prefix(q).and_then(|r| r.strip_suffix(q)) {
            return inner.to_owned();
        }
    }
    s.to_owned()
}

/// Names a `package.json` depends on, in every dependency field.
pub(crate) fn dependency_names(pkg: &Value) -> BTreeSet<String> {
    DEP_FIELDS
        .iter()
        .filter_map(|f| pkg.get(*f).and_then(Value::as_object))
        .flat_map(|deps| deps.keys().cloned())
        .collect()
}

/// Nx `implicitDependencies` of a `project.json` (or a `package.json`'s `nx` section),
/// without `!excluded` entries.
pub(crate) fn implicit_dependencies(project: &Value) -> BTreeSet<String> {
    project
        .get("implicitDependencies")
        .or_else(|| project.get("nx")?.get("implicitDependencies"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|d| !d.starts_with('!'))
        .map(str::to_owned)
        .collect()
}

/// Lifecycle scripts that run on install.
pub(crate) fn install_scripts(pkg: &Value) -> Vec<&'static str> {
    let scripts = pkg.get("scripts").and_then(Value::as_object);
    INSTALL_SCRIPTS
        .iter()
        .copied()
        .filter(|s| scripts.is_some_and(|m| m.contains_key(*s)))
        .collect()
}

/// The `name` field.
pub(crate) fn name(v: &Value) -> Option<&str> {
    v.get("name")
        .and_then(Value::as_str)
        .filter(|n| !n.trim().is_empty())
}

/// Canonical JSON of the `package.json` fields dependents see (keys sorted at every level).
pub(crate) fn iface_fields(pkg: &Value) -> String {
    let mut out = String::from("{");
    for f in IFACE_FIELDS {
        if let Some(v) = pkg.get(*f) {
            out.push_str(&Value::String((*f).to_owned()).to_string());
            out.push(':');
            canonical(v, &mut out);
            out.push(',');
        }
    }
    out.push('}');
    out
}

fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for k in keys {
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                if let Some(v) = map.get(k) {
                    canonical(v, out);
                }
                out.push(',');
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for i in items {
                canonical(i, out);
                out.push(',');
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Parses JSON with comments and trailing commas, as in `tsconfig.json`.
pub(crate) fn parse_jsonc(text: &str) -> Option<Value> {
    let text = strip_c_comments(text);
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    serde_json::from_str(&out).ok()
}

/// `compilerOptions.paths` of a tsconfig: each alias pattern with its target patterns,
/// relative to the repository root. `dir` is the tsconfig's directory.
pub(crate) fn tsconfig_paths(tsconfig: &Value, dir: &str) -> Vec<(String, Vec<String>)> {
    let Some(opts) = tsconfig.get("compilerOptions") else {
        return Vec::new();
    };
    let base_url = opts.get("baseUrl").and_then(Value::as_str).unwrap_or(".");
    let base = crate::paths::join(dir, base_url);
    let Some(paths) = opts.get("paths").and_then(Value::as_object) else {
        return Vec::new();
    };
    paths
        .iter()
        .map(|(alias, targets)| {
            let targets = targets
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter_map(|t| crate::paths::join(base.as_deref()?, t))
                .collect();
            (alias.clone(), targets)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn npm_workspaces_as_array_or_object() {
        let a = json!({ "workspaces": ["packages/*", "apps/web"] });
        assert_eq!(npm_workspaces(&a), ["packages/*", "apps/web"]);
        let o = json!({ "workspaces": { "packages": ["libs/**"], "nohoist": ["x"] } });
        assert_eq!(npm_workspaces(&o), ["libs/**"]);
        assert!(npm_workspaces(&json!({ "name": "x" })).is_empty());
        assert_eq!(lerna_packages(&json!({})), ["packages/*"]);
        assert_eq!(
            lerna_packages(&json!({ "packages": ["modules/*"] })),
            ["modules/*"]
        );
    }

    #[test]
    fn pnpm_workspace_yaml() {
        let yaml = "# workspace\npackages:\n  - 'packages/*'\n  - \"apps/**\" # apps\n  - tools/cli\n  - '!**/test/**'\n\ncatalog:\n  react: ^18\n  - not-a-package\n";
        assert_eq!(
            pnpm_packages(yaml),
            ["packages/*", "apps/**", "tools/cli", "!**/test/**"]
        );
        assert_eq!(pnpm_packages("packages: ['a/*', \"b\"]\n"), ["a/*", "b"]);
        assert!(pnpm_packages("onlyBuiltDependencies:\n  - esbuild\n").is_empty());
    }

    #[test]
    fn dependencies_scripts_and_interface() {
        let pkg = json!({
            "name": "@acme/web",
            "dependencies": { "@acme/ui": "workspace:*", "react": "^18" },
            "devDependencies": { "@acme/utils": "*" },
            "scripts": { "build": "tsc", "postinstall": "node fetch.js" },
            "nx": { "implicitDependencies": ["docs", "!api"] }
        });
        assert_eq!(
            dependency_names(&pkg).into_iter().collect::<Vec<_>>(),
            ["@acme/ui", "@acme/utils", "react"]
        );
        assert_eq!(install_scripts(&pkg), ["postinstall"]);
        assert_eq!(
            implicit_dependencies(&pkg).into_iter().collect::<Vec<_>>(),
            ["docs"]
        );
        // Key order does not matter, scripts and devDependencies are not part of it.
        let same = json!({
            "devDependencies": {},
            "dependencies": { "react": "^18", "@acme/ui": "workspace:*" },
            "name": "@acme/web",
            "scripts": {}
        });
        assert_eq!(iface_fields(&pkg), iface_fields(&same));
        let bumped = json!({ "name": "@acme/web", "dependencies": { "react": "^19", "@acme/ui": "workspace:*" } });
        assert_ne!(iface_fields(&pkg), iface_fields(&bumped));
    }

    #[test]
    fn tsconfig_with_comments_and_paths() {
        let text = "{\n  // base\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@acme/utils\": [\"packages/utils/src/index.ts\",], \"@acme/*\": [\"packages/*/src\"] },\n  },\n}\n";
        let v = parse_jsonc(text).unwrap();
        let paths = tsconfig_paths(&v, "");
        assert_eq!(
            paths,
            [
                ("@acme/*".to_owned(), vec!["packages/*/src".to_owned()]),
                (
                    "@acme/utils".to_owned(),
                    vec!["packages/utils/src/index.ts".to_owned()]
                ),
            ]
        );
        assert_eq!(
            parse_jsonc("{\"a\": \"x,}\"}").unwrap(),
            json!({ "a": "x,}" })
        );
    }
}
