//! The public interface and imports of JavaScript and TypeScript sources, read without a
//! compiler.
//!
//! The scanner splits comment-free source into top-level statements and keeps those that can
//! appear in what dependents see: exports, type-level declarations and classes. Function
//! bodies are dropped when their opening brace can be told apart from the signature;
//! otherwise the whole statement is kept. A file whose brackets or quotes do not balance is
//! kept whole. When unsure, include more.

use std::collections::BTreeSet;

use sheltie_adapter::scan::{normalize, strip_c_comments};

/// Extensions of the sources that are scanned.
const SOURCE_EXTS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];

/// Words that start a top-level statement: a line at depth 0 starting with one ends the
/// statement before it (automatic semicolon insertion, approximated).
const STATEMENT_WORDS: &[&str] = &[
    "export",
    "import",
    "const",
    "let",
    "var",
    "function",
    "async",
    "class",
    "abstract",
    "interface",
    "type",
    "enum",
    "declare",
    "namespace",
    "module",
    "exports",
    "if",
    "for",
    "while",
    "do",
    "switch",
    "try",
    "throw",
];

/// Whether `path` is a JavaScript or TypeScript source.
pub(crate) fn is_source(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, ext)| SOURCE_EXTS.contains(&ext))
}

/// Whether `path` is a TypeScript declaration file (`.d.ts`, `.d.mts`, `.d.cts`).
pub(crate) fn is_declaration(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|s| name.ends_with(s))
}

/// Whether `rel` (a path relative to its unit's directory) is a test: `*.test.*`, `*.spec.*`,
/// or anything under `__tests__/`, `test/` or `tests/`.
pub(crate) fn is_test(rel: &str) -> bool {
    let mut segments: Vec<&str> = rel.split('/').collect();
    let name = segments.pop().unwrap_or_default();
    segments
        .iter()
        .any(|s| matches!(*s, "__tests__" | "test" | "tests"))
        || name.contains(".test.")
        || name.contains(".spec.")
}

/// The interface parts of one source file, normalized. Empty when it declares nothing
/// dependents can see.
pub(crate) fn interface(path: &str, src: &str) -> Vec<String> {
    let text = strip_c_comments(src);
    if is_declaration(path) {
        return vec![normalize(&text)];
    }
    let Some(chunks) = split(&text, false) else {
        return vec![normalize(&text)];
    };
    let exported = locally_exported(&chunks);
    let mut parts = Vec::new();
    for chunk in &chunks {
        let (word, rest) = first_word(chunk);
        match word {
            "export" => {
                let decl = rest.trim_start();
                let (next, after) = first_word(decl);
                let decl = if next == "default" {
                    after.trim_start()
                } else {
                    decl
                };
                if decl.starts_with('{') || decl.starts_with('*') || decl.starts_with('=') {
                    parts.push(normalize(chunk));
                } else {
                    parts.extend(declaration(chunk, decl));
                }
            }
            "module" | "exports" if is_commonjs_export(chunk) => parts.push(normalize(chunk)),
            "declare" | "interface" | "enum" | "namespace" | "abstract" | "class" => {
                parts.extend(declaration(chunk, chunk));
            }
            "type" if starts_with_ident(rest) => parts.push(normalize(chunk)),
            _ if chunk.starts_with('@') => parts.push(normalize(chunk)),
            _ => {
                if declared_name(chunk).is_some_and(|n| exported.contains(n)) {
                    parts.extend(declaration(chunk, chunk));
                }
            }
        }
    }
    parts
}

/// Module specifiers imported or re-exported by `src`: `from '…'`, `import '…'`,
/// `import('…')` and `require('…')`.
pub(crate) fn imports(src: &str) -> BTreeSet<String> {
    let text = strip_c_comments(src);
    let mut found = BTreeSet::new();
    let mut quote: Option<(char, usize)> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        if let Some((q, start)) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q || c == '\n' {
                quote = None;
                if c == q && q != '`' && is_import_position(&text[..start]) {
                    let spec = &text[start + 1..i];
                    if !spec.is_empty() {
                        found.insert(spec.to_owned());
                    }
                }
            }
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            quote = Some((c, i));
        }
    }
    found
}

/// Whether a string literal after `before` is a module specifier.
fn is_import_position(before: &str) -> bool {
    let b = before.trim_end();
    let b = b.strip_suffix('(').map_or(b, str::trim_end);
    ["from", "import", "require"].iter().any(|w| {
        b.strip_suffix(w)
            .is_some_and(|pre| !pre.chars().next_back().is_some_and(is_ident_char))
    })
}

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The leading identifier of `s` (after leading whitespace) and what follows it.
fn first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s.find(|c: char| !is_ident_char(c)).unwrap_or(s.len());
    (&s[..end], &s[end..])
}

fn starts_with_ident(s: &str) -> bool {
    s.starts_with(char::is_whitespace) && !first_word(s).0.is_empty()
}

fn is_commonjs_export(chunk: &str) -> bool {
    chunk.starts_with("module.exports") || chunk.starts_with("exports.")
}

/// The name a non-exported top-level `function`, `const`, `let`, `var` or `class` declares.
fn declared_name(chunk: &str) -> Option<&str> {
    let (mut word, mut rest) = first_word(chunk);
    if word == "async" {
        (word, rest) = first_word(rest);
    }
    match word {
        "function" => {
            let rest = rest.trim_start().trim_start_matches('*');
            Some(first_word(rest).0).filter(|n| !n.is_empty())
        }
        "const" | "let" | "var" | "class" => Some(first_word(rest).0).filter(|n| !n.is_empty()),
        _ => None,
    }
}

/// Names exported by reference: `export { a, b as c }` (without `from`), `export default a`,
/// `export = a`, and every identifier on the right of `module.exports =` or `exports.x =`.
fn locally_exported(chunks: &[String]) -> BTreeSet<&str> {
    let mut names = BTreeSet::new();
    for chunk in chunks {
        if is_commonjs_export(chunk) {
            if let Some((_, rhs)) = chunk.split_once('=') {
                names.extend(identifiers(rhs));
            }
            continue;
        }
        let (word, rest) = first_word(chunk);
        if word != "export" {
            continue;
        }
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("type ").map_or(rest, str::trim_start);
        if let Some(list) = rest.strip_prefix('{') {
            if list.contains("from") && has_word(list, "from") {
                continue;
            }
            let list = list.split('}').next().unwrap_or_default();
            for item in list.split(',') {
                let item = item.trim();
                let item = item.strip_prefix("type ").unwrap_or(item);
                let name = first_word(item).0;
                if !name.is_empty() {
                    names.insert(name);
                }
            }
        } else if let Some(r) = rest.strip_prefix('=') {
            names.extend(identifiers(r));
        } else {
            let (next, after) = first_word(rest);
            if next == "default" {
                let after = after.trim().trim_end_matches(';').trim();
                if !after.is_empty() && after.chars().all(is_ident_char) {
                    names.insert(after);
                }
            }
        }
    }
    names
}

fn has_word(text: &str, word: &str) -> bool {
    identifiers(text).any(|w| w == word)
}

fn identifiers(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !is_ident_char(c))
        .filter(|w| w.chars().next().is_some_and(|c| !c.is_ascii_digit()))
}

/// The interface parts of a declaration statement. `decl` is the statement without its
/// `export` / `export default` prefix.
fn declaration(chunk: &str, decl: &str) -> Vec<String> {
    let mut head = decl;
    loop {
        let (w, rest) = first_word(head);
        if matches!(w, "declare" | "abstract" | "async" | "default") {
            head = rest;
        } else {
            break;
        }
    }
    let (word, _) = first_word(head);
    if word == "class" {
        return class(chunk);
    }
    let first_line = head.lines().next().unwrap_or_default();
    let function_like = word == "function"
        || (matches!(word, "const" | "let" | "var")
            && (first_line.contains("=>") || has_word(first_line, "function")));
    // Declarations and overloads without a body are kept whole.
    if function_like && let Some(split) = body(chunk, true) {
        let mut parts = vec![normalize(&chunk[..split.open])];
        let tail = normalize(&chunk[split.close + 1..]);
        if !matches!(tail.as_str(), "" | ";") {
            parts.push(tail);
        }
        return parts;
    }
    vec![normalize(chunk)]
}

/// A class: its header, every member's signature (methods with a recognisable body) or
/// whole text (everything else), and anything after the class body.
fn class(chunk: &str) -> Vec<String> {
    let Some(split) = body(chunk, false) else {
        return vec![normalize(chunk)];
    };
    let Some(members) = split_members(&chunk[split.open + 1..split.close]) else {
        return vec![normalize(chunk)];
    };
    let mut parts = vec![normalize(&chunk[..=split.open])];
    for m in &members {
        match body(m, true) {
            Some(s) => {
                parts.push(normalize(&m[..s.open]));
                let tail = normalize(&m[s.close + 1..]);
                if !matches!(tail.as_str(), "" | ";") {
                    parts.push(tail);
                }
            }
            None => parts.push(normalize(m)),
        }
    }
    parts.push(normalize(&chunk[split.close..]));
    parts
}

fn split_members(body: &str) -> Option<Vec<String>> {
    split(body, true)
}

/// Byte offsets of a block body's braces.
struct Body {
    open: usize,
    close: usize,
}

/// Finds the body block of a function-like or class statement: the first line ending with
/// `{` after which parentheses and brackets are balanced, that brace is the only one open,
/// and the character before it cannot end a type (so `): {` starts a return type, not a
/// body). The body must close on a line of its own (`}` or `};`), and with `needs_params`
/// the signature must contain `(`. `None` when the body cannot be told apart.
fn body(chunk: &str, needs_params: bool) -> Option<Body> {
    let mut paren = 0i32;
    let mut brace = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut open = None;
    let mut line_start = 0;
    for (i, c) in chunk.char_indices() {
        if c == '\n' {
            line_start = i + 1;
        }
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q || (c == '\n' && q != '`') {
                quote = None;
            }
            // A newline that ended a quote still ends the line; anything else is quoted.
            if quote.is_some() || c != '\n' {
                continue;
            }
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' => paren += 1,
            ')' | ']' => paren -= 1,
            '{' => brace += 1,
            '}' => {
                brace -= 1;
                if brace == 0
                    && let Some(open) = open
                {
                    let line_end = chunk[i..].find('\n').map_or(chunk.len(), |n| i + n);
                    let closing = chunk[line_start..line_end].trim();
                    return matches!(closing, "}" | "};").then_some(Body { open, close: i });
                }
            }
            _ => {}
        }
        if open.is_none() && c == '{' && paren == 0 && brace == 1 && quote.is_none() {
            let rest_of_line = chunk[i + 1..].split('\n').next().unwrap_or_default();
            let before = chunk[..i].trim_end();
            let ends_type = before
                .chars()
                .next_back()
                .is_none_or(|b| matches!(b, ':' | '|' | '&' | '<' | ',' | '=' | '(' | '[' | '?'));
            if rest_of_line.trim().is_empty()
                && !ends_type
                && (!needs_params || before.contains('('))
            {
                open = Some(i);
            }
        }
    }
    None
}

/// Splits comment-free source into statements (top level) or members (`class_body`).
/// `None` when brackets or quotes do not balance.
fn split(text: &str, class_body: bool) -> Option<Vec<String>> {
    let mut chunks = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        cur.push(c);
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q || (c == '\n' && q != '`') {
                quote = None;
            }
            // A newline that ended a quote still ends the line; anything else is quoted.
            if quote.is_some() || c != '\n' {
                continue;
            }
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            ';' if depth == 0 => end(&mut chunks, &mut cur),
            '\n' if depth == 0 => {
                let next = text[i + 1..]
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or_default();
                let ends = if class_body {
                    !continues(cur.trim_end(), next)
                } else {
                    starts_statement(next)
                };
                if ends {
                    end(&mut chunks, &mut cur);
                }
            }
            _ => {}
        }
    }
    if depth != 0 || quote == Some('`') {
        return None;
    }
    end(&mut chunks, &mut cur);
    Some(chunks)
}

fn end(chunks: &mut Vec<String>, cur: &mut String) {
    let t = cur.trim();
    if !t.is_empty() && t != ";" {
        chunks.push(t.to_owned());
    }
    cur.clear();
}

/// Whether a top-level line starts a new statement.
fn starts_statement(line: &str) -> bool {
    line.starts_with('@') || STATEMENT_WORDS.contains(&first_word(line).0)
}

/// Whether a class member continues on the next line (an operator ends this one or starts
/// the next).
fn continues(cur: &str, next: &str) -> bool {
    const OPS: &[char] = &['=', ':', '|', '&', ',', '.', '?', '+', '-', '*', '/'];
    cur.ends_with(OPS) || next.starts_with(OPS) || next.starts_with("=>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheltie_adapter::scan::hash_parts;

    fn iface(src: &str) -> String {
        hash_parts(interface("src/index.ts", src))
    }

    #[test]
    fn classifies_paths() {
        assert!(is_source("src/a.tsx") && is_source("a.cjs") && !is_source("a.json"));
        assert!(is_declaration("types/index.d.ts") && !is_declaration("src/d.ts"));
        assert!(is_test("src/a.test.ts") && is_test("src/a.spec.js"));
        assert!(is_test("__tests__/a.ts") && is_test("test/a.ts") && is_test("src/tests/a.ts"));
        assert!(!is_test("src/testing.ts") && !is_test("src/contest/a.ts"));
    }

    #[test]
    fn realistic_patterns() {
        let src = r#""use client";
import type { Req } from "./types";

@Component({
  selector: "app-cart",
})
export class CartComponent implements OnInit {
  @Input() name!: string;
  constructor(private http: HttpClient) {}
  ngOnInit(): void {
    this.load();
  }
}

export function f(a: string): string;
export function f(a: any): any {
  return a;
}

export enum Color { Red, Green }

export const id = <T,>(x: T): T => {
  return x;
};

export default async function handler(req: Req) {
  await run(req);
}

export function Banner({ title }: Props) {
  return <p>Don't miss {title}</p>;
}
"#;
        assert_eq!(
            interface("x.tsx", src),
            [
                "@Component({ selector: \"app-cart\", })",
                "export class CartComponent implements OnInit {",
                "@Input() name!: string;",
                "constructor(private http: HttpClient) {}",
                "ngOnInit(): void",
                "}",
                "export function f(a: string): string;",
                "export function f(a: any): any",
                "export enum Color { Red, Green }",
                "export const id = <T,>(x: T): T =>",
                "export default async function handler(req: Req)",
                "export function Banner({ title }: Props)",
            ]
        );
        let base = iface(src);
        for (find, replace) in [
            ("this.load();", "this.reload();"),
            ("return a;", "return a ?? null;"),
            ("return x;", "return x as T;"),
            ("await run(req);", "await run(req, {});"),
            ("Don't miss {title}", "Do not miss {title}!"),
        ] {
            assert_eq!(base, iface(&src.replace(find, replace)), "{find}");
        }
        // An object of functions is kept whole: its methods' bodies count (pessimistic).
        let api = "export const api = {\n  get: (id: string) => {\n    return id;\n  },\n};\n";
        assert_ne!(
            iface(api),
            iface(&api.replace("return id;", "return `${id}`;"))
        );
    }

    const BASE: &str = r#"
import { round } from "./math";

// Formats a price.
export function formatPrice(cents: number, currency: Currency = "EUR"): string {
  const value = round(cents / 100);
  return `${value} ${currency}`;
}

export const parse = (text: string): number => {
  return Number(text.replace(",", "."));
};

function helper(x: number) {
  return x * 2;
}

type Currency = "EUR" | "USD";

export interface Price {
  cents: number;
  currency: Currency;
}

export * from "./money";
export { helper as double };
"#;

    #[test]
    fn comments_and_formatting_do_not_count() {
        let reformatted = BASE
            .replace("// Formats a price.\n", "/** Formats a price, nicely. */\n")
            .replace("export interface Price {", "export   interface Price\n{")
            .replace("  cents: number;", "  cents: number; // in cents");
        assert_eq!(iface(BASE), iface(&reformatted));
    }

    #[test]
    fn bodies_do_not_count() {
        let base = iface(BASE);
        assert_eq!(base, iface(&BASE.replace("cents / 100", "cents / 100.0")));
        assert_eq!(base, iface(&BASE.replace("\",\", \".\"", "\",\", \"\"")));
        // A non-exported helper's body, even though it is exported by reference.
        assert_eq!(base, iface(&BASE.replace("x * 2", "x + x")));
    }

    #[test]
    fn signatures_types_and_reexports_count() {
        let base = iface(BASE);
        let changes = [
            ("cents: number, currency", "cents: bigint, currency"),
            (
                "(text: string): number",
                "(text: string, radix: number): number",
            ),
            (
                "currency: Currency;",
                "currency: Currency;\n  note?: string;",
            ),
            ("\"EUR\" | \"USD\"", "\"EUR\" | \"USD\" | \"PLN\""),
            ("export * from \"./money\";", "export * from \"./money2\";"),
            ("helper as double", "helper as twice"),
            (
                "function helper(x: number)",
                "function helper(x: number, y = 1)",
            ),
        ];
        for (find, replace) in changes {
            assert_ne!(base, iface(&BASE.replace(find, replace)), "{find}");
        }
    }

    #[test]
    fn multi_line_signatures_and_return_types() {
        let src = "export function f({\n  a,\n  b,\n}: Props): {\n  ok: boolean\n} {\n  return { ok: a > b };\n}\n";
        assert_eq!(
            interface("x.ts", src),
            ["export function f({ a, b, }: Props): { ok: boolean }"]
        );
        // The return type's object is not mistaken for a body.
        assert_ne!(iface(src), iface(&src.replace("ok: boolean", "ok: string")));
        // Unbalanced parentheses on the opening line: the whole statement is kept.
        let wrapped = "export const g = memo((p: P) => {\n  return p;\n});\n";
        assert_eq!(interface("x.ts", wrapped), [normalize(wrapped)]);
    }

    #[test]
    fn classes_keep_member_signatures() {
        let src = "class Cart {\n  private items: Item[] = [];\n  constructor(\n    private readonly tax: number,\n  ) {}\n  add(item: Item): void {\n    this.items.push(item);\n  }\n  get total() {\n    return 1;\n  }\n}\n";
        let base = iface(src);
        assert_eq!(
            base,
            iface(&src.replace("this.items.push(item);", "this.items.unshift(item);"))
        );
        assert_eq!(base, iface(&src.replace("return 1;", "return 2;")));
        assert_ne!(
            base,
            iface(&src.replace("add(item: Item)", "add(item: Item, n = 1)"))
        );
        assert_ne!(base, iface(&src.replace("Item[] = []", "Item[] = [first]")));
    }

    #[test]
    fn commonjs_and_no_semicolons() {
        let src = "const x = require('./x')\nfunction add(a, b) {\n  return a + b\n}\nmodule.exports = { add }\n";
        let base = iface(src);
        assert_eq!(base, iface(&src.replace("a + b", "b + a")));
        assert_ne!(base, iface(&src.replace("add(a, b)", "add(a, b, c)")));
        assert_ne!(base, iface(&src.replace("{ add }", "{ add, x }")));
        let asi = "export const a = 1\nexport type T =\n  | 'a'\n  | 'b'\nconst internal = 2\n";
        assert_eq!(
            interface("x.ts", asi),
            ["export const a = 1", "export type T = | 'a' | 'b'"]
        );
    }

    #[test]
    fn unbalanced_files_are_kept_whole() {
        let src = "export const re = /{/;\nfunction f() { return 1 }\n";
        assert_eq!(interface("x.ts", src), [normalize(src)]);
        let dts = "declare function f(): void;\n";
        assert_eq!(interface("x.d.ts", dts), [normalize(dts)]);
    }

    #[test]
    fn finds_imports() {
        let src = "import a from '@acme/utils';\nimport type { B } from \"../b/src\";\nexport * from './c';\nconst d = require('d/sub');\nconst e = await import('e');\nimport 'side';\nconst s = 'from' + 'x';\n// import 'commented';\n";
        let found: Vec<String> = imports(src).into_iter().collect();
        assert_eq!(
            found,
            ["../b/src", "./c", "@acme/utils", "d/sub", "e", "side"]
        );
    }
}
