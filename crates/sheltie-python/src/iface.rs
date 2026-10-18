//! The public interface of a Python module, read from its statements.
//!
//! What dependents of a module can see, as canonical text ([`scanner::canonical`]): public
//! function signatures (with decorators, not bodies), public classes with their bases,
//! method signatures and class-level attributes, public module-level assignments, type
//! aliases and `__all__`, and in `__init__.py` every import (re-exports). Statements inside
//! module-level `if`, `try`, `with`, `for` and `while` blocks count as module level, and so
//! do the block headers. When unsure, a statement is included: an extra one only widens a
//! footprint, a missing one could hide a conflict.

use crate::imports::{self, Import};
use crate::scanner::{self, Stmt};

/// What a module exposes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Surface {
    /// The interface statements, canonical, in source order.
    pub parts: Vec<String>,
    /// Imports whose names the module re-exports: every module-level import of an
    /// `__init__.py`, and `from … import *` anywhere at module level.
    pub reexports: Vec<Import>,
}

/// Whether a name is public: no leading underscore, or a dunder such as `__all__`.
pub fn is_public(name: &str) -> bool {
    !name.starts_with('_') || (name.len() > 4 && name.starts_with("__") && name.ends_with("__"))
}

/// The surface of a module's source. `is_init` is true for `__init__.py` and `__init__.pyi`.
pub fn surface(src: &str, is_init: bool) -> Surface {
    let stmts = scanner::statements(src);
    let mut out = Surface::default();
    module_level(&stmts, is_init, &mut out);
    out
}

/// End (exclusive) of the block opened by `stmts[i]`: the following statements indented
/// deeper than it.
fn block_end(stmts: &[Stmt], i: usize) -> usize {
    let indent = stmts[i].indent;
    let mut j = i + 1;
    while j < stmts.len() && stmts[j].indent > indent {
        j += 1;
    }
    j
}

enum Kind<'a> {
    Decorator,
    Def(&'a str),
    Class(&'a str),
    Import,
    Block,
    Other,
}

fn kind(text: &str) -> Kind<'_> {
    if text.starts_with('@') {
        return Kind::Decorator;
    }
    let word = scanner::first_word(text);
    let after = |w: &str| text[w.len()..].trim_start();
    match word {
        "def" => Kind::Def(scanner::first_word(after(word))),
        "async" if scanner::first_word(after(word)) == "def" => {
            Kind::Def(scanner::first_word(after(word)["def".len()..].trim_start()))
        }
        "class" => Kind::Class(scanner::first_word(after(word))),
        "import" | "from" if !imports::parse(text).is_empty() => Kind::Import,
        _ if scanner::is_compound(text) => Kind::Block,
        _ => Kind::Other,
    }
}

/// The header of a block statement, through its colon.
fn header(text: &str) -> &str {
    scanner::header_colon(text).map_or(text, |c| &text[..=c])
}

fn with_decorators(decorators: &[String], text: &str) -> String {
    let mut part = decorators.join("\n");
    if !part.is_empty() {
        part.push('\n');
    }
    part.push_str(&scanner::canonical(text));
    part
}

fn module_level(stmts: &[Stmt], is_init: bool, out: &mut Surface) {
    let mut decorators: Vec<String> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        let end = block_end(stmts, i);
        let text = stmts[i].text.as_str();
        let body = &stmts[i + 1..end];
        match kind(text) {
            Kind::Decorator => {
                decorators.push(scanner::canonical(text));
                i = end;
                continue;
            }
            Kind::Def(name) => {
                if is_public(name) {
                    out.parts.push(with_decorators(&decorators, header(text)));
                }
            }
            Kind::Class(name) => {
                if is_public(name) {
                    out.parts
                        .push(with_decorators(&decorators, class_line(text)));
                    class_body(body, out);
                }
            }
            Kind::Import => {
                let found = imports::parse(text);
                if is_init {
                    out.parts.push(scanner::canonical(text));
                    out.reexports.extend(found);
                } else if found.iter().any(|f| f.star) {
                    out.parts.push(scanner::canonical(text));
                    out.reexports.extend(found.into_iter().filter(|f| f.star));
                }
            }
            Kind::Block => {
                out.parts.push(scanner::canonical(header(text)));
                if let Some(inline) = scanner::inline_body(text) {
                    module_level(&scanner::statements(inline), is_init, out);
                }
                if !body.is_empty() {
                    module_level(body, is_init, out);
                }
            }
            Kind::Other => {
                if assigned_names(text).iter().any(|n| is_public(n)) {
                    out.parts.push(scanner::canonical(text));
                }
            }
        }
        decorators.clear();
        i = end;
    }
}

/// A class statement's header; a one-line class (`class A: x = 1`) counts whole.
fn class_line(text: &str) -> &str {
    if scanner::inline_body(text).is_some() {
        text
    } else {
        header(text)
    }
}

/// Members of a public class: method signatures except `_private` ones (dunders count),
/// nested public classes, and every class-level assignment or annotation (private ones
/// too: a dataclass field named `_x` is still a constructor parameter).
fn class_body(stmts: &[Stmt], out: &mut Surface) {
    let mut decorators: Vec<String> = Vec::new();
    let mut i = 0;
    while i < stmts.len() {
        let end = block_end(stmts, i);
        let text = stmts[i].text.as_str();
        let body = &stmts[i + 1..end];
        match kind(text) {
            Kind::Decorator => {
                decorators.push(scanner::canonical(text));
                i = end;
                continue;
            }
            Kind::Def(name) => {
                if is_public(name) {
                    out.parts.push(with_decorators(&decorators, header(text)));
                }
            }
            Kind::Class(name) => {
                if is_public(name) {
                    out.parts
                        .push(with_decorators(&decorators, class_line(text)));
                    class_body(body, out);
                }
            }
            Kind::Block => {
                out.parts.push(scanner::canonical(header(text)));
                class_body(body, out);
            }
            Kind::Import => {}
            Kind::Other => {
                if !assigned_names(text).is_empty() {
                    out.parts.push(scanner::canonical(text));
                }
            }
        }
        decorators.clear();
        i = end;
    }
}

/// Statements that start with these are never assignments.
const NOT_TARGETS: &[&str] = &[
    "assert", "del", "global", "nonlocal", "raise", "return", "yield", "pass", "break", "continue",
    "lambda", "await", "not", "print",
];

/// The names a statement assigns or annotates at its top level (`a, b = …`, `X: int`,
/// `x += 1`, `type T = …`), by their base name (`obj.attr = …` gives `obj`). Empty for any
/// other statement.
pub fn assigned_names(text: &str) -> Vec<String> {
    let word = scanner::first_word(text);
    if word == "type" {
        let rest = text["type".len()..].trim_start();
        let name = scanner::first_word(rest);
        if !name.is_empty() && rest.contains('=') {
            return vec![name.to_owned()];
        }
    }
    if NOT_TARGETS.contains(&word) {
        return Vec::new();
    }
    let top = scanner::top_level(text);
    let mut targets: Vec<&str> = Vec::new();
    let mut from = 0;
    for (at, c) in top {
        let next_eq = text[at + 1..].starts_with('=');
        if c == ':' {
            if targets.is_empty() && !next_eq {
                // An annotation: `name: T` or `name: T = v`.
                targets.push(&text[from..at]);
                break;
            }
            continue;
        }
        if c != '=' || next_eq {
            continue;
        }
        let before = &text[..at];
        let shift = before.ends_with(">>") || before.ends_with("<<");
        if !shift && before.ends_with(['=', '!', '<', '>', ':']) {
            continue; // `==`, `!=`, `<=`, `>=`, `:=`
        }
        let target = before[from..].trim_end_matches(|ch: char| "+-*/%&|^@<>".contains(ch));
        if scanner::first_word(target.trim_start()) == "lambda" {
            break; // `f = lambda x=1: x`: the rest is the value
        }
        targets.push(target);
        from = at + 1;
    }
    let mut names = Vec::new();
    for target in targets {
        for piece in scanner::split_top(target, ',') {
            let piece = piece.trim_start_matches(['*', '(', '[', ' ']);
            let name = scanner::first_word(piece);
            if name.is_empty() || name.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                continue;
            }
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#""""Pricing."""
from decimal import Decimal
from ._util import _round

TAX_RATE = 20
_CACHE = {}
__all__ = ["price", "Money"]


@lru_cache(maxsize=None)
def price(net: Decimal, qty: int = 1) -> Decimal:
    """The gross price."""
    total = net * qty
    return _round(total * (100 + TAX_RATE) / 100)


def _helper(x):
    return x


class Money(Base):
    """An amount."""

    currency: str = "EUR"
    _cents: int = 0

    def __init__(self, cents: int) -> None:
        self._cents = cents

    @property
    def cents(self) -> int:
        return self._cents

    def _check(self):
        pass


class _Private:
    def visible(self): ...


if TYPE_CHECKING:
    Alias = Money
"#;

    fn hash(src: &str) -> Vec<String> {
        surface(src, false).parts
    }

    fn edit(find: &str, replace: &str) -> Vec<String> {
        assert_eq!(BASE.matches(find).count(), 1, "{find}");
        hash(&BASE.replace(find, replace))
    }

    #[test]
    fn surface_lists_public_declarations() {
        assert_eq!(
            hash(BASE),
            [
                "TAX_RATE=20",
                "__all__=[\"price\",\"Money\"]",
                "@lru_cache(maxsize=None)\ndef price(net:Decimal,qty:int=1)->Decimal:",
                "class Money(Base):",
                "currency:str=\"EUR\"",
                "_cents:int=0",
                "def __init__(self,cents:int)->None:",
                "@property\ndef cents(self)->int:",
                "if TYPE_CHECKING:",
                "Alias=Money",
            ]
        );
    }

    #[test]
    fn bodies_private_names_and_formatting_do_not_count() {
        let base = hash(BASE);
        assert_eq!(edit("total = net * qty", "total = qty * net"), base);
        assert_eq!(edit("    return x\n", "    return x + 1\n"), base);
        assert_eq!(edit("def _helper(x):", "def _helper(x, y=2):"), base);
        assert_eq!(edit("_CACHE = {}", "_CACHE = {1: 2}"), base);
        assert_eq!(edit("        pass\n", "        return 1\n"), base);
        assert_eq!(
            edit("def visible(self): ...", "def visible(self, x): ..."),
            base
        );
        assert_eq!(edit("TAX_RATE = 20", "TAX_RATE = 20  # percent"), base);
        assert_eq!(
            edit(
                "def price(net: Decimal, qty: int = 1) -> Decimal:",
                "def price(\n    net: Decimal,\n    qty: int = 1,\n) -> Decimal:"
            )
            .len(),
            base.len()
        );
        assert_eq!(
            edit(
                "def price(net: Decimal, qty: int = 1) -> Decimal:",
                "def price(net:Decimal,  qty:int=1)->Decimal:"
            ),
            base
        );
        assert_eq!(edit("\"\"\"An amount.\"\"\"", "\"\"\"A sum.\"\"\""), base);
    }

    #[test]
    fn interface_changes_count() {
        let base = hash(BASE);
        for (find, replace) in [
            ("qty: int = 1)", "qty: int = 2)"),
            ("-> Decimal:", "-> float:"),
            ("@lru_cache(maxsize=None)", "@lru_cache(maxsize=8)"),
            ("TAX_RATE = 20", "TAX_RATE = 21"),
            ("class Money(Base):", "class Money(Base, Generic[T]):"),
            ("currency: str = \"EUR\"", "currency: str = \"USD\""),
            ("    def _check(self):", "    def check(self):"),
            (
                "def __init__(self, cents: int)",
                "def __init__(self, cents: int, rate: int)",
            ),
            ("    Alias = Money", "    Alias = Decimal"),
            ("def _helper(x):", "def helper(x):"),
        ] {
            assert_ne!(edit(find, replace), base, "{find} -> {replace}");
        }
    }

    #[test]
    fn init_reexports() {
        let src = "from .pricing import price\nfrom shop_core import *\nimport os\n_x = 1\n";
        let init = surface(src, true);
        assert_eq!(init.parts.len(), 3);
        assert_eq!(init.reexports.len(), 3);
        let module = surface(src, false);
        assert_eq!(module.parts, ["from shop_core import*"]);
        assert_eq!(module.reexports.len(), 1);
        assert_ne!(
            surface("from .pricing import price\n", true),
            surface("from .pricing import price, Money\n", true)
        );
    }

    #[test]
    fn assignments() {
        assert_eq!(assigned_names("a, *b = f()"), ["a", "b"]);
        assert_eq!(assigned_names("x: Final[int] = 3"), ["x"]);
        assert_eq!(assigned_names("y: int"), ["y"]);
        assert_eq!(assigned_names("__all__ += ['z']"), ["__all__"]);
        assert_eq!(assigned_names("a = b = 1"), ["a", "b"]);
        assert_eq!(assigned_names("obj.attr[0] = 1"), ["obj"]);
        assert_eq!(assigned_names("type Vec = list[float]"), ["Vec"]);
        assert_eq!(assigned_names("f = lambda x=1: x"), ["f"]);
        assert_eq!(assigned_names("x >>= 2"), ["x"]);
        assert!(assigned_names("f(a=1)").is_empty());
        assert!(assigned_names("a == b").is_empty());
        assert!(assigned_names("assert a, 'x=1'").is_empty());
        assert!(assigned_names("print(x)").is_empty());
        assert!(assigned_names("x <= 3").is_empty());
        assert!(is_public("__all__") && is_public("price"));
        assert!(!is_public("_x") && !is_public("__mangled") && !is_public("__"));
    }
}
