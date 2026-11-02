//! A small tokenizer for the source files there is no syntax tree for (script in Vue and Svelte
//! files, Objective-C, style sheets and JSON), and the imports and top-level definitions read
//! from its tokens. Comments are skipped and strings kept whole, so neither is mistaken for code.
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// The languages read from tokens.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Lang {
    Script,
    /// C and Objective-C.
    C,
    Css,
    Json,
}

/// Tokens read at most from one file.
const MAX_TOKENS: usize = 2_000_000;
/// Tokens an import statement may span.
const MAX_STATEMENT: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Tok {
    Ident(String),
    Str(String),
    Punct(char),
    Number(String),
}
#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    /// The first token of its line.
    pub first: bool,
    /// Brackets open around it; a bracket itself counts as outside.
    pub depth: usize,
}
impl Token {
    fn ident(&self) -> Option<&str> {
        match &self.tok {
            Tok::Ident(s) => Some(s),
            _ => None,
        }
    }
    fn is(&self, c: char) -> bool {
        self.tok == Tok::Punct(c)
    }
    fn is_word(&self, word: &str) -> bool {
        self.ident() == Some(word)
    }
}

/// The tokens of `source`; whether there were more than could be read.
pub fn tokens(lang: Lang, source: &str) -> (Vec<Token>, bool) {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line, mut depth) = (0, 1, 0usize);
    let mut fresh = true;
    let slash_comments = matches!(lang, Lang::Script | Lang::C | Lang::Css);
    let at = |i: usize| source[i..].chars().next().unwrap_or('\0');
    // Skips to the end of the line, leaving the newline.
    let line_end = |i: usize| {
        bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |n| i + n)
    };
    while i < bytes.len() {
        if out.len() >= MAX_TOKENS {
            return (out, true);
        }
        let c = at(i);
        let start_line = line;
        match c {
            '\n' => {
                line += 1;
                fresh = true;
                i += 1;
                continue;
            }
            ' ' | '\t' | '\r' | '\u{c}' => {
                i += 1;
                continue;
            }
            '/' if slash_comments && bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                    if bytes[i] == b'\n' {
                        line += 1;
                        fresh = true;
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                continue;
            }
            // In style sheets, `//` comments only where `url(http://...)` cannot be.
            '/' if slash_comments
                && bytes.get(i + 1) == Some(&b'/')
                && (lang != Lang::Css || i == 0 || bytes[i - 1].is_ascii_whitespace()) =>
            {
                i = line_end(i);
                continue;
            }
            _ => {}
        }
        let first = std::mem::replace(&mut fresh, false);
        let token = |tok: Tok, line: usize, depth: usize| Token {
            tok,
            line,
            first,
            depth,
        };
        let quote = match c {
            '"' => true,
            '\'' => lang != Lang::Json,
            '`' => lang == Lang::Script,
            _ => false,
        };
        if quote {
            let close = [bytes[i]];
            let body = i + 1;
            let mut j = body;
            let multiline = c == '`';
            while j < bytes.len() && !bytes[j..].starts_with(&close) {
                match bytes[j] {
                    b'\\' => {
                        if bytes.get(j + 1) == Some(&b'\n') {
                            line += 1;
                        }
                        j += 1;
                    }
                    b'\n' if !multiline => break,
                    b'\n' => line += 1,
                    _ => {}
                }
                j += 1;
            }
            let end = j.min(bytes.len());
            let text = source.get(body..end).unwrap_or_default().to_owned();
            out.push(token(Tok::Str(text), start_line, depth));
            i = if bytes[end..].starts_with(&close) {
                end + close.len()
            } else {
                end
            };
            continue;
        }
        if c.is_alphabetic() || c == '_' || (c == '$' && lang == Lang::Script) {
            let mut j = i;
            while j < bytes.len() {
                let d = at(j);
                if d.is_alphanumeric() || d == '_' || (d == '$' && lang == Lang::Script) {
                    j += d.len_utf8();
                } else {
                    break;
                }
            }
            let word = &source[i..j];
            out.push(token(Tok::Ident(word.to_owned()), line, depth));
            i = j;
            continue;
        }
        if c.is_ascii_digit() {
            let mut j = i;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || matches!(bytes[j], b'.' | b'_'))
            {
                j += 1;
            }
            out.push(token(Tok::Number(source[i..j].to_owned()), line, depth));
            i = j;
            continue;
        }
        if matches!(c, ')' | ']' | '}') {
            depth = depth.saturating_sub(1);
        }
        out.push(token(Tok::Punct(c), line, depth));
        if matches!(c, '(' | '[' | '{') {
            depth += 1;
        }
        i += c.len_utf8();
    }
    (out, false)
}

/// What an import statement names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Import {
    /// As written: a relative path, a Python module, a Rust module name, or `use:` and a Rust
    /// path such as `use:crate::parser::parse`.
    pub spec: String,
    pub line: usize,
    /// The names it brings in by name, each with the name it is used by.
    pub names: Vec<(String, String)>,
    /// The tokens of the statement, when read from tokens.
    pub tokens: Range<usize>,
}

/// The identifiers of `tokens` with the line each is first used on outside import statements.
pub fn uses(tokens: &[Token], imports: &[Import]) -> HashMap<String, usize> {
    let mut skip = vec![false; tokens.len()];
    for import in imports {
        let end = import.tokens.end.min(tokens.len());
        for flag in &mut skip[import.tokens.start.min(end)..end] {
            *flag = true;
        }
    }
    let mut out = HashMap::new();
    for (token, skipped) in tokens.iter().zip(skip) {
        if let (Some(name), false) = (token.ident(), skipped) {
            out.entry(name.to_owned()).or_insert(token.line);
        }
    }
    out
}

/// Names in braces from `tokens[at]`, an opening brace: `a`, `a as b` (`a: b` with `colon`),
/// and `type a`. Returns them and the index after the closing brace.
fn braced_names(tokens: &[Token], at: usize, colon: bool) -> (Vec<(String, String)>, usize) {
    let mut names = Vec::new();
    let mut i = at + 1;
    let end = (at + MAX_STATEMENT).min(tokens.len());
    let mut item: Vec<&str> = Vec::new();
    let mut flush = |item: &mut Vec<&str>| {
        let words: Vec<&str> = item.drain(..).filter(|w| *w != "type").collect();
        match words.as_slice() {
            [name] => names.push((name.to_string(), name.to_string())),
            [name, "as", local] if !colon => names.push((name.to_string(), local.to_string())),
            [name, ":", local] if colon => names.push((name.to_string(), local.to_string())),
            _ => {}
        }
    };
    while i < end {
        let t = &tokens[i];
        if t.is('}') {
            flush(&mut item);
            return (names, i + 1);
        }
        if t.is(',') {
            flush(&mut item);
        } else if let Some(word) = t.ident() {
            item.push(word);
        } else if t.is(':') && colon {
            item.push(":");
        } else if let Tok::Str(s) = &t.tok {
            // import { "a-b" as c }
            item.push(s);
        } else {
            item.push("?");
        }
        i += 1;
    }
    (names, i)
}

pub(super) fn relative(spec: &str) -> bool {
    spec.starts_with("./") || spec.starts_with("../")
}

fn script_imports(tokens: &[Token]) -> Vec<Import> {
    let mut out = Vec::new();
    let str_at = |i: usize| match tokens.get(i).map(|t| &t.tok) {
        Some(Tok::Str(s)) => Some(s.clone()),
        _ => None,
    };
    for (i, t) in tokens.iter().enumerate() {
        let after_dot = i > 0 && tokens[i - 1].is('.');
        if after_dot {
            continue;
        }
        if t.is_word("require") || t.is_word("import") {
            // require("./a"), import("./a")
            if tokens.get(i + 1).is_some_and(|t| t.is('(')) {
                if let Some(spec) = str_at(i + 2) {
                    let mut names = Vec::new();
                    let mut start = i;
                    // const { a, b: c } = require("./a")
                    if i >= 2 && tokens[i - 1].is('=') && tokens[i - 2].is('}') {
                        if let Some(open) = (0..i - 2)
                            .rev()
                            .take(MAX_STATEMENT)
                            .find(|&k| tokens[k].is('{') && tokens[k].depth == tokens[i - 2].depth)
                        {
                            names = braced_names(tokens, open, true).0;
                            start = open;
                        }
                    }
                    let mut import_end = i + 3;
                    if tokens.get(import_end).is_some_and(|t| t.is(')')) {
                        import_end += 1;
                    }
                    if relative(&spec) {
                        out.push(Import {
                            spec,
                            line: t.line,
                            names,
                            tokens: start..import_end,
                        });
                    }
                }
                continue;
            }
        }
        let export = t.is_word("export")
            && tokens
                .get(i + 1)
                .is_some_and(|n| n.is('{') || n.is('*') || n.is_word("type"));
        if !(t.is_word("import") || export) {
            continue;
        }
        let mut push = |spec: String, names, end: usize| {
            if relative(&spec) {
                out.push(Import {
                    spec,
                    line: t.line,
                    names,
                    tokens: i..end,
                });
            }
        };
        // import "./a"
        if let Some(spec) = str_at(i + 1) {
            push(spec, Vec::new(), i + 2);
            continue;
        }
        let mut names = Vec::new();
        let mut j = i + 1;
        let end = (i + MAX_STATEMENT).min(tokens.len());
        while j < end {
            let n = &tokens[j];
            if n.is('{') {
                let (found, next) = braced_names(tokens, j, false);
                names = found;
                j = next;
                continue;
            }
            if n.is(';') || n.is_word("import") || n.is_word("export") {
                break;
            }
            if n.is_word("from") {
                if let Some(spec) = str_at(j + 1) {
                    push(spec, names, j + 2);
                }
                break;
            }
            j += 1;
        }
    }
    out
}

/// The import of a Rust `use` path inside the crate, with the name it is used by (`*` for a
/// glob).
pub(super) fn rust_use(mut path: Vec<String>, local: String, line: usize) -> Option<Import> {
    if !matches!(
        path.first().map(String::as_str),
        Some("crate" | "super" | "self")
    ) {
        return None;
    }
    let last = path.last().cloned().unwrap_or_default();
    let names = if local == "*" {
        Vec::new()
    } else if last == "self" && path.len() > 1 {
        path.pop();
        Vec::new()
    } else {
        vec![(last, local)]
    };
    Some(Import {
        spec: format!("use:{}", path.join("::")),
        line,
        names,
        tokens: 0..0,
    })
}

/// The imports in `tokens` of a file in `lang`.
pub fn imports(lang: Lang, tokens: &[Token]) -> Vec<Import> {
    let str_at = |i: usize| match tokens.get(i).map(|t| &t.tok) {
        Some(Tok::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let simple = |i: usize, spec: String, len: usize| Import {
        spec,
        line: tokens[i].line,
        names: Vec::new(),
        tokens: i..(i + len).min(tokens.len()),
    };
    match lang {
        Lang::Script => script_imports(tokens),
        Lang::C => (0..tokens.len())
            .filter(|&i| tokens[i].first && tokens[i].is('#'))
            .filter(|&i| {
                tokens
                    .get(i + 1)
                    .is_some_and(|t| t.is_word("include") || t.is_word("import"))
            })
            .filter_map(|i| Some(simple(i, str_at(i + 2)?, 3)))
            .collect(),
        Lang::Css => (0..tokens.len())
            .filter(|&i| {
                tokens[i].is('@') && tokens.get(i + 1).is_some_and(|t| t.is_word("import"))
            })
            .filter_map(|i| match str_at(i + 2) {
                Some(spec) => Some(simple(i, spec, 3)),
                None if tokens.get(i + 2).is_some_and(|t| t.is_word("url")) => {
                    Some(simple(i, str_at(i + 4)?, 6))
                }
                None => None,
            })
            .collect(),
        Lang::Json => (0..tokens.len())
            .filter(|&i| tokens[i].tok == Tok::Str("$ref".into()))
            .filter(|&i| tokens.get(i + 1).is_some_and(|t| t.is(':')))
            .filter_map(|i| {
                let spec = str_at(i + 2)?;
                let file = spec.split('#').next().unwrap_or_default().to_owned();
                (!file.is_empty() && !file.contains("://")).then(|| simple(i, file, 3))
            })
            .collect(),
    }
}

/// What a top-level definition is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Function,
    Type,
    Value,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Function => "function",
            Kind::Type => "type",
            Kind::Value => "value",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub kind: Kind,
    /// A hash of its tokens.
    pub print: u64,
    pub line: usize,
}

fn print(tokens: &[Token]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for t in tokens {
        t.tok.hash(&mut hasher);
    }
    hasher.finish()
}

/// One hash of the hashes `a` then `b`.
pub fn mix(a: u64, b: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (a, b).hash(&mut hasher);
    hasher.finish()
}

/// `source`, a Vue or Svelte file, with everything outside its `<script>` blocks blanked and
/// its line breaks kept.
pub fn script_blocks(source: &str) -> String {
    let lower = source.to_ascii_lowercase();
    let mut out = String::with_capacity(source.len());
    let blank = |out: &mut String, text: &str| {
        out.extend(text.chars().map(|c| if c == '\n' { '\n' } else { ' ' }));
    };
    let mut at = 0;
    while let Some(open) = lower[at..].find("<script").map(|i| at + i) {
        // the `>` ending the start tag, not one in a quoted value such as `generic="Array<T>"`
        let mut quote = None;
        let tag_end = lower[open..].char_indices().find(|&(_, c)| match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
                false
            }
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                false
            }
            None => c == '>',
        });
        let Some(body) = tag_end.map(|(i, _)| open + i + 1) else {
            break;
        };
        let end = lower[body..]
            .find("</script")
            .map_or(source.len(), |i| body + i);
        blank(&mut out, &source[at..body]);
        out.push_str(&source[body..end]);
        at = end;
    }
    blank(&mut out, &source[at..]);
    out
}

/// Where a definition starting at `tokens[start]` ends: at a top-level `;`, at the `}` closing a
/// block with `block`, or before a top-level `starts` word on a later line.
fn definition_end(tokens: &[Token], start: usize, block: bool, starts: &[&str]) -> usize {
    let base = tokens[start].depth;
    let line = tokens[start].line;
    for (k, t) in tokens.iter().enumerate().skip(start + 1) {
        if t.depth < base {
            return k;
        }
        if t.depth == base {
            if t.is(';') || (block && t.is('}')) {
                return k + 1;
            }
            if t.line > line && t.first && t.ident().is_some_and(|w| starts.contains(&w)) {
                return k;
            }
        }
    }
    tokens.len()
}

const SCRIPT_STARTS: [&str; 12] = [
    "export",
    "import",
    "function",
    "class",
    "interface",
    "enum",
    "type",
    "const",
    "let",
    "var",
    "async",
    "declare",
];
const C_KEYWORDS: [&str; 16] = [
    "if",
    "while",
    "for",
    "switch",
    "return",
    "sizeof",
    "defined",
    "else",
    "do",
    "case",
    "goto",
    "alignof",
    "typeof",
    "_Alignas",
    "_Static_assert",
    "__attribute__",
];

/// The top-level definitions in `tokens` of a file in `lang`, by name.
pub fn definitions(lang: Lang, tokens: &[Token]) -> BTreeMap<String, Definition> {
    let mut out = BTreeMap::new();
    let mut add = |name: &str, kind: Kind, range: Range<usize>| {
        let hash = print(&tokens[range.clone()]);
        out.entry(name.to_owned())
            .and_modify(|d: &mut Definition| d.print = mix(d.print, hash))
            .or_insert(Definition {
                kind,
                print: hash,
                line: tokens[range.start].line,
            });
    };
    let name_at = |i: usize| tokens.get(i).and_then(Token::ident);
    match lang {
        Lang::Script => {
            for (i, t) in tokens.iter().enumerate() {
                if t.depth != 0 || (i > 0 && tokens[i - 1].is('.')) {
                    continue;
                }
                let Some(word) = t.ident() else { continue };
                let (kind, block) = match word {
                    "function" => (Kind::Function, true),
                    "class" | "interface" | "enum" => (Kind::Type, true),
                    "type" => (Kind::Type, false),
                    "const" | "let" | "var" => (Kind::Value, false),
                    _ => continue,
                };
                let at = if tokens.get(i + 1).is_some_and(|t| t.is('*')) {
                    i + 2
                } else {
                    i + 1
                };
                let Some(name) = name_at(at) else { continue };
                if word == "type" && !tokens.get(at + 1).is_some_and(|t| t.is('=') || t.is('<')) {
                    continue;
                }
                let end = definition_end(tokens, i, block, &SCRIPT_STARTS);
                let arrow = tokens[i..end]
                    .windows(2)
                    .any(|w| w[0].is('=') && w[1].is('>'));
                let kind = if kind == Kind::Value && arrow {
                    Kind::Function
                } else {
                    kind
                };
                add(name, kind, i..end);
            }
        }
        Lang::C => {
            let (mut directive, mut next) = (0, 0);
            for (i, t) in tokens.iter().enumerate() {
                if t.first && t.is('#') {
                    directive = t.line;
                    if tokens.get(i + 1).is_some_and(|t| t.is_word("define")) {
                        if let Some(name) = name_at(i + 2) {
                            let end = tokens[i..]
                                .iter()
                                .position(|n| n.line != t.line)
                                .map_or(tokens.len(), |n| i + n);
                            add(name, Kind::Value, i..end);
                        }
                    }
                    continue;
                }
                if i < next || t.line == directive || t.depth != 0 || t.is(';') || t.is('}') {
                    continue;
                }
                let end = definition_end(tokens, i, true, &[]);
                next = end;
                let s = &tokens[i..end];
                let top = |t: &Token| t.depth == 0;
                if let Some(k) = s.iter().position(|t| t.is_word("typedef")) {
                    let pointer = s
                        .windows(4)
                        .find(|w| w[0].is('(') && w[1].is('*') && w[3].is(')'));
                    let name = match pointer {
                        Some(w) => w[2].ident(),
                        None => s[k..]
                            .iter()
                            .rev()
                            .filter(|t| top(t))
                            .find_map(Token::ident),
                    };
                    if let Some(name) = name {
                        add(name, Kind::Type, i..end);
                    }
                } else if let Some(k) = s
                    .iter()
                    .position(|t| t.is_word("struct") || t.is_word("enum") || t.is_word("union"))
                {
                    let name = s.get(k + 1).and_then(Token::ident);
                    if let (Some(name), true) = (name, s.get(k + 2).is_some_and(|t| t.is('{'))) {
                        add(name, Kind::Type, i..end);
                    }
                } else if let Some(w) = s.windows(2).find(|w| {
                    top(&w[0])
                        && w[1].is('(')
                        && w[0].ident().is_some_and(|n| !C_KEYWORDS.contains(&n))
                }) {
                    add(w[0].ident().unwrap_or_default(), Kind::Function, i..end);
                }
            }
        }
        Lang::Css | Lang::Json => {}
    }
    out
}

/// The definitions of `before` changed or gone in `after`.
pub fn changed(
    before: &BTreeMap<String, Definition>,
    after: &BTreeMap<String, Definition>,
) -> BTreeMap<String, (Kind, bool)> {
    before
        .iter()
        .filter_map(|(name, def)| match after.get(name) {
            None => Some((name.clone(), (def.kind, true))),
            Some(now) if now.print != def.print => Some((name.clone(), (def.kind, false))),
            Some(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    type Names = Vec<(String, String)>;

    fn imports_of(lang: Lang, source: &str) -> Vec<(String, usize, Names)> {
        let (tokens, _) = tokens(lang, source);
        imports(lang, &tokens)
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
const later = await import(`./dynamic`);
import x from "react";
foo.import("./not-an-import");
"#;
        assert_eq!(
            imports_of(Lang::Script, source),
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
            ]
        );
    }

    #[test]
    fn c_css_and_json_imports() {
        assert_eq!(
            imports_of(Lang::C, "// #include \"no.h\"\n#include \"x.h\"\n#include <stdio.h>\n  #  include \"y.h\"\n"),
            [("x.h".into(), 2, vec![]), ("y.h".into(), 4, vec![])]
        );
        assert_eq!(
            imports_of(Lang::Css, "/* @import \"no.css\"; */\n@import url(\"a.css\");\n@import 'b.css';\nbody { background: url(http://x/y.png) }\n"),
            [("a.css".into(), 2, vec![]), ("b.css".into(), 3, vec![])]
        );
        assert_eq!(
            imports_of(Lang::Json, "{\"$ref\":\n  \"defs.json#/a\", \"x\": {\"$ref\": \"#/local\"}, \"y\": {\"$ref\": \"https://x/s.json\"}}"),
            [("defs.json".into(), 1, vec![])]
        );
        assert_eq!(
            imports_of(Lang::C, "#import \"View.h\"\n#import <UIKit/UIKit.h>\n"),
            [("View.h".into(), 1, vec![])],
            "Objective-C imports"
        );
    }

    #[test]
    fn a_style_sheet_ending_inside_an_import_is_read() {
        for source in [
            "@import url(\"b.css\"",
            "@import url(\"b.css\")",
            "@import \"b.css\"",
        ] {
            let (tokens, _) = tokens(Lang::Css, source);
            let found = imports(Lang::Css, &tokens);
            assert_eq!(found.len(), 1, "{source}");
            assert!(found[0].tokens.end <= tokens.len());
            uses(&tokens, &found);
        }
    }

    #[test]
    fn only_the_script_blocks_of_a_component_are_kept() {
        let vue = "<template>\n  <p>It`s {{ x }}</p>\n</template>\n<SCRIPT setup lang=\"ts\">\nimport { x } from './x';\n</SCRIPT>\n<style>a{}</style>\n";
        let script = script_blocks(vue);
        assert_eq!(script.lines().count(), vue.lines().count());
        assert!(!script.contains('`') && !script.contains("style"));
        let (tokens, _) = tokens(Lang::Script, &script);
        let found = imports(Lang::Script, &tokens);
        assert_eq!((found[0].spec.as_str(), found[0].line), ("./x", 5));
        let generic = "<script setup lang=\"ts\" generic=\"T extends Array<string>\">\nimport { y } from './y';\n</script>\n";
        assert_eq!(
            script_blocks(generic),
            format!(
                "{}\nimport {{ y }} from './y';\n{}\n",
                " ".repeat(generic.find('\n').unwrap()),
                " ".repeat("</script>".len())
            )
        );
    }

    #[test]
    fn a_name_defined_twice_changes_with_either_definition() {
        let of = |s| definitions(Lang::C, &tokens(Lang::C, s).0);
        let before = of("int f(int a);\nint f(int a) { return a; }\n");
        let after = of("int f(int a);\nint f(int a) { return a + 1; }\n");
        assert_eq!(
            changed(&before, &after).into_iter().collect::<Vec<_>>(),
            [("f".into(), (Kind::Function, false))]
        );
    }

    fn defs(lang: Lang, source: &str) -> Vec<(String, Kind)> {
        let (tokens, _) = tokens(lang, source);
        definitions(lang, &tokens)
            .into_iter()
            .map(|(name, d)| (name, d.kind))
            .collect()
    }

    #[test]
    fn top_level_definitions_of_each_language() {
        let script = "export function parse(a) { return { a }; }\nexport const run = (x) => x\nconst LIMIT = 3;\nexport class Parser { method() { const inner = 1; } }\nexport type Options = { a: 1 } | { b: 2 };\ninterface Shape { }\nfunction* gen() {}\n";
        assert_eq!(
            defs(Lang::Script, script),
            [
                ("LIMIT".into(), Kind::Value),
                ("Options".into(), Kind::Type),
                ("Parser".into(), Kind::Type),
                ("Shape".into(), Kind::Type),
                ("gen".into(), Kind::Function),
                ("parse".into(), Kind::Function),
                ("run".into(), Kind::Function),
            ]
        );
        let c = "#define LIMIT 3\nint x(void);\nstatic int helper(int a) { if (a) { return 1; } return 0; }\nstruct point { int x; };\ntypedef unsigned long size;\ntypedef int (*callback)(int);\n";
        assert_eq!(
            defs(Lang::C, c),
            [
                ("LIMIT".into(), Kind::Value),
                ("callback".into(), Kind::Type),
                ("helper".into(), Kind::Function),
                ("point".into(), Kind::Type),
                ("size".into(), Kind::Type),
                ("x".into(), Kind::Function),
            ]
        );
    }

    #[test]
    fn changed_definitions_ignore_comments_and_spacing() {
        let before = "function a() { 1 }\n// note\nfunction b() { 2 }\nfunction c() {}\n";
        let after = "function a() {\n    1 // same\n}\nfunction b() { 3 }\n";
        let of = |s| definitions(Lang::Script, &tokens(Lang::Script, s).0);
        let changed = changed(&of(before), &of(after));
        assert_eq!(
            changed.into_iter().collect::<Vec<_>>(),
            [
                ("b".into(), (Kind::Function, false)),
                ("c".into(), (Kind::Function, true)),
            ]
        );
    }

    #[test]
    fn lexing_keeps_lines_through_strings_and_comments() {
        let (tokens, _) = tokens(
            Lang::Script,
            "let a = `x\ny`;\n/* one\ntwo */ let b = 'c'; function f() {}\nlet s = \"unterminated\nlet d = 1;\n",
        );
        let b = tokens.iter().find(|t| t.is_word("b")).unwrap();
        assert_eq!(b.line, 4);
        assert!(tokens.iter().any(|t| t.tok == Tok::Str("c".into())));
        let f = tokens.iter().find(|t| t.is_word("f")).unwrap();
        assert_eq!((f.line, f.depth), (4, 0));
        let d = tokens.iter().find(|t| t.is_word("d")).unwrap();
        assert_eq!(d.line, 6, "a string ends at the end of its line");
        let (many, more) = super::tokens(Lang::Script, &"a ".repeat(MAX_TOKENS + 1));
        assert!(more && many.len() == MAX_TOKENS);
    }
}
