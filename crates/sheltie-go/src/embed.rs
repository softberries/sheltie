//! Resolving `//go:embed` patterns to the files they embed, by the go command's rules:
//!
//! - a pattern is a `path.Match` glob relative to the package directory, without `.` or `..`
//!   elements and without a leading or trailing `/`;
//! - a match that is a file embeds it; a match that is a directory embeds every file below
//!   it, except names starting with `.` or `_` (unless the pattern starts with `all:`);
//! - files in another module (a directory below with its own `go.mod`) are never embedded;
//! - a pattern that matches nothing, or is malformed, is a build error.
//!
//! Embedded files can only come from the package's own directory tree, so they are known
//! exactly from the tree and the package stays hermetic: they become part of the package.

use std::collections::BTreeSet;

use sheltie_adapter::TreeFiles;
use sheltie_core::RepoPath;

/// The files `patterns` embed for the package in `dir`, or `None` if any pattern is
/// malformed or matches nothing (the build would fail, so nothing is claimed).
/// `modules` are the module root directories of the repository.
pub(crate) fn resolve(
    files: &TreeFiles,
    dir: &str,
    patterns: &[String],
    modules: &BTreeSet<&str>,
) -> Option<BTreeSet<RepoPath>> {
    let mut out = BTreeSet::new();
    // Files of this package's directory tree that belong to its module.
    let own: Vec<(&RepoPath, &str)> = files
        .paths()
        .filter_map(|p| {
            let rel = if dir.is_empty() {
                p.as_str()
            } else {
                p.as_str().strip_prefix(dir)?.strip_prefix('/')?
            };
            let nested = modules.iter().any(|m| {
                let m_rel = if dir.is_empty() {
                    Some(*m)
                } else {
                    m.strip_prefix(dir).and_then(|r| r.strip_prefix('/'))
                };
                m_rel.is_some_and(|m| !m.is_empty() && rel.starts_with(&format!("{m}/")))
            });
            (!nested).then_some((p, rel))
        })
        .collect();
    for pattern in patterns {
        let (all, pattern) = match pattern.strip_prefix("all:") {
            Some(p) => (true, p),
            None => (false, pattern.as_str()),
        };
        let segments: Vec<&str> = pattern.split('/').collect();
        if pattern.is_empty()
            || segments
                .iter()
                .any(|s| s.is_empty() || *s == "." || *s == "..")
        {
            return None;
        }
        let mut matched = false;
        for (path, rel) in &own {
            let parts: Vec<&str> = rel.split('/').collect();
            if parts.len() < segments.len() {
                continue;
            }
            let mut ok = true;
            for (s, p) in segments.iter().zip(&parts) {
                match glob(s, p) {
                    Some(true) => {}
                    Some(false) => {
                        ok = false;
                        break;
                    }
                    None => return None,
                }
            }
            if !ok {
                continue;
            }
            // Below a matched directory, hidden and `_` names are skipped unless `all:`.
            let below = &parts[segments.len()..];
            if !all
                && below
                    .iter()
                    .any(|s| s.starts_with('.') || s.starts_with('_'))
            {
                continue;
            }
            matched = true;
            out.insert((*path).clone());
        }
        if !matched {
            return None;
        }
    }
    Some(out)
}

/// `path.Match` for one path element: `*`, `?`, `[...]` (with `^` negation, ranges and
/// `\` escapes) and `\` escapes. `None` if the pattern is malformed.
fn glob(pattern: &str, name: &str) -> Option<bool> {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // Validate the whole pattern first: Go reports a malformed pattern even when an
    // earlier part already fails to match.
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '\\' if i + 1 >= p.len() => return None,
            '\\' => i += 2,
            '[' => i = class_end(&p, i)?,
            _ => i += 1,
        }
    }
    Some(matches(&p, &n))
}

/// The index after the `]` closing the class that starts at `p[start] == '['`.
fn class_end(p: &[char], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if p.get(i) == Some(&'^') {
        i += 1;
    }
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == ']' && !first {
            return Some(i + 1);
        }
        first = false;
        i += if c == '\\' { 2 } else { 1 };
        if p.get(i) == Some(&'-') && p.get(i + 1).is_some_and(|c| *c != ']') {
            i += 1;
            i += if p.get(i) == Some(&'\\') { 2 } else { 1 };
        }
        if i > p.len() {
            return None;
        }
    }
}

fn matches(p: &[char], n: &[char]) -> bool {
    match p.first() {
        None => n.is_empty(),
        Some('*') => (0..=n.len()).any(|k| matches(&p[1..], &n[k..])),
        Some('?') => !n.is_empty() && matches(&p[1..], &n[1..]),
        Some('[') => {
            let Some(end) = class_end(p, 0) else {
                return false;
            };
            let Some(&c) = n.first() else {
                return false;
            };
            class_matches(&p[1..end - 1], c) && matches(&p[end..], &n[1..])
        }
        Some('\\') if p.len() > 1 => n.first() == Some(&p[1]) && matches(&p[2..], &n[1..]),
        Some(&c) => n.first() == Some(&c) && matches(&p[1..], &n[1..]),
    }
}

/// Whether `c` is in the class body (between `[` and `]`).
fn class_matches(body: &[char], c: char) -> bool {
    let (negated, body) = match body.first() {
        Some('^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut i = 0;
    let mut hit = false;
    while i < body.len() {
        let (lo, next) = if body[i] == '\\' && i + 1 < body.len() {
            (body[i + 1], i + 2)
        } else {
            (body[i], i + 1)
        };
        i = next;
        let mut hi = lo;
        if body.get(i) == Some(&'-') && i + 1 < body.len() {
            let (h, next) = if body[i + 1] == '\\' && i + 2 < body.len() {
                (body[i + 2], i + 3)
            } else {
                (body[i + 1], i + 2)
            };
            hi = h;
            i = next;
        }
        hit |= lo <= c && c <= hi;
    }
    hit != negated
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(paths: &[&str]) -> TreeFiles {
        TreeFiles(
            paths
                .iter()
                .map(|p| (RepoPath::new(*p), "0".repeat(40)))
                .collect(),
        )
    }

    fn names(s: Option<BTreeSet<RepoPath>>) -> Option<Vec<String>> {
        s.map(|s| s.into_iter().map(|p| p.as_str().to_owned()).collect())
    }

    #[test]
    fn globs() {
        assert_eq!(glob("*.txt", "a.txt"), Some(true));
        assert_eq!(glob("*.txt", "a.md"), Some(false));
        assert_eq!(glob("a?c", "abc"), Some(true));
        assert_eq!(glob("[a-c]x", "bx"), Some(true));
        assert_eq!(glob("[^a-c]x", "bx"), Some(false));
        assert_eq!(glob("[]]", "]"), Some(true));
        assert_eq!(glob(r"\*", "*"), Some(true));
        assert_eq!(glob("[a-", "a"), None);
        assert_eq!(glob("x[", "y"), None);
    }

    #[test]
    fn files_directories_and_hidden_names() {
        let t = tree(&[
            "web/web.go",
            "web/index.html",
            "web/static/app.js",
            "web/static/.cache",
            "web/static/_draft.js",
            "web/static/img/logo.png",
            "web/sub/go.mod",
            "web/sub/data.txt",
            "other/x.txt",
        ]);
        let mods: BTreeSet<&str> = ["", "web/sub"].into();
        let r = |p: &[&str]| {
            names(resolve(
                &t,
                "web",
                &p.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                &mods,
            ))
        };
        assert_eq!(r(&["index.html"]).unwrap(), ["web/index.html"]);
        assert_eq!(
            r(&["static"]).unwrap(),
            ["web/static/app.js", "web/static/img/logo.png"]
        );
        assert_eq!(
            r(&["all:static"]).unwrap(),
            [
                "web/static/.cache",
                "web/static/_draft.js",
                "web/static/app.js",
                "web/static/img/logo.png"
            ]
        );
        assert_eq!(r(&["*.html"]).unwrap(), ["web/index.html"]);
        // Another module's files are never embedded; no match is a build error.
        assert_eq!(r(&["sub/data.txt"]), None);
        assert_eq!(r(&["missing.txt"]), None);
        assert_eq!(r(&["../other/x.txt"]), None);
        assert_eq!(r(&["/index.html"]), None);
        assert_eq!(r(&["static/"]), None);
    }

    #[test]
    fn root_package() {
        let t = tree(&["go.mod", "main.go", "assets/a.css"]);
        let mods: BTreeSet<&str> = [""].into();
        assert_eq!(
            names(resolve(&t, "", &["assets".to_owned()], &mods)).unwrap(),
            ["assets/a.css"]
        );
    }
}
