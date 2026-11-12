//! Web pages as Markdown: a page's HTML turned into Markdown blocks (paragraphs, headings, list
//! items, tables, code blocks), which are compared block by block and shown formatted.

use super::diff::{inline_diff, DiffOptions};
use super::web::{attr, entity, has_attr, tokens, Token, BLOCK};
use similar::{Algorithm, DiffTag};
use std::ops::Range;
use url::Url;

/// One block of a page as Markdown, complete in itself so it can be shown on its own.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Block {
    /// How many list items it is nested in, not counting one it starts.
    pub depth: usize,
    pub text: String,
}

/// Elements without contents or a closing tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "source", "track",
    "wbr",
];

/// Elements whose contents a page does not show as text.
const SKIP: &[&str] = &[
    "title", "select", "datalist", "option", "textarea", "iframe", "object", "canvas", "video",
    "audio", "script", "style", "noscript", "template", "svg", "math", "head",
];

/// Class names of text only meant for screen readers.
const SCREEN_READER_ONLY: &[&str] = &[
    "sr-only",
    "visually-hidden",
    "visuallyhidden",
    "screen-reader-text",
];

fn is_hidden(attrs: &str) -> bool {
    if has_attr(attrs, "hidden")
        || attr(attrs, "aria-hidden").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    if let Some(style) = attr(attrs, "style") {
        let style: String = style
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_lowercase();
        if style.contains("display:none") || style.contains("visibility:hidden") {
            return true;
        }
    }
    attr(attrs, "class").is_some_and(|c| {
        c.split_whitespace()
            .any(|c| SCREEN_READER_ONLY.contains(&c))
    })
}

/// Elements whose end tag a page may leave out.
const OPTIONAL_END: &[&str] = &[
    "p", "li", "dt", "dd", "option", "optgroup", "rt", "rp", "td", "th", "tr", "thead", "tbody",
    "tfoot",
];

/// Start tags that end an open `p`.
const ENDS_P: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "plaintext",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "ul",
    "xmp",
];

/// Most elements followed open in an element being left out; deeper ones are taken as empty.
const MAX_SKIP_DEPTH: usize = 256;

/// Whether a start tag `tag` ends the open element `open`, whose end tag was left out.
fn ended_by(open: &str, tag: &str) -> bool {
    match open {
        "p" => ENDS_P.contains(&tag),
        "li" => tag == "li",
        "dt" | "dd" => matches!(tag, "dt" | "dd"),
        "option" => matches!(tag, "option" | "optgroup"),
        "optgroup" => tag == "optgroup",
        "rt" | "rp" => matches!(tag, "rt" | "rp"),
        "td" | "th" => matches!(tag, "td" | "th" | "tr" | "thead" | "tbody" | "tfoot"),
        "tr" => matches!(tag, "tr" | "thead" | "tbody" | "tfoot"),
        "thead" | "tbody" | "tfoot" => matches!(tag, "thead" | "tbody" | "tfoot"),
        _ => false,
    }
}

/// Follows `tok` through `open`, the elements open in an element being left out, outermost
/// first: whether it is part of them. As in a browser, an element whose end tag was left out
/// ends at a tag that cannot be inside it, such as the next `<li>` after `<li hidden>`, or at
/// the end tag of the element around it, when `around` says that element is open; that tag is
/// then not part of them.
fn skipped(open: &mut Vec<String>, tok: &Token, around: impl Fn(&str) -> bool) -> bool {
    let Token::Tag(name, attrs, closing) = tok else {
        return true;
    };
    let name = name.as_str();
    if *closing {
        if let Some(i) = open.iter().rposition(|o| o == name) {
            open.truncate(i);
            return true;
        }
        let ends = matches!(name, "td" | "th")
            || (BLOCK.contains(&name)
                && !matches!(name, "p" | "br" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"));
        if ends && OPTIONAL_END.contains(&open[0].as_str()) && around(name) {
            open.clear();
            return false;
        }
        return true;
    }
    let mut cut = None;
    for (i, o) in open.iter().enumerate().rev() {
        if ended_by(o, name) {
            cut = Some(i);
        } else if matches!(name, "li" | "dd" | "dt")
            && matches!(o.as_str(), "p" | "div" | "address")
        {
            // a browser looks past these for the item to end
        } else if BLOCK.contains(&o.as_str()) || OPTIONAL_END.contains(&o.as_str()) {
            break;
        }
    }
    if let Some(i) = cut {
        open.truncate(i);
        if open.is_empty() {
            return false;
        }
    }
    if !VOID.contains(&name) && !attrs.trim_end().ends_with('/') && open.len() < MAX_SKIP_DEPTH {
        open.push(name.to_string());
    }
    true
}

/// Decodes character references, keeping the white space.
fn decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp + 1..];
        let semi = tail.find(';').filter(|&k| k <= 10);
        match semi.and_then(|k| entity(&tail[..k]).map(|c| (k, c))) {
            Some((k, c)) => {
                out.push(c);
                rest = &tail[k + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Characters that would otherwise be read as Markdown within a line.
pub(crate) fn escapes(c: char) -> bool {
    matches!(
        c,
        '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '~' | '|'
    )
}

/// Whether `rest`, the text after a `&`, would be read as a character reference.
fn entity_follows(rest: &str) -> bool {
    let name = rest.strip_prefix('#').unwrap_or(rest);
    let len = name.len()
        - name
            .trim_start_matches(|c: char| c.is_ascii_alphanumeric())
            .len();
    (1..=32).contains(&len) && name[len..].starts_with(';')
}

/// Escapes what would start a heading, list item or rule at the start of a line.
pub(crate) fn escape_line_start(line: &str) -> String {
    if line.starts_with(['#', '+', '-', '=']) {
        return format!("\\{line}");
    }
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits > 0 && line[digits..].starts_with(['.', ')']) {
        return format!("{}\\{}", &line[..digits], &line[digits..]);
    }
    line.to_string()
}

/// A link destination, in angle brackets when it has characters that would end it.
pub(crate) fn destination(url: &str) -> String {
    if url.contains([' ', '(', ')']) {
        format!("<{}>", url.replace('<', "%3C").replace('>', "%3E"))
    } else {
        url.to_string()
    }
}

enum Ctx {
    Quote,
    List {
        ordered: bool,
        next: u64,
    },
    /// A list item, with its marker until a block written in it has taken it.
    Item(Option<String>),
}

enum Mark {
    Strong,
    Em,
    Strike,
    Code,
    Link(String),
}

/// Inline formatting opened in the current block: its tag, where its text starts, and what it
/// becomes. The Markdown is written when it closes; formatting never closed is left out.
struct Open {
    tag: String,
    start: usize,
    mark: Mark,
}

#[derive(Default)]
struct Table {
    rows: Vec<Vec<SpanCell>>,
    /// The rows where a row group (`thead`, `tbody`, `tfoot`) starts or ends: a cell spans
    /// rows only within its own group.
    groups: Vec<usize>,
    /// How many rows and columns the cell being read spans.
    span: (usize, usize),
    in_cell: bool,
    in_caption: bool,
    caption: String,
    /// Tables inside this one, whose cells run together.
    nested: usize,
    /// Whether a `thead` is open, and whether the first row has a data (`td`) cell outside one.
    in_head: bool,
    data_first: bool,
}

struct Converter {
    base: Option<Url>,
    /// The page itself, when comparing its own links by fragment in Formatted view.
    document_url: Option<Url>,
    /// Whether a `<base href>` set `base`: only the first one counts.
    based: bool,
    blocks: Vec<Block>,
    /// Inline text of the block being read.
    line: String,
    ctx: Vec<Ctx>,
    /// Quotes and lists opened past [`MAX_NESTING`], which are read as their text only.
    deep_quotes: usize,
    deep_lists: usize,
    open: Vec<Open>,
    heading: Option<usize>,
    /// Text of a `pre` element being read, with the language of its code.
    pre: Option<(String, String)>,
    table: Option<Table>,
    /// The elements open in an element being left out, outermost first; empty when none is.
    skip: Vec<String>,
}

impl Converter {
    fn in_code(&self) -> bool {
        self.open.iter().any(|o| matches!(o.mark, Mark::Code))
    }

    fn in_link(&self) -> bool {
        self.open.iter().any(|o| matches!(o.mark, Mark::Link(_)))
    }

    fn resolve(&self, href: &str) -> String {
        let href = decode(href.trim());
        match &self.base {
            Some(b) => b.join(&href).map_or(href, |u| u.to_string()),
            None => href,
        }
    }

    /// Where a link to `href` goes, when it is one a click may open: a web or mail address, a
    /// file on this computer from a page on it, or an address relative to a page without one.
    /// Decided after entities are decoded and the address is made absolute, so `&#106;avascript:`
    /// is caught; what is kept is the address as parsed, with no line break left in it.
    fn link_to(&self, href: &str) -> Option<String> {
        let url = self.resolve(href);
        let local_page = self.base.as_ref().is_none_or(|b| b.scheme() == "file");
        match Url::parse(&url) {
            Ok(u) => match u.scheme() {
                "http" | "https" | "mailto" => Some(u.to_string()),
                "file" if local_page && u.host_str().is_none_or(|h| h == "localhost") => {
                    Some(u.to_string())
                }
                _ => None,
            },
            Err(url::ParseError::RelativeUrlWithoutBase) if !url.contains(char::is_control) => {
                Some(url)
            }
            Err(_) => None,
        }
    }

    fn anchor_to(&self, href: &str) -> Option<String> {
        let url = self.link_to(href)?;
        if let Some(document) = self.document_url.as_ref().filter(|_| {
            decode(href.trim())
                .trim_start_matches(|c| c <= ' ')
                .starts_with('#')
        }) {
            if let Ok(target) = Url::parse(&url) {
                let end = url::Position::AfterQuery;
                if target.fragment().is_some() && target[..end] == document[..end] {
                    return Some(target[end..].to_owned());
                }
            }
        }
        Some(url)
    }

    fn space(&mut self) {
        if !self.line.is_empty() && !self.line.ends_with([' ', '\n']) {
            self.line.push(' ');
        }
    }

    fn text(&mut self, raw: &str) {
        if let Some((pre, _)) = &mut self.pre {
            pre.push_str(&decode(raw));
            return;
        }
        let code = self.in_code();
        // A `|` ends a table cell even inside code.
        let in_table = self.table.is_some();
        let text = decode(raw);
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                self.space();
            } else {
                let escape = if code {
                    in_table && c == '|'
                } else {
                    escapes(c) || (c == '&' && entity_follows(&text[i + 1..]))
                };
                if escape {
                    self.line.push('\\');
                }
                self.line.push(c);
            }
        }
    }

    fn open_mark(&mut self, tag: &str, mark: Mark) {
        // Formatting within code is shown as code.
        if self.in_code() {
            return;
        }
        self.open.push(Open {
            tag: tag.to_string(),
            start: self.line.len(),
            mark,
        });
    }

    fn close_mark(&mut self, tag: &str) {
        let Some(i) = self.open.iter().rposition(|o| o.tag == tag) else {
            return;
        };
        while self.open.len() > i {
            let o = self.open.pop().expect("open formatting");
            let inner = self.line.split_off(o.start);
            let body = inner.trim();
            if body.is_empty() {
                self.line.push_str(&inner);
                continue;
            }
            let lead = &inner[..inner.len() - inner.trim_start().len()];
            let trail = &inner[inner.trim_end().len()..];
            let body = match o.mark {
                Mark::Strong => format!("**{body}**"),
                Mark::Em => format!("*{body}*"),
                Mark::Strike => format!("~~{body}~~"),
                Mark::Code => {
                    let longest = body.split(|c| c != '`').map(str::len).max().unwrap_or(0);
                    let fence = "`".repeat(longest + 1);
                    let pad = if body.starts_with('`') || body.ends_with('`') {
                        " "
                    } else {
                        ""
                    };
                    format!("{fence}{pad}{body}{pad}{fence}")
                }
                Mark::Link(url) => format!("[{body}]({})", destination(&url)),
            };
            self.line.push_str(lead);
            self.line.push_str(&body);
            self.line.push_str(trail);
        }
    }

    /// Writes `body` as a block in the lists and quotes it is in: the first line after their
    /// markers, the others indented to stay in them.
    fn emit(&mut self, body: &str) {
        let mut first = String::new();
        let mut rest = String::new();
        let mut depth = 0;
        for c in &mut self.ctx {
            match c {
                Ctx::Quote => {
                    first.push_str("> ");
                    rest.push_str("> ");
                }
                Ctx::Item(Some(marker)) => {
                    first.push_str(marker);
                    rest.push_str(&" ".repeat(marker.len()));
                }
                Ctx::Item(None) => depth += 1,
                Ctx::List { .. } => {}
            }
        }
        for c in &mut self.ctx {
            if let Ctx::Item(marker) = c {
                *marker = None;
            }
        }
        let mut text = String::new();
        for (i, line) in body.lines().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let prefix = if i == 0 { &first } else { &rest };
            if line.is_empty() {
                text.push_str(prefix.trim_end());
            } else {
                text.push_str(prefix);
                text.push_str(line);
            }
        }
        self.blocks.push(Block { depth, text });
    }

    /// Ends the block being read.
    fn flush(&mut self) {
        if let Some(t) = &mut self.table {
            if t.in_cell || t.in_caption {
                self.space();
            }
            return;
        }
        // Formatting left open ends with its block.
        self.open.clear();
        let line = std::mem::take(&mut self.line);
        let lines: Vec<String> = line
            .split("\\\n")
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(escape_line_start)
            .collect();
        if lines.is_empty() {
            return;
        }
        let body = match self.heading {
            Some(n) => format!("{} {}", "#".repeat(n), lines.join(" ")),
            None => lines.join("\\\n"),
        };
        self.emit(&body);
    }

    fn pop_to(&mut self, found: impl Fn(&Ctx) -> bool) {
        if let Some(i) = self.ctx.iter().rposition(found) {
            self.ctx.truncate(i);
        }
    }

    fn start_cell(&mut self, caption: bool) {
        self.end_cell();
        let Some(t) = &mut self.table else { return };
        if caption {
            t.in_caption = true;
        } else {
            if t.rows.is_empty() {
                t.rows.push(Vec::new());
            }
            t.in_cell = true;
            t.span = (1, 1);
        }
        self.line.clear();
    }

    fn end_cell(&mut self) {
        let Some(t) = &mut self.table else { return };
        if !(t.in_cell || t.in_caption) {
            return;
        }
        self.open.clear();
        let text = std::mem::take(&mut self.line)
            .replace("\\\n", " ")
            .trim()
            .to_string();
        if t.in_caption {
            t.caption = text;
        } else if let Some(row) = t.rows.last_mut() {
            let (rows, cols) = t.span;
            row.push(SpanCell {
                text: escape_cell(&text),
                rows,
                cols,
            });
        }
        t.in_cell = false;
        t.in_caption = false;
    }

    fn end_table(&mut self) {
        self.end_cell();
        let Some(t) = self.table.take() else { return };
        if !t.caption.is_empty() {
            self.emit(&escape_line_start(&t.caption));
        }
        // Each row group is laid out on its own, so that no cell spans into the next one. An
        // empty row still counts, as a row a cell above may span, and is left out only when
        // nothing covers it.
        let data_first = t.data_first;
        let mut rows = Vec::new();
        let mut cells = t.rows.into_iter();
        let mut at = 0;
        let mut ends = t.groups;
        ends.push(usize::MAX);
        // One budget for the whole table, so that many row groups cannot add up past it.
        let mut budget = MAX_TABLE_CELLS;
        for end in ends {
            if end <= at {
                continue;
            }
            let group: Vec<_> = cells.by_ref().take(end - at).collect();
            at += group.len();
            rows.extend(
                lay_out_within(group, &mut budget)
                    .into_iter()
                    .filter(|r| !r.is_empty()),
            );
        }
        let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
        // Every row is written out to the widest one: past the budget, the rest is left out.
        rows.truncate(MAX_TABLE_CELLS / cols.max(1));
        if cols == 0
            || rows.iter().all(|r| {
                r.iter()
                    .all(|c| c.is_empty() || c == COVERED_LEFT || c == COVERED_ABOVE)
            })
        {
            return;
        }
        // A Markdown table's first row is its header, and a merged cell in it makes the rows it
        // reaches header rows too. When those are data rows, an empty header goes above them.
        if data_first
            && rows
                .get(1)
                .is_some_and(|r| r.iter().any(|c| c == COVERED_ABOVE))
        {
            rows.insert(0, Vec::new());
        }
        let line = |cells: &mut dyn Iterator<Item = &str>| {
            let mut s = String::from("|");
            for c in cells {
                s.push(' ');
                s.push_str(c);
                s.push_str(" |");
            }
            s
        };
        let mut out = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            let mut cells = (0..cols).map(|k| r.get(k).map_or("", String::as_str));
            out.push(line(&mut cells));
            if i == 0 {
                out.push(line(&mut (0..cols).map(|_| "---")));
            }
        }
        self.emit(&out.join("\n"));
    }

    fn start_tag(&mut self, name: &str, attrs: &str) {
        if self.table.as_ref().is_some_and(|t| t.nested > 0) {
            match name {
                "table" => self.table.as_mut().expect("table").nested += 1,
                "td" | "th" | "tr" | "br" => self.space(),
                _ => {}
            }
            return;
        }
        if self.pre.is_some() {
            match name {
                "br" => self.text("\n"),
                "code" => {
                    if let Some((text, lang)) = &mut self.pre {
                        if text.trim().is_empty() && lang.is_empty() {
                            *lang = code_language(attrs);
                        }
                    }
                }
                _ => {}
            }
            return;
        }
        let in_table = self.table.is_some();
        match name {
            "base" => {
                if let Some(href) = attr(attrs, "href").filter(|_| !self.based) {
                    self.based = true;
                    if let Ok(url) = Url::parse(&self.resolve(href)) {
                        self.base = Some(url);
                    }
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !in_table => {
                self.flush();
                self.heading = Some(usize::from(name.as_bytes()[1] - b'0'));
            }
            "ul" | "ol" if !in_table => {
                self.flush();
                if self.ctx.len() >= MAX_NESTING {
                    self.deep_lists += 1;
                    return;
                }
                let next = attr(attrs, "start")
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(1);
                self.ctx.push(Ctx::List {
                    ordered: name == "ol",
                    next,
                });
            }
            "li" if !in_table => {
                self.flush();
                if self.deep_lists > 0 || self.ctx.len() >= MAX_NESTING {
                    return;
                }
                // An item left open ends where the next one starts.
                if matches!(self.ctx.last(), Some(Ctx::Item(_))) {
                    self.ctx.pop();
                }
                let list = self.ctx.iter_mut().rev().find_map(|c| match c {
                    Ctx::List { ordered, next } => Some((ordered, next)),
                    _ => None,
                });
                let marker = match list {
                    Some((true, next)) => {
                        let n = attr(attrs, "value")
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(*next);
                        *next = n + 1;
                        format!("{n}. ")
                    }
                    _ => "- ".to_string(),
                };
                self.ctx.push(Ctx::Item(Some(marker)));
            }
            "blockquote" if !in_table => {
                self.flush();
                if self.ctx.len() >= MAX_NESTING {
                    self.deep_quotes += 1;
                } else {
                    self.ctx.push(Ctx::Quote);
                }
            }
            "pre" if !in_table => {
                self.flush();
                self.pre = Some((String::new(), code_language(attrs)));
            }
            "hr" if !in_table => {
                self.flush();
                self.emit("---");
            }
            "table" => {
                if let Some(t) = &mut self.table {
                    t.nested += 1;
                    self.space();
                } else {
                    self.flush();
                    self.table = Some(Table::default());
                }
            }
            "caption" if in_table => self.start_cell(true),
            "thead" | "tbody" | "tfoot" if in_table => {
                self.end_cell();
                if let Some(t) = &mut self.table {
                    t.groups.push(t.rows.len());
                    t.in_head = name == "thead";
                }
            }
            "tr" if in_table => {
                self.end_cell();
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            "td" | "th" if in_table => {
                self.start_cell(false);
                // `rowspan="0"` reaches to the end of the row group.
                let span = |name, zero, most| {
                    attr(attrs, name)
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .map_or(1, |n| if n == 0 { zero } else { n.min(most) })
                };
                if let Some(t) = &mut self.table {
                    t.span = (
                        span("rowspan", usize::MAX, 65_534),
                        span("colspan", 1, 1_000),
                    );
                    if name == "td" && t.rows.len() == 1 && !t.in_head {
                        t.data_first = true;
                    }
                }
            }
            "br" => {
                if in_table || self.heading.is_some() {
                    self.space();
                } else if !self.line.trim().is_empty() && !self.line.ends_with("\\\n") {
                    let end = self.line.trim_end_matches(' ').len();
                    self.line.truncate(end);
                    self.line.push_str("\\\n");
                }
            }
            "img" => self.image(attrs),
            "a" => {
                let href = attr(attrs, "href").filter(|h| !h.trim().is_empty());
                if let (Some(href), false) = (href, self.in_link()) {
                    if let Some(url) = self.anchor_to(href) {
                        self.open_mark(name, Mark::Link(url));
                    }
                }
            }
            "strong" | "b" => self.open_mark(name, Mark::Strong),
            "em" | "i" | "cite" | "dfn" | "var" => self.open_mark(name, Mark::Em),
            "del" | "s" | "strike" => self.open_mark(name, Mark::Strike),
            "code" | "kbd" | "samp" | "tt" => self.open_mark(name, Mark::Code),
            "dt" | "summary" => {
                self.flush();
                self.open_mark(name, Mark::Strong);
            }
            _ if BLOCK.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn end_tag(&mut self, name: &str) {
        if let Some(t) = &mut self.table {
            if t.nested > 0 {
                if name == "table" {
                    t.nested -= 1;
                }
                return;
            }
        }
        if self.pre.is_some() {
            if name == "pre" {
                self.end_pre();
            }
            return;
        }
        let in_table = self.table.is_some();
        match name {
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" if !in_table => {
                self.flush();
                self.heading = None;
            }
            "ul" | "ol" if !in_table => {
                self.flush();
                if self.deep_lists > 0 {
                    self.deep_lists -= 1;
                } else {
                    self.pop_to(|c| matches!(c, Ctx::List { .. }));
                }
            }
            "li" if !in_table => {
                self.flush();
                // Only an item of the innermost list ends: one past the depth was never opened.
                let item = self.ctx.iter().rposition(|c| matches!(c, Ctx::Item(_)));
                let list = self.ctx.iter().rposition(|c| matches!(c, Ctx::List { .. }));
                if let Some(i) = item.filter(|&i| self.deep_lists == 0 && list < Some(i)) {
                    self.ctx.truncate(i);
                }
            }
            "blockquote" if !in_table => {
                self.flush();
                if self.deep_quotes > 0 {
                    self.deep_quotes -= 1;
                } else {
                    self.pop_to(|c| matches!(c, Ctx::Quote));
                }
            }
            "table" => self.end_table(),
            "td" | "th" | "caption" if in_table => self.end_cell(),
            "tr" if in_table => self.end_cell(),
            "thead" | "tbody" | "tfoot" if in_table => {
                self.end_cell();
                if let Some(t) = &mut self.table {
                    t.groups.push(t.rows.len());
                    t.in_head = false;
                }
            }
            "dt" | "summary" => {
                self.close_mark(name);
                self.flush();
            }
            _ if self.open.iter().any(|o| o.tag == name) => self.close_mark(name),
            _ if BLOCK.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn end_pre(&mut self) {
        let Some((text, lang)) = self.pre.take() else {
            return;
        };
        // A browser leaves out the line break right after `<pre>`.
        let text = text.strip_prefix('\n').unwrap_or(&text);
        let text = text.trim_end();
        if text.trim().is_empty() {
            return;
        }
        let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
        let fence = "`".repeat(longest.max(2) + 1);
        self.emit(&format!("{fence}{lang}\n{text}\n{fence}"));
    }

    /// An image, written as a link to it: Markdown images would need loading to be shown.
    fn image(&mut self, attrs: &str) {
        let alt = attr(attrs, "alt").map(decode).unwrap_or_default();
        let alt = alt.split_whitespace().collect::<Vec<_>>().join(" ");
        let src = attr(attrs, "src").map(str::trim).unwrap_or_default();
        let url = (!self.in_link() && !self.in_code())
            .then(|| self.link_to(src))
            .flatten();
        let Some(url) = url.filter(|_| !src.is_empty()) else {
            self.text(&alt);
            return;
        };
        let name = if alt.is_empty() {
            Url::parse(&url)
                .ok()
                .and_then(|u| {
                    u.path_segments()
                        .and_then(|mut s| s.next_back().map(str::to_string))
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| url.clone())
        } else {
            alt
        };
        self.space();
        self.open_mark("img", Mark::Link(url));
        self.text(&name);
        self.close_mark("img");
    }
}

fn code_language(attrs: &str) -> String {
    attr(attrs, "class")
        .and_then(|c| {
            c.split_whitespace().find_map(|c| {
                c.strip_prefix("language-")
                    .or_else(|| c.strip_prefix("lang-"))
            })
        })
        .filter(|l| {
            l.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-_#.".contains(c))
        })
        .unwrap_or_default()
        .to_string()
}

/// The page `html` as Markdown blocks, in the order they appear. Links and images are made
/// absolute against `base`, the page's address. Hidden elements are left out.
pub fn html_to_blocks(html: &str, base: Option<&Url>) -> Vec<Block> {
    convert_html(html, base, None, &std::sync::atomic::AtomicBool::new(false)).unwrap_or_default()
}

/// Converts HTML for Formatted view. Same-page links compare by fragment.
pub fn formatted_html_to_blocks(
    html: &str,
    page: &Url,
    abort: &std::sync::atomic::AtomicBool,
) -> Option<Vec<Block>> {
    convert_html(html, Some(page), Some(page), abort)
}

fn convert_html(
    html: &str,
    base: Option<&Url>,
    document_url: Option<&Url>,
    abort: &std::sync::atomic::AtomicBool,
) -> Option<Vec<Block>> {
    let mut c = Converter {
        base: base.cloned(),
        document_url: document_url.cloned(),
        based: false,
        blocks: Vec::new(),
        line: String::new(),
        ctx: Vec::new(),
        deep_quotes: 0,
        deep_lists: 0,
        open: Vec::new(),
        heading: None,
        pre: None,
        table: None,
        skip: Vec::new(),
    };
    // The elements open outside the elements left out, outermost first.
    let mut opened = OpenElements::default();
    for (n, tok) in tokens(html).into_iter().enumerate() {
        if n % 4096 == 0 && abort.load(std::sync::atomic::Ordering::Relaxed) {
            return None;
        }
        if !c.skip.is_empty() && skipped(&mut c.skip, &tok, |name| opened.contains(name)) {
            // the page's `<base>` is in its `<head>`, which is otherwise left out
            if let Token::Tag(name, attrs, false) = &tok {
                if name == "base" && c.skip == ["head"] {
                    c.start_tag(name, attrs);
                }
            }
            continue;
        }
        match tok {
            Token::Text(t) => c.text(t),
            Token::Tag(name, attrs, false) => {
                if SKIP.contains(&name.as_str()) || is_hidden(attrs) {
                    if !VOID.contains(&name.as_str()) && !attrs.trim_end().ends_with('/') {
                        c.skip.push(name);
                    }
                    continue;
                }
                c.start_tag(&name, attrs);
                opened.start(name, attrs);
            }
            Token::Tag(name, _, true) => {
                opened.end(&name);
                c.end_tag(&name);
            }
        }
    }
    c.end_pre();
    c.end_table();
    c.flush();
    Some(c.blocks)
}

/// The tokens of the text and elements a page shows: hidden elements and elements without text
/// to show, such as `<head>` and `<select>`, are left out as in [`html_to_blocks`].
pub(super) fn visible_tokens(html: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut skip: Vec<String> = Vec::new();
    let mut opened = OpenElements::default();
    for tok in tokens(html) {
        if !skip.is_empty() && skipped(&mut skip, &tok, |name| opened.contains(name)) {
            continue;
        }
        match &tok {
            Token::Text(_) => {}
            Token::Tag(name, attrs, false) => {
                if SKIP.contains(&name.as_str()) || is_hidden(attrs) {
                    if !VOID.contains(&name.as_str()) && !attrs.trim_end().ends_with('/') {
                        skip.push(name.clone());
                    }
                    continue;
                }
                opened.start(name.clone(), attrs);
            }
            Token::Tag(name, _, true) => opened.end(name),
        }
        out.push(tok);
    }
    out
}

/// Elements open outside the elements left out, as far as [`MAX_OPEN_DEPTH`] deep.
#[derive(Default)]
struct OpenElements {
    /// Outermost first.
    stack: Vec<String>,
    /// How many of each name are on the stack.
    counts: std::collections::HashMap<String, usize>,
}

/// Elements nested deeper than this are not followed; a page cannot make each tag cost more.
const MAX_OPEN_DEPTH: usize = 1024;

/// The most quotes, lists and list items one block is shown inside: each adds to the start of
/// every line of the blocks in it, so a page of many unclosed ones would otherwise grow with
/// their number squared.
const MAX_NESTING: usize = 32;

impl OpenElements {
    fn contains(&self, name: &str) -> bool {
        self.counts.get(name).is_some_and(|&n| n > 0)
    }

    fn pop(&mut self) {
        if let Some(name) = self.stack.pop() {
            if let Some(n) = self.counts.get_mut(&name) {
                *n -= 1;
            }
        }
    }

    /// The start tag `name`, which first ends the elements it cannot be inside.
    fn start(&mut self, name: String, attrs: &str) {
        while self.stack.last().is_some_and(|o| ended_by(o, &name)) {
            self.pop();
        }
        if !VOID.contains(&name.as_str())
            && !attrs.trim_end().ends_with('/')
            && self.stack.len() < MAX_OPEN_DEPTH
        {
            *self.counts.entry(name.clone()).or_default() += 1;
            self.stack.push(name);
        }
    }

    /// The end tag `name`, which ends the elements inside its element; a stray one ends nothing.
    fn end(&mut self, name: &str) {
        if self.contains(name) {
            while let Some(top) = self.stack.last() {
                let done = top == name;
                self.pop();
                if done {
                    break;
                }
            }
        }
    }
}

/// Markdown blocks as one text, for comparing line by line: nested blocks are indented.
pub fn to_text(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        for line in b.text.lines() {
            out.push_str(&"  ".repeat(b.depth));
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// One aligned row of the compared pages: the block each page shows there, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Option<usize>>,
    /// Whether the pages differ here.
    pub changed: bool,
}

/// What kind of block it is, and how deep: changed blocks are only shown side by side with
/// blocks of the same kind. The last value is a heading's level.
type Kind = (usize, &'static str, usize);

fn kind(b: &Block) -> Kind {
    let t = b.text.trim_start_matches("> ");
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let level = t.len() - t.trim_start_matches('#').len();
    let kind = if level > 0 {
        "heading"
    } else if t.starts_with("- ") || (digits > 0 && t[digits..].starts_with(". ")) {
        "item"
    } else if t.starts_with('|') {
        "table"
    } else if t.starts_with("```") {
        "code"
    } else if t == "---" {
        "rule"
    } else if b.text.starts_with('>') {
        "quote"
    } else {
        "text"
    };
    (b.depth, kind, level)
}

/// The kind of block, whatever the level of a heading.
fn any_level(b: &Block) -> Kind {
    let (depth, kind, _) = kind(b);
    (depth, kind, 0)
}

/// Pairs the changed blocks `gone` of `a` and `new` of `b` side by side where the first of
/// `keys` gives them the same kind, then the rest by the next key. What is left faces a gap.
fn pair_changed(
    (a, gone): (&[Block], &[usize]),
    (b, new): (&[Block], &[usize]),
    keys: &[fn(&Block) -> Kind],
    out: &mut Vec<(Option<usize>, Option<usize>)>,
) {
    let Some((key, rest)) = keys.split_first() else {
        out.extend(gone.iter().map(|&i| (Some(i), None)));
        out.extend(new.iter().map(|&j| (None, Some(j))));
        return;
    };
    let ka: Vec<_> = gone.iter().map(|&i| key(&a[i])).collect();
    let kb: Vec<_> = new.iter().map(|&j| key(&b[j])).collect();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    for op in similar::capture_diff_slices(Algorithm::Myers, &ka, &kb) {
        let (tag, ra, rb) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            pair_changed((a, &left), (b, &right), rest, out);
            left.clear();
            right.clear();
            out.extend(ra.zip(rb).map(|(i, j)| (Some(gone[i]), Some(new[j]))));
        } else {
            left.extend(ra.map(|i| gone[i]));
            right.extend(rb.map(|j| new[j]));
        }
    }
    pair_changed((a, &left), (b, &right), rest, out);
}

/// Pairs the blocks of two pages: equal ones side by side, changed ones side by side with
/// changed ones of the same kind, and the rest against a gap.
fn pair(a: &[Block], b: &[Block]) -> Vec<(Option<usize>, Option<usize>)> {
    const KEYS: [fn(&Block) -> Kind; 2] = [kind, any_level];
    let ops = similar::capture_diff_slices(Algorithm::Myers, a, b);
    let (mut out, mut gone, mut new) = (Vec::new(), Vec::new(), Vec::new());
    for op in ops {
        let (tag, ra, rb) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            pair_changed((a, &gone), (b, &new), &KEYS, &mut out);
            gone.clear();
            new.clear();
            out.extend(ra.zip(rb).map(|(i, j)| (Some(i), Some(j))));
        } else {
            gone.extend(ra);
            new.extend(rb);
        }
    }
    pair_changed((a, &gone), (b, &new), &KEYS, &mut out);
    out
}

/// Lines up the blocks of two or three pages. A third page is lined up with the second.
pub fn align(pages: &[Vec<Block>]) -> Vec<Row> {
    let mut rows: Vec<Vec<Option<usize>>> = match pages {
        [] => return Vec::new(),
        [only] => (0..only.len()).map(|i| vec![Some(i)]).collect(),
        [a, b, ..] => pair(a, b).into_iter().map(|(i, j)| vec![i, j]).collect(),
    };
    for k in 2..pages.len() {
        let pairs = pair(&pages[k - 1], &pages[k]);
        let mut next = pairs.iter().peekable();
        let mut merged = Vec::new();
        for row in rows {
            if let Some(j) = row[k - 1] {
                // Blocks only the new page has, before the one lined up with `j`.
                while let Some(&&(None, c)) = next.peek() {
                    let mut r = vec![None; k];
                    r.push(c);
                    merged.push(r);
                    next.next();
                }
                let c = match next.peek() {
                    Some(&&(Some(p), c)) if p == j => {
                        next.next();
                        c
                    }
                    _ => None,
                };
                let mut row = row;
                row.push(c);
                merged.push(row);
            } else {
                let mut row = row;
                row.push(None);
                merged.push(row);
            }
        }
        for &(_, c) in next {
            let mut r = vec![None; k];
            r.push(c);
            merged.push(r);
        }
        rows = merged;
    }
    rows.into_iter()
        .map(|cells| {
            let first = cells[0].map(|i| &pages[0][i]);
            let changed = cells
                .iter()
                .enumerate()
                .any(|(p, c)| c.map(|i| &pages[p][i]) != first || c.is_none());
            Row { cells, changed }
        })
        .collect()
}

/// The cells of a table block, row by row with the header row first, each one Markdown that
/// can be shown on its own. `None` when the block is not a table.
pub fn table_cells(text: &str) -> Option<Vec<Vec<String>>> {
    let lines: Vec<&str> = text.lines().collect();
    let rule = lines.get(1)?;
    if !lines.iter().all(|l| l.starts_with('|') && l.ends_with('|'))
        || !split_row(rule).iter().all(|c| c == "---")
    {
        return None;
    }
    Some(
        lines
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != 1)
            .map(|(_, l)| split_row(l).iter().map(|c| escape_line_start(c)).collect())
            .collect(),
    )
}

/// The cells of a row of a pipe table.
fn split_row(line: &str) -> Vec<String> {
    let inner = line.strip_prefix('|').unwrap_or(line);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut slashes = 0;
    for c in inner.chars() {
        if c == '|' {
            if slashes % 2 == 1 {
                // Outside a table the `|` needs no escape, and within code it would show.
                cell.pop();
                cell.push('|');
            } else {
                cells.push(cell.trim().to_string());
                cell.clear();
            }
            slashes = 0;
            continue;
        }
        slashes = if c == '\\' { slashes + 1 } else { 0 };
        cell.push(c);
    }
    cells.push(cell.trim().to_string());
    cells
}

/// Written in a table cell that a merged cell to its left covers, as in MultiMarkdown.
pub const COVERED_LEFT: &str = "<";
/// Written in a table cell that a merged cell above it covers.
pub const COVERED_ABOVE: &str = "^";

/// The most columns a table with merged cells is laid out to.
pub const MAX_TABLE_COLUMNS: usize = 1_024;

/// The most cells a table with merged cells is laid out to, the ones merged cells cover
/// included: past it, a cell no longer spans other cells, so that a few cells with large spans
/// cannot fill memory.
pub const MAX_TABLE_CELLS: usize = 1 << 20;

/// A cell of a table, as Markdown, with how many rows and columns it spans.
pub struct SpanCell {
    pub text: String,
    pub rows: usize,
    pub cols: usize,
}

/// Lays out rows of cells, each placed in the first column not yet covered by a cell above it
/// that spans rows, as a cell for every column of every row: the cells a merged cell covers are
/// written as [`COVERED_ABOVE`] below it and [`COVERED_LEFT`] elsewhere. A span reaching past
/// the last row, or past [`MAX_TABLE_COLUMNS`], stops there; once the table holds
/// [`MAX_TABLE_CELLS`], cells no longer span, and past that the rest is left out.
pub fn lay_out(rows: Vec<Vec<SpanCell>>) -> Vec<Vec<String>> {
    lay_out_within(rows, &mut MAX_TABLE_CELLS.clone())
}

/// [`lay_out`] with `budget` cells left, for the row groups of one table to share; the cells
/// laid out are taken from it.
pub fn lay_out_within(rows: Vec<Vec<SpanCell>>, budget: &mut usize) -> Vec<Vec<String>> {
    let count = rows.len();
    let mut grid: Vec<Vec<Option<String>>> = vec![Vec::new(); count];
    'rows: for (r, row) in rows.into_iter().enumerate() {
        let mut c = 0;
        for cell in row {
            while grid[r].get(c).is_some_and(Option::is_some) {
                c += 1;
            }
            if c >= MAX_TABLE_COLUMNS {
                break;
            }
            let mut span_rows = cell.rows.clamp(1, count - r);
            let mut span_cols = cell.cols.clamp(1, MAX_TABLE_COLUMNS - c);
            let grow = |rows: usize, cols: usize| -> usize {
                grid[r..r + rows]
                    .iter()
                    .map(|line| (c + cols).saturating_sub(line.len()))
                    .sum()
            };
            // A span whose area alone is past the budget does not span, without measuring it:
            // measuring every such span again would take time growing with rows times rows.
            let fits = span_rows.saturating_mul(span_cols) <= *budget;
            let mut more = if fits {
                grow(span_rows, span_cols)
            } else {
                usize::MAX
            };
            if more > *budget {
                (span_rows, span_cols) = (1, 1);
                more = grow(1, 1);
                if more > *budget {
                    break 'rows;
                }
            }
            *budget -= more;
            let mut text = Some(cell.text);
            for (dr, line) in grid[r..r + span_rows].iter_mut().enumerate() {
                if line.len() < c + span_cols {
                    line.resize(c + span_cols, None);
                }
                for (dc, slot) in line[c..c + span_cols].iter_mut().enumerate() {
                    *slot = Some(match (dr, dc) {
                        (0, 0) => text.take().unwrap_or_default(),
                        (_, 0) => COVERED_ABOVE.to_string(),
                        _ => COVERED_LEFT.to_string(),
                    });
                }
            }
            c += span_cols;
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().map(Option::unwrap_or_default).collect())
        .collect()
}

/// A cell's Markdown with a lone `<` or `^` escaped, so it is not taken for a merged cell.
pub fn escape_cell(text: &str) -> String {
    if text == COVERED_LEFT || text == COVERED_ABOVE {
        format!("\\{text}")
    } else {
        text.to_string()
    }
}

/// Where a table cell is, when its cells may be merged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    /// The first cell of a merged area this many columns wide and rows tall (1 and 1 when it
    /// is not merged), whose text is shown across the whole area.
    Anchor { cols: usize, rows: usize },
    /// A cell that the merged area starting at this row and column covers.
    Covered { row: usize, col: usize },
}

/// The merged areas of a table's `cells` (from [`table_cells`]): a cell that is
/// [`COVERED_LEFT`] belongs to the area of the cell left of it, one that is [`COVERED_ABOVE`]
/// to the area of the cell above it. A marker with no cell to belong to is text.
pub fn spans<S: AsRef<str>>(cells: &[Vec<S>]) -> Vec<Vec<Slot>> {
    let mut owner: Vec<Vec<(usize, usize)>> = Vec::with_capacity(cells.len());
    for (r, row) in cells.iter().enumerate() {
        let mut line: Vec<(usize, usize)> = Vec::with_capacity(row.len());
        for (k, text) in row.iter().enumerate() {
            let above = r.checked_sub(1).and_then(|up| owner[up].get(k).copied());
            let o = match text.as_ref() {
                COVERED_LEFT if k > 0 => line[k - 1],
                COVERED_ABOVE if above.is_some() => above.unwrap_or((r, k)),
                _ => (r, k),
            };
            line.push(o);
        }
        owner.push(line);
    }
    // Each area reaches as far right and down as the cells it owns.
    let mut reach: std::collections::HashMap<(usize, usize), (usize, usize)> =
        std::collections::HashMap::new();
    for (r, line) in owner.iter().enumerate() {
        for (k, &o) in line.iter().enumerate() {
            let e = reach.entry(o).or_insert((r, k));
            *e = (e.0.max(r), e.1.max(k));
        }
    }
    owner
        .iter()
        .enumerate()
        .map(|(r, line)| {
            line.iter()
                .enumerate()
                .map(|(k, &(ar, ak))| {
                    if (ar, ak) == (r, k) {
                        let (er, ek) = reach[&(r, k)];
                        Slot::Anchor {
                            cols: ek - k + 1,
                            rows: er - r + 1,
                        }
                    } else {
                        Slot::Covered { row: ar, col: ak }
                    }
                })
                .collect()
        })
        .collect()
}

/// What differs within a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Marks {
    /// Byte ranges of the block's text.
    Text(Vec<Range<usize>>),
    /// The cells of a table, row by row, with what differs in each.
    Table(Vec<Vec<(String, Cell)>>),
}

/// What differs within a table cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Byte ranges of the cell's text; none when it is the same.
    Words(Vec<Range<usize>>),
    /// A paired cell differs, with changed characters only on the other side.
    PairedChange,
    /// The whole cell, in a row or column the other page does not have.
    Whole,
}

impl Cell {
    fn add(&mut self, more: Vec<Range<usize>>) {
        match self {
            Cell::Words(v) if more.is_empty() && v.is_empty() => *self = Cell::PairedChange,
            Cell::Words(v) => add_ranges(v, more),
            Cell::PairedChange if !more.is_empty() => {
                *self = Cell::Words(Vec::new());
                self.add(more);
            }
            _ => {}
        }
    }
}

/// Adds `more` to the sorted ranges `v`, joining those that overlap or touch.
fn add_ranges(v: &mut Vec<Range<usize>>, more: Vec<Range<usize>>) {
    if more.is_empty() {
        return;
    }
    v.extend(more);
    v.sort_by_key(|r| r.start);
    let mut out: Vec<Range<usize>> = Vec::with_capacity(v.len());
    for r in v.drain(..) {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    *v = out;
}

/// What differs within the blocks of each aligned row, page by page: words or characters, as
/// `opts` asks for the text compare. Each page is compared with the pages next to it. Tables
/// are always split into cells, so they can be drawn as a grid.
pub fn marks(pages: &[Vec<Block>], rows: &[Row], opts: &DiffOptions) -> Vec<Vec<Option<Marks>>> {
    rows.iter()
        .map(|row| {
            let texts: Vec<Option<&str>> = row
                .cells
                .iter()
                .enumerate()
                .map(|(p, c)| c.map(|i| pages[p][i].text.as_str()))
                .collect();
            let mut marks: Vec<Option<Marks>> = texts
                .iter()
                .map(|t| {
                    t.map(|t| match table_cells(t) {
                        Some(rows) => Marks::Table(
                            rows.into_iter()
                                .map(|r| {
                                    r.into_iter()
                                        .map(|c| (c, Cell::Words(Vec::new())))
                                        .collect()
                                })
                                .collect(),
                        ),
                        None => Marks::Text(Vec::new()),
                    })
                })
                .collect();
            if row.changed {
                for p in 1..marks.len() {
                    let (before, after) = marks.split_at_mut(p);
                    let (Some(a), Some(b), Some(ta), Some(tb)) =
                        (&mut before[p - 1], &mut after[0], texts[p - 1], texts[p])
                    else {
                        continue;
                    };
                    match (a, b) {
                        (Marks::Text(ra), Marks::Text(rb)) => {
                            let (x, y) = inline_diff(ta, tb, opts);
                            add_ranges(ra, x);
                            add_ranges(rb, y);
                        }
                        (Marks::Table(a), Marks::Table(b)) => mark_tables(a, b, opts),
                        _ => {}
                    }
                }
            }
            marks
        })
        .collect()
}

type TableCells = [Vec<(String, Cell)>];

/// Marks what differs between two tables: rows are lined up, equal ones side by side, then
/// changed ones with the same first cell, then the rest in order; the cells of rows side by
/// side are compared column by column.
fn mark_tables(a: &mut TableCells, b: &mut TableCells, opts: &DiffOptions) {
    // Cell backgrounds and row markers still need a comparison when word highlights are off.
    let without_inline = opts.inline == super::diff::InlineMode::Off;
    let comparison = without_inline.then(|| DiffOptions {
        inline: super::diff::InlineMode::Word,
        ..opts.clone()
    });
    let compare_opts = comparison.as_ref().unwrap_or(opts);
    let key = |r: &Vec<(String, Cell)>| -> Vec<String> { r.iter().map(|c| c.0.clone()).collect() };
    let ka: Vec<_> = a.iter().map(key).collect();
    let kb: Vec<_> = b.iter().map(key).collect();
    let mut pairs = Vec::new();
    let (mut gone, mut new) = (Vec::new(), Vec::new());
    for op in similar::capture_diff_slices(Algorithm::Myers, &ka, &kb) {
        let (tag, ra, rb) = op.as_tag_tuple();
        if tag == DiffTag::Equal {
            pair_rows(&ka, &kb, &gone, &new, &mut pairs);
            gone.clear();
            new.clear();
        } else {
            gone.extend(ra);
            new.extend(rb);
        }
    }
    pair_rows(&ka, &kb, &gone, &new, &mut pairs);
    for pair in pairs {
        match pair {
            (Some(i), Some(j)) => {
                for k in 0..a[i].len().max(b[j].len()) {
                    match (a[i].get_mut(k), b[j].get_mut(k)) {
                        (Some(x), Some(y)) if x.0 != y.0 => {
                            let (rx, ry) = inline_diff(&x.0, &y.0, compare_opts);
                            if !rx.is_empty() || !ry.is_empty() {
                                if without_inline {
                                    x.1.add(Vec::new());
                                    y.1.add(Vec::new());
                                } else {
                                    x.1.add(rx);
                                    y.1.add(ry);
                                }
                            }
                        }
                        (Some(x), None) => x.1 = Cell::Whole,
                        (None, Some(y)) => y.1 = Cell::Whole,
                        _ => {}
                    }
                }
            }
            (Some(i), None) => a[i].iter_mut().for_each(|c| c.1 = Cell::Whole),
            (None, Some(j)) => b[j].iter_mut().for_each(|c| c.1 = Cell::Whole),
            (None, None) => {}
        }
    }
}

/// Pairs the changed rows `gone` of one table with the changed rows `new` of another.
fn pair_rows(
    ka: &[Vec<String>],
    kb: &[Vec<String>],
    gone: &[usize],
    new: &[usize],
    out: &mut Vec<(Option<usize>, Option<usize>)>,
) {
    let first = |k: &[Vec<String>], i: usize| k[i].first().cloned().unwrap_or_default();
    let fa: Vec<String> = gone.iter().map(|&i| first(ka, i)).collect();
    let fb: Vec<String> = new.iter().map(|&j| first(kb, j)).collect();
    for op in similar::capture_diff_slices(Algorithm::Myers, &fa, &fb) {
        let (_, ra, rb) = op.as_tag_tuple();
        let (ra, rb): (Vec<_>, Vec<_>) =
            (ra.map(|i| gone[i]).collect(), rb.map(|j| new[j]).collect());
        for k in 0..ra.len().max(rb.len()) {
            out.push((ra.get(k).copied(), rb.get(k).copied()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn md(html: &str) -> String {
        let base = Url::parse("https://example.com/docs/page.html").unwrap();
        html_to_blocks(html, Some(&base))
            .iter()
            .map(|b| format!("{}{}", "  ".repeat(b.depth), b.text))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    #[test]
    fn headings_paragraphs_and_inline_formatting() {
        let html = "<html><head><title>T</title><style>p{}</style></head><body>\
            <h1>Main  title</h1><p>Some <b>bold</b>, <em> slanted </em> and <code>a*b</code> \
            text with <del>old</del> words.</p><h3>Sub</h3><p>Line one<br>line two</p></body>";
        assert_eq!(
            md(html),
            "# Main title\n\n\
             Some **bold**, *slanted* and `a*b` text with ~~old~~ words.\n\n\
             ### Sub\n\n\
             Line one\\\nline two"
        );
    }

    #[test]
    fn links_and_images_are_made_absolute() {
        let html = r#"<p><a href="../a b.html">A <i>link</i></a> <a href="javascript:x()">js</a>
            <a href="/x"></a><img src="img/logo.png" alt="Logo"> <img src="/i/pic.jpg">
            <img src="data:image/png;base64,AAA" alt="inline"></p>"#;
        assert_eq!(
            md(html),
            "[A *link*](https://example.com/a%20b.html) js \
             [Logo](https://example.com/docs/img/logo.png) \
             [pic.jpg](https://example.com/i/pic.jpg) inline"
        );
    }

    #[test]
    fn formatted_comparison_compares_same_page_links_by_fragment() {
        let abort = std::sync::atomic::AtomicBool::new(false);
        let left = Url::parse("file:///D:/reports/standard.html").unwrap();
        let right = Url::parse("file:///D:/reports/full.html").unwrap();
        let html = r##"<p><a href="#entry">Open</a></p>
            <table><tr><th>File</th></tr><tr><td><a href="#entry">Report</a></td></tr></table>
            <p>Standard detail</p>"##;
        let a = formatted_html_to_blocks(html, &left, &abort).unwrap();
        let b = formatted_html_to_blocks(
            &html.replace("Standard detail", "Full detail"),
            &right,
            &abort,
        )
        .unwrap();
        assert_eq!(a[0].text, "[Open](#entry)");
        let rows = align(&[a.clone(), b]);
        assert_eq!(
            rows.iter().map(|r| r.changed).collect::<Vec<_>>(),
            [false, false, true]
        );
        assert_eq!(
            html_to_blocks(html, Some(&left))[0].text,
            "[Open](file:///D:/reports/standard.html#entry)"
        );
        let changed =
            formatted_html_to_blocks(&html.replace("#entry", "#other"), &right, &abort).unwrap();
        assert!(align(&[a, changed])[0].changed);

        let elsewhere =
            r##"<head><base href="other.html"></head><p><a href="#entry">Open</a></p>"##;
        let blocks = formatted_html_to_blocks(elsewhere, &right, &abort).unwrap();
        assert_eq!(
            blocks[0].text,
            "[Open](file:///D:/reports/other.html#entry)"
        );
        let explicit = r#"<a href="standard.html#entry">Open</a>"#;
        let a = formatted_html_to_blocks(explicit, &left, &abort).unwrap();
        let b = formatted_html_to_blocks(explicit, &right, &abort).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].text, "[Open](file:///D:/reports/standard.html#entry)");

        for href in ["&#32;#entry", "&#9;&#10;#entry"] {
            let html = format!(r#"<a href="{href}">Open</a>"#);
            let a = formatted_html_to_blocks(&html, &left, &abort).unwrap();
            let b = formatted_html_to_blocks(&html, &right, &abort).unwrap();
            assert_eq!(a[0].text, "[Open](#entry)");
            assert!(!align(&[a, b])[0].changed);
        }
    }

    #[test]
    fn markdown_characters_in_text_are_escaped() {
        assert_eq!(
            md("<p>1. not a list *really* [x] a_b</p><p># no heading</p><p>- item?</p>"),
            "1\\. not a list \\*really\\* \\[x\\] a\\_b\n\n\\# no heading\n\n\\- item?"
        );
        // Text that reads like a character reference stays as it is shown.
        assert_eq!(
            md("<p>&amp;copy; &amp;#169; AT&amp;T &amp; &amp;x y; <code>&amp;lt;</code></p>"),
            "\\&copy; \\&#169; AT&T & &x y; `&lt;`"
        );
    }

    #[test]
    fn cells_that_span_rows_and_columns_are_written_as_markers() {
        let html = "<table><tr><th rowspan=2>Area</th><th colspan=\"2\">Revenue</th></tr>\
            <tr><th>Q2</th><th>Q3</th></tr>\
            <tr><td rowspan=\"0\">East</td><td>1</td><td>2</td></tr>\
            <tr><td colspan=2>Both</td></tr>\
            <tr><td>^</td><td>x</td></tr></table>";
        assert_eq!(
            md(html),
            "| Area | Revenue | < |\n| --- | --- | --- |\n| ^ | Q2 | Q3 |\n| East | 1 | 2 |\n\
             | ^ | Both | < |\n| ^ | \\^ | x |"
        );
    }

    #[test]
    fn a_cell_spans_rows_only_within_its_row_group() {
        let html = "<table><thead><tr><th rowspan=0>Region</th><th rowspan=3>P1</th></tr>\
            <tr></tr></thead>\
            <tbody><tr><td>North</td><td>4</td></tr></tbody></table>";
        assert_eq!(
            md(html),
            "| Region | P1 |\n| --- | --- |\n| ^ | ^ |\n| North | 4 |"
        );
    }

    #[test]
    fn an_empty_row_still_counts_for_the_cells_spanning_it() {
        let html = "<table><tr><td rowspan=3>A</td><td>1</td></tr><tr></tr>\
            <tr><td>2</td></tr><tr><td>x</td><td>y</td></tr></table>";
        assert_eq!(
            md(html),
            "|  |  |\n| --- | --- |\n| A | 1 |\n| ^ |  |\n| ^ | 2 |\n| x | y |"
        );
        // A row nothing covers is left out.
        assert_eq!(
            md("<table><tr><td>a</td></tr><tr></tr><tr><td>b</td></tr></table>"),
            "| a |\n| --- |\n| b |"
        );
    }

    #[test]
    fn large_spans_are_laid_out_within_a_bounded_number_of_cells() {
        let mut rows = vec![vec![SpanCell {
            text: "x".into(),
            rows: usize::MAX,
            cols: 1_000,
        }]];
        rows.extend((0..20_000).map(|_| {
            vec![SpanCell {
                text: "a".into(),
                rows: 1,
                cols: 1,
            }]
        }));
        let grid = lay_out(rows);
        let cells: usize = grid.iter().map(Vec::len).sum();
        assert!(cells <= MAX_TABLE_CELLS, "{cells}");
        // The span that did not fit is a single cell, and the rows are all still there.
        assert_eq!(grid.len(), 20_001);
        assert_eq!(grid[0], ["x"]);
        assert!(grid[1..].iter().all(|r| r == &["a"]));
    }

    #[test]
    fn a_table_of_many_groups_or_padded_rows_stays_within_its_budget() {
        // Many row groups each full of spanned cells share one budget.
        let group = format!(
            "<tbody><tr><td rowspan=0 colspan=1000>x</td></tr>{}</tbody>",
            "<tr></tr>".repeat(999)
        );
        let page = format!("<table>{}</table>", group.repeat(20));
        let text = md(&page);
        let cells: usize = text.lines().map(|l| l.matches(" |").count()).sum();
        assert!(cells <= 2 * MAX_TABLE_CELLS, "{cells}");
        // A wide first row and many narrow ones: every row is written as wide as the widest.
        let page = format!(
            "<table><tr><td colspan=1000>w</td></tr>{}</table>",
            "<tr><td>a</td></tr>".repeat(20_000)
        );
        let text = md(&page);
        assert!(text.len() < 8 * MAX_TABLE_CELLS, "{}", text.len());
        // Row spans to the end of a long group are not measured again for every row.
        let page = format!(
            "<table>{}</table>",
            "<tr><td rowspan=0>a</td></tr>".repeat(100_000)
        );
        let start = std::time::Instant::now();
        md(&page);
        assert!(start.elapsed() < std::time::Duration::from_secs(10));
    }

    #[test]
    fn deeply_nested_quotes_and_lists_stop_adding_to_each_line() {
        let page = "<blockquote>x".repeat(10_000);
        let text = md(&page);
        let widest = text.lines().map(str::len).max().unwrap_or(0);
        assert!(widest <= 2 * MAX_NESTING + 1, "{widest}");
        let page = "<ul><li>x".repeat(10_000);
        let blocks = html_to_blocks(&page, None);
        assert!(blocks.iter().all(|b| b.depth <= MAX_NESTING));
        // Lists past the depth do not end the ones around them.
        let page = format!(
            "<ul><li>a{}<li>b</li></ul>",
            "<ul><li>deep".repeat(40) + &"</li></ul>".repeat(40)
        );
        assert!(md(&page).ends_with("- b"), "{}", md(&page));
    }

    #[test]
    fn only_web_mail_and_local_file_links_are_kept() {
        let base = Url::parse("https://example.com/a/").unwrap();
        let link = |html: &str| html_to_blocks(html, Some(&base))[0].text.clone();
        assert_eq!(link("<a href='&#106;avascript:alert(1)'>x</a>"), "x");
        assert_eq!(link("<a href='java&#9;script:alert(1)'>x</a>"), "x");
        assert_eq!(link("<a href='file://host/share/x'>x</a>"), "x");
        assert_eq!(link("<a href='vbscript:x'>x</a>"), "x");
        assert_eq!(link("<img src='javascript:x' alt='pic'>"), "pic");
        assert_eq!(
            link("<a href='b.html'>x</a>"),
            "[x](https://example.com/a/b.html)"
        );
        assert_eq!(
            link("<a href='mailto:me@example.com'>x</a>"),
            "[x](mailto:me@example.com)"
        );
        // A line break in an address cannot start Markdown of its own.
        let text = link("<a href='https://exa mple.com/&#10;[y](https://evil)'>x</a>");
        assert!(
            !text.contains('\n') && !text.contains("](https://evil)"),
            "{text}"
        );
        // A page on this computer may link to its files.
        let local = Url::parse("file:///tmp/page.html").unwrap();
        let blocks = html_to_blocks("<a href='notes.txt'>n</a>", Some(&local));
        assert_eq!(blocks[0].text, "[n](file:///tmp/notes.txt)");
    }

    #[test]
    fn a_merged_data_cell_in_the_first_row_does_not_make_header_rows() {
        let text =
            md("<table><tr><td rowspan=2>2024</td><td>Jan</td></tr><tr><td>Feb</td></tr></table>");
        assert_eq!(text, "|  |  |\n| --- | --- |\n| 2024 | Jan |\n| ^ | Feb |");
        // Header cells keep their rows as the header.
        let text = md(
            "<table><thead><tr><th rowspan=2>Name</th><th>Q</th></tr><tr><th>Q1</th></tr>\
             </thead><tr><td>a</td><td>1</td></tr></table>",
        );
        assert!(text.starts_with("| Name | Q |"), "{text}");
    }

    #[test]
    fn merged_cells_are_read_from_their_markers() {
        let cells = |rows: &[&[&str]]| -> Vec<Vec<String>> {
            rows.iter()
                .map(|r| r.iter().map(|c| c.to_string()).collect())
                .collect()
        };
        // A 2 by 2 merged cell, and markers with nothing to belong to.
        let slots = spans(&cells(&[
            &["<", "A", "<"],
            &["^", "^", "^"],
            &["x", "^", "y"],
        ]));
        assert_eq!(slots[0][0], Slot::Anchor { cols: 1, rows: 2 });
        assert_eq!(slots[0][1], Slot::Anchor { cols: 2, rows: 3 });
        assert_eq!(slots[1][2], Slot::Covered { row: 0, col: 1 });
        assert_eq!(slots[2][1], Slot::Covered { row: 0, col: 1 });
        assert_eq!(slots[2][2], Slot::Anchor { cols: 1, rows: 1 });
        // A cell of a web page that only holds a marker character is text.
        let blocks = html_to_blocks(
            "<table><tr><td>a</td><td>^</td><td>&lt;</td></tr></table>",
            None,
        );
        let row = &table_cells(&blocks[0].text).unwrap()[0];
        assert_eq!(row, &["a", "\\^", "\\<"]);
        assert!(spans(std::slice::from_ref(row))[0]
            .iter()
            .all(|s| *s == Slot::Anchor { cols: 1, rows: 1 }));
    }

    #[test]
    fn lists_nest_and_number_their_items() {
        let html = "<ul><li>One</li><li>Two<ul><li>Two a</li></ul></li></ul>\
            <ol start=\"3\"><li>Three</li><li><p>Four</p><p>More of four</p></li></ol>";
        let blocks = html_to_blocks(html, None);
        let shown: Vec<(usize, &str)> = blocks.iter().map(|b| (b.depth, b.text.as_str())).collect();
        assert_eq!(
            shown,
            [
                (0, "- One"),
                (0, "- Two"),
                (1, "- Two a"),
                (0, "3. Three"),
                (0, "4. Four"),
                (1, "More of four"),
            ]
        );
    }

    #[test]
    fn quotes_code_blocks_and_rules() {
        let html = "<blockquote><p>Quoted <b>text</b></p></blockquote><hr>\
            <pre class=\"language-rust\">\nfn main() {\n    let a = 1 &lt; 2;\n}\n</pre>\
            <pre><code class=\"lang-js\">x = `a`</code></pre>";
        assert_eq!(
            md(html),
            "> Quoted **text**\n\n---\n\n\
             ```rust\nfn main() {\n    let a = 1 < 2;\n}\n```\n\n\
             ```js\nx = `a`\n```"
        );
    }

    #[test]
    fn tables_become_pipe_tables() {
        let html = "<table><caption>Prices</caption><thead><tr><th>Item</th><th>Cost</th></tr>\
            </thead><tbody><tr><td>A|B</td><td><p>1</p><p>2</p></td></tr>\
            <tr><td>Only <code>x|y</code></td></tr></tbody></table><p><code>x|y</code></p>";
        assert_eq!(
            md(html),
            "Prices\n\n\
             | Item | Cost |\n| --- | --- |\n| A\\|B | 1 2 |\n| Only `x\\|y` |  |\n\n\
             `x|y`"
        );
    }

    #[test]
    fn hidden_elements_are_left_out() {
        let html = r#"<p>Shown</p><div hidden><p>Gone</p><div><p>Gone too</p></div></div>
            <p style="display: none">No</p><span class="sr-only">Skip</span>
            <p aria-hidden="true">Icon</p><select><option>Pick</option></select>
            <p>After <img hidden src="x.png" alt="x"></p>"#;
        assert_eq!(md(html), "Shown\n\nAfter");
    }

    #[test]
    fn hidden_elements_end_where_a_browser_ends_them() {
        assert_eq!(md("<p hidden>Gone<p>Shown"), "Shown");
        assert_eq!(md("<p hidden>Gone <b>bold<div>Shown</div>"), "Shown");
        assert_eq!(md("<div><p hidden>Gone</div><p>After"), "After");
        assert_eq!(md("<ul><li hidden>Gone<li>b</ul><p>c"), "- b\n\nc");
        assert_eq!(md("<ul><li>a<li hidden>Gone</ul><p>After"), "- a\n\nAfter");
        assert_eq!(
            md("<ul><li hidden>Gone<ul><li>Gone too</ul></p><li>b</ul>"),
            "- b"
        );
        assert_eq!(
            md("<table><tr><td hidden>Gone<td>b<tr><td>c</table>"),
            md("<table><tr><td>b</td></tr><tr><td>c</td></tr></table>")
        );
        assert_eq!(
            md("<div hidden><p>Gone<li>Gone too</ul></div><p>After"),
            "After"
        );
        assert_eq!(md("<ul><li><p hidden>Gone<li>b</ul>"), "- b");
        assert_eq!(md("<ul><li hidden><div>Gone<li>b</ul>"), "- b");
        assert_eq!(md("<dl><dd hidden><p>Gone<dt>Term</dt></dl>"), "**Term**");
        // a stray end tag of an element that is not open does not end the hidden one
        assert_eq!(md("<p hidden>Gone</div>Still gone<p>Shown"), "Shown");
        // nor does one of an element that was closed when the element around it was
        assert_eq!(
            md("<table><tr><td>a<td>b</table><p hidden>Gone</td>Still gone<p>Shown"),
            format!("{}\n\nShown", md("<table><tr><td>a<td>b</table>"))
        );
        assert_eq!(
            md("<ul><li>a<li>b</ul><p hidden>Gone</li>Still gone<p>Shown"),
            "- a\n\n- b\n\nShown"
        );
        // many end tags against deep markup stay fast
        let stray = format!(
            "<p>a</p>{}{}",
            "<span>".repeat(60_000),
            "</i>".repeat(60_000)
        );
        let start = std::time::Instant::now();
        md(&stray);
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        let deep = format!("<div hidden>{}Gone</div><p>After", "<b>".repeat(10_000));
        assert!(!md(&deep).contains("Gone"));
    }

    #[test]
    fn the_first_base_of_the_head_resolves_links() {
        let html = r#"<html><head><base href="https://cdn.example.org/v2/"><base href="/no/">
            <title>T</title></head><body><a href="guide.html">Guide</a></body></html>"#;
        assert_eq!(md(html), "[Guide](https://cdn.example.org/v2/guide.html)");
    }

    #[test]
    fn unclosed_formatting_and_tags_do_not_break_the_output() {
        assert_eq!(md("<p>Open <b>bold<p>Next</p>"), "Open bold\n\nNext");
        assert_eq!(md("<ul><li>a<li>b</ul><p>c"), "- a\n\n- b\n\nc");
    }

    #[test]
    fn blocks_are_aligned_and_changes_marked() {
        let b = |t: &str| Block {
            depth: 0,
            text: t.into(),
        };
        let left = vec![b("same"), b("old"), b("end")];
        let right = vec![b("same"), b("new"), b("added"), b("end")];
        let rows = align(&[left.clone(), right.clone()]);
        let cells: Vec<(Vec<Option<usize>>, bool)> =
            rows.iter().map(|r| (r.cells.clone(), r.changed)).collect();
        assert_eq!(
            cells,
            [
                (vec![Some(0), Some(0)], false),
                (vec![Some(1), Some(1)], true),
                (vec![None, Some(2)], true),
                (vec![Some(2), Some(3)], false),
            ]
        );
        // Changed blocks face changed blocks of their kind.
        let kinds = |l: Vec<Block>, r: Vec<Block>| -> Vec<Vec<Option<usize>>> {
            align(&[l, r]).into_iter().map(|r| r.cells).collect()
        };
        assert_eq!(
            kinds(
                vec![b("| a |"), b("> quote"), b("```\nx\n```")],
                vec![b("| b |"), b("```\ny\n```")]
            ),
            [
                vec![Some(0), Some(0)],
                vec![Some(1), None],
                vec![Some(2), Some(1)],
            ]
        );
        // A third page lines up with the second.
        let third = vec![b("same"), b("new"), b("end"), b("extra")];
        let rows = align(&[left, right, third]);
        let cells: Vec<Vec<Option<usize>>> = rows.iter().map(|r| r.cells.clone()).collect();
        assert_eq!(
            cells,
            [
                vec![Some(0), Some(0), Some(0)],
                vec![Some(1), Some(1), Some(1)],
                vec![None, Some(2), None],
                vec![Some(2), Some(3), Some(2)],
                vec![None, None, Some(3)],
            ]
        );
        assert!(rows[1].changed && rows[2].changed && rows[4].changed && !rows[3].changed);
        // Headings face headings of their level first, then of any level.
        assert_eq!(
            kinds(
                vec![b("## Old"), b("#### Moved")],
                vec![b("### Added"), b("text"), b("## New"), b("### Moved")]
            ),
            [
                vec![None, Some(0)],
                vec![None, Some(1)],
                vec![Some(0), Some(2)],
                vec![Some(1), Some(3)],
            ]
        );
    }

    #[test]
    fn table_blocks_split_into_cells_that_stand_alone() {
        let t = "| Item | Cost |\n| --- | --- |\n| `x\\|y` \\| z | 1. one |\n| # |  |";
        assert_eq!(
            table_cells(t).unwrap(),
            [
                vec!["Item".to_string(), "Cost".into()],
                vec!["`x|y` | z".into(), "1\\. one".into()],
                vec!["\\#".into(), String::new()],
            ]
        );
        assert_eq!(table_cells("| a |"), None);
        assert_eq!(table_cells("> | a |\n> | --- |"), None);
        assert_eq!(table_cells("| a |\n| b |"), None);
    }

    #[test]
    fn changed_words_and_table_cells_are_marked() {
        let b = |t: &str| Block {
            depth: 0,
            text: t.into(),
        };
        let left = vec![
            b("Same"),
            b("Sync every **5** minutes"),
            b("| Plan | Price |\n| --- | --- |\n| Free | $0 |\n| Pro | $4 |\n| Team | $8 |"),
        ];
        let right = vec![
            b("Same"),
            b("Sync every **10** minutes"),
            b("| Plan | Price | Seats |\n| --- | --- | --- |\n| Free | $0 | 1 |\n| Family | $9 | 6 |\n| Pro | $5 | 1 |"),
        ];
        let pages = [left, right];
        let rows = align(&pages);
        let marks = marks(&pages, &rows, &DiffOptions::default());
        assert_eq!(
            marks[0],
            [Some(Marks::Text(vec![])), Some(Marks::Text(vec![]))]
        );
        // "5" and "10" between the bold marks.
        let (five, ten) = (13..14, 13..15);
        assert_eq!(
            marks[1],
            [Some(Marks::Text(vec![five])), Some(Marks::Text(vec![ten]))]
        );
        let (Some(Marks::Table(l)), Some(Marks::Table(r))) = (&marks[2][0], &marks[2][1]) else {
            panic!("tables expected: {:?}", marks[2]);
        };
        let shown = |t: &TableCells| -> Vec<Vec<String>> {
            t.iter()
                .map(|row| {
                    row.iter()
                        .map(|(text, cell)| match cell {
                            Cell::Whole => format!("[{text}]"),
                            Cell::PairedChange => text.clone(),
                            Cell::Words(w) if w.is_empty() => text.clone(),
                            Cell::Words(w) => {
                                let mut s = text.clone();
                                for m in w.iter().rev() {
                                    s.insert(m.end, '}');
                                    s.insert(m.start, '{');
                                }
                                s
                            }
                        })
                        .collect()
                })
                .collect()
        };
        // Rows with the same first cell face each other; the rest are whole rows.
        assert_eq!(
            shown(l),
            [
                vec!["Plan", "Price"],
                vec!["Free", "$0"],
                vec!["Pro", "${4}"],
                vec!["[Team]", "[$8]"],
            ]
        );
        assert_eq!(
            shown(r),
            [
                vec!["Plan", "Price", "[Seats]"],
                vec!["Free", "$0", "[1]"],
                vec!["[Family]", "[$9]", "[6]"],
                vec!["Pro", "${5}", "[1]"],
            ]
        );
        // Unchanged rows have no marks, and without word differences only blocks are marked.
        let off = DiffOptions {
            inline: crate::core::diff::InlineMode::Off,
            ..DiffOptions::default()
        };
        let marks = super::marks(&pages, &rows, &off);
        assert_eq!(
            marks[1],
            [Some(Marks::Text(vec![])), Some(Marks::Text(vec![]))]
        );
    }

    /// The changed rows of a sample pair in `samples/web/formatted`, as "left => right".
    fn sample_changes(left: &str, right: &str) -> Vec<String> {
        let pages = [html_to_blocks(left, None), html_to_blocks(right, None)];
        let side = |p: usize, c: Option<usize>| c.map_or("", |i| pages[p][i].text.as_str());
        align(&pages)
            .iter()
            .filter(|r| r.changed)
            .map(|r| format!("{} => {}", side(0, r.cells[0]), side(1, r.cells[1])))
            .collect()
    }

    #[test]
    fn formatted_samples_show_their_changes() {
        let text = sample_changes(
            include_str!("../../samples/web/formatted/text/left.html"),
            include_str!("../../samples/web/formatted/text/right.html"),
        );
        assert_eq!(text.len(), 16, "{text:#?}");
        assert!(text.contains(&"Notes sync in the background. Large attachments upload after the text. => Notes sync in the background.".to_string()));
        assert!(text
            .contains(&" => Large attachments upload after the text, one at a time.".to_string()));
        assert!(text
            .iter()
            .any(|c| c.contains("(https://example.com/migrate/v2)")
                && c.contains("(https://example.com/migrate/v3)")));

        let headings = sample_changes(
            include_str!("../../samples/web/formatted/headings/left.html"),
            include_str!("../../samples/web/formatted/headings/right.html"),
        );
        assert_eq!(headings.len(), 17, "{headings:#?}");
        for pair in [
            "## Getting started => ## Quick start",
            "#### Nested folders => ### Nested folders",
            "## Troubleshooting => ## Troubleshooting `sync`",
            " => ### Conflicts",
            "## Appendix: *keyboard* shortcuts => ",
        ] {
            assert!(
                headings.contains(&pair.to_string()),
                "{pair}: {headings:#?}"
            );
        }

        let tables = sample_changes(
            include_str!("../../samples/web/formatted/tables/left.html"),
            include_str!("../../samples/web/formatted/tables/right.html"),
        );
        assert_eq!(tables.len(), 7, "{tables:#?}");
        assert_eq!(tables[0], "Prices for 2026 => Prices from July 2026");
        assert!(tables[1].contains("| **Family** | $9 | 50 GB shared by 6 people | Unlimited |"));
        assert!(tables[2].ends_with("| Toggle sidebar | `Ctrl`+`\\` | `⌘`+`\\` | `Ctrl`+`\\` |"));
        assert!(tables[3].contains("| `q` | string | Words to find. Use `a\\|b` to match"));
        assert!(tables[4].contains("| 2.4 | To be announced | < |"));
        assert!(tables[6].contains("| Attachment size | Free 10 MB Paid 250 MB |"));
    }
}
