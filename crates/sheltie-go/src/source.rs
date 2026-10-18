//! Reading one Go source file without a compiler: its package clause, imports, exported
//! declarations and the directives that make a build non-hermetic.
//!
//! The file is split into top-level declarations by bracket depth (outside string and rune
//! literals), after comments are stripped. Each declaration is reduced to the part
//! dependents see and put in a canonical form, so formatting and comments do not count.

use std::collections::{BTreeMap, BTreeSet};

use sheltie_adapter::scan::strip_c_comments;

/// What one `.go` file contributes to its package.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct GoFile {
    /// Import paths, in source order.
    pub(crate) imports: Vec<String>,
    /// Canonical parts of the public interface: the package clause, build constraints and
    /// exported declarations (function signatures, types, constants and variables).
    pub(crate) exported: Vec<String>,
    /// Unexported types and constants by name (canonical text). Pulled into the interface
    /// when an exported part mentions them: a field of an unexported embedded type, or an
    /// exported constant computed from an unexported one, is visible to dependents.
    pub(crate) private: BTreeMap<String, Vec<String>>,
    /// Whether the file has a `//go:embed` directive.
    pub(crate) embeds: bool,
}

impl GoFile {
    /// Whether the file uses cgo (`import "C"`).
    pub(crate) fn cgo(&self) -> bool {
        self.imports.iter().any(|i| i == "C")
    }
}

/// Parses one Go source file.
pub(crate) fn parse(src: &str) -> GoFile {
    let mut file = GoFile::default();
    for line in src.lines() {
        let l = line.trim_start();
        if l.starts_with("//go:build") || l.starts_with("// +build") {
            file.exported.push(canon(l));
        }
        if l.starts_with("//go:embed") {
            file.embeds = true;
        }
    }
    for decl in top_level(&strip_c_comments(src)) {
        let (keyword, rest) = ident(&decl);
        match keyword {
            "package" => file.exported.push(canon(&decl)),
            "import" => file.imports.extend(string_literals(rest)),
            "func" => {
                if let Some(sig) = func_signature(&decl) {
                    file.exported.push(canon(sig));
                }
            }
            "type" => {
                for spec in specs(rest) {
                    let text = canon(&format!("type {spec}"));
                    let names = names(&spec);
                    if names.iter().any(|n| exported(n)) {
                        file.exported.push(text);
                    } else {
                        for n in names {
                            file.private.entry(n).or_default().push(text.clone());
                        }
                    }
                }
            }
            "const" => {
                let specs = specs(rest);
                let names: Vec<String> = specs.iter().flat_map(|s| names(s)).collect();
                // A group shares iota and implicit repetition: any spec can change any
                // other's value, so the whole group counts.
                let text = canon(&decl);
                if names.iter().any(|n| exported(n)) {
                    file.exported.push(text);
                } else {
                    for n in names {
                        file.private.entry(n).or_default().push(text.clone());
                    }
                }
            }
            "var" => {
                for spec in specs(rest) {
                    if names(&spec).iter().any(|n| exported(n)) {
                        file.exported.push(canon(&format!("var {spec}")));
                    }
                }
            }
            _ => {}
        }
    }
    file
}

/// The interface parts of a package: the exported parts of its files plus every unexported
/// type or constant they mention (transitively), sorted.
pub(crate) fn interface<'a>(files: impl IntoIterator<Item = &'a GoFile>) -> Vec<String> {
    let mut parts = Vec::new();
    let mut private: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for f in files {
        parts.extend(f.exported.iter().cloned());
        for (name, texts) in &f.private {
            private
                .entry(name.as_str())
                .or_default()
                .extend(texts.iter().map(String::as_str));
        }
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut work: Vec<String> = parts.clone();
    while let Some(text) = work.pop() {
        for id in identifiers(&text) {
            if let Some((name, texts)) = private.get_key_value(id.as_str())
                && seen.insert(name)
            {
                for t in texts {
                    parts.push((*t).to_owned());
                    work.push((*t).to_owned());
                }
            }
        }
    }
    parts.sort();
    parts
}

/// Whether a Go identifier is exported.
fn exported(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The leading identifier of `s` (after whitespace) and the rest, trimmed at the start.
fn ident(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(|c: char| !is_ident(c)).unwrap_or(s.len());
    (&s[..end], s[end..].trim_start())
}

/// Tracks string and rune literals while walking Go source one character at a time.
#[derive(Default)]
struct Literals {
    quote: Option<char>,
    escaped: bool,
}

impl Literals {
    /// Feeds `c`; true when it is code rather than part of a literal (quotes included).
    fn code(&mut self, c: char) -> bool {
        if let Some(q) = self.quote {
            if self.escaped {
                self.escaped = false;
            } else if c == '\\' && q != '`' {
                self.escaped = true;
            } else if c == q || (c == '\n' && q != '`') {
                // A newline ends an unterminated interpreted literal: recover, do not swallow
                // the rest of the file.
                self.quote = None;
            }
            return false;
        }
        if matches!(c, '"' | '\'' | '`') {
            self.quote = Some(c);
            return false;
        }
        true
    }
}

fn depth_delta(c: char) -> i32 {
    match c {
        '(' | '[' | '{' => 1,
        ')' | ']' | '}' => -1,
        _ => 0,
    }
}

const KEYWORDS: &[&str] = &["package", "import", "func", "type", "const", "var"];

/// Top-level declarations of comment-free source. A declaration ends at a newline outside
/// brackets; a line that does not start with a keyword continues the previous one (a
/// continued expression).
fn top_level(src: &str) -> Vec<String> {
    let mut decls: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut lit = Literals::default();
    let mut depth = 0i32;
    let flush = |decls: &mut Vec<String>, cur: &mut String| {
        let t = cur.trim();
        if !t.is_empty() {
            match decls.last_mut() {
                Some(last) if !KEYWORDS.contains(&ident(t).0) => {
                    last.push('\n');
                    last.push_str(t);
                }
                _ => decls.push(t.to_owned()),
            }
        }
        cur.clear();
    };
    for c in src.chars() {
        if lit.code(c) {
            depth = (depth + depth_delta(c)).max(0);
            if (c == '\n' || c == ';') && depth == 0 {
                flush(&mut decls, &mut cur);
                continue;
            }
        }
        cur.push(c);
    }
    flush(&mut decls, &mut cur);
    decls
}

/// The specs of a declaration after its keyword: one, or each line of a `( … )` group.
fn specs(rest: &str) -> Vec<String> {
    let Some(inner) = rest.strip_prefix('(') else {
        return vec![rest.trim().to_owned()];
    };
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut lit = Literals::default();
    let mut depth = 0i32;
    for c in inner.chars() {
        if lit.code(c) {
            depth += depth_delta(c);
            if depth < 0 {
                break; // the group's closing parenthesis
            }
            if (c == '\n' || c == ';') && depth == 0 {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_owned());
                }
                cur.clear();
                continue;
            }
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// The names a spec declares: `A, b int = 1, 2` gives `A` and `b`; `T[K any] struct{}`
/// gives `T`.
fn names(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = spec;
    loop {
        let (name, after) = ident(rest);
        if name.is_empty() {
            break;
        }
        out.push(name.to_owned());
        match after.strip_prefix(',') {
            Some(more) => rest = more,
            None => break,
        }
    }
    out
}

/// The byte length of the bracketed group `s` starts with, closer included.
fn group_len(s: &str) -> usize {
    let mut lit = Literals::default();
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if lit.code(c) {
            depth += depth_delta(c);
            if depth == 0 {
                return i + c.len_utf8();
            }
        }
    }
    s.len()
}

/// The signature of an exported function or method (up to the body), or `None` when its
/// name is unexported. Methods count whatever their receiver: an unexported type's exported
/// methods are reachable through interfaces and embedding.
fn func_signature(decl: &str) -> Option<&str> {
    let rest = decl.get(4..)?.trim_start();
    let after_receiver = if rest.starts_with('(') {
        &rest[group_len(rest)..]
    } else {
        rest
    };
    let (name, _) = ident(after_receiver);
    if !exported(name) {
        return None;
    }
    let mut lit = Literals::default();
    let mut depth = 0;
    for (i, c) in decl.char_indices() {
        if !lit.code(c) {
            continue;
        }
        if c == '{' && depth == 0 {
            // `struct{…}` and `interface{…}` in the signature are types, not the body.
            let before = decl[..i].trim_end();
            let is_type = ["struct", "interface"].iter().any(|kw| {
                before
                    .strip_suffix(kw)
                    .is_some_and(|b| !b.ends_with(is_ident))
            });
            if !is_type {
                return Some(&decl[..i]);
            }
        }
        depth += depth_delta(c);
    }
    Some(decl)
}

/// The contents of every string literal (interpreted or raw) in `s`.
fn string_literals(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '"' && c != '`' {
            continue;
        }
        let mut lit = String::new();
        while let Some(n) = chars.next() {
            if n == c {
                break;
            }
            if n == '\\' && c == '"' {
                if let Some(e) = chars.next() {
                    lit.push(e);
                }
                continue;
            }
            lit.push(n);
        }
        out.push(lit);
    }
    out
}

/// Identifiers outside literals in canonical text.
fn identifiers(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut lit = Literals::default();
    let mut cur = String::new();
    for c in text.chars() {
        if lit.code(c) && is_ident(c) {
            cur.push(c);
            continue;
        }
        if !cur.is_empty() && !cur.starts_with(|c: char| c.is_ascii_digit()) {
            out.insert(std::mem::take(&mut cur));
        }
        cur.clear();
    }
    if !cur.is_empty() && !cur.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(cur);
    }
    out
}

/// Canonical form of comment-free Go text: whitespace outside literals collapsed to one
/// space, none after an opening bracket or a comma nor before a closing bracket or a comma,
/// and trailing commas before a closing bracket removed, so gofmt and line-wrapping changes
/// do not count. Literals are kept verbatim.
fn canon(text: &str) -> String {
    const AFTER: &[char] = &['(', '[', '{', ',', ';'];
    const BEFORE: &[char] = &[')', ']', '}', ',', ';'];
    let mut out = String::with_capacity(text.len());
    let mut lit = Literals::default();
    let mut space = false;
    for c in text.trim().chars() {
        if !lit.code(c) {
            if space {
                out.push(' ');
                space = false;
            }
            out.push(c);
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if matches!(c, ')' | ']' | '}') && out.ends_with(',') {
            out.pop();
        }
        if space && !out.is_empty() && !out.ends_with(AFTER) && !BEFORE.contains(&c) {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheltie_adapter::scan::hash_parts;

    fn iface(src: &str) -> String {
        hash_parts(interface([&parse(src)]))
    }

    const PRICING: &str = r#"// Package pricing computes prices.
package pricing

import (
	"fmt"
	str "strings"
	_ "embed"
	. "math"
	`example.com/shop/internal/util`
)

// Rate is a tax rate in percent.
const Rate = 20

const (
	unit = 100
	Cents = unit * 1
)

type Item struct {
	Name  string
	Price int64
	sku   string
}

// Total sums the items.
func Total(items []Item, opts struct{ Round bool }) int64 {
	var t int64
	for _, i := range items {
		t += i.Price
	}
	return t + int64(Rate)
}

func (i Item) Label() string { return fmt.Sprintf("%s", str.ToUpper(i.Name)) }

func helper() int { return 1 }

var Default = Item{Name: "x"}
var internal = 3
"#;

    #[test]
    fn imports_in_every_form() {
        let f = parse(PRICING);
        assert_eq!(
            f.imports,
            [
                "fmt",
                "strings",
                "embed",
                "math",
                "example.com/shop/internal/util"
            ]
        );
        let single = parse("package a\nimport \"x/y\"\nimport z \"x/z\"\nimport \"C\"\n");
        assert_eq!(single.imports, ["x/y", "x/z", "C"]);
        assert!(single.cgo());
        assert!(!f.cgo());
    }

    #[test]
    fn exported_declarations() {
        let f = parse(PRICING);
        let parts = interface([&f]);
        assert_eq!(
            parts,
            [
                "const (unit = 100 Cents = unit * 1)",
                "const Rate = 20",
                "func (i Item) Label() string",
                "func Total(items []Item,opts struct{Round bool}) int64",
                "package pricing",
                "type Item struct {Name string Price int64 sku string}",
                "var Default = Item{Name: \"x\"}",
            ]
        );
    }

    #[test]
    fn comments_and_formatting_do_not_count() {
        let reformatted = PRICING
            .replace("// Total sums the items.\n", "/* Totals. */\n")
            .replace(
                "func Total(items []Item, opts struct{ Round bool }) int64 {",
                "func Total(\n\titems []Item,\n\topts struct{ Round bool },\n) int64 {",
            )
            .replace("Name  string", "Name string // the name");
        assert_eq!(iface(PRICING), iface(&reformatted));
    }

    #[test]
    fn body_and_unexported_changes_do_not_count() {
        let base = iface(PRICING);
        assert_eq!(
            base,
            iface(&PRICING.replace("t += i.Price", "t = t + i.Price"))
        );
        assert_eq!(base, iface(&PRICING.replace("return 1 }", "return 2 }")));
        assert_eq!(
            base,
            iface(&PRICING.replace("var internal = 3", "var internal = 4"))
        );
        assert_eq!(
            base,
            iface(&PRICING.replace("func helper() int", "func helper(x int) int"))
        );
    }

    #[test]
    fn interface_changes_count() {
        let base = iface(PRICING);
        for (find, replace) in [
            ("const Rate = 20", "const Rate = 21"),
            ("func Total(items []Item,", "func Total(items []*Item,"),
            ("Price int64", "Price int32"),
            ("sku   string", "sku   []byte"),
            ("Label() string", "Label() []byte"),
            ("unit = 100", "unit = 1000"),
            ("Item{Name: \"x\"}", "Item{Name: \"y\"}"),
            ("package pricing", "package prices"),
            ("struct{ Round bool }", "struct{ Round, Floor bool }"),
        ] {
            assert_ne!(base, iface(&PRICING.replace(find, replace)), "{find}");
        }
        let added = format!("{PRICING}\nfunc Discount() int64 {{ return 0 }}\n");
        assert_ne!(base, iface(&added));
        let tagged = format!("//go:build linux\n\n{PRICING}");
        assert_ne!(base, iface(&tagged));
    }

    #[test]
    fn unexported_types_reached_from_exported_ones_count() {
        let src = "package a\ntype inner struct { Name string }\ntype Outer struct { inner }\ntype other struct{ X int }\n";
        let base = iface(src);
        assert_ne!(base, iface(&src.replace("Name string", "Name []byte")));
        assert_eq!(base, iface(&src.replace("X int", "X uint")));
    }

    #[test]
    fn iota_groups_count_as_a_whole() {
        let src = "package a\nconst (\n\tfirst = iota\n\tSecond\n)\n";
        let swapped = "package a\nconst (\n\tzero = iota\n\tfirst\n\tSecond\n)\n";
        assert_ne!(iface(src), iface(swapped));
    }

    #[test]
    fn grouped_types_and_vars() {
        let src = "package a\ntype (\n\tA int\n\tb string\n)\nvar (\n\tX, y = 1, 2\n\tz = 3\n)\n";
        let parts = interface([&parse(src)]);
        assert_eq!(parts, ["package a", "type A int", "var X,y = 1,2"]);
    }

    #[test]
    fn signatures_stop_at_the_body() {
        assert_eq!(
            func_signature("func F() interface{ M() } {\n\treturn nil\n}"),
            Some("func F() interface{ M() } ")
        );
        assert_eq!(
            func_signature("func (r *T[K]) Get(k K) (V, bool) { return }"),
            Some("func (r *T[K]) Get(k K) (V, bool) ")
        );
        assert_eq!(func_signature("func (r *T) get() {}"), None);
        // Implemented in assembly: no body at all.
        assert_eq!(
            func_signature("func Add(a, b int) int"),
            Some("func Add(a, b int) int")
        );
    }

    #[test]
    fn literals_are_kept_verbatim() {
        assert_ne!(canon("const A = \"a  b\""), canon("const A = \"a b\""));
        assert_eq!(canon("f( a ,\n b , )"), "f(a,b)");
        let f = parse("package a\nconst A = \"} // {\"\nfunc B() {}\n");
        assert_eq!(
            interface([&f]),
            ["const A = \"} // {\"", "func B()", "package a"]
        );
    }

    #[test]
    fn directives() {
        let f =
            parse("package a\n\nimport _ \"embed\"\n\n//go:embed static/*\nvar files embed.FS\n");
        assert!(f.embeds);
    }
}
