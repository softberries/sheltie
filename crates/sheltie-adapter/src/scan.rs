//! Helpers for extracting interfaces from source text without a compiler.
//!
//! Adapters for tools without a cheap, exact interface (Go, TypeScript, Python, Java, Kotlin)
//! hash the declarations that make up a unit's public surface, read line by line. The rule
//! for every scanner: when unsure, include more. Including too much only makes footprints
//! wider (a body change looks like an interface change); including too little would miss a
//! conflict.

use sha2::{Digest, Sha256};

/// Hex SHA-256 of `text`.
pub fn sha256(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

/// Hash of several parts, order-sensitive, unambiguous (length-prefixed).
pub fn hash_parts<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut h = Sha256::new();
    for p in parts {
        let p = p.as_ref();
        h.update((p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Removes `//` and `/* */` comments from C-like source (Go, JavaScript, TypeScript, Java,
/// Kotlin), keeping line breaks so line structure survives. Quoted strings (`"`, `'` and
/// backticks) are left untouched, so comment markers inside them do not count.
pub fn strip_c_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == q || (c == '\n' && q != '`') {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                quote = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                    }
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Collapses runs of whitespace to one space and trims, so formatting changes do not count.
pub fn normalize(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Change in brace depth over `line`, ignoring braces in quoted strings. Input should be
/// comment-free ([`strip_c_comments`]).
pub fn brace_delta(line: &str) -> i32 {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '{' => depth += 1,
            '}' => depth -= 1,
            _ => {}
        }
    }
    depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes() {
        assert_eq!(sha256("").len(), 64);
        assert_ne!(hash_parts(["ab", "c"]), hash_parts(["a", "bc"]));
        assert_eq!(hash_parts(["x"]), hash_parts(vec!["x".to_owned()]));
    }

    #[test]
    fn comments_go_strings_stay() {
        let src = "a // c\nb /* x\ny */ c\n\"// not\" '/*' `//`\n";
        assert_eq!(strip_c_comments(src), "a \nb \n c\n\"// not\" '/*' `//`\n");
        assert_eq!(
            strip_c_comments("u := \"a\\\"//b\" // gone"),
            "u := \"a\\\"//b\" "
        );
    }

    #[test]
    fn braces_and_whitespace() {
        assert_eq!(brace_delta("func f() { x := \"{\" "), 1);
        assert_eq!(brace_delta("}}"), -2);
        assert_eq!(normalize("  fn  f( a ,b )\n"), "fn f( a ,b )");
    }
}
