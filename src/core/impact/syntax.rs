//! Imports, identifiers and top-level definitions read from a Tree-sitter syntax tree, for the
//! languages it has a grammar for here.

use super::code::{self, Definition, Import, Kind};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use tree_sitter::{Language, Node, Parser, Tree};

/// How deep definitions are looked for inside namespaces and preprocessor blocks, and how
/// deep a Rust `use` tree is followed.
const MAX_NESTING: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grammar {
    JavaScript,
    TypeScript,
    Tsx,
    Python,
    C,
    Cpp,
    Rust,
}

impl Grammar {
    pub fn of(path: &str) -> Option<Grammar> {
        Some(
            match crate::core::text::extension(std::path::Path::new(path)).as_str() {
                "js" | "jsx" | "mjs" | "cjs" => Grammar::JavaScript,
                "ts" | "mts" | "cts" => Grammar::TypeScript,
                "tsx" => Grammar::Tsx,
                "py" | "pyi" => Grammar::Python,
                "c" | "h" => Grammar::C,
                "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Grammar::Cpp,
                "rs" => Grammar::Rust,
                _ => return None,
            },
        )
    }

    fn language(self) -> Language {
        match self {
            Grammar::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Grammar::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::C => tree_sitter_c::LANGUAGE.into(),
            Grammar::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    fn script(self) -> bool {
        matches!(
            self,
            Grammar::JavaScript | Grammar::TypeScript | Grammar::Tsx
        )
    }
}

pub struct Syntax {
    pub imports: Vec<Import>,
    /// Each identifier with the line it is first used on outside import statements.
    pub uses: HashMap<String, usize>,
    pub definitions: BTreeMap<String, Definition>,
    /// The tree has syntax errors, around which imports and definitions may be missed.
    pub errors: bool,
}

fn parse(grammar: Grammar, source: &str, cancel: &AtomicBool) -> anyhow::Result<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&grammar.language())?;
    let bytes = source.as_bytes();
    let mut stop = |_: &tree_sitter::ParseState| {
        if cancel.load(Ordering::Relaxed) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |at, _| bytes.get(at..).unwrap_or_default(),
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut stop)),
    );
    crate::core::analysis::check_cancel(cancel)?;
    tree.ok_or_else(|| anyhow::anyhow!("The file could not be parsed"))
}

/// Reads `source`, a file in `grammar`.
pub fn read(grammar: Grammar, source: &str, cancel: &AtomicBool) -> anyhow::Result<Syntax> {
    let tree = parse(grammar, source, cancel)?;
    let root = tree.root_node();
    let text = |node: Node| node.utf8_text(source.as_bytes()).unwrap_or_default();
    let line = |node: Node| node.start_position().row + 1;
    let mut imports = Vec::new();
    let mut uses = HashMap::new();
    let mut cursor = root.walk();
    let mut steps = 0usize;
    // A depth-first walk that does not descend into import statements.
    'walk: loop {
        let node = cursor.node();
        steps += 1;
        if steps.is_multiple_of(4096) {
            crate::core::analysis::check_cancel(cancel)?;
        }
        let found = imports_at(grammar, node, &text, line(node));
        let skip = found.is_some();
        imports.extend(found.into_iter().flatten());
        if !skip
            && node.child_count() == 0
            && node.is_named()
            && node.kind().ends_with("identifier")
        {
            uses.entry(text(node).to_owned()).or_insert(line(node));
        }
        if !skip && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    let definitions = definitions(grammar, root, source);
    Ok(Syntax {
        imports,
        uses,
        definitions,
        errors: root.has_error(),
    })
}

fn string_value<'a>(node: Node, text: &impl Fn(Node) -> &'a str) -> Option<String> {
    match node.kind() {
        "string" | "string_literal" | "template_string" => {
            let mut cursor = node.walk();
            let mut parts = node.named_children(&mut cursor);
            let mut value = String::new();
            for part in &mut parts {
                match part.kind() {
                    "string_fragment" | "string_content" => value.push_str(text(part)),
                    _ => return None,
                }
            }
            Some(value)
        }
        _ => None,
    }
}

/// The imports of `node` that may name a file here, if it is an import statement.
fn imports_at<'a>(
    grammar: Grammar,
    node: Node,
    text: &impl Fn(Node) -> &'a str,
    line: usize,
) -> Option<Vec<Import>> {
    let import = |spec: String, names: Vec<(String, String)>| Import {
        spec,
        line,
        names,
        tokens: 0..0,
    };
    let field_text = |node: Node, field: &str| node.child_by_field_name(field).map(text);
    match (grammar, node.kind()) {
        (g, "import_statement" | "export_statement") if g.script() => {
            let source = node.child_by_field_name("source")?;
            let Some(spec) = string_value(source, text).filter(|s| code::relative(s)) else {
                return Some(Vec::new());
            };
            let mut names = Vec::new();
            let mut cursor = node.walk();
            let mut stack: Vec<Node> = node.named_children(&mut cursor).collect();
            while let Some(n) = stack.pop() {
                match n.kind() {
                    "import_specifier" | "export_specifier" => {
                        if let Some(name) = field_text(n, "name") {
                            let local = field_text(n, "alias").unwrap_or(name);
                            names.push((name.to_owned(), local.to_owned()));
                        }
                    }
                    "import_clause" | "named_imports" | "export_clause" => {
                        let mut c = n.walk();
                        stack.extend(n.named_children(&mut c));
                    }
                    _ => {}
                }
            }
            names.reverse();
            Some(vec![import(spec, names)])
        }
        (g, "call_expression") if g.script() => {
            let function = node.child_by_field_name("function")?;
            let dynamic = function.kind() == "import";
            if !(dynamic || function.kind() == "identifier" && text(function) == "require") {
                return None;
            }
            let spec = node
                .child_by_field_name("arguments")
                .and_then(|a| a.named_child(0))
                .and_then(|s| string_value(s, text))
                .filter(|s| code::relative(s))?;
            let mut names = Vec::new();
            // const { a, b: c } = require("./a")
            let pattern = node
                .parent()
                .filter(|p| p.kind() == "variable_declarator" && !dynamic)
                .and_then(|p| p.child_by_field_name("name"))
                .filter(|n| n.kind() == "object_pattern");
            if let Some(pattern) = pattern {
                let mut cursor = pattern.walk();
                for item in pattern.named_children(&mut cursor) {
                    match item.kind() {
                        "shorthand_property_identifier_pattern" => {
                            names.push((text(item).to_owned(), text(item).to_owned()))
                        }
                        "pair_pattern" => {
                            if let (Some(key), Some(value)) =
                                (field_text(item, "key"), item.child_by_field_name("value"))
                            {
                                if value.kind() == "identifier" {
                                    names.push((key.to_owned(), text(value).to_owned()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some(vec![import(spec, names)])
        }
        (Grammar::Python, "import_statement") => {
            let mut cursor = node.walk();
            let modules = node
                .children_by_field_name("name", &mut cursor)
                .filter_map(|n| match n.kind() {
                    "dotted_name" => Some(text(n)),
                    "aliased_import" => field_text(n, "name"),
                    _ => None,
                })
                .map(|module| import(module.replace(char::is_whitespace, ""), Vec::new()))
                .collect();
            Some(modules)
        }
        (Grammar::Python, "import_from_statement") => {
            let module =
                text(node.child_by_field_name("module_name")?).replace(char::is_whitespace, "");
            let mut cursor = node.walk();
            let names: Vec<(String, String)> = node
                .children_by_field_name("name", &mut cursor)
                .filter_map(|n| match n.kind() {
                    "dotted_name" => Some((text(n).to_owned(), text(n).to_owned())),
                    "aliased_import" => Some((
                        field_text(n, "name")?.to_owned(),
                        field_text(n, "alias")?.to_owned(),
                    )),
                    _ => None,
                })
                .collect();
            Some(if module.chars().all(|c| c == '.') {
                // from . import a: each name is a module
                names
                    .into_iter()
                    .map(|(name, _)| import(format!("{module}{name}"), Vec::new()))
                    .collect()
            } else {
                vec![import(module, names)]
            })
        }
        (Grammar::Rust, "mod_item") if node.child_by_field_name("body").is_none() => {
            Some(vec![import(
                field_text(node, "name")?.to_owned(),
                Vec::new(),
            )])
        }
        (Grammar::Rust, "use_declaration") => {
            let mut paths = Vec::new();
            if let Some(argument) = node.child_by_field_name("argument") {
                use_tree(argument, &[], text, &mut paths, 0);
            }
            Some(
                paths
                    .into_iter()
                    .filter_map(|(path, local)| code::rust_use(path, local, line))
                    .collect(),
            )
        }
        (Grammar::C | Grammar::Cpp, "preproc_include") => {
            let spec = node
                .child_by_field_name("path")
                .and_then(|p| string_value(p, text));
            Some(spec.map(|s| import(s, Vec::new())).into_iter().collect())
        }
        _ => None,
    }
}

/// The paths of a Rust `use` tree below `prefix`, each with the name it is used by (`*` for a
/// glob).
fn use_tree<'a>(
    node: Node,
    prefix: &[String],
    text: &impl Fn(Node) -> &'a str,
    out: &mut Vec<(Vec<String>, String)>,
    depth: usize,
) {
    if depth > MAX_NESTING {
        return;
    }
    let segments = |n: Option<Node>| -> Vec<String> {
        let mut path = prefix.to_vec();
        if let Some(n) = n {
            path.extend(
                text(n)
                    .replace(char::is_whitespace, "")
                    .split("::")
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        }
        path
    };
    match node.kind() {
        "scoped_use_list" => {
            let path = segments(node.child_by_field_name("path"));
            if let Some(list) = node.child_by_field_name("list") {
                use_tree(list, &path, text, out, depth + 1);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                use_tree(child, prefix, text, out, depth + 1);
            }
        }
        "use_as_clause" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                out.push((
                    segments(node.child_by_field_name("path")),
                    text(alias).to_owned(),
                ));
            }
        }
        "use_wildcard" => out.push((segments(node.named_child(0)), "*".into())),
        "scoped_identifier" | "identifier" | "self" | "crate" | "super" => {
            let path = segments(Some(node));
            if let Some(last) = path.last().cloned() {
                out.push((path, last));
            }
        }
        _ => {}
    }
}

/// A hash of the leaves of `node`, leaving out comments.
fn print(node: Node, source: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut cursor = node.walk();
    'walk: loop {
        let n = cursor.node();
        let comment = n.kind().contains("comment");
        if !comment && n.child_count() == 0 {
            n.kind().hash(&mut hasher);
            n.utf8_text(source.as_bytes())
                .unwrap_or_default()
                .hash(&mut hasher);
        }
        if !comment && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.node() == node {
                break 'walk;
            }
            if cursor.goto_next_sibling() {
                continue 'walk;
            }
            if !cursor.goto_parent() {
                break 'walk;
            }
        }
    }
    hasher.finish()
}

/// The name a C/C++ declarator declares, and whether it declares a function.
fn declared<'a>(mut node: Node<'a>, text: &impl Fn(Node) -> &'a str) -> Option<(&'a str, bool)> {
    let mut function = false;
    for _ in 0..MAX_NESTING {
        match node.kind() {
            "identifier" | "type_identifier" => return Some((text(node), function)),
            "function_declarator" => function = true,
            "pointer_declarator"
            | "reference_declarator"
            | "array_declarator"
            | "init_declarator"
            | "attributed_declarator"
            | "parenthesized_declarator" => {}
            _ => return None,
        }
        node = node
            .child_by_field_name("declarator")
            .or_else(|| node.named_child(0))?;
    }
    None
}

/// The top-level definitions of the tree from `root`, by name.
fn definitions(grammar: Grammar, root: Node, source: &str) -> BTreeMap<String, Definition> {
    let text = |node: Node| node.utf8_text(source.as_bytes()).unwrap_or_default();
    let mut out = BTreeMap::new();
    let mut add = |name: &str, kind: Kind, node: Node| {
        if !name.is_empty() {
            out.entry(name.to_owned()).or_insert(Definition {
                kind,
                print: print(node, source),
                line: node.start_position().row + 1,
            });
        }
    };
    let name_of = |node: Node| node.child_by_field_name("name").map(text);
    // Nodes holding top-level items, with how deep they are.
    let mut stack = vec![(root, 0)];
    while let Some((holder, depth)) = stack.pop() {
        let mut cursor = holder.walk();
        let items: Vec<Node> = holder.named_children(&mut cursor).collect();
        for item in items {
            // Where the definition is, and the statement it is printed by.
            let (node, whole) = match item.kind() {
                "export_statement" => match item.child_by_field_name("declaration") {
                    Some(d) => (d, item),
                    None => continue,
                },
                _ => (item, item),
            };
            match (grammar, node.kind()) {
                (g, kind) if g.script() => match kind {
                    "function_declaration"
                    | "generator_function_declaration"
                    | "function_signature" => {
                        if let Some(name) = name_of(node) {
                            add(name, Kind::Function, whole);
                        }
                    }
                    "class_declaration"
                    | "abstract_class_declaration"
                    | "interface_declaration"
                    | "type_alias_declaration"
                    | "enum_declaration" => {
                        if let Some(name) = name_of(node) {
                            add(name, Kind::Type, whole);
                        }
                    }
                    "lexical_declaration" | "variable_declaration" => {
                        let mut c = node.walk();
                        for declarator in node.named_children(&mut c) {
                            let Some(name) = declarator
                                .child_by_field_name("name")
                                .filter(|n| n.kind() == "identifier")
                            else {
                                continue;
                            };
                            let kind =
                                match declarator.child_by_field_name("value").map(|v| v.kind()) {
                                    Some(
                                        "arrow_function"
                                        | "function_expression"
                                        | "function"
                                        | "generator_function",
                                    ) => Kind::Function,
                                    Some("class") => Kind::Type,
                                    _ => Kind::Value,
                                };
                            add(text(name), kind, declarator);
                        }
                    }
                    "ambient_declaration" if depth < MAX_NESTING => stack.push((node, depth + 1)),
                    _ => {}
                },
                (Grammar::Python, kind) => {
                    let node = if kind == "decorated_definition" {
                        match node.child_by_field_name("definition") {
                            Some(d) => d,
                            None => continue,
                        }
                    } else {
                        node
                    };
                    match node.kind() {
                        "function_definition" => {
                            if let Some(name) = name_of(node) {
                                add(name, Kind::Function, whole);
                            }
                        }
                        "class_definition" => {
                            if let Some(name) = name_of(node) {
                                add(name, Kind::Type, whole);
                            }
                        }
                        "expression_statement" => {
                            let assignment =
                                node.named_child(0).filter(|a| a.kind() == "assignment");
                            if let Some(left) = assignment
                                .and_then(|a| a.child_by_field_name("left"))
                                .filter(|l| l.kind() == "identifier")
                            {
                                add(text(left), Kind::Value, whole);
                            }
                        }
                        _ => {}
                    }
                }
                (Grammar::Rust, kind) => {
                    let kind = match kind {
                        "function_item" | "macro_definition" => Kind::Function,
                        "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => {
                            Kind::Type
                        }
                        "const_item" | "static_item" => Kind::Value,
                        _ => continue,
                    };
                    if let Some(name) = name_of(node) {
                        add(name, kind, whole);
                    }
                }
                (Grammar::C | Grammar::Cpp, kind) => match kind {
                    "preproc_def" => {
                        if let Some(name) = name_of(node) {
                            add(name, Kind::Value, whole);
                        }
                    }
                    "preproc_function_def" => {
                        if let Some(name) = name_of(node) {
                            add(name, Kind::Function, whole);
                        }
                    }
                    "function_definition" => {
                        if let Some((name, _)) = node
                            .child_by_field_name("declarator")
                            .and_then(|d| declared(d, &text))
                        {
                            add(name, Kind::Function, whole);
                        }
                    }
                    "declaration" => {
                        let mut c = node.walk();
                        let declarators: Vec<Node> =
                            node.children_by_field_name("declarator", &mut c).collect();
                        for d in declarators {
                            if let Some((name, true)) = declared(d, &text) {
                                add(name, Kind::Function, whole);
                            }
                        }
                    }
                    "type_definition" => {
                        let mut c = node.walk();
                        let declarators: Vec<Node> =
                            node.children_by_field_name("declarator", &mut c).collect();
                        for d in declarators {
                            if let Some((name, _)) = declared(d, &text) {
                                add(name, Kind::Type, whole);
                            }
                        }
                    }
                    "struct_specifier" | "union_specifier" | "enum_specifier"
                    | "class_specifier" => {
                        if let (Some(name), Some(_)) =
                            (name_of(node), node.child_by_field_name("body"))
                        {
                            add(name, Kind::Type, whole);
                        }
                    }
                    "alias_declaration" => {
                        if let Some(name) = name_of(node) {
                            add(name, Kind::Type, whole);
                        }
                    }
                    "template_declaration"
                    | "preproc_ifdef"
                    | "preproc_if"
                    | "preproc_else"
                    | "preproc_elif"
                    | "preproc_elifdef"
                        if depth < MAX_NESTING =>
                    {
                        stack.push((node, depth + 1))
                    }
                    "namespace_definition" | "linkage_specification" if depth < MAX_NESTING => {
                        if let Some(body) = node.child_by_field_name("body") {
                            match body.kind() {
                                "declaration_list" => stack.push((body, depth + 1)),
                                _ => stack.push((node, depth + 1)),
                            }
                        }
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    type Names = Vec<(String, String)>;

    fn imports_of(path: &str, source: &str) -> Vec<(String, usize, Names)> {
        read(Grammar::of(path).unwrap(), source, &AtomicBool::new(false))
            .unwrap()
            .imports
            .into_iter()
            .map(|i| (i.spec, i.line, i.names))
            .collect()
    }
    fn names(pairs: &[(&str, &str)]) -> Names {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }
    fn defs(path: &str, source: &str) -> Vec<(String, Kind)> {
        read(Grammar::of(path).unwrap(), source, &AtomicBool::new(false))
            .unwrap()
            .definitions
            .into_iter()
            .map(|(name, d)| (name, d.kind))
            .collect()
    }

    #[test]
    fn script_imports_span_lines_and_skip_comments_and_strings() {
        let source = r#"
// import { no } from "./commented";
/* require("./also-commented") */
const text = "import { x } from './in-a-string'";
import {
  parse as p,
  type Options,
  run, // trailing comment
} from "./parser";
import Default, * as ns from '../lib/all';
export { a, b as c } from "./re";
export * from "./star";
import "./side-effect";
const { load, save: store } = require("./io");
async function f() { const later = await import(`./dynamic`); }
import x from "react";
foo.import("./not-an-import");
if (debug) { require("./nested"); }
"#;
        assert_eq!(
            imports_of("a.ts", source),
            [
                (
                    "./parser".into(),
                    5,
                    names(&[("parse", "p"), ("Options", "Options"), ("run", "run")])
                ),
                ("../lib/all".into(), 10, vec![]),
                ("./re".into(), 11, names(&[("a", "a"), ("b", "c")])),
                ("./star".into(), 12, vec![]),
                ("./side-effect".into(), 13, vec![]),
                (
                    "./io".into(),
                    14,
                    names(&[("load", "load"), ("save", "store")])
                ),
                ("./dynamic".into(), 15, vec![]),
                ("./nested".into(), 18, vec![]),
            ]
        );
        let jsx = "import { Button } from './button';\nexport const App = () => <Button label=\"import './no'\" />;\n";
        assert_eq!(
            imports_of("app.jsx", jsx),
            [("./button".into(), 1, names(&[("Button", "Button")]))]
        );
    }

    #[test]
    fn python_imports_follow_parentheses_and_continuations() {
        let source = "# from .commented import x\n\"\"\"\nfrom .docstring import y\n\"\"\"\nfrom .helpers import (\n    f,\n    g as h,  # why\n)\nfrom . import a, b as c\nimport os.path, pkg.mod as m\nfrom .x import \\\n    z\nif True:\n    from ..up import *\nimport q; from .r import s\ndef late():\n    from .lazy import t\n";
        assert_eq!(
            imports_of("m.py", source),
            [
                (".helpers".into(), 5, names(&[("f", "f"), ("g", "h")])),
                (".a".into(), 9, vec![]),
                (".b".into(), 9, vec![]),
                ("os.path".into(), 10, vec![]),
                ("pkg.mod".into(), 10, vec![]),
                (".x".into(), 11, names(&[("z", "z")])),
                ("..up".into(), 14, vec![]),
                ("q".into(), 15, vec![]),
                (".r".into(), 15, names(&[("s", "s")])),
                (".lazy".into(), 17, names(&[("t", "t")])),
            ]
        );
    }

    #[test]
    fn rust_and_c_imports() {
        let rust = "// mod commented;\nmod parser;\npub(crate) mod util;\nmod inline { }\nuse crate::parser::{self, parse, Parser as P, sub::{x, y as z}};\nuse super::sibling::*;\nuse std::io;\nlet s = r#\"mod fake;\"#;\nuse self::util::helper;\nfn f() { use crate::inner::Thing; }\n";
        assert_eq!(
            imports_of("src/lib.rs", rust),
            [
                ("parser".into(), 2, vec![]),
                ("util".into(), 3, vec![]),
                ("use:crate::parser".into(), 5, vec![]),
                (
                    "use:crate::parser::parse".into(),
                    5,
                    names(&[("parse", "parse")])
                ),
                (
                    "use:crate::parser::Parser".into(),
                    5,
                    names(&[("Parser", "P")])
                ),
                ("use:crate::parser::sub::x".into(), 5, names(&[("x", "x")])),
                ("use:crate::parser::sub::y".into(), 5, names(&[("y", "z")])),
                ("use:super::sibling".into(), 6, vec![]),
                (
                    "use:self::util::helper".into(),
                    9,
                    names(&[("helper", "helper")])
                ),
                (
                    "use:crate::inner::Thing".into(),
                    10,
                    names(&[("Thing", "Thing")])
                ),
            ]
        );
        let c = "// #include \"no.h\"\n#include \"x.h\"\n#include <stdio.h>\n#ifdef WIN32\n  #  include \"y.h\"\n#endif\nconst char *s = \"#include \\\"z.h\\\"\";\n";
        assert_eq!(
            imports_of("a.c", c),
            [("x.h".into(), 2, vec![]), ("y.h".into(), 5, vec![])]
        );
    }

    #[test]
    fn uses_leave_out_imports_comments_and_strings() {
        let source = "import { parse } from './parser';\n// parse\nconst s = 'parse';\nconst out = parse(s);\n";
        let syntax = read(Grammar::JavaScript, source, &AtomicBool::new(false)).unwrap();
        assert_eq!(syntax.uses.get("parse"), Some(&4));
        assert_eq!(syntax.uses.get("out"), Some(&4));
        assert!(!syntax.errors);
        assert!(
            read(Grammar::Rust, "fn main( {", &AtomicBool::new(false))
                .unwrap()
                .errors
        );
    }

    #[test]
    fn top_level_definitions_of_each_language() {
        let script = "export function parse(a) { return { a }; }\nexport const run = (x) => x\nconst LIMIT = 3, helper = function () {};\nexport class Parser { method() { const inner = 1; } }\nexport type Options = { a: 1 } | { b: 2 };\ninterface Shape { }\nfunction* gen() {}\nexport default class Main {}\nenum E { A }\ndeclare function ambient(): void;\n";
        assert_eq!(
            defs("a.ts", script),
            [
                ("E".into(), Kind::Type),
                ("LIMIT".into(), Kind::Value),
                ("Main".into(), Kind::Type),
                ("Options".into(), Kind::Type),
                ("Parser".into(), Kind::Type),
                ("Shape".into(), Kind::Type),
                ("ambient".into(), Kind::Function),
                ("gen".into(), Kind::Function),
                ("helper".into(), Kind::Function),
                ("parse".into(), Kind::Function),
                ("run".into(), Kind::Function),
            ]
        );
        let rust = "pub fn parse(input: &str) -> Result<()> { let x = 1; Ok(()) }\npub struct Parser<'a> { s: &'a str }\nstruct Unit;\npub const fn limit() -> usize { 3 }\nstatic mut COUNT: usize = 0;\nimpl Parser<'_> { fn method(&self) {} }\nmacro_rules! m { () => {} }\npub(crate) enum E { A }\nmod inner { pub fn hidden() {} }\n";
        assert_eq!(
            defs("a.rs", rust),
            [
                ("COUNT".into(), Kind::Value),
                ("E".into(), Kind::Type),
                ("Parser".into(), Kind::Type),
                ("Unit".into(), Kind::Type),
                ("limit".into(), Kind::Function),
                ("m".into(), Kind::Function),
                ("parse".into(), Kind::Function),
            ]
        );
        let c = "#ifndef A_H\n#define A_H\n#define LIMIT 3\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\nint x(void);\nstatic int *helper(int a) { if (a) { return 0; } return 0; }\nstruct point { int x; };\nstruct fwd;\ntypedef unsigned long size;\ntypedef int (*callback)(int);\nint global = 1;\n#endif\n";
        assert_eq!(
            defs("a.h", c),
            [
                ("A_H".into(), Kind::Value),
                ("LIMIT".into(), Kind::Value),
                ("MAX".into(), Kind::Function),
                ("callback".into(), Kind::Type),
                ("helper".into(), Kind::Function),
                ("point".into(), Kind::Type),
                ("size".into(), Kind::Type),
                ("x".into(), Kind::Function),
            ]
        );
        let cpp = "namespace ns { int inner(); class Widget { void m(); }; }\ntemplate <typename T> T id(T t) { return t; }\nint Widget::m2() { return 1; }\nusing Alias = int;\nextern \"C\" { void c_api(); }\n";
        assert_eq!(
            defs("a.hpp", cpp),
            [
                ("Alias".into(), Kind::Type),
                ("Widget".into(), Kind::Type),
                ("c_api".into(), Kind::Function),
                ("id".into(), Kind::Function),
                ("inner".into(), Kind::Function),
            ]
        );
        let python = "import os\nLIMIT = 3\n\n@decorator\ndef parse(text):\n    def inner():\n        pass\n    return text\n\nclass Parser(Base):\n    x = 1\n\nasync def fetch():\n    pass\nif LIMIT == 3:\n    pass\n";
        assert_eq!(
            defs("a.py", python),
            [
                ("LIMIT".into(), Kind::Value),
                ("Parser".into(), Kind::Type),
                ("fetch".into(), Kind::Function),
                ("parse".into(), Kind::Function),
            ]
        );
    }

    #[test]
    fn changed_definitions_ignore_comments_and_spacing() {
        let before = "fn a() { 1 }\n// note\nfn b() { 2 }\nfn c() {}\n/// doc\nfn d() {}\n";
        let after = "fn a() {\n    1 // same\n}\nfn b() { 3 }\n/// other doc\nfn d() {}\n";
        let of = |s| {
            read(Grammar::Rust, s, &AtomicBool::new(false))
                .unwrap()
                .definitions
        };
        assert_eq!(
            code::changed(&of(before), &of(after))
                .into_iter()
                .collect::<Vec<_>>(),
            [
                ("b".into(), (Kind::Function, false)),
                ("c".into(), (Kind::Function, true)),
            ]
        );
    }

    #[test]
    fn deep_nesting_and_cancelling_are_bounded() {
        let deep = format!("fn f() {{ {} }}\n", "(".repeat(50_000));
        assert!(read(Grammar::Rust, &deep, &AtomicBool::new(false)).is_ok());
        let nested = format!("use crate::{}x{};\n", "a::{".repeat(500), "}".repeat(500));
        assert!(read(Grammar::Rust, &nested, &AtomicBool::new(false)).is_ok());
        assert!(read(Grammar::Rust, "fn f() {}", &AtomicBool::new(true)).is_err());
    }
}
