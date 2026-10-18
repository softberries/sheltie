//! The public surface of Java and Kotlin sources, shared by the Maven and Gradle adapters.
//!
//! A JVM unit's interface is what other units can compile against: its public and protected
//! declarations. [`java_surface`] and [`kotlin_surface`] extract them from source text without
//! a compiler. Method bodies are skipped, so body-only changes leave the surface unchanged;
//! comments and formatting never count. The rule of [`crate::scan`] applies: when unsure,
//! include more.
//!
//! [`interface_parts`] and [`unit_files`] apply this to the standard layout
//! (`src/<source set>/{java,kotlin,resources}`).

use std::collections::BTreeSet;

use sheltie_core::RepoPath;

use crate::Error;
use crate::git::{Git, TreeFiles};

/// A JVM source language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Java (`.java`).
    Java,
    /// Kotlin (`.kt`).
    Kotlin,
}

impl Lang {
    /// The language of a source file, by extension.
    pub fn of(path: &str) -> Option<Self> {
        if path.ends_with(".java") {
            Some(Self::Java)
        } else if path.ends_with(".kt") {
            Some(Self::Kotlin)
        } else {
            None
        }
    }
}

/// The public surface of a source file in `lang`.
pub fn surface(lang: Lang, src: &str) -> String {
    match lang {
        Lang::Java => java_surface(src),
        Lang::Kotlin => kotlin_surface(src),
    }
}

/// The public surface of a Java compilation unit, one declaration per line.
///
/// Included: the `package` line; `public` and `protected` types with their headers;
/// in their bodies, `public` and `protected` members, every non-private member of interfaces
/// and annotation types, and enum constants; nested types by the same rules. Fields are kept
/// whole (constant values are inlined by `javac`), methods and constructors up to their body.
/// Annotations on included declarations are kept. A `module-info.java` is kept whole.
pub fn java_surface(src: &str) -> String {
    let t = Text::lex(src, Lang::Java);
    let mut out = Vec::new();
    java_body(&t, 0, t.len(), Ctx::Top, &mut out);
    out.join("\n")
}

/// The public surface of a Kotlin file, one declaration per line.
///
/// Included: the `package` line and every declaration that is not `private` or `internal`
/// (`@PublishedApi internal` counts as public). Classes, interfaces and objects keep their
/// header and the surface of their body. Functions with a block body keep their signature;
/// expression-bodied and `inline` functions, properties and type aliases are kept whole,
/// since an inferred type or an inlined body depends on them.
pub fn kotlin_surface(src: &str) -> String {
    let t = Text::lex(src, Lang::Kotlin);
    let mut out = Vec::new();
    kotlin_body(&t, 0, t.len(), false, &mut out);
    out.join("\n")
}

/// Joins a unit directory (empty for the repository root) and a relative path.
pub fn join(root: &str, rel: &str) -> String {
    if root.is_empty() {
        rel.to_owned()
    } else {
        format!("{root}/{rel}")
    }
}

/// The files a unit owns under the standard layout: everything under `<root>/src`.
pub fn unit_files(files: &TreeFiles, root: &str) -> BTreeSet<RepoPath> {
    files
        .under(&join(root, "src"))
        .map(|(p, _)| p.clone())
        .collect()
}

/// Whether a file (relative to its unit's directory) feeds the unit's interface, and in which
/// language: Java and Kotlin sources of `src/main`, of Kotlin Multiplatform `src/<x>Main`
/// sets and of `src/testFixtures`, and with `with_tests` also of `src/test`.
pub fn interface_source(rel: &str, with_tests: bool) -> Option<Lang> {
    let mut parts = rel.split('/');
    let (Some("src"), Some(set), Some("java" | "kotlin")) =
        (parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let visible = set == "main"
        || set.ends_with("Main")
        || set == "testFixtures"
        || (with_tests && set == "test");
    if visible { Lang::of(rel) } else { None }
}

/// The parts of a unit's interface hash from its sources: the surface of every
/// [`interface_source`] that declares something beyond its package, and the blob ids of
/// `src/main/resources/META-INF/**` (service registrations and similar files that dependents
/// load at run time). Adapters add their build file and hash the parts with
/// [`crate::scan::hash_parts`].
pub fn interface_parts(
    git: &Git,
    files: &TreeFiles,
    root: &str,
    with_tests: bool,
) -> Result<Vec<String>, Error> {
    let src = join(root, "src");
    let prefix_len = if root.is_empty() { 0 } else { root.len() + 1 };
    let mut parts = Vec::new();
    for (path, oid) in files.under(&src) {
        let rel = &path.as_str()[prefix_len..];
        if let Some(lang) = interface_source(rel, with_tests) {
            let s = surface(lang, &git.text(oid)?);
            if s.lines().any(|l| !l.starts_with("package ")) {
                parts.push(format!("{rel}\n{s}"));
            }
        } else if rel.starts_with("src/main/resources/META-INF/") {
            parts.push(format!("{rel}={oid}"));
        }
    }
    Ok(parts)
}

// ---------------------------------------------------------------------------------------
// Lexing: comments removed, string and character literals marked as not code.

/// Source text without comments, with a mask telling code from literals.
struct Text {
    c: Vec<char>,
    code: Vec<bool>,
}

impl Text {
    fn lex(src: &str, lang: Lang) -> Self {
        let s: Vec<char> = src.chars().collect();
        let mut t = Self {
            c: Vec::with_capacity(s.len()),
            code: Vec::with_capacity(s.len()),
        };
        let mut i = 0;
        while i < s.len() {
            let ch = s[i];
            let next = s.get(i + 1).copied();
            if ch == '/' && next == Some('/') {
                while i < s.len() && s[i] != '\n' {
                    i += 1;
                }
                t.push(' ', true);
            } else if ch == '/' && next == Some('*') {
                // Kotlin block comments nest; Java ones do not.
                let mut depth = 1;
                i += 2;
                t.push(' ', true);
                while i < s.len() && depth > 0 {
                    if s[i] == '*' && s.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                    } else if lang == Lang::Kotlin && s[i] == '/' && s.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else {
                        if s[i] == '\n' {
                            t.push('\n', true);
                        }
                        i += 1;
                    }
                }
            } else if ch == '"' || ch == '\'' || (ch == '`' && lang == Lang::Kotlin) {
                let end = skip_literal(&s, i, lang).min(s.len());
                for &c in &s[i..end] {
                    t.push(c, false);
                }
                i = end;
            } else {
                t.push(ch, true);
                i += 1;
            }
        }
        t
    }

    fn push(&mut self, c: char, code: bool) {
        self.c.push(c);
        self.code.push(code);
    }

    fn len(&self) -> usize {
        self.c.len()
    }

    fn slice(&self, a: usize, b: usize) -> String {
        let end = b.min(self.len());
        self.c[a.min(end)..end].iter().collect()
    }

    /// Whether position `i` holds the code character `c`.
    fn is(&self, i: usize, c: char) -> bool {
        i < self.len() && self.code[i] && self.c[i] == c
    }

    /// The bracket closing the one at `open` (any of `([{`), or `end` if unbalanced.
    fn matching(&self, open: usize, end: usize) -> usize {
        let mut depth = 0usize;
        for k in open..end {
            if !self.code[k] {
                continue;
            }
            match self.c[k] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return k;
                    }
                }
                _ => {}
            }
        }
        end
    }

    /// The first `target` in `[a, b)` outside brackets (an opening bracket can be the target).
    fn find_top(&self, a: usize, b: usize, target: char) -> Option<usize> {
        let mut i = a;
        while i < b {
            if self.code[i] {
                let c = self.c[i];
                if c == target {
                    return Some(i);
                }
                if matches!(c, '(' | '[' | '{') {
                    i = self.matching(i, b) + 1;
                    continue;
                }
            }
            i += 1;
        }
        None
    }

    /// The first assignment `=` in `[a, b)` outside brackets (not `==`, `!=`, `<=`, `>=`).
    fn find_assign(&self, a: usize, b: usize) -> Option<usize> {
        let mut i = a;
        while i < b {
            if self.code[i] {
                match self.c[i] {
                    '(' | '[' | '{' => {
                        i = self.matching(i, b) + 1;
                        continue;
                    }
                    '=' => {
                        let prev = if i > a { self.c[i - 1] } else { ' ' };
                        let next = if i + 1 < b { self.c[i + 1] } else { ' ' };
                        if !matches!(prev, '=' | '!' | '<' | '>') && next != '=' {
                            return Some(i);
                        }
                        if next == '=' {
                            i += 2;
                            continue;
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        None
    }

    /// `[a, b)` without whitespace in code, except one space between two words, so formatting
    /// never counts; literals stay verbatim.
    fn render(&self, a: usize, b: usize) -> String {
        self.render_with(a, b, false)
    }

    /// [`Self::render`], with the contents of top-level braces dropped (enum constant bodies).
    fn without_blocks(&self, a: usize, b: usize) -> String {
        self.render_with(a, b, true)
    }

    fn render_with(&self, a: usize, b: usize, skip_blocks: bool) -> String {
        let b = b.min(self.len());
        let mut s = String::new();
        let mut space = false;
        let mut i = a;
        while i < b {
            let c = self.c[i];
            if self.code[i] && c.is_whitespace() {
                space = true;
                i += 1;
                continue;
            }
            if space && s.chars().last().is_some_and(is_ident) && is_ident(c) {
                s.push(' ');
            }
            space = false;
            if skip_blocks && self.is(i, '{') {
                s.push_str("{}");
                i = self.matching(i, b) + 1;
                continue;
            }
            s.push(c);
            i += 1;
        }
        s
    }

    /// The words of a declaration header in `[a, b)`, up to its first `(`, `=`, `{` or `;`
    /// outside annotations, and the simple names of its annotations.
    fn head(&self, a: usize, b: usize) -> Head {
        let mut h = Head::default();
        let mut i = a;
        while i < b {
            if !self.code[i] {
                i += 1;
                continue;
            }
            let c = self.c[i];
            if c == '@' {
                let mut j = i + 1;
                let start = j;
                while j < b
                    && self.code[j]
                    && (is_ident(self.c[j]) || matches!(self.c[j], '.' | ':'))
                {
                    j += 1;
                }
                let name = self.slice(start, j);
                if name == "interface" {
                    h.words.push("@interface".into());
                    i = j;
                    continue;
                }
                let simple = name
                    .rsplit([':', '.'])
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                h.annotations.push(simple);
                let mut k = j;
                while k < b && self.c[k].is_whitespace() {
                    k += 1;
                }
                if self.is(k, '(') || (name.is_empty() && self.is(k, '[')) {
                    i = self.matching(k, b) + 1;
                } else {
                    i = j;
                }
                continue;
            }
            if matches!(c, '(' | '=' | '{' | ';') {
                break;
            }
            if is_ident(c) {
                let start = i;
                while i < b && self.code[i] && is_ident(self.c[i]) {
                    i += 1;
                }
                h.words.push(self.slice(start, i));
                continue;
            }
            i += 1;
        }
        h
    }
}

#[derive(Default)]
struct Head {
    words: Vec<String>,
    annotations: Vec<String>,
}

impl Head {
    fn has(&self, word: &str) -> bool {
        self.words.iter().any(|w| w == word)
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The end (exclusive) of the string, character or backtick literal starting at `i`.
fn skip_literal(s: &[char], i: usize, lang: Lang) -> usize {
    let q = s[i];
    if q == '"' && s.get(i + 1) == Some(&'"') && s.get(i + 2) == Some(&'"') {
        // Java text block or Kotlin raw string.
        let mut j = i + 3;
        while j < s.len() {
            if lang == Lang::Java && s[j] == '\\' {
                j += 2;
            } else if lang == Lang::Kotlin && s[j] == '$' && s.get(j + 1) == Some(&'{') {
                j = skip_template(s, j + 2, lang);
            } else if s[j] == '"' && s.get(j + 1) == Some(&'"') && s.get(j + 2) == Some(&'"') {
                j += 3;
                while j < s.len() && s[j] == '"' {
                    j += 1;
                }
                return j;
            } else {
                j += 1;
            }
        }
        return s.len();
    }
    let mut j = i + 1;
    while j < s.len() {
        match s[j] {
            '\\' if q != '`' => j += 2,
            '\n' => return j,
            c if c == q => return j + 1,
            '$' if lang == Lang::Kotlin && q == '"' && s.get(j + 1) == Some(&'{') => {
                j = skip_template(s, j + 2, lang);
            }
            _ => j += 1,
        }
    }
    s.len()
}

/// The end of a Kotlin string template `${…}` whose contents start at `j`.
fn skip_template(s: &[char], mut j: usize, lang: Lang) -> usize {
    let mut depth = 0usize;
    while j < s.len() {
        match s[j] {
            '"' | '\'' => j = skip_literal(s, j, lang),
            '{' => {
                depth += 1;
                j += 1;
            }
            '}' => {
                if depth == 0 {
                    return j + 1;
                }
                depth -= 1;
                j += 1;
            }
            _ => j += 1,
        }
    }
    s.len()
}

// ---------------------------------------------------------------------------------------
// Java.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    /// A compilation unit.
    Top,
    /// A class or record body: members need `public` or `protected`.
    Class,
    /// An interface or annotation body: members are public unless `private`.
    Interface,
    /// An enum body: constants, then members as in a class.
    Enum,
}

/// The surface of the declarations in `[a, b)`, appended to `out`.
fn java_body(t: &Text, a: usize, b: usize, ctx: Ctx, out: &mut Vec<String>) {
    let mut start = a;
    if ctx == Ctx::Enum {
        let semi = t.find_top(a, b, ';');
        let constants = t.without_blocks(a, semi.unwrap_or(b));
        if !constants.is_empty() {
            out.push(constants);
        }
        start = semi.map_or(b, |s| s + 1);
    }
    let mut i = start;
    let mut chunk = start;
    while i < b {
        if !t.code[i] {
            i += 1;
            continue;
        }
        match t.c[i] {
            '(' | '[' => i = t.matching(i, b) + 1,
            ';' => {
                java_member(t, chunk, i, None, ctx, out);
                i += 1;
                chunk = i;
            }
            '{' => {
                let close = t.matching(i, b);
                if !is_block(t, chunk, i) {
                    // An array initialiser, anonymous class or lambda in a field initialiser,
                    // or an annotation element's default: part of the declaration.
                    i = close + 1;
                    continue;
                }
                java_member(t, chunk, i, Some((i + 1, close)), ctx, out);
                i = close + 1;
                chunk = i;
            }
            _ => i += 1,
        }
    }
    if chunk < b {
        java_member(t, chunk, b, None, ctx, out);
    }
}

/// Whether the `{` at `brace` opens a body (type, method, initialiser) rather than an
/// expression inside the declaration starting at `chunk`.
fn is_block(t: &Text, chunk: usize, brace: usize) -> bool {
    if t.find_assign(chunk, brace).is_some() {
        return false;
    }
    let before = t.render(chunk, brace);
    let ends_default = before
        .strip_suffix("default")
        .is_some_and(|rest| !rest.ends_with(is_ident));
    !ends_default
}

/// The kind of type a header declares, if it declares one.
fn java_kind(h: &Head) -> Option<Ctx> {
    if h.has("@interface") {
        return Some(Ctx::Interface);
    }
    for (i, w) in h.words.iter().enumerate() {
        let declares = h.words.get(i + 1).is_some();
        match w.as_str() {
            "class" | "record" if declares => return Some(Ctx::Class),
            "interface" if declares => return Some(Ctx::Interface),
            "enum" if declares => return Some(Ctx::Enum),
            _ => {}
        }
    }
    None
}

/// Adds the declaration in `[a, b)` (with its body at `block`, if any) to `out` if it is
/// visible outside its package.
fn java_member(
    t: &Text,
    a: usize,
    b: usize,
    block: Option<(usize, usize)>,
    ctx: Ctx,
    out: &mut Vec<String>,
) {
    let head = t.render(a, b);
    if head.is_empty() {
        return;
    }
    let h = t.head(a, b);
    let first = h.words.first().map(String::as_str);
    if ctx == Ctx::Top {
        match first {
            Some("package") => return out.push(head),
            Some("import") => return,
            Some("module" | "open") if block.is_some() && h.has("module") => {
                // module-info.java: every directive is interface.
                let end = block.map_or(b, |(_, close)| close + 1);
                return out.push(t.render(a, end));
            }
            _ => {}
        }
    }
    let visible = match ctx {
        Ctx::Interface => !h.has("private"),
        Ctx::Top | Ctx::Class | Ctx::Enum => h.has("public") || h.has("protected"),
    };
    if !visible {
        return;
    }
    match (block, java_kind(&h)) {
        (Some((x, y)), Some(kind)) => {
            let mut inner = Vec::new();
            java_body(t, x, y, kind, &mut inner);
            out.push(format!("{head} {{\n{}\n}}", inner.join("\n")));
        }
        // A method or constructor signature, or a field or abstract method kept whole.
        _ => out.push(head),
    }
}

// ---------------------------------------------------------------------------------------
// Kotlin.

const KOTLIN_KEYWORDS: &[&str] = &[
    "package",
    "import",
    "class",
    "interface",
    "object",
    "fun",
    "val",
    "var",
    "typealias",
    "constructor",
    "init",
];

const KOTLIN_MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "open",
    "abstract",
    "final",
    "override",
    "sealed",
    "data",
    "enum",
    "annotation",
    "inner",
    "value",
    "inline",
    "suspend",
    "operator",
    "infix",
    "tailrec",
    "external",
    "const",
    "lateinit",
    "expect",
    "actual",
    "companion",
];

/// The surface of the declarations in `[a, b)`, appended to `out`.
fn kotlin_body(t: &Text, a: usize, b: usize, is_enum: bool, out: &mut Vec<String>) {
    let mut start = a;
    if is_enum {
        let semi = t.find_top(a, b, ';');
        let entries = t.without_blocks(a, semi.unwrap_or(b));
        if !entries.is_empty() {
            out.push(entries);
        }
        start = semi.map_or(b, |s| s + 1);
    }
    for (x, y) in kotlin_statements(t, start, b) {
        kotlin_member(t, x, y, out);
    }
}

/// Splits `[a, b)` into top-level statements: at `;` and at line ends, unless the statement
/// continues on the next line.
fn kotlin_statements(t: &Text, a: usize, b: usize) -> Vec<(usize, usize)> {
    let mut statements = Vec::new();
    let mut push = |s: usize, e: usize| {
        if !t.slice(s, e).trim().is_empty() {
            statements.push((s, e.min(b)));
        }
    };
    let mut s = a;
    let mut i = a;
    while i < b {
        if !t.code[i] {
            i += 1;
            continue;
        }
        match t.c[i] {
            '(' | '[' | '{' => i = t.matching(i, b) + 1,
            ';' => {
                push(s, i);
                i += 1;
                s = i;
            }
            '\n' => {
                if !continues(t, s, i, b) {
                    push(s, i);
                    s = i + 1;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    if s < b {
        push(s, b);
    }
    statements
}

/// Whether the statement in `[s, nl)` continues past the line break at `nl`.
fn continues(t: &Text, s: usize, nl: usize, b: usize) -> bool {
    let current = t.render(s, nl);
    if current.is_empty() {
        return false;
    }
    let h = t.head(s, nl);
    // Annotations or modifiers on a line of their own.
    if h.words
        .iter()
        .all(|w| KOTLIN_MODIFIERS.contains(&w.as_str()))
        && (current.starts_with('@') || !h.words.is_empty())
    {
        return true;
    }
    if ["=", ",", ".", ":", "->", "&&", "||", "?:"]
        .iter()
        .any(|end| current.ends_with(end))
    {
        return true;
    }
    let mut j = nl + 1;
    while j < b && t.c[j].is_whitespace() {
        j += 1;
    }
    let line_end = (j..b).find(|&k| t.c[k] == '\n').unwrap_or(b);
    let next = t.slice(j, line_end);
    let next = next.trim();
    if [
        ".", "?.", "?:", ":", "=", "{", "->", "&&", "||", "where ", "by ", "else",
    ]
    .iter()
    .any(|start| next.starts_with(start))
    {
        return true;
    }
    // Property accessors on the following lines.
    if h.has("val") || h.has("var") {
        let accessor = next
            .split_whitespace()
            .find(|w| !matches!(*w, "public" | "private" | "protected" | "internal"));
        if accessor.is_some_and(|w| {
            w == "get" || w == "set" || w.starts_with("get(") || w.starts_with("set(")
        }) {
            return true;
        }
    }
    false
}

/// Adds the declaration in `[a, b)` to `out` if it is visible outside its module.
fn kotlin_member(t: &Text, a: usize, b: usize, out: &mut Vec<String>) {
    let text = t.render(a, b);
    if text.is_empty() {
        return;
    }
    let h = t.head(a, b);
    let kw = h
        .words
        .iter()
        .position(|w| KOTLIN_KEYWORDS.contains(&w.as_str()));
    let modifiers = &h.words[..kw.unwrap_or(h.words.len())];
    let has_mod = |m: &str| modifiers.iter().any(|w| w == m);
    let keyword = kw.map(|k| h.words[k].as_str());
    match keyword {
        Some("package") => return out.push(text),
        Some("import" | "init") => return,
        _ => {}
    }
    let published = h.annotations.iter().any(|a| a == "PublishedApi");
    if has_mod("private") || (has_mod("internal") && !published) {
        return;
    }
    let next_word = kw.and_then(|k| h.words.get(k + 1)).map(String::as_str);
    let is_type = matches!(keyword, Some("class" | "interface" | "object"))
        || (keyword == Some("fun") && next_word == Some("interface"));
    let header_only = |out: &mut Vec<String>| match t.find_top(a, b, '{') {
        Some(o) => out.push(t.render(a, o)),
        None => out.push(text.clone()),
    };
    if is_type {
        match t.find_top(a, b, '{') {
            Some(o) => {
                let close = t.matching(o, b);
                let mut inner = Vec::new();
                kotlin_body(t, o + 1, close, has_mod("enum"), &mut inner);
                let header = t.render(a, o);
                out.push(format!("{header} {{\n{}\n}}", inner.join("\n")));
            }
            None => out.push(text),
        }
        return;
    }
    match keyword {
        Some("fun") => {
            if has_mod("inline") {
                return out.push(text);
            }
            match (t.find_assign(a, b), t.find_top(a, b, '{')) {
                (Some(e), Some(o)) if e < o => out.push(text),
                (_, Some(_)) => header_only(out),
                _ => out.push(text),
            }
        }
        Some("constructor") => header_only(out),
        // Properties, type aliases and anything unrecognised: kept whole.
        _ => out.push(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How a declaration line looks in a surface (type headers end with ` {`).
    fn tight(lang: Lang, s: &str) -> String {
        let (s, open) = match s.strip_suffix(" {") {
            Some(head) => (head, " {"),
            None => (s, ""),
        };
        let t = Text::lex(s, lang);
        format!("{}{open}", t.render(0, t.len()))
    }

    fn same(f: fn(&str) -> String, a: &str, b: &str) -> bool {
        f(a) == f(b)
    }

    const JAVA: &str = r#"
package com.acme.core;

import java.util.List;

/** Money. */
@Immutable
public final class Money implements Comparable<Money> {
    public static final int SCALE = 2;
    private static final int SECRET = 7;
    int packagePrivate;
    protected long cents;

    public Money(long cents) {
        this.cents = cents;
    }

    public Money plus(Money other) {
        return new Money(cents + other.cents);
    }

    private long raw() { return cents; }

    void helper() { }

    @Override
    public int compareTo(Money o) { return Long.compare(cents, o.cents); }

    public static final int[] DENOMS = {1, 2, 5};

    public static final Runnable R = () -> { System.out.println("x"); };

    public interface Rounding {
        long round(long v);
        default long twice(long v) { return round(round(v)); }
        private void hidden() { }
    }

    static class Hidden {
        public void notVisible() { }
    }

    public enum Currency {
        EUR("€"), USD("$") {
            @Override public String symbol() { return "US$"; }
        };
        private final String s;
        Currency(String s) { this.s = s; }
        public String symbol() { return s; }
    }

    public record Pair(long a, long b) {
        public long sum() { return a + b; }
    }
}

class PackageOnly {
    public void x() { }
}

public @interface Marker {
    String value() default "x";
    String[] tags() default {"a", "b"};
}
"#;

    fn java_edit(find: &str, replace: &str) -> String {
        assert_eq!(JAVA.matches(find).count(), 1, "{find}");
        JAVA.replace(find, replace)
    }

    #[test]
    fn java_surface_has_public_declarations() {
        let s = java_surface(JAVA);
        let expected = [
            "package com.acme.core",
            "@Immutable public final class Money implements Comparable<Money> {",
            "public static final int SCALE = 2",
            "protected long cents",
            "public Money(long cents)",
            "public Money plus(Money other)",
            "@Override public int compareTo(Money o)",
            "public static final int[] DENOMS = {1, 2, 5}",
            "public interface Rounding {",
            "long round(long v)",
            "default long twice(long v)",
            "public enum Currency {",
            "EUR(\"€\"), USD(\"$\") {}",
            "public String symbol()",
            "public record Pair(long a, long b) {",
            "public long sum()",
            "public @interface Marker {",
            "String value() default \"x\"",
            "String[] tags() default {\"a\", \"b\"}",
        ];
        for e in expected {
            let e = tight(Lang::Java, e);
            assert!(s.lines().any(|l| l == e), "missing {e:?} in\n{s}");
        }
        for absent in [
            "SECRET",
            "packagePrivate",
            "raw()",
            "helper",
            "hidden",
            "Hidden",
            "PackageOnly",
            "import",
            "return",
            "US$",
        ] {
            assert!(!s.contains(absent), "unexpected {absent:?} in\n{s}");
        }
    }

    #[test]
    fn java_body_and_private_changes_do_not_count() {
        let f = java_surface;
        for (find, replace) in [
            (
                "return new Money(cents + other.cents);",
                "long sum = cents + other.cents;\n        return new Money(sum);",
            ),
            (
                "private static final int SECRET = 7;",
                "private static final int SECRET = 8;",
            ),
            (
                "private long raw() { return cents; }",
                "private long raw(int x) { return cents; }",
            ),
            ("void helper() { }", "void helper(int y) { }"),
            ("int packagePrivate;", "int packagePrivate = 3;"),
            (
                "public void notVisible() { }",
                "public void notVisible(int z) { }",
            ),
            (
                "@Override public String symbol() { return \"US$\"; }",
                "@Override public String symbol() { return \"USD\"; }",
            ),
            (
                "default long twice(long v) { return round(round(v)); }",
                "default long twice(long v) { return 2 * round(v); }",
            ),
            (
                "private void hidden() { }",
                "private void hidden(int q) { }",
            ),
            (
                "import java.util.List;",
                "import java.util.List;\nimport java.util.Map;",
            ),
            ("/** Money. */", "/** Money, in cents. */"),
            (
                "public Money plus(Money other) {",
                "public   Money plus(\n        Money other)   {",
            ),
        ] {
            assert!(
                same(f, JAVA, &java_edit(find, replace)),
                "{find} -> {replace}"
            );
        }
    }

    #[test]
    fn java_interface_changes_count() {
        let f = java_surface;
        for (find, replace) in [
            (
                "public Money plus(Money other) {",
                "public Money plus(Money other, int times) {",
            ),
            (
                "public static final int SCALE = 2;",
                "public static final int SCALE = 4;",
            ),
            ("protected long cents;", "protected int cents;"),
            (
                "public record Pair(long a, long b)",
                "public record Pair(long a, long b, long c)",
            ),
            ("long round(long v);", "long round(long v, int mode);"),
            ("EUR(\"€\"), USD", "EUR(\"€\"), GBP(\"£\"), USD"),
            (
                "String value() default \"x\";",
                "String value() default \"y\";",
            ),
            ("void helper() { }", "public void helper() { }"),
            ("@Immutable\npublic final class", "public final class"),
            ("package com.acme.core;", "package com.acme.money;"),
            (
                "public static final int[] DENOMS = {1, 2, 5};",
                "public static final int[] DENOMS = {1, 2};",
            ),
        ] {
            assert!(
                !same(f, JAVA, &java_edit(find, replace)),
                "{find} -> {replace}"
            );
        }
    }

    #[test]
    fn java_literals_and_module_info() {
        let src = "public class A {\n  public static final String S = \"}{;\";\n  public char c() { return '}'; }\n  public void b() { String t = \"\"\"\n  }\n  \"\"\"; }\n}\n";
        let s = java_surface(src);
        assert!(s.contains("public static final String S=\"}{;\""), "{s}");
        assert!(
            s.contains("public char c()") && s.contains("public void b()"),
            "{s}"
        );
        let module = "module com.acme.core {\n  exports com.acme.core;\n}\n";
        assert_eq!(
            java_surface(module),
            "module com.acme.core{exports com.acme.core;}"
        );
    }

    const KOTLIN: &str = r#"
package com.acme.core

import kotlin.math.max

/* A /* nested */ comment. */
@JvmInline
value class Cents(val value: Long)

const val SCALE = 2
private const val SECRET = 7
internal val hidden = 3
val inferred = listOf(1, 2)

typealias Amount = Long

fun total(items: List<Cents>): Cents {
    return Cents(items.sumOf { it.value })
}

fun double(x: Int) = x * 2

fun template(name: String): String {
    return "hi ${name.let { "{$it}" }}"
}

private fun helper() = 1

internal fun internalHelper() { }

@PublishedApi internal fun published() = 1

inline fun <T> measure(block: () -> T): T {
    return block()
}

interface Repo {
    fun find(id: Long): Cents?
    fun all(): List<Cents> = emptyList()
}

data class Order(
    val id: Long,
    val lines: List<Cents>,
) : Comparable<Order> {
    override fun compareTo(other: Order): Int {
        return id.compareTo(other.id)
    }

    private fun secret() = 42

    val size: Int
        get() = lines.size

    companion object {
        fun empty(): Order {
            return Order(0, emptyList())
        }
        private val CACHE = mutableMapOf<Long, Order>()
    }
}

enum class Status(val code: Int) {
    NEW(1),
    DONE(2) {
        override fun label() = "done!"
    };

    open fun label(): String {
        return name.lowercase()
    }
}

sealed interface Event

object Registry {
    @Deprecated("use other")
    fun register(name: String) { println(name) }
}
"#;

    fn kotlin_edit(find: &str, replace: &str) -> String {
        assert_eq!(KOTLIN.matches(find).count(), 1, "{find}");
        KOTLIN.replace(find, replace)
    }

    #[test]
    fn kotlin_surface_has_public_declarations() {
        let s = kotlin_surface(KOTLIN);
        let expected = [
            "package com.acme.core",
            "@JvmInline value class Cents(val value: Long)",
            "const val SCALE = 2",
            "val inferred = listOf(1, 2)",
            "typealias Amount = Long",
            "fun total(items: List<Cents>): Cents",
            "fun double(x: Int) = x * 2",
            "fun template(name: String): String",
            "@PublishedApi internal fun published() = 1",
            "inline fun <T> measure(block: () -> T): T { return block() }",
            "interface Repo {",
            "fun find(id: Long): Cents?",
            "fun all(): List<Cents> = emptyList()",
            "data class Order( val id: Long, val lines: List<Cents>, ) : Comparable<Order> {",
            "override fun compareTo(other: Order): Int",
            "val size: Int get() = lines.size",
            "companion object {",
            "fun empty(): Order",
            "enum class Status(val code: Int) {",
            "NEW(1), DONE(2) {}",
            "open fun label(): String",
            "sealed interface Event",
            "@Deprecated(\"use other\") fun register(name: String)",
        ];
        for e in expected {
            let e = tight(Lang::Kotlin, e);
            assert!(s.lines().any(|l| l == e), "missing {e:?} in\n{s}");
        }
        for absent in [
            "SECRET",
            "hidden",
            "helper",
            "secret",
            "CACHE",
            "import",
            "return Cents",
            "done!",
            "println",
        ] {
            assert!(!s.contains(absent), "unexpected {absent:?} in\n{s}");
        }
    }

    #[test]
    fn kotlin_body_and_private_changes_do_not_count() {
        let f = kotlin_surface;
        for (find, replace) in [
            (
                "return Cents(items.sumOf { it.value })",
                "val sum = items.sumOf { it.value }\n    return Cents(sum)",
            ),
            (
                "private const val SECRET = 7",
                "private const val SECRET = 8",
            ),
            ("internal val hidden = 3", "internal val hidden = 4"),
            ("private fun helper() = 1", "private fun helper() = 2"),
            (
                "internal fun internalHelper() { }",
                "internal fun internalHelper(x: Int) { }",
            ),
            ("private fun secret() = 42", "private fun secret() = 43"),
            (
                "return id.compareTo(other.id)",
                "return other.id.compareTo(id)",
            ),
            (
                "override fun label() = \"done!\"",
                "override fun label() = \"finished\"",
            ),
            (
                "fun register(name: String) { println(name) }",
                "fun register(name: String) { print(name) }",
            ),
            (
                "return \"hi ${name.let { \"{$it}\" }}\"",
                "return \"hello ${name}\"",
            ),
            ("/* A /* nested */ comment. */", "// Another comment."),
            (
                "fun double(x: Int) = x * 2",
                "fun   double(x: Int)   =   x * 2",
            ),
            (
                "import kotlin.math.max",
                "import kotlin.math.max\nimport kotlin.math.min",
            ),
        ] {
            assert!(
                same(f, KOTLIN, &kotlin_edit(find, replace)),
                "{find} -> {replace}"
            );
        }
    }

    #[test]
    fn kotlin_interface_changes_count() {
        let f = kotlin_surface;
        for (find, replace) in [
            (
                "fun total(items: List<Cents>): Cents {",
                "fun total(items: List<Cents>, tax: Int): Cents {",
            ),
            ("fun double(x: Int) = x * 2", "fun double(x: Int) = x * 2L"),
            ("const val SCALE = 2", "const val SCALE = 3"),
            (
                "val inferred = listOf(1, 2)",
                "val inferred = listOf(1L, 2L)",
            ),
            ("return block()", "return block().also { println(it) }"),
            ("fun find(id: Long): Cents?", "fun find(id: Long): Cents"),
            (
                "    val lines: List<Cents>,\n",
                "    val lines: List<Cents>,\n    val note: String,\n",
            ),
            ("    NEW(1),\n", "    NEW(1),\n    OPEN(3),\n"),
            (
                "internal fun internalHelper() { }",
                "fun internalHelper() { }",
            ),
            (
                "@PublishedApi internal fun published() = 1",
                "@PublishedApi internal fun published() = 2",
            ),
            ("fun empty(): Order {", "fun empty(id: Long): Order {"),
            (
                "sealed interface Event",
                "sealed interface Event { val at: Long }",
            ),
            ("get() = lines.size", "private set"),
        ] {
            assert!(
                !same(f, KOTLIN, &kotlin_edit(find, replace)),
                "{find} -> {replace}"
            );
        }
    }

    #[test]
    fn interface_sources_follow_the_standard_layout() {
        assert_eq!(
            interface_source("src/main/java/a/A.java", false),
            Some(Lang::Java)
        );
        assert_eq!(
            interface_source("src/main/kotlin/a/A.kt", false),
            Some(Lang::Kotlin)
        );
        assert_eq!(
            interface_source("src/commonMain/kotlin/A.kt", false),
            Some(Lang::Kotlin)
        );
        assert_eq!(
            interface_source("src/testFixtures/java/A.java", false),
            Some(Lang::Java)
        );
        assert_eq!(interface_source("src/test/java/ATest.java", false), None);
        assert_eq!(
            interface_source("src/test/java/ATest.java", true),
            Some(Lang::Java)
        );
        assert_eq!(
            interface_source("src/main/resources/a.properties", false),
            None
        );
        assert_eq!(interface_source("src/main/java/a/notes.txt", false), None);
        assert_eq!(join("", "src"), "src");
        assert_eq!(join("core", "src"), "core/src");
    }
}
