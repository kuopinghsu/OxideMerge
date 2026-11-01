//! A small tokenizer for source files, and the imports and top-level definitions read from its
//! tokens. Comments are skipped and strings kept whole, so neither is mistaken for code.
use super::Lang;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::ops::Range;

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
    /// The first token of its line, not counting a line continued with `\`.
    pub first: bool,
    /// Columns of indentation of its line.
    pub indent: usize,
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
    let (mut i, mut line, mut indent, mut depth) = (0, 1, 0, 0usize);
    let (mut fresh, mut continued) = (true, false);
    let slash_comments = matches!(lang, Lang::Script | Lang::C | Lang::Rust | Lang::Css);
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
                indent = 0;
                fresh = !continued;
                continued = false;
                i += 1;
                continue;
            }
            ' ' | '\t' | '\r' | '\u{c}' => {
                if fresh {
                    indent += 1;
                }
                i += 1;
                continue;
            }
            '\\' if lang == Lang::Python => {
                continued = true;
                i += 1;
                continue;
            }
            '#' if lang == Lang::Python => {
                i = line_end(i);
                continue;
            }
            '/' if slash_comments && bytes.get(i + 1) == Some(&b'*') => {
                let nests = lang == Lang::Rust;
                let mut open = 1;
                i += 2;
                while i < bytes.len() && open > 0 {
                    if bytes[i..].starts_with(b"*/") {
                        open -= 1;
                        i += 2;
                    } else if nests && bytes[i..].starts_with(b"/*") {
                        open += 1;
                        i += 2;
                    } else {
                        if bytes[i] == b'\n' {
                            line += 1;
                            indent = 0;
                            fresh = true;
                        }
                        i += 1;
                    }
                }
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
            indent,
            depth,
        };
        let quote = match c {
            '"' => true,
            '\'' => match lang {
                Lang::Rust => {
                    // a character, not a lifetime
                    let next = at(i + 1);
                    next == '\\' || (next != '\0' && at(i + 1 + next.len_utf8()) == '\'')
                }
                Lang::Json => false,
                _ => true,
            },
            '`' => lang == Lang::Script,
            _ => false,
        };
        if quote {
            let triple = lang == Lang::Python
                && (bytes[i..].starts_with(b"\"\"\"") || bytes[i..].starts_with(b"'''"));
            let (close, open_len): (&[u8], usize) = if triple {
                (&bytes[i..i + 3], 3)
            } else {
                (&bytes[i..i + 1], 1)
            };
            let close = close.to_vec();
            let body = i + open_len;
            let mut j = body;
            let multiline = triple || c == '`' || (c == '"' && lang == Lang::Rust);
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
            // a raw string: r"..", r#".."#, br".."
            if lang == Lang::Rust && (word == "r" || word == "br") {
                let hashes = bytes[j..].iter().take_while(|&&b| b == b'#').count();
                if bytes.get(j + hashes) == Some(&b'"') {
                    let mut close = vec![b'"'];
                    close.extend(std::iter::repeat_n(b'#', hashes));
                    let body = j + hashes + 1;
                    let end = bytes[body..]
                        .windows(close.len())
                        .position(|w| w == close.as_slice())
                        .map_or(bytes.len(), |n| body + n);
                    line += bytes[body..end].iter().filter(|&&b| b == b'\n').count();
                    let text = source.get(body..end).unwrap_or_default().to_owned();
                    out.push(token(Tok::Str(text), start_line, depth));
                    i = (end + close.len()).min(bytes.len());
                    continue;
                }
            }
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
    /// The tokens of the statement.
    pub tokens: Range<usize>,
}

/// The identifiers of `tokens` with the line each is first used on outside import statements.
pub fn uses(tokens: &[Token], imports: &[Import]) -> HashMap<String, usize> {
    let mut skip = vec![false; tokens.len()];
    for import in imports {
        for flag in &mut skip[import.tokens.clone()] {
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

fn relative(spec: &str) -> bool {
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

/// Statements of a Python file: tokens from one that starts a logical line, or follows `;`,
/// to the next.
fn python_statements(tokens: &[Token]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, t) in tokens.iter().enumerate() {
        let new_line = t.first && t.depth == 0;
        let after_semicolon = i > 0 && tokens[i - 1].is(';') && tokens[i - 1].depth == 0;
        if i > start && (new_line || after_semicolon) {
            out.push(start..i);
            start = i;
        }
    }
    if start < tokens.len() {
        out.push(start..tokens.len());
    }
    out
}

/// A dotted name from `tokens[at]`; it and the index after it.
fn dotted(tokens: &[Token], mut at: usize) -> (String, usize) {
    let mut name = String::new();
    while let Some(word) = tokens.get(at).and_then(Token::ident) {
        name.push_str(word);
        if tokens.get(at + 1).is_some_and(|t| t.is('.')) {
            name.push('.');
            at += 2;
        } else {
            return (name, at + 1);
        }
    }
    (name, at)
}

fn python_imports(tokens: &[Token]) -> Vec<Import> {
    let mut out = Vec::new();
    for range in python_statements(tokens) {
        let s = &tokens[range.clone()];
        let line = s[0].line;
        // `import a as b`, `a`: each a name and its local name
        let names = |from: usize| -> Vec<(String, String)> {
            let mut items = Vec::new();
            let mut item: Vec<String> = Vec::new();
            let mut i = from;
            let flush = |item: &mut Vec<String>, items: &mut Vec<(String, String)>| {
                match item.as_slice() {
                    _ if item.iter().any(|w| w == "?") => {}
                    [name] => items.push((name.clone(), name.clone())),
                    [name, r#as, local] if r#as == "as" => {
                        items.push((name.clone(), local.clone()))
                    }
                    _ => {}
                }
                item.clear();
            };
            while i < s.len() {
                let t = &s[i];
                if t.is(',') {
                    flush(&mut item, &mut items);
                } else if let Some(word) = t.ident() {
                    let (name, next) = dotted(s, i);
                    item.push(if name.is_empty() {
                        word.to_owned()
                    } else {
                        name
                    });
                    i = next.max(i + 1);
                    continue;
                } else if !(t.is('(') || t.is(')') || t.is(';')) {
                    item.push("?".into());
                }
                i += 1;
            }
            flush(&mut item, &mut items);
            items
        };
        if s[0].is_word("from") {
            let dots = s[1..].iter().take_while(|t| t.is('.')).count();
            let (module, next) = if s.get(1 + dots).is_some_and(|t| t.is_word("import")) {
                (String::new(), 1 + dots)
            } else {
                dotted(s, 1 + dots)
            };
            if !s.get(next).is_some_and(|t| t.is_word("import")) {
                continue;
            }
            let prefix = ".".repeat(dots);
            let imported = names(next + 1);
            if module.is_empty() {
                for (name, _) in imported {
                    out.push(Import {
                        spec: format!("{prefix}{name}"),
                        line,
                        names: Vec::new(),
                        tokens: range.clone(),
                    });
                }
            } else {
                out.push(Import {
                    spec: format!("{prefix}{module}"),
                    line,
                    names: imported,
                    tokens: range.clone(),
                });
            }
        } else if s[0].is_word("import") {
            for (module, _) in names(1) {
                out.push(Import {
                    spec: module,
                    line,
                    names: Vec::new(),
                    tokens: range.clone(),
                });
            }
        }
    }
    out
}

/// The Rust `use` paths of the tree from `tokens[at]`, below `prefix`: `a::b`, `a::{b, c}`,
/// `a::*`, `a as b`. Each is the path and the name it is used by. Returns the index after it.
fn use_tree(
    tokens: &[Token],
    mut at: usize,
    prefix: &[String],
    out: &mut Vec<(Vec<String>, String)>,
) -> usize {
    let mut path = prefix.to_vec();
    let end = tokens.len().min(at + MAX_STATEMENT);
    while at < end {
        let t = &tokens[at];
        if t.is('{') {
            at += 1;
            while at < end && !tokens[at].is('}') {
                at = use_tree(tokens, at, &path, out);
                if tokens.get(at).is_some_and(|t| t.is(',')) {
                    at += 1;
                }
            }
            return at + 1;
        }
        if t.is('*') {
            out.push((path, "*".into()));
            return at + 1;
        }
        let Some(word) = t.ident() else {
            return at;
        };
        path.push(word.to_owned());
        at += 1;
        if tokens.get(at).is_some_and(|t| t.is(':'))
            && tokens.get(at + 1).is_some_and(|t| t.is(':'))
        {
            at += 2;
            continue;
        }
        let mut local = word.to_owned();
        if tokens.get(at).is_some_and(|t| t.is_word("as")) {
            if let Some(alias) = tokens.get(at + 1).and_then(Token::ident) {
                local = alias.to_owned();
            }
            at += 2;
        }
        out.push((path, local));
        return at;
    }
    at
}

fn rust_imports(tokens: &[Token]) -> Vec<Import> {
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        if t.is_word("mod") {
            if let (Some(name), Some(semi)) =
                (tokens.get(i + 1).and_then(Token::ident), tokens.get(i + 2))
            {
                if semi.is(';') {
                    out.push(Import {
                        spec: name.to_owned(),
                        line: t.line,
                        names: Vec::new(),
                        tokens: i..i + 3,
                    });
                }
            }
            continue;
        }
        if !t.is_word("use") {
            continue;
        }
        let mut paths = Vec::new();
        let end = use_tree(tokens, i + 1, &[], &mut paths);
        for (path, local) in paths {
            if !matches!(
                path.first().map(String::as_str),
                Some("crate" | "super" | "self")
            ) {
                continue;
            }
            let mut path = path;
            let last = path.last().cloned().unwrap_or_default();
            let names = if local == "*" {
                Vec::new()
            } else if last == "self" && path.len() > 1 {
                path.pop();
                Vec::new()
            } else {
                vec![(last, local)]
            };
            out.push(Import {
                spec: format!("use:{}", path.join("::")),
                line: t.line,
                names,
                tokens: i..end,
            });
        }
    }
    out
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
        tokens: i..i + len,
    };
    match lang {
        Lang::Script => script_imports(tokens),
        Lang::Python => python_imports(tokens),
        Lang::Rust => rust_imports(tokens),
        Lang::C => (0..tokens.len())
            .filter(|&i| tokens[i].first && tokens[i].is('#'))
            .filter(|&i| tokens.get(i + 1).is_some_and(|t| t.is_word("include")))
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
const RUST_STARTS: [&str; 14] = [
    "pub",
    "fn",
    "struct",
    "enum",
    "trait",
    "type",
    "const",
    "static",
    "union",
    "impl",
    "mod",
    "use",
    "macro_rules",
    "unsafe",
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
        out.entry(name.to_owned()).or_insert(Definition {
            kind,
            print: print(&tokens[range.clone()]),
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
        Lang::Rust => {
            for (i, t) in tokens.iter().enumerate() {
                if t.depth != 0 {
                    continue;
                }
                let Some(word) = t.ident() else { continue };
                let (kind, block) = match word {
                    "fn" => (Kind::Function, true),
                    "struct" | "enum" | "trait" | "union" => (Kind::Type, true),
                    "type" => (Kind::Type, false),
                    "const" | "static" => (Kind::Value, false),
                    "macro_rules" if tokens.get(i + 1).is_some_and(|t| t.is('!')) => {
                        if let Some(name) = name_at(i + 2) {
                            let end = definition_end(tokens, i, true, &RUST_STARTS);
                            add(name, Kind::Function, i..end);
                        }
                        continue;
                    }
                    _ => continue,
                };
                let Some(name) = name_at(i + 1) else { continue };
                // `const fn`, `static mut`
                if matches!(name, "fn" | "unsafe" | "async" | "extern") {
                    continue;
                }
                let at = if name == "mut" { i + 2 } else { i + 1 };
                let Some(name) = name_at(at) else { continue };
                add(
                    name,
                    kind,
                    i..definition_end(tokens, i, block, &RUST_STARTS),
                );
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
        Lang::Python => {
            for (i, t) in tokens.iter().enumerate() {
                if !(t.first && t.indent == 0 && t.depth == 0) {
                    continue;
                }
                let at = if t.is_word("async") { i + 1 } else { i };
                let (name, kind) = match name_at(at) {
                    Some("def") => (name_at(at + 1), Kind::Function),
                    Some("class") => (name_at(at + 1), Kind::Type),
                    Some(word)
                        if tokens.get(i + 1).is_some_and(|t| t.is('='))
                            && !tokens.get(i + 2).is_some_and(|t| t.is('=')) =>
                    {
                        (Some(word), Kind::Value)
                    }
                    _ => continue,
                };
                let Some(name) = name else { continue };
                let end = tokens[i + 1..]
                    .iter()
                    .position(|n| n.first && n.indent == 0 && n.depth == 0)
                    .map_or(tokens.len(), |n| i + 1 + n);
                add(name, kind, i..end);
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
    fn python_imports_follow_parentheses_and_continuations() {
        let source = "# from .commented import x\n\"\"\"\nfrom .docstring import y\n\"\"\"\nfrom .helpers import (\n    f,\n    g as h,  # why\n)\nfrom . import a, b as c\nimport os.path, pkg.mod as m\nfrom .x import \\\n    z\nif True:\n    from ..up import *\nimport q; from .r import s\n";
        assert_eq!(
            imports_of(Lang::Python, source),
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
            ]
        );
    }

    #[test]
    fn rust_c_css_and_json_imports() {
        let rust = "// mod commented;\nmod parser;\npub(crate) mod util;\nmod inline { }\nuse crate::parser::{self, parse, Parser as P};\nuse super::sibling::*;\nuse std::io;\nlet s = r#\"mod fake;\"#;\nuse self::util::helper;\n";
        assert_eq!(
            imports_of(Lang::Rust, rust),
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
                ("use:super::sibling".into(), 6, vec![]),
                (
                    "use:self::util::helper".into(),
                    9,
                    names(&[("helper", "helper")])
                ),
            ]
        );
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
        let rust = "pub fn parse(input: &str) -> Result<()> { let x = 1; Ok(()) }\npub struct Parser<'a> { s: &'a str }\nstruct Unit;\npub const fn limit() -> usize { 3 }\nstatic mut COUNT: usize = 0;\nimpl Parser<'_> { fn method(&self) {} }\nmacro_rules! m { () => {} }\npub(crate) enum E { A }\n";
        assert_eq!(
            defs(Lang::Rust, rust),
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
        let python = "import os\nLIMIT = 3\n\n@decorator\ndef parse(text):\n    def inner():\n        pass\n    return text\n\nclass Parser(Base):\n    x = 1\n\nasync def fetch():\n    pass\nif LIMIT == 3:\n    pass\n";
        assert_eq!(
            defs(Lang::Python, python),
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
        let before = "fn a() { 1 }\n// note\nfn b() { 2 }\nfn c() {}\n";
        let after = "fn a() {\n    1 // same\n}\nfn b() { 3 }\n";
        let of = |s| definitions(Lang::Rust, &tokens(Lang::Rust, s).0);
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
        let (tokens, _) = tokens(Lang::Rust, "let a = \"x\ny\";\n/* one\ntwo */ let b = 'c'; fn f<'a>() {}\nlet s = r##\"raw \"# still\"##;\n");
        let b = tokens.iter().find(|t| t.is_word("b")).unwrap();
        assert_eq!(b.line, 4);
        assert!(tokens.iter().any(|t| t.tok == Tok::Str("c".into())));
        assert!(tokens
            .iter()
            .any(|t| t.tok == Tok::Str("raw \"# still".into())));
        let f = tokens.iter().find(|t| t.is_word("f")).unwrap();
        assert_eq!((f.line, f.depth), (4, 0));
        let (many, more) = super::tokens(Lang::Script, &"a ".repeat(MAX_TOKENS + 1));
        assert!(more && many.len() == MAX_TOKENS);
    }
}
