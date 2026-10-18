//! A line scanner for Python source: logical statements without comments.
//!
//! Python needs no interpreter to find statements: a statement ends at a newline outside
//! brackets and strings, unless the line ends with a backslash. The scanner handles single,
//! double and triple-quoted strings (with any prefix: `r`, `b`, `f`, `u`), `#` comments,
//! backslash continuations, parenthesised multi-line statements and `;`-separated statements.
//! Strings are kept verbatim; comments are dropped.

/// One logical statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stmt {
    /// Indentation of its first line (a tab advances to the next multiple of 8, as in Python).
    pub indent: usize,
    /// Its text: physical lines joined with spaces, comments removed, string literals verbatim.
    pub text: String,
}

/// Splits Python source into logical statements, in order.
pub fn statements(src: &str) -> Vec<Stmt> {
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut indent = 0;
    let mut depth = 0usize;
    let mut line_start = true;
    let mut i = 0;
    while i < n {
        if line_start {
            let mut width = 0;
            while i < n && matches!(chars[i], ' ' | '\t' | '\x0c') {
                width = match chars[i] {
                    '\t' => (width / 8 + 1) * 8,
                    ' ' => width + 1,
                    _ => 0,
                };
                i += 1;
            }
            indent = width;
            line_start = false;
            continue;
        }
        let c = chars[i];
        match c {
            '#' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
            }
            '"' | '\'' => {
                let end = string_end(&chars, i);
                cur.extend(&chars[i..end]);
                i = end;
            }
            '(' | '[' | '{' => {
                depth += 1;
                cur.push(c);
                i += 1;
            }
            ')' | ']' | '}' => {
                depth = depth.saturating_sub(1);
                cur.push(c);
                i += 1;
            }
            '\\' if chars.get(i + 1) == Some(&'\n') => {
                cur.push(' ');
                i += 2;
            }
            '\\' if chars.get(i + 1) == Some(&'\r') && chars.get(i + 2) == Some(&'\n') => {
                cur.push(' ');
                i += 3;
            }
            '\n' => {
                i += 1;
                if depth > 0 {
                    cur.push(' ');
                } else {
                    flush(&mut out, &mut cur, indent);
                    line_start = true;
                }
            }
            ';' if depth == 0 => {
                flush(&mut out, &mut cur, indent);
                i += 1;
            }
            '\r' => i += 1,
            _ => {
                cur.push(c);
                i += 1;
            }
        }
    }
    flush(&mut out, &mut cur, indent);
    out
}

fn flush(out: &mut Vec<Stmt>, cur: &mut String, indent: usize) {
    let text = cur.trim();
    if !text.is_empty() {
        out.push(Stmt {
            indent,
            text: text.to_owned(),
        });
    }
    cur.clear();
}

/// End (exclusive) of the string literal whose opening quote is `chars[start]`. A backslash
/// always skips the next character (in raw strings too: `r"\""` does not end at the second
/// quote). An unterminated single-quoted string ends before the newline.
fn string_end(chars: &[char], start: usize) -> usize {
    let q = chars[start];
    let triple = chars.get(start + 1) == Some(&q) && chars.get(start + 2) == Some(&q);
    let mut i = if triple { start + 3 } else { start + 1 };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' {
            i += 2;
            continue;
        }
        if triple {
            if c == q && chars.get(i + 1) == Some(&q) && chars.get(i + 2) == Some(&q) {
                return i + 3;
            }
        } else if c == q {
            return i + 1;
        } else if c == '\n' {
            return i;
        }
        i += 1;
    }
    chars.len()
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The statement's text with formatting removed: whitespace outside strings is dropped,
/// except one space between two identifier characters (`async def`), so reformatting a
/// signature does not count as a change. String literals are kept verbatim.
pub fn canonical(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut pending_space = false;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            pending_space = true;
            i += 1;
            continue;
        }
        if pending_space && out.chars().next_back().is_some_and(is_ident) && is_ident(c) {
            out.push(' ');
        }
        pending_space = false;
        if c == '"' || c == '\'' {
            let end = string_end(&chars, i);
            out.extend(&chars[i..end]);
            i = end;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// The characters of `text` outside string literals and brackets, with their byte offsets.
/// Brackets themselves are left out.
pub fn top_level(text: &str) -> Vec<(usize, char)> {
    let indexed: Vec<(usize, char)> = text.char_indices().collect();
    let chars: Vec<char> = indexed.iter().map(|&(_, c)| c).collect();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '"' | '\'' => {
                i = string_end(&chars, i);
                continue;
            }
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push((indexed[i].0, c)),
            _ => {}
        }
        i += 1;
    }
    out
}

/// `text` split at `sep` outside strings and brackets, each piece trimmed.
pub fn split_top(text: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut from = 0;
    for (at, c) in top_level(text) {
        if c == sep {
            parts.push(text[from..at].trim());
            from = at + c.len_utf8();
        }
    }
    parts.push(text[from..].trim());
    parts
}

/// Byte offset of the colon that ends a compound statement's header (`def f(x: int) -> T:`,
/// `if a:`): the first `:` outside strings and brackets that is not part of `:=`.
pub fn header_colon(text: &str) -> Option<usize> {
    top_level(text)
        .into_iter()
        .find(|&(at, c)| c == ':' && !text[at + 1..].starts_with('='))
        .map(|(at, _)| at)
}

/// The leading identifier of `text` (empty if it starts with something else).
pub fn first_word(text: &str) -> &str {
    let end = text
        .char_indices()
        .find(|&(_, c)| !is_ident(c))
        .map_or(text.len(), |(i, _)| i);
    &text[..end]
}

/// Keywords that open a block (`async` precedes `def`, `for` or `with`).
const COMPOUND: &[&str] = &[
    "if", "elif", "else", "try", "except", "finally", "with", "for", "while", "def", "class",
    "async",
];

/// Whether `text` opens a block: `if …:`, `def …:`, `try:` and so on.
pub fn is_compound(text: &str) -> bool {
    COMPOUND.contains(&first_word(text)) && header_colon(text).is_some()
}

/// The statement after the header's colon of a one-line block (`if x: import y` gives
/// `import y`), if any.
pub fn inline_body(text: &str) -> Option<&str> {
    if !is_compound(text) {
        return None;
    }
    let colon = header_colon(text)?;
    let rest = text[colon + 1..].trim();
    (!rest.is_empty()).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> Vec<(usize, String)> {
        statements(src)
            .into_iter()
            .map(|s| (s.indent, s.text))
            .collect()
    }

    #[test]
    fn comments_go_strings_stay() {
        let src = "x = '#not' # gone\n# only a comment\n\ny = \"a\\\"#b\"  # c\n";
        assert_eq!(
            texts(src),
            [(0, "x = '#not'".into()), (0, "y = \"a\\\"#b\"".into())]
        );
    }

    #[test]
    fn triple_quoted_strings_span_lines() {
        let src = "def f():\n    \"\"\"Doc\n    import nothing # here\n    \"\"\"\n    return 1\ns = r'''a\n'b'\n'''\n";
        let t = texts(src);
        assert_eq!(t.len(), 4);
        assert_eq!(t[0], (0, "def f():".into()));
        assert!(t[1].1.starts_with("\"\"\"Doc") && t[1].1.contains("# here"));
        assert_eq!(t[2], (4, "return 1".into()));
        assert_eq!(t[3], (0, "s = r'''a\n'b'\n'''".into()));
    }

    #[test]
    fn continuations_and_brackets_join_lines() {
        let src = "from a import (\n    b,  # first\n    c,\n)\nx = 1 + \\\n    2\nif a: b = 1; c = 2\n\tz = 3\n";
        let t = texts(src);
        assert_eq!(canonical(&t[0].1), "from a import(b,c,)");
        assert_eq!(canonical(&t[1].1), "x=1+2");
        assert_eq!(t[2], (0, "if a: b = 1".into()));
        assert_eq!(t[3], (0, "c = 2".into()));
        assert_eq!(t[4], (8, "z = 3".into()));
    }

    #[test]
    fn crlf_and_unterminated_strings() {
        let t = texts("a = 1\r\nb = 'open\r\nc = \\\r\n 2\r\n");
        assert_eq!(t.len(), 3);
        assert_eq!(canonical(&t[2].1), "c=2");
    }

    #[test]
    fn canonical_drops_formatting_only() {
        assert_eq!(
            canonical("async  def f( a : int = 1 ,\n  b='x  y' ) -> None :"),
            "async def f(a:int=1,b='x  y')->None:"
        );
        assert_ne!(canonical("def f(a, b)"), canonical("def f(ab)"));
    }

    #[test]
    fn top_level_structure() {
        assert_eq!(
            split_top("a, (b, c), 'd, e'", ','),
            ["a", "(b, c)", "'d, e'"]
        );
        let h = "def f(x: int = {'a': 1}) -> dict[str, int]: return x";
        assert_eq!(
            &h[..=header_colon(h).unwrap()],
            "def f(x: int = {'a': 1}) -> dict[str, int]:"
        );
        assert_eq!(inline_body("if (y := 2): import os"), Some("import os"));
        assert_eq!(inline_body("x = {'a': 1}"), None);
        assert_eq!(inline_body("else:"), None);
        assert!(is_compound("try:"));
        assert_eq!(first_word("class A(B):"), "class");
    }
}
