//! Import statements: parsing them and resolving them to dotted module names.

use crate::scanner::{self, Stmt};

/// One imported module, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Import {
    /// Leading dots of a relative import (`from ..x import y` has 2); 0 for absolute imports.
    pub level: usize,
    /// The dotted module after the dots (empty for `from . import x`).
    pub module: String,
    /// Names imported by `from … import …` (aliases dropped); empty for `import a.b`.
    pub names: Vec<String>,
    /// `from … import *`.
    pub star: bool,
}

/// The imports of one statement (`import a, b.c as d` gives two). Anything that is not an
/// import statement gives none.
pub fn parse(stmt: &str) -> Vec<Import> {
    let text = stmt.trim();
    if let Some(rest) = keyword(text, "import") {
        return scanner::split_top(rest, ',')
            .into_iter()
            .filter_map(|part| {
                let module = before_as(part);
                (!module.is_empty()).then(|| Import {
                    level: 0,
                    module,
                    names: Vec::new(),
                    star: false,
                })
            })
            .collect();
    }
    let Some(rest) = text.strip_prefix("from") else {
        return Vec::new();
    };
    if !rest.starts_with(|c: char| c.is_whitespace() || c == '.') {
        return Vec::new();
    }
    let mut rest = rest.trim_start();
    let mut level = 0;
    while let Some(r) = rest.strip_prefix('.') {
        level += 1;
        rest = r.trim_start();
    }
    let (module, names) = match keyword(rest, "import") {
        Some(names) => ("", names),
        None => {
            let Some(at) = find_import_keyword(rest) else {
                return Vec::new();
            };
            (&rest[..at], &rest[at + "import".len()..])
        }
    };
    let module = dotted(module);
    if level == 0 && module.is_empty() {
        return Vec::new();
    }
    let names = names.trim();
    let names = names
        .strip_prefix('(')
        .and_then(|n| n.strip_suffix(')'))
        .unwrap_or(names);
    let mut out = Import {
        level,
        module,
        names: Vec::new(),
        star: false,
    };
    for name in scanner::split_top(names, ',') {
        let name = before_as(name);
        if name == "*" {
            out.star = true;
        } else if !name.is_empty() {
            out.names.push(name);
        }
    }
    vec![out]
}

/// `text` after a leading keyword followed by whitespace or `(` (so `importlib` is not `import`).
fn keyword<'a>(text: &'a str, kw: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(kw)?;
    rest.starts_with(|c: char| c.is_whitespace() || c == '(')
        .then_some(rest)
}

/// Byte offset of the ` import` keyword that ends `from <module>`.
fn find_import_keyword(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = text[from..].find("import") {
        let at = from + at;
        let before = text[..at].chars().next_back();
        if before.is_some_and(char::is_whitespace) && keyword(&text[at..], "import").is_some() {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

/// The name before an `as` alias, whitespace removed (`a . b as x` is `a.b`).
fn before_as(part: &str) -> String {
    part.split_whitespace().take_while(|w| *w != "as").collect()
}

/// A dotted name with any whitespace removed (`a . b` is `a.b`).
fn dotted(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Every import in a file's statements, wherever it is: module level, inside functions,
/// `try` blocks, `if TYPE_CHECKING:` and one-line blocks (`if x: import y`).
pub fn imports(stmts: &[Stmt]) -> Vec<Import> {
    let mut out = Vec::new();
    for s in stmts {
        collect(&s.text, &mut out);
    }
    out
}

fn collect(text: &str, out: &mut Vec<Import>) {
    match scanner::inline_body(text) {
        Some(body) => collect(body, out),
        None => out.extend(parse(text)),
    }
}

/// The absolute dotted module an import names, given the package of the importing file
/// (`None` outside any package, where relative imports cannot be resolved).
pub fn absolute(import: &Import, package: Option<&str>) -> Option<String> {
    if import.level == 0 {
        return Some(import.module.clone());
    }
    let package = package?;
    let mut parts: Vec<&str> = package.split('.').filter(|p| !p.is_empty()).collect();
    let up = import.level - 1;
    if up > parts.len() {
        return None;
    }
    parts.truncate(parts.len() - up);
    if !import.module.is_empty() {
        parts.push(&import.module);
    }
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The modules a dependency lookup should try, for `import`ing `name`: `name` itself and
/// then each enclosing package, deepest first (`a.b.c`, `a.b`, `a`).
pub fn candidates(name: &str) -> impl Iterator<Item = &str> {
    let mut next = Some(name);
    std::iter::from_fn(move || {
        let cur = next?;
        next = cur.rfind('.').map(|i| &cur[..i]);
        Some(cur)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::statements;

    fn imp(level: usize, module: &str, names: &[&str]) -> Import {
        Import {
            level,
            module: module.into(),
            names: names.iter().map(|n| (*n).to_owned()).collect(),
            star: false,
        }
    }

    #[test]
    fn absolute_imports() {
        assert_eq!(
            parse("import a.b.c as x, d"),
            [imp(0, "a.b.c", &[]), imp(0, "d", &[])]
        );
        assert_eq!(
            parse("from a.b import c, d as e"),
            [imp(0, "a.b", &["c", "d"])]
        );
        assert!(parse("importlib.reload(x)").is_empty());
        assert!(parse("fromage = 1").is_empty());
        assert!(parse("x = 'import y'").is_empty());
        let star = parse("from a import *");
        assert!(star[0].star && star[0].names.is_empty());
    }

    #[test]
    fn relative_imports() {
        assert_eq!(parse("from . import x"), [imp(1, "", &["x"])]);
        assert_eq!(parse("from .import x"), [imp(1, "", &["x"])]);
        assert_eq!(parse("from ..pkg import y"), [imp(2, "pkg", &["y"])]);
        assert_eq!(parse("from .imports import z"), [imp(1, "imports", &["z"])]);
        assert_eq!(parse("from ... import w"), [imp(3, "", &["w"])]);

        let pkg = Some("shop.cart");
        assert_eq!(
            absolute(&imp(1, "", &["x"]), pkg).as_deref(),
            Some("shop.cart")
        );
        assert_eq!(
            absolute(&imp(2, "pricing", &[]), pkg).as_deref(),
            Some("shop.pricing")
        );
        assert_eq!(absolute(&imp(3, "", &[]), pkg), None);
        assert_eq!(absolute(&imp(1, "x", &[]), None), None);
        assert_eq!(
            absolute(&imp(0, "os.path", &[]), None).as_deref(),
            Some("os.path")
        );
    }

    #[test]
    fn multi_line_and_nested_imports() {
        let src = "\
from shop.pricing import (
    price,  # the main one
    Money as M,
)
def f():
    import json
try:
    from fast import x
except ImportError:  # fallback
    x = None
if TYPE_CHECKING: from shop.cart import Cart
s = '''
import not_an_import
'''
";
        let found = imports(&statements(src));
        assert_eq!(
            found,
            [
                imp(0, "shop.pricing", &["price", "Money"]),
                imp(0, "json", &[]),
                imp(0, "fast", &["x"]),
                imp(0, "shop.cart", &["Cart"]),
            ]
        );
    }

    #[test]
    fn lookup_candidates() {
        assert_eq!(
            candidates("a.b.c").collect::<Vec<_>>(),
            ["a.b.c", "a.b", "a"]
        );
        assert_eq!(candidates("a").collect::<Vec<_>>(), ["a"]);
    }
}
