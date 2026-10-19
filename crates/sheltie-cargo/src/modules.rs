//! Module trees and per-module summaries, parsed with `syn`.
//!
//! For each module this records what the footprint needs: the files it owns, its signature
//! (items with function bodies removed, used for the interface hash), the paths it mentions
//! (for edges) and anything that makes the crate non-hermetic.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use sheltie_core::RepoPath;
use syn::ext::IdentExt as _;
use syn::visit::Visit;
use syn::visit_mut::VisitMut;
use syn::{Attribute, Item, Meta, Visibility};

use crate::Error;
use sheltie_adapter::git::{Git, TreeFiles};

/// A path as written in source: its segments, e.g. `["crate", "invoice", "bill"]`.
pub(crate) type PathRef = Vec<String>;

/// One entry of a `use` declaration, flattened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UseEntry {
    /// `use a::b::c as name;` (or without `as`): `name` refers to `a::b::c`.
    Name { path: PathRef, name: String },
    /// `use a::b::*;`
    Glob { path: PathRef },
}

/// A parsed module.
#[derive(Debug, Clone, Default)]
pub(crate) struct Module {
    /// Path from the crate root; empty for the root.
    pub(crate) path: Vec<String>,
    pub(crate) parent: Option<usize>,
    /// Child modules by name: index and whether declared `pub`.
    pub(crate) children: BTreeMap<String, (usize, bool)>,
    /// Inside `#[cfg(test)]` (directly or through an ancestor).
    pub(crate) is_test: bool,
    /// Files whose contents belong to this module.
    pub(crate) files: BTreeSet<RepoPath>,
    /// Item signatures with bodies, docs and test-only items removed.
    pub(crate) signature: String,
    /// Paths mentioned in `signature`.
    pub(crate) sig_paths: Vec<PathRef>,
    /// Every path mentioned in the module's own items, bodies included.
    pub(crate) paths: Vec<PathRef>,
    pub(crate) uses: Vec<UseEntry>,
    /// Defines `#[test]`-like functions or `proptest!` blocks.
    pub(crate) has_tests: bool,
    pub(crate) defines_macros: bool,
    /// Why this module makes its crate non-hermetic, if it does.
    pub(crate) leaks: Vec<String>,
}

/// A crate's module tree; index 0 is the root.
#[derive(Debug, Clone, Default)]
pub(crate) struct ModuleTree {
    pub(crate) modules: Vec<Module>,
}

impl ModuleTree {
    /// Parses the module tree rooted at `root` from the tree `files`.
    pub(crate) fn parse(git: &Git, files: &TreeFiles, root: &RepoPath) -> Result<Self, Error> {
        let mut tree = ModuleTree::default();
        let dir = parent_dir(root.as_str());
        let mut root_module = Module::default();
        root_module.files.insert(root.clone());
        tree.modules.push(root_module);
        match read_source(git, files, root.as_str())? {
            Some(source) => match syn::parse_file(&source) {
                Ok(file) => tree.add_items(git, files, 0, file.items, &dir, root.as_str(), true)?,
                Err(e) => tree.modules[0]
                    .leaks
                    .push(format!("cannot parse {root}: {e}")),
            },
            None => tree.modules[0]
                .leaks
                .push(format!("missing or non-UTF-8 crate root {root}")),
        }
        Ok(tree)
    }

    /// Records `items` as the contents of module `idx`. `dir` is where `mod x;` looks for
    /// `x.rs` / `x/mod.rs`; `file` is the file the items come from. `top` is true for a
    /// file's own items, false inside an inline `mod x { .. }`.
    #[allow(
        clippy::too_many_arguments,
        reason = "one recursive helper; the arguments are the walk's state"
    )]
    fn add_items(
        &mut self,
        git: &Git,
        files: &TreeFiles,
        idx: usize,
        items: Vec<Item>,
        dir: &str,
        file: &str,
        top: bool,
    ) -> Result<(), Error> {
        let parent_is_test = self.modules[idx].is_test;
        let mut own_items = Vec::new();
        for item in items {
            let Item::Mod(m) = item else {
                own_items.push(item);
                continue;
            };
            let name = m.ident.to_string();
            // `mod r#type;` lives in `type.rs`.
            let file_name = m.ident.unraw().to_string();
            let is_pub = matches!(m.vis, Visibility::Public(_));
            let child = Module {
                path: [self.modules[idx].path.clone(), vec![name.clone()]].concat(),
                parent: Some(idx),
                is_test: parent_is_test || has_test_cfg(&m.attrs),
                ..Module::default()
            };
            let child_idx = self.modules.len();
            self.modules.push(child);
            self.modules[idx]
                .children
                .insert(name.clone(), (child_idx, is_pub));
            // Keep the declaration (`pub mod x;`) in the parent's signature.
            own_items.push(Item::Mod(syn::ItemMod {
                content: None,
                semi: Some(Default::default()),
                ..m.clone()
            }));

            let path_attr = path_attr(&m.attrs);
            match m.content {
                Some((_, inner)) => {
                    // Inline module: shares the file; `mod y;` inside looks in `dir/name/`.
                    let child_dir = match &path_attr {
                        Some(p) => join(dir, p),
                        None => join(dir, &file_name),
                    };
                    self.modules[child_idx].files.insert(RepoPath::new(file));
                    self.add_items(git, files, child_idx, inner, &child_dir, file, false)?;
                }
                None => {
                    let candidates = match &path_attr {
                        // A file's own `#[path]` is relative to the file's directory, even
                        // in a non-`mod.rs` file; inside an inline module, to `dir`.
                        Some(p) if top => vec![(join(&parent_dir(file), p), true)],
                        Some(p) => vec![(join(dir, p), true)],
                        None => vec![
                            (join(dir, &format!("{file_name}.rs")), false),
                            (join(dir, &format!("{file_name}/mod.rs")), true),
                        ],
                    };
                    let Some((child_file, mod_rs)) =
                        candidates.into_iter().find(|(p, _)| files.contains(p))
                    else {
                        self.modules[child_idx].leaks.push(format!(
                            "module file for `mod {name};` not found in {dir:?}"
                        ));
                        continue;
                    };
                    self.modules[child_idx]
                        .files
                        .insert(RepoPath::new(&child_file));
                    let child_dir = if mod_rs || path_attr.is_some() {
                        parent_dir(&child_file)
                    } else {
                        join(dir, &file_name)
                    };
                    match read_source(git, files, &child_file)? {
                        Some(source) => match syn::parse_file(&source) {
                            Ok(parsed) => self.add_items(
                                git,
                                files,
                                child_idx,
                                parsed.items,
                                &child_dir,
                                &child_file,
                                true,
                            )?,
                            Err(e) => self.modules[child_idx]
                                .leaks
                                .push(format!("cannot parse {child_file}: {e}")),
                        },
                        None => self.modules[child_idx]
                            .leaks
                            .push(format!("non-UTF-8 source {child_file}")),
                    }
                }
            }
        }
        summarize(&mut self.modules[idx], own_items, file, files);
        Ok(())
    }
}

/// Fills in signature, paths, uses and flags from a module's own (non-module) items.
fn summarize(module: &mut Module, items: Vec<Item>, file: &str, files: &TreeFiles) {
    let mut collector = PathCollector::new(parent_dir(file));
    for item in &items {
        collector.visit_item(item);
    }

    let mut signature = TokenStream::new();
    let mut sig_collector = PathCollector::new(parent_dir(file));
    for item in items {
        if item_attrs(&item).is_some_and(|a| has_test_cfg(a)) {
            continue;
        }
        let mut item = item;
        StripBodies.visit_item_mut(&mut item);
        sig_collector.visit_item(&item);
        item.to_tokens(&mut signature);
    }

    for included in &collector.includes {
        if files.contains(included) {
            module.files.insert(RepoPath::new(included));
        } else {
            module
                .leaks
                .push(format!("included file {included} not in tree"));
        }
    }
    module.leaks.extend(collector.leaks);
    module
        .signature
        .push_str(&without_docs(signature).to_string());
    module.sig_paths.extend(sig_collector.paths);
    module.paths.extend(collector.paths);
    module.uses.extend(collector.uses);
    module.has_tests |= collector.has_tests;
    module.defines_macros |= collector.defines_macros;
}

/// Removes `#[doc = ...]` attributes (doc comments) from a token stream.
fn without_docs(tokens: TokenStream) -> TokenStream {
    let trees: Vec<TokenTree> = tokens.into_iter().collect();
    let mut out = TokenStream::new();
    let mut i = 0;
    while i < trees.len() {
        let is_doc = matches!(&trees[i], TokenTree::Punct(p) if p.as_char() == '#')
            && matches!(trees.get(i + 1), Some(TokenTree::Group(g))
                if g.delimiter() == proc_macro2::Delimiter::Bracket
                    && matches!(g.stream().into_iter().next(), Some(TokenTree::Ident(id)) if id == "doc"));
        if is_doc {
            i += 2;
            continue;
        }
        match &trees[i] {
            TokenTree::Group(g) => {
                let mut group = proc_macro2::Group::new(g.delimiter(), without_docs(g.stream()));
                group.set_span(g.span());
                out.extend([TokenTree::Group(group)]);
            }
            t => out.extend([t.clone()]),
        }
        i += 1;
    }
    out
}

/// Removes function bodies: what remains is what readers compile against.
struct StripBodies;

impl VisitMut for StripBodies {
    fn visit_block_mut(&mut self, block: &mut syn::Block) {
        block.stmts.clear();
    }

    fn visit_item_mod_mut(&mut self, _: &mut syn::ItemMod) {}
}

/// Collects paths, `use` entries, test markers, macro definitions and `include*!` files.
struct PathCollector {
    dir: String,
    paths: Vec<PathRef>,
    uses: Vec<UseEntry>,
    includes: Vec<String>,
    leaks: Vec<String>,
    has_tests: bool,
    defines_macros: bool,
}

impl PathCollector {
    fn new(dir: String) -> Self {
        Self {
            dir,
            paths: Vec::new(),
            uses: Vec::new(),
            includes: Vec::new(),
            leaks: Vec::new(),
            has_tests: false,
            defines_macros: false,
        }
    }

    /// Finds `a::b::c` sequences in macro input, which `syn` does not parse.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        let mut i = 0;
        while i < trees.len() {
            if let TokenTree::Group(g) = &trees[i] {
                self.scan_tokens(g.stream());
                i += 1;
                continue;
            }
            let leading = is_path_sep(&trees, i);
            let start = if leading { i + 2 } else { i };
            if let Some(TokenTree::Ident(first)) = trees.get(start) {
                let mut segs = vec![first.to_string()];
                let mut j = start + 1;
                while is_path_sep(&trees, j) {
                    match trees.get(j + 2) {
                        Some(TokenTree::Ident(next)) => {
                            segs.push(next.to_string());
                            j += 3;
                        }
                        _ => break,
                    }
                }
                if segs.len() > 1 || leading {
                    if leading {
                        segs.insert(0, "::".into());
                    }
                    self.paths.push(segs);
                }
                i = j.max(i + 1);
            } else {
                i += 1;
            }
        }
    }
}

fn is_path_sep(trees: &[TokenTree], i: usize) -> bool {
    matches!(
        (trees.get(i), trees.get(i + 1)),
        (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
            if a.as_char() == ':' && b.as_char() == ':' && a.spacing() == proc_macro2::Spacing::Joint
    )
}

impl<'ast> Visit<'ast> for PathCollector {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        let mut segs: PathRef = path.segments.iter().map(|s| s.ident.to_string()).collect();
        if path.leading_colon.is_some() {
            segs.insert(0, "::".into());
        }
        self.paths.push(segs);
        syn::visit::visit_path(self, path);
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let prefix = if item.leading_colon.is_some() {
            vec!["::".to_owned()]
        } else {
            Vec::new()
        };
        flatten_use(&item.tree, prefix, &mut self.uses);
    }

    fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
        // Child modules are summarized separately.
    }

    fn visit_vis_restricted(&mut self, _: &'ast syn::VisRestricted) {
        // `pub(crate)` / `pub(in path)` name a scope, not a dependency.
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.has_tests |= f.attrs.iter().any(is_test_attr);
        syn::visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        self.has_tests |= f.attrs.iter().any(is_test_attr);
        syn::visit::visit_impl_item_fn(self, f);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let name = mac
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        match name.as_str() {
            "macro_rules" => self.defines_macros = true,
            "proptest" | "quickcheck" => self.has_tests = true,
            "include" | "include_str" | "include_bytes" => {
                match syn::parse2::<syn::LitStr>(mac.tokens.clone()) {
                    Ok(lit) => self.includes.push(join(&self.dir, &lit.value())),
                    Err(_) => self.leaks.push(format!("{name}! with a computed path")),
                }
            }
            _ => {}
        }
        self.scan_tokens(mac.tokens.clone());
        syn::visit::visit_macro(self, mac);
    }
}

fn flatten_use(tree: &syn::UseTree, prefix: PathRef, out: &mut Vec<UseEntry>) {
    match tree {
        syn::UseTree::Path(p) => {
            let mut prefix = prefix;
            prefix.push(p.ident.to_string());
            flatten_use(&p.tree, prefix, out);
        }
        syn::UseTree::Name(n) => {
            let ident = n.ident.to_string();
            if ident == "self" {
                let name = prefix.last().cloned().unwrap_or_default();
                out.push(UseEntry::Name { path: prefix, name });
            } else {
                let mut path = prefix;
                path.push(ident.clone());
                out.push(UseEntry::Name { path, name: ident });
            }
        }
        syn::UseTree::Rename(r) => {
            let mut path = prefix;
            let ident = r.ident.to_string();
            if ident != "self" {
                path.push(ident);
            }
            out.push(UseEntry::Name {
                path,
                name: r.rename.to_string(),
            });
        }
        syn::UseTree::Glob(_) => out.push(UseEntry::Glob { path: prefix }),
        syn::UseTree::Group(g) => {
            for t in &g.items {
                flatten_use(t, prefix.clone(), out);
            }
        }
    }
}

fn item_attrs(item: &Item) -> Option<&Vec<Attribute>> {
    Some(match item {
        Item::Const(i) => &i.attrs,
        Item::Enum(i) => &i.attrs,
        Item::ExternCrate(i) => &i.attrs,
        Item::Fn(i) => &i.attrs,
        Item::ForeignMod(i) => &i.attrs,
        Item::Impl(i) => &i.attrs,
        Item::Macro(i) => &i.attrs,
        Item::Mod(i) => &i.attrs,
        Item::Static(i) => &i.attrs,
        Item::Struct(i) => &i.attrs,
        Item::Trait(i) => &i.attrs,
        Item::TraitAlias(i) => &i.attrs,
        Item::Type(i) => &i.attrs,
        Item::Union(i) => &i.attrs,
        Item::Use(i) => &i.attrs,
        _ => return None,
    })
}

/// `#[test]`, `#[tokio::test]`, `#[sqlx::test]`, `#[rstest]` and similar.
fn is_test_attr(attr: &Attribute) -> bool {
    attr.path().segments.last().is_some_and(|s| {
        matches!(
            s.ident.to_string().as_str(),
            "test" | "rstest" | "test_case"
        )
    })
}

/// True for `#[cfg(test)]` and `#[cfg(all(test, ...))]`; false for `not(test)` and `any(..)`.
pub(crate) fn has_test_cfg(attrs: &[Attribute]) -> bool {
    fn requires_test(meta: &Meta) -> bool {
        match meta {
            Meta::Path(p) => p.is_ident("test"),
            Meta::List(list) if list.path.is_ident("all") => list
                .parse_args_with(
                    syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|items| items.iter().any(requires_test)),
            _ => false,
        }
    }
    attrs.iter().any(|a| {
        a.path().is_ident("cfg") && a.parse_args::<Meta>().is_ok_and(|m| requires_test(&m))
    })
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn read_source(git: &Git, files: &TreeFiles, path: &str) -> Result<Option<String>, Error> {
    let Some(oid) = files.oid(path) else {
        return Ok(None);
    };
    let blob = git.blob(oid)?;
    Ok(String::from_utf8(blob.to_vec()).ok())
}

/// The directory part of a repository path ("" at the top level).
pub(crate) fn parent_dir(path: &str) -> String {
    path.rsplit_once('/')
        .map_or(String::new(), |(d, _)| d.to_owned())
}

/// Joins and normalizes `dir/rel`, resolving `.` and `..`.
pub(crate) fn join(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attrs(src: &str) -> Vec<Attribute> {
        syn::parse_str::<syn::ItemFn>(&format!("{src} fn f() {{}}"))
            .unwrap()
            .attrs
    }

    #[test]
    fn test_cfg_detection() {
        assert!(has_test_cfg(&attrs("#[cfg(test)]")));
        assert!(has_test_cfg(&attrs("#[cfg(all(test, feature = \"x\"))]")));
        assert!(!has_test_cfg(&attrs("#[cfg(not(test))]")));
        assert!(!has_test_cfg(&attrs("#[cfg(any(test, feature = \"x\"))]")));
        assert!(!has_test_cfg(&attrs("#[inline]")));
    }

    #[test]
    fn path_join() {
        assert_eq!(join("src", "a.rs"), "src/a.rs");
        assert_eq!(join("src/a", "../b.rs"), "src/b.rs");
        assert_eq!(join("", "./x.rs"), "x.rs");
    }

    #[test]
    fn use_flattening() {
        let item: syn::ItemUse =
            syn::parse_str("use crate::a::{b, c as d, e::*, self as top};").unwrap();
        let mut out = Vec::new();
        flatten_use(&item.tree, Vec::new(), &mut out);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            out,
            vec![
                UseEntry::Name {
                    path: s(&["crate", "a", "b"]),
                    name: "b".into()
                },
                UseEntry::Name {
                    path: s(&["crate", "a", "c"]),
                    name: "d".into()
                },
                UseEntry::Glob {
                    path: s(&["crate", "a", "e"])
                },
                UseEntry::Name {
                    path: s(&["crate", "a"]),
                    name: "top".into()
                },
            ]
        );
    }

    #[test]
    fn signature_drops_bodies_and_docs() {
        let mut item: Item = syn::parse_str("/// docs\npub fn f(x: u8) -> u8 { x + 1 }").unwrap();
        StripBodies.visit_item_mut(&mut item);
        let tokens = without_docs(item.to_token_stream()).to_string();
        assert_eq!(tokens, "pub fn f (x : u8) -> u8 { }");
    }

    #[test]
    fn visibility_is_not_a_dependency() {
        let mut c = PathCollector::new(String::new());
        let item: Item = syn::parse_str("pub(crate) fn f(x: crate::a::T) {}").unwrap();
        c.visit_item(&item);
        assert_eq!(
            c.paths,
            vec![vec!["crate".to_owned(), "a".into(), "T".into()]]
        );
    }

    #[test]
    fn macro_tokens_yield_paths() {
        let mut c = PathCollector::new(String::new());
        let mac: syn::Macro = syn::parse_str("assert_eq!(crate::tax::rate(), ::other::X)").unwrap();
        c.visit_macro(&mac);
        assert!(
            c.paths
                .contains(&vec!["crate".into(), "tax".into(), "rate".into()])
        );
        assert!(
            c.paths
                .contains(&vec!["::".into(), "other".into(), "X".into()])
        );
    }
}
