//! Which workspace packages a `Cargo.lock` change affects.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
struct Lock {
    #[serde(default)]
    package: Vec<LockPackage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct LockPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

/// Workspace packages (no `source`) whose resolved dependency graph differs between two
/// lockfiles. Unparseable input yields `None`, meaning "assume everything".
pub(crate) fn affected_packages(
    base: Option<&str>,
    head: Option<&str>,
) -> Option<BTreeSet<String>> {
    let parse = |s: Option<&str>| -> Option<Lock> {
        match s {
            None => Some(Lock::default()),
            Some(s) => toml::from_str(s).ok(),
        }
    };
    let (base, head) = (parse(base)?, parse(head)?);
    let key = |p: &LockPackage| format!("{} {}", p.name, p.version);
    let index = |lock: &Lock| -> BTreeMap<String, LockPackage> {
        lock.package.iter().map(|p| (key(p), p.clone())).collect()
    };
    let (b, h) = (index(&base), index(&head));
    let changed: BTreeSet<String> = b
        .keys()
        .chain(h.keys())
        .filter(|k| b.get(*k) != h.get(*k))
        .cloned()
        .collect();

    let mut affected = BTreeSet::new();
    for lock in [&b, &h] {
        // Reverse edges: package key -> packages that depend on it.
        let mut dependents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (k, p) in lock {
            for dep in &p.dependencies {
                for target in resolve_dep(lock, dep) {
                    dependents.entry(target).or_default().insert(k.clone());
                }
            }
        }
        let mut stack: Vec<String> = changed.iter().cloned().collect();
        let mut seen: BTreeSet<String> = stack.iter().cloned().collect();
        while let Some(k) = stack.pop() {
            if let Some(p) = lock.get(&k)
                && p.source.is_none()
            {
                affected.insert(p.name.clone());
            }
            for d in dependents.get(&k).into_iter().flatten() {
                if seen.insert(d.clone()) {
                    stack.push(d.clone());
                }
            }
        }
    }
    Some(affected)
}

/// A lockfile dependency entry is `name`, `name version` or `name version (source)`.
fn resolve_dep(lock: &BTreeMap<String, LockPackage>, dep: &str) -> Vec<String> {
    let mut parts = dep.split_whitespace();
    let name = parts.next().unwrap_or_default();
    match parts.next() {
        Some(version) => vec![format!("{name} {version}")],
        None => lock
            .values()
            .filter(|p| p.name == name)
            .map(|p| format!("{} {}", p.name, p.version))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = ["serde"]

[[package]]
name = "tool"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"
"#;

    #[test]
    fn version_bump_affects_dependents_only() {
        let head = BASE.replace("1.0.0", "1.0.1").replace("\"aa\"", "\"bb\"");
        let affected = affected_packages(Some(BASE), Some(&head)).unwrap();
        assert_eq!(affected, BTreeSet::from(["app".to_owned()]));
    }

    #[test]
    fn identical_locks_affect_nothing() {
        assert!(
            affected_packages(Some(BASE), Some(BASE))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn garbage_means_unknown() {
        assert!(affected_packages(Some(BASE), Some("not toml [")).is_none());
    }
}
