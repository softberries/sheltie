//! Repository-relative path arithmetic.

/// `rel` resolved against the directory `dir` (both relative to the repository root, `""` for
/// the root), with `.` and `..` removed. `None` when it leaves the repository or is absolute.
pub(crate) fn join(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

/// The directory of a file path (`""` for a file at the root).
pub(crate) fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Whether `path` is `dir` or under it (`""` contains everything).
pub(crate) fn within(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `path` relative to `dir`, which contains it.
pub(crate) fn relative<'a>(path: &'a str, dir: &str) -> &'a str {
    if dir.is_empty() {
        path
    } else {
        path.strip_prefix(dir)
            .map_or(path, |r| r.trim_start_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_arithmetic() {
        assert_eq!(
            join("apps/web/src", "../../../packages/ui").as_deref(),
            Some("packages/ui")
        );
        assert_eq!(
            join("", "./packages/*/src").as_deref(),
            Some("packages/*/src")
        );
        assert_eq!(join("a", "../.."), None);
        assert_eq!(join("a", "/etc"), None);
        assert_eq!(parent("a/b/c.ts"), "a/b");
        assert_eq!(parent("c.ts"), "");
        assert!(within("a/b", "a") && within("a", "a") && within("x", ""));
        assert!(!within("ab/c", "a"));
        assert_eq!(relative("a/b/c", "a"), "b/c");
        assert_eq!(relative("a/b/c", ""), "a/b/c");
    }
}
