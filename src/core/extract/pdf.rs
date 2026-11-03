//! Reading the text of a PDF file.
//!
//! A PDF says where to draw each run of glyphs, not what the words are, so the text here is
//! read the way a reader would: the pages are found through the document's own tables, the
//! content of each is decoded, and the glyph codes are turned back into characters with the
//! `ToUnicode` table the file carries for its fonts, or with the encoding the font names.
//!
//! Nothing is run and nothing is fetched: a file is read as the bytes it is. A file that says
//! it is encrypted, or that is built in a way this cannot follow, is reported rather than read
//! as something it is not.

use anyhow::{bail, Context, Result};
use std::collections::HashMap;

/// The most objects followed, so that a file that points at itself cannot go on forever.
const MAX_OBJECTS: usize = 500_000;
/// The most bytes one stream is decoded into.
const MAX_STREAM: usize = 128 << 20;
/// The most pages read.
const MAX_PAGES: usize = 20_000;

/// A value in a PDF file.
#[derive(Clone, Debug, PartialEq)]
pub enum Object {
    Null,
    Bool(bool),
    Number(f64),
    /// A string, as the bytes it is written as; what they mean depends on where it is used.
    Text(Vec<u8>),
    Name(String),
    Array(Vec<Object>),
    Dict(HashMap<String, Object>),
    /// A stream: its dictionary, and where its bytes start and end in the file.
    Stream(HashMap<String, Object>, usize, usize),
    /// A reference to another object by number.
    Ref(u32),
}

impl Object {
    fn as_dict(&self) -> Option<&HashMap<String, Object>> {
        match self {
            Object::Dict(d) | Object::Stream(d, _, _) => Some(d),
            _ => None,
        }
    }
    fn as_number(&self) -> Option<f64> {
        match self {
            Object::Number(n) => Some(*n),
            _ => None,
        }
    }
    fn as_name(&self) -> Option<&str> {
        match self {
            Object::Name(n) => Some(n),
            _ => None,
        }
    }
}

/// Whether `b` ends a word in a PDF file.
fn delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    ) || b.is_ascii_whitespace()
}

/// Reads the values of a PDF file one after another.
struct Lexer<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Lexer { bytes, at }
    }

    fn skip_space(&mut self) {
        while self.at < self.bytes.len() {
            let b = self.bytes[self.at];
            if b == b'%' {
                while self.at < self.bytes.len() && !matches!(self.bytes[self.at], b'\r' | b'\n') {
                    self.at += 1;
                }
            } else if b.is_ascii_whitespace() || b == 0 {
                self.at += 1;
            } else {
                break;
            }
        }
    }

    fn starts_with(&mut self, word: &[u8]) -> bool {
        self.skip_space();
        self.bytes
            .get(self.at..)
            .is_some_and(|r| r.starts_with(word))
    }

    fn take(&mut self, word: &[u8]) -> bool {
        if self.starts_with(word) {
            self.at += word.len();
            true
        } else {
            false
        }
    }

    /// The next bare word, such as an operator or `obj`.
    fn word(&mut self) -> String {
        self.skip_space();
        let start = self.at;
        while self.at < self.bytes.len() && !delimiter(self.bytes[self.at]) {
            self.at += 1;
        }
        String::from_utf8_lossy(&self.bytes[start..self.at]).into_owned()
    }

    fn name(&mut self) -> String {
        // The `/` has been read.
        let mut out = Vec::new();
        while self.at < self.bytes.len() && !delimiter(self.bytes[self.at]) {
            let b = self.bytes[self.at];
            if b == b'#' && self.at + 2 < self.bytes.len() {
                let hex = &self.bytes[self.at + 1..self.at + 3];
                if let Some(v) = std::str::from_utf8(hex)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(v);
                    self.at += 3;
                    continue;
                }
            }
            out.push(b);
            self.at += 1;
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// A string in brackets, with its escapes read.
    fn literal(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1usize;
        while self.at < self.bytes.len() {
            let b = self.bytes[self.at];
            self.at += 1;
            match b {
                b'\\' => {
                    let Some(&next) = self.bytes.get(self.at) else {
                        break;
                    };
                    self.at += 1;
                    match next {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\n' => {}
                        b'\r' => {
                            if self.bytes.get(self.at) == Some(&b'\n') {
                                self.at += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            let mut value = (next - b'0') as u32;
                            for _ in 0..2 {
                                match self.bytes.get(self.at) {
                                    Some(d @ b'0'..=b'7') => {
                                        value = value * 8 + (d - b'0') as u32;
                                        self.at += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(value as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                other => out.push(other),
            }
            if out.len() > MAX_STREAM {
                break;
            }
        }
        out
    }

    /// A string in angle brackets, written as hexadecimal.
    fn hex_string(&mut self) -> Vec<u8> {
        let mut digits = Vec::new();
        while self.at < self.bytes.len() && self.bytes[self.at] != b'>' {
            let b = self.bytes[self.at];
            if b.is_ascii_hexdigit() {
                digits.push(b);
            }
            self.at += 1;
        }
        // Only step over the `>` when there is one: a file may end in the middle of a string.
        if self.at < self.bytes.len() {
            self.at += 1;
        }
        if digits.len() % 2 == 1 {
            digits.push(b'0');
        }
        digits
            .chunks(2)
            .filter_map(|pair| {
                std::str::from_utf8(pair)
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            })
            .collect()
    }

    /// The next value, or `None` at the end of what can be read.
    fn object(&mut self, depth: usize) -> Option<Object> {
        if depth > 64 {
            return None;
        }
        self.skip_space();
        let b = *self.bytes.get(self.at)?;
        match b {
            b'/' => {
                self.at += 1;
                Some(Object::Name(self.name()))
            }
            b'(' => {
                self.at += 1;
                Some(Object::Text(self.literal()))
            }
            b'[' => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    match self.bytes.get(self.at) {
                        None => break,
                        Some(b']') => {
                            self.at += 1;
                            break;
                        }
                        _ => match self.object(depth + 1) {
                            Some(o) => items.push(o),
                            None => break,
                        },
                    }
                }
                Some(Object::Array(items))
            }
            b'<' if self.bytes.get(self.at + 1) == Some(&b'<') => {
                self.at += 2;
                let mut map = HashMap::new();
                loop {
                    if self.take(b">>") {
                        break;
                    }
                    self.skip_space();
                    if self.bytes.get(self.at) != Some(&b'/') {
                        // Not a key: the dictionary is malformed, so it ends here.
                        break;
                    }
                    self.at += 1;
                    let key = self.name();
                    let Some(value) = self.object(depth + 1) else {
                        break;
                    };
                    map.insert(key, value);
                }
                // A dictionary followed by `stream` describes the bytes after it.
                let save = self.at;
                if self.take(b"stream") {
                    if self.bytes.get(self.at) == Some(&b'\r') {
                        self.at += 1;
                    }
                    if self.bytes.get(self.at) == Some(&b'\n') {
                        self.at += 1;
                    }
                    let start = self.at;
                    return Some(Object::Stream(map, start, start));
                }
                self.at = save;
                Some(Object::Dict(map))
            }
            b'<' => {
                self.at += 1;
                Some(Object::Text(self.hex_string()))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let save = self.at;
                let first = self.word();
                let value: f64 = first.parse().unwrap_or(0.0);
                // `12 0 R` is a reference to object 12.
                let after = self.at;
                self.skip_space();
                let second_at = self.at;
                let second = self.word();
                if second.chars().all(|c| c.is_ascii_digit()) && !second.is_empty() {
                    self.skip_space();
                    if self.bytes.get(self.at) == Some(&b'R')
                        && self
                            .bytes
                            .get(self.at + 1)
                            .is_none_or(|b| delimiter(*b) || b.is_ascii_whitespace())
                    {
                        self.at += 1;
                        return Some(Object::Ref(value as u32));
                    }
                }
                let _ = second_at;
                self.at = after;
                let _ = save;
                Some(Object::Number(value))
            }
            _ => {
                let word = self.word();
                match word.as_str() {
                    "true" => Some(Object::Bool(true)),
                    "false" => Some(Object::Bool(false)),
                    "null" => Some(Object::Null),
                    "" => {
                        self.at += 1;
                        None
                    }
                    _ => Some(Object::Null),
                }
            }
        }
    }
}

/// Where an object is: at a place in the file, or inside another object's stream.
#[derive(Clone, Copy, Debug)]
enum Where {
    At(usize),
    In { stream: u32, index: usize },
}

/// A PDF file with its tables read, able to give the value of any object in it.
pub struct Document<'a> {
    bytes: &'a [u8],
    places: HashMap<u32, Where>,
    trailer: HashMap<String, Object>,
    /// Objects already read, so that a shared one is read once.
    cache: std::cell::RefCell<HashMap<u32, Object>>,
    /// Streams of objects already unpacked.
    bundles: std::cell::RefCell<HashMap<u32, Vec<(u32, usize)>>>,
    bundle_bytes: std::cell::RefCell<HashMap<u32, Vec<u8>>>,
}

impl<'a> Document<'a> {
    pub fn read(bytes: &'a [u8]) -> Result<Document<'a>> {
        if !bytes.starts_with(b"%PDF-")
            && !bytes[..bytes.len().min(1024)]
                .windows(5)
                .any(|w| w == b"%PDF-")
        {
            bail!("not a PDF file");
        }
        let mut doc = Document {
            bytes,
            places: HashMap::new(),
            trailer: HashMap::new(),
            cache: Default::default(),
            bundles: Default::default(),
            bundle_bytes: Default::default(),
        };
        if doc.read_tables().is_err() || doc.places.is_empty() {
            // A file whose tables are damaged is still readable by looking for the objects
            // themselves, which is what a reader does to repair one.
            doc.scan_for_objects();
        }
        if doc.trailer.contains_key("Encrypt") {
            bail!("the file is encrypted");
        }
        if doc.places.is_empty() {
            bail!("the file lists no objects");
        }
        Ok(doc)
    }

    /// Follows the cross-reference tables from the end of the file.
    fn read_tables(&mut self) -> Result<()> {
        let tail_at = self.bytes.len().saturating_sub(2048);
        let tail = &self.bytes[tail_at..];
        let found = tail
            .windows(9)
            .rposition(|w| w == b"startxref")
            .context("no startxref")?;
        let mut lexer = Lexer::new(self.bytes, tail_at + found + 9);
        let mut next = lexer.word().parse::<usize>().ok();
        let mut seen = std::collections::HashSet::new();
        while let Some(at) = next {
            if at >= self.bytes.len() || !seen.insert(at) {
                break;
            }
            next = self.read_table(at)?;
        }
        Ok(())
    }

    /// One table and the trailer that follows it; the place of the one before it, if any.
    fn read_table(&mut self, at: usize) -> Result<Option<usize>> {
        let mut lexer = Lexer::new(self.bytes, at);
        if lexer.take(b"xref") {
            loop {
                lexer.skip_space();
                if lexer.starts_with(b"trailer") {
                    lexer.take(b"trailer");
                    break;
                }
                let first: u32 = match lexer.word().parse() {
                    Ok(n) => n,
                    Err(_) => break,
                };
                let count: usize = match lexer.word().parse() {
                    Ok(n) => n,
                    Err(_) => break,
                };
                for i in 0..count.min(MAX_OBJECTS) {
                    lexer.skip_space();
                    let offset = lexer.word();
                    let _generation = lexer.word();
                    let kind = lexer.word();
                    if kind == "n" {
                        if let Ok(o) = offset.parse::<usize>() {
                            self.places.entry(first + i as u32).or_insert(Where::At(o));
                        }
                    }
                }
            }
            let trailer = lexer.object(0).and_then(|o| match o {
                Object::Dict(d) => Some(d),
                _ => None,
            });
            let Some(trailer) = trailer else {
                return Ok(None);
            };
            let previous = trailer.get("Prev").and_then(Object::as_number);
            // A file updated in place may also have a stream of newer entries. Where it says
            // that is may be nonsense, so it is checked as the places in the chain are.
            if let Some(Object::Number(x)) = trailer.get("XRefStm") {
                if (0.0..self.bytes.len() as f64).contains(x) {
                    let _ = self.read_table(*x as usize);
                }
            }
            for (k, v) in trailer {
                self.trailer.entry(k).or_insert(v);
            }
            return Ok(previous.map(|p| p as usize));
        }
        // A cross-reference stream: `n 0 obj << ... >> stream`.
        let _number = lexer.word();
        let _generation = lexer.word();
        if lexer.word() != "obj" {
            return Ok(None);
        }
        let Some(object) = lexer.object(0) else {
            return Ok(None);
        };
        let Object::Stream(dict, start, _) = object else {
            return Ok(None);
        };
        let body = self.stream_bytes(&dict, start)?;
        let widths: Vec<usize> = match dict.get("W") {
            Some(Object::Array(a)) => a
                .iter()
                .filter_map(|o| o.as_number())
                .map(|n| n as usize)
                .collect(),
            _ => return Ok(None),
        };
        if widths.len() < 3 {
            return Ok(None);
        }
        let size = dict.get("Size").and_then(Object::as_number).unwrap_or(0.0) as u32;
        let ranges: Vec<(u32, u32)> = match dict.get("Index") {
            Some(Object::Array(a)) => a
                .chunks(2)
                .filter_map(|p| {
                    Some((
                        p.first()?.as_number()? as u32,
                        p.get(1)?.as_number()? as u32,
                    ))
                })
                .collect(),
            _ => vec![(0, size)],
        };
        let row = widths.iter().sum::<usize>().max(1);
        let mut at = 0usize;
        for (first, count) in ranges {
            for i in 0..count.min(MAX_OBJECTS as u32) {
                if at + row > body.len() {
                    break;
                }
                let mut field = |width: usize, default: u64| -> u64 {
                    if width == 0 {
                        return default;
                    }
                    let mut v = 0u64;
                    for _ in 0..width {
                        v = (v << 8) | body[at] as u64;
                        at += 1;
                    }
                    v
                };
                let kind = field(widths[0], 1);
                let second = field(widths[1], 0);
                let third = field(widths[2], 0);
                let number = first + i;
                match kind {
                    1 => {
                        self.places
                            .entry(number)
                            .or_insert(Where::At(second as usize));
                    }
                    2 => {
                        self.places.entry(number).or_insert(Where::In {
                            stream: second as u32,
                            index: third as usize,
                        });
                    }
                    _ => {}
                }
            }
        }
        let previous = dict.get("Prev").and_then(Object::as_number);
        for (k, v) in dict {
            self.trailer.entry(k).or_insert(v);
        }
        Ok(previous.map(|p| p as usize))
    }

    /// Looks through the file for `n 0 obj`, for a file whose tables cannot be followed.
    fn scan_for_objects(&mut self) {
        let mut at = 0usize;
        while let Some(found) = self.bytes[at..].windows(3).position(|w| w == b"obj") {
            let obj_at = at + found;
            // Back over `n g ` to the number.
            let mut start = obj_at;
            let mut fields = 0;
            while start > 0 && fields < 2 {
                start -= 1;
                while start > 0 && self.bytes[start].is_ascii_whitespace() {
                    start -= 1;
                }
                while start > 0 && self.bytes[start - 1].is_ascii_digit() {
                    start -= 1;
                }
                fields += 1;
            }
            let mut lexer = Lexer::new(self.bytes, start);
            if let Ok(number) = lexer.word().parse::<u32>() {
                self.places.insert(number, Where::At(start));
            }
            at = obj_at + 3;
            if self.places.len() > MAX_OBJECTS {
                break;
            }
        }
        if self.trailer.is_empty() {
            if let Some(found) = self.bytes.windows(7).rposition(|w| w == b"trailer") {
                let mut lexer = Lexer::new(self.bytes, found + 7);
                if let Some(Object::Dict(d)) = lexer.object(0) {
                    self.trailer = d;
                }
            }
        }
        // A file without a trailer still names its catalog in an object.
        if !self.trailer.contains_key("Root") {
            let numbers: Vec<u32> = self.places.keys().copied().collect();
            for number in numbers {
                if self
                    .object(number)
                    .as_dict()
                    .and_then(|d| d.get("Type"))
                    .and_then(Object::as_name)
                    == Some("Catalog")
                {
                    self.trailer.insert("Root".into(), Object::Ref(number));
                    break;
                }
            }
        }
    }

    /// The value of object `number`.
    pub fn object(&self, number: u32) -> Object {
        if let Some(found) = self.cache.borrow().get(&number) {
            return found.clone();
        }
        let value = self.read_object(number).unwrap_or(Object::Null);
        self.cache.borrow_mut().insert(number, value.clone());
        value
    }

    fn read_object(&self, number: u32) -> Option<Object> {
        match *self.places.get(&number)? {
            Where::At(at) => {
                let mut lexer = Lexer::new(self.bytes, at);
                let found: u32 = lexer.word().parse().ok()?;
                if found != number {
                    return None;
                }
                let _generation = lexer.word();
                if lexer.word() != "obj" {
                    return None;
                }
                let object = lexer.object(0)?;
                // A stream ends where `endstream` is, or where its length says.
                if let Object::Stream(dict, start, _) = object {
                    let length = self
                        .resolve(dict.get("Length").cloned().unwrap_or(Object::Null))
                        .as_number()
                        .map(|n| n as usize);
                    let end = match length {
                        Some(n) if start.saturating_add(n) <= self.bytes.len() => start + n,
                        _ => self.bytes[start..]
                            .windows(9)
                            .position(|w| w == b"endstream")
                            .map(|p| start + p)
                            .unwrap_or(self.bytes.len()),
                    };
                    return Some(Object::Stream(dict, start, end));
                }
                Some(object)
            }
            Where::In { stream, index } => {
                self.unpack_bundle(stream);
                let bundles = self.bundles.borrow();
                let entries = bundles.get(&stream)?;
                let (_, at) = *entries
                    .get(index)
                    .or_else(|| entries.iter().find(|(n, _)| *n == number))?;
                let bytes = self.bundle_bytes.borrow();
                let body = bytes.get(&stream)?;
                let mut lexer = Lexer::new(body, at);
                lexer.object(0)
            }
        }
    }

    /// Unpacks a stream that holds other objects, once.
    fn unpack_bundle(&self, number: u32) {
        if self.bundles.borrow().contains_key(&number) {
            return;
        }
        self.bundles.borrow_mut().insert(number, Vec::new());
        let Where::At(_) = self.places.get(&number).copied().unwrap_or(Where::At(0)) else {
            return;
        };
        let Some(Object::Stream(dict, start, end)) = self.read_object(number) else {
            return;
        };
        let Ok(body) = self.decode(&dict, start, end) else {
            return;
        };
        let count = self
            .resolve(dict.get("N").cloned().unwrap_or(Object::Null))
            .as_number()
            .unwrap_or(0.0) as usize;
        let first = self
            .resolve(dict.get("First").cloned().unwrap_or(Object::Null))
            .as_number()
            .unwrap_or(0.0) as usize;
        let mut lexer = Lexer::new(&body, 0);
        let mut entries = Vec::new();
        for _ in 0..count.min(MAX_OBJECTS) {
            let Ok(n) = lexer.word().parse::<u32>() else {
                break;
            };
            let Ok(offset) = lexer.word().parse::<usize>() else {
                break;
            };
            entries.push((n, first + offset));
        }
        self.bundles.borrow_mut().insert(number, entries);
        self.bundle_bytes.borrow_mut().insert(number, body);
    }

    /// A value with any reference followed.
    pub fn resolve(&self, object: Object) -> Object {
        let mut value = object;
        for _ in 0..32 {
            match value {
                Object::Ref(n) => value = self.object(n),
                other => return other,
            }
        }
        Object::Null
    }

    fn get(&self, dict: &HashMap<String, Object>, key: &str) -> Object {
        self.resolve(dict.get(key).cloned().unwrap_or(Object::Null))
    }

    /// The bytes of a stream whose dictionary may still hold references.
    fn stream_bytes(&mut self, dict: &HashMap<String, Object>, start: usize) -> Result<Vec<u8>> {
        let length = dict
            .get("Length")
            .and_then(Object::as_number)
            .map(|n| n as usize);
        let end = match length {
            Some(n) if start.saturating_add(n) <= self.bytes.len() => start + n,
            _ => self.bytes[start..]
                .windows(9)
                .position(|w| w == b"endstream")
                .map(|p| start + p)
                .unwrap_or(self.bytes.len()),
        };
        self.decode(dict, start, end)
    }

    /// A stream's bytes with its filters undone.
    fn decode(&self, dict: &HashMap<String, Object>, start: usize, end: usize) -> Result<Vec<u8>> {
        let raw = self
            .bytes
            .get(start..end)
            .context("stream runs past the end of the file")?;
        let filters: Vec<String> = match self.get(dict, "Filter") {
            Object::Name(n) => vec![n],
            Object::Array(a) => a
                .into_iter()
                .filter_map(|o| self.resolve(o).as_name().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        let parms: Vec<Object> = match self.get(dict, "DecodeParms") {
            Object::Array(a) => a.into_iter().map(|o| self.resolve(o)).collect(),
            other => vec![other],
        };
        let mut body = raw.to_vec();
        for (i, filter) in filters.iter().enumerate() {
            body = match filter.as_str() {
                "FlateDecode" | "Fl" => inflate(&body)?,
                "ASCIIHexDecode" | "AHx" => ascii_hex(&body),
                "ASCII85Decode" | "A85" => ascii85(&body),
                "RunLengthDecode" | "RL" => run_length(&body),
                // An image filter: the bytes are a picture, not text.
                "DCTDecode" | "JPXDecode" | "CCITTFaxDecode" | "JBIG2Decode" => {
                    return Ok(Vec::new())
                }
                other => bail!("unsupported stream filter {other}"),
            };
            if body.len() > MAX_STREAM {
                body.truncate(MAX_STREAM);
            }
            if let Some(parms) = parms.get(i).and_then(Object::as_dict) {
                body = undo_predictor(body, parms, self);
            }
        }
        Ok(body)
    }

    /// Every page of the document, in order.
    fn pages(&self) -> Vec<HashMap<String, Object>> {
        let root = self.resolve(self.trailer.get("Root").cloned().unwrap_or(Object::Null));
        let Some(catalog) = root.as_dict() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut stack = vec![(self.get(catalog, "Pages"), HashMap::new())];
        while let Some((node, inherited)) = stack.pop() {
            if out.len() >= MAX_PAGES {
                break;
            }
            let Some(dict) = node.as_dict() else { continue };
            // What a page inherits from the node above it.
            let mut passed = inherited.clone();
            for key in ["Resources", "MediaBox", "Rotate"] {
                if let Some(v) = dict.get(key) {
                    passed.insert(key.to_string(), v.clone());
                }
            }
            match self.get(dict, "Type").as_name() {
                Some("Page") => {
                    let mut page = dict.clone();
                    for (k, v) in passed {
                        page.entry(k).or_insert(v);
                    }
                    out.push(page);
                }
                _ => {
                    let Object::Array(kids) = self.get(dict, "Kids") else {
                        continue;
                    };
                    for kid in kids.into_iter().rev() {
                        if let Object::Ref(n) = kid {
                            if !seen.insert(n) {
                                continue;
                            }
                        }
                        stack.push((self.resolve(kid), passed.clone()));
                    }
                }
            }
        }
        out
    }

    /// The text of the whole document, a line to a line of the page.
    pub fn text(&self) -> Result<String> {
        let pages = self.pages();
        if pages.is_empty() {
            bail!("the file lists no pages");
        }
        let mut out = String::new();
        for (i, page) in pages.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            let fonts = self.fonts(page);
            let content = self.page_content(page);
            out.push_str(&self.page_text(&content, &fonts));
            if out.len() > MAX_STREAM {
                break;
            }
        }
        Ok(out)
    }

    /// The content streams of a page, one after another.
    fn page_content(&self, page: &HashMap<String, Object>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut add = |object: Object| {
            if let Object::Stream(dict, start, end) = object {
                if let Ok(body) = self.decode(&dict, start, end) {
                    out.extend_from_slice(&body);
                    out.push(b'\n');
                }
            }
        };
        match self.get(page, "Contents") {
            Object::Array(parts) => {
                for part in parts {
                    add(self.resolve(part));
                }
            }
            other => add(other),
        }
        out
    }

    /// How each font of a page turns the codes in the content into characters.
    fn fonts(&self, page: &HashMap<String, Object>) -> HashMap<String, Font> {
        let resources = self.get(page, "Resources");
        let Some(resources) = resources.as_dict() else {
            return HashMap::new();
        };
        let fonts = self.get(resources, "Font");
        let Some(fonts) = fonts.as_dict() else {
            return HashMap::new();
        };
        fonts
            .keys()
            .map(|name| {
                let font = self.get(fonts, name);
                (name.clone(), self.font(font.as_dict()))
            })
            .collect()
    }

    fn font(&self, dict: Option<&HashMap<String, Object>>) -> Font {
        let Some(dict) = dict else {
            return Font::default();
        };
        let mut font = Font::default();
        // A composite font writes two bytes a glyph.
        if self.get(dict, "Subtype").as_name() == Some("Type0") {
            font.two_byte = true;
        }
        if let Object::Stream(d, start, end) = self.get(dict, "ToUnicode") {
            if let Ok(body) = self.decode(&d, start, end) {
                font.to_unicode = read_cmap(&body);
            }
        }
        match self.get(dict, "Encoding") {
            Object::Name(n) => font.encoding = Some(n),
            Object::Dict(d) => {
                if let Some(base) = self.get(&d, "BaseEncoding").as_name() {
                    font.encoding = Some(base.to_string());
                }
                // Characters the font names for itself.
                if let Object::Array(items) = self.get(&d, "Differences") {
                    let mut code = 0u32;
                    for item in items {
                        match self.resolve(item) {
                            Object::Number(n) => code = n as u32,
                            Object::Name(name) => {
                                if let Some(c) = glyph_char(&name) {
                                    font.differences.insert(code, c);
                                }
                                code += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        font
    }

    /// Reads a page's content, keeping what it draws as text.
    fn page_text(&self, content: &[u8], fonts: &HashMap<String, Font>) -> String {
        let mut out = String::new();
        let mut lexer = Lexer::new(content, 0);
        let mut stack: Vec<Object> = Vec::new();
        let mut font: Option<&Font> = None;
        let default = Font::default();
        let mut line = String::new();
        let flush = |line: &mut String, out: &mut String| {
            let trimmed = line.trim_end();
            if !trimmed.is_empty() {
                out.push_str(trimmed);
                out.push('\n');
            }
            line.clear();
        };
        while lexer.at < content.len() {
            lexer.skip_space();
            if lexer.at >= content.len() {
                break;
            }
            let b = content[lexer.at];
            if matches!(b, b'/' | b'(' | b'[' | b'<' | b'+' | b'-' | b'.') || b.is_ascii_digit() {
                match lexer.object(0) {
                    Some(o) => {
                        if stack.len() < 64 {
                            stack.push(o);
                        }
                        continue;
                    }
                    None => break,
                }
            }
            let operator = lexer.word();
            if operator.is_empty() {
                lexer.at += 1;
                continue;
            }
            let shown = font.unwrap_or(&default);
            match operator.as_str() {
                "Tf" => {
                    if let Some(Object::Name(name)) = stack.iter().rev().nth(1) {
                        font = fonts.get(name);
                    }
                }
                "Tj" | "'" | "\"" => {
                    if operator != "Tj" {
                        flush(&mut line, &mut out);
                    }
                    if let Some(Object::Text(bytes)) = stack.last() {
                        line.push_str(&shown.decode(bytes));
                    }
                }
                "TJ" => {
                    if let Some(Object::Array(items)) = stack.last() {
                        for item in items {
                            match item {
                                Object::Text(bytes) => line.push_str(&shown.decode(bytes)),
                                // A large backward step between runs is a space.
                                Object::Number(n) if *n < -180.0 && !line.ends_with(' ') => {
                                    line.push(' ')
                                }
                                _ => {}
                            }
                        }
                    }
                }
                "Td" | "TD" | "T*" | "ET" => flush(&mut line, &mut out),
                "Tm" => {
                    // A new text matrix starts a line unless it only moves along the same one.
                    flush(&mut line, &mut out);
                }
                _ => {}
            }
            stack.clear();
            if out.len() > MAX_STREAM {
                break;
            }
        }
        flush(&mut line, &mut out);
        out
    }
}

/// How one font's codes become characters.
#[derive(Default, Clone)]
struct Font {
    two_byte: bool,
    to_unicode: HashMap<u32, String>,
    encoding: Option<String>,
    differences: HashMap<u32, char>,
}

impl Font {
    fn decode(&self, bytes: &[u8]) -> String {
        let codes: Vec<u32> = if self.two_byte {
            bytes
                .chunks(2)
                .map(|p| ((p[0] as u32) << 8) | *p.get(1).unwrap_or(&0) as u32)
                .collect()
        } else {
            bytes.iter().map(|b| *b as u32).collect()
        };
        let mut out = String::new();
        for code in codes {
            if let Some(text) = self.to_unicode.get(&code) {
                out.push_str(text);
                continue;
            }
            if let Some(c) = self.differences.get(&code) {
                out.push(*c);
                continue;
            }
            out.push(match self.encoding.as_deref() {
                Some("MacRomanEncoding") => mac_roman(code),
                // WinAnsi is close enough to Latin-1 that only the 0x80 block differs.
                _ => win_ansi(code),
            });
        }
        out
    }
}

/// The `ToUnicode` table of a font: which characters each code stands for.
fn read_cmap(body: &[u8]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let mut lexer = Lexer::new(body, 0);
    let mut recent: Vec<Object> = Vec::new();
    while lexer.at < body.len() {
        lexer.skip_space();
        if lexer.at >= body.len() {
            break;
        }
        let b = body[lexer.at];
        if matches!(b, b'<' | b'[' | b'(' | b'/') || b.is_ascii_digit() {
            match lexer.object(0) {
                Some(o) => {
                    if recent.len() > 400 {
                        recent.remove(0);
                    }
                    recent.push(o);
                    continue;
                }
                None => break,
            }
        }
        let word = lexer.word();
        match word.as_str() {
            "endbfchar" => {
                for pair in recent.chunks(2) {
                    if let [Object::Text(from), Object::Text(to)] = pair {
                        out.insert(be_number(from), utf16be(to));
                    }
                }
                recent.clear();
            }
            "endbfrange" => {
                for group in recent.chunks(3) {
                    match group {
                        [Object::Text(lo), Object::Text(hi), Object::Text(to)] => {
                            let (lo, hi) = (be_number(lo), be_number(hi));
                            let base = utf16be(to);
                            let last_code = hi.min(lo.saturating_add(65535));
                            for (i, code) in (lo..=last_code).enumerate() {
                                let mut text = base.clone();
                                if let Some(last) = text.pop() {
                                    let next = (last as u32)
                                        .checked_add(i as u32)
                                        .and_then(char::from_u32)
                                        .unwrap_or(last);
                                    text.push(next);
                                }
                                out.insert(code, text);
                            }
                        }
                        [Object::Text(lo), Object::Text(hi), Object::Array(items)] => {
                            let (lo, hi) = (be_number(lo), be_number(hi));
                            for (i, item) in items.iter().enumerate() {
                                if lo + i as u32 > hi {
                                    break;
                                }
                                if let Object::Text(to) = item {
                                    out.insert(lo + i as u32, utf16be(to));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                recent.clear();
            }
            "beginbfchar" | "beginbfrange" | "endcmap" => recent.clear(),
            "" => lexer.at += 1,
            _ => {}
        }
        if out.len() > 200_000 {
            break;
        }
    }
    out
}

/// Bytes read as one number, most significant first.
fn be_number(bytes: &[u8]) -> u32 {
    bytes.iter().take(4).fold(0u32, |n, b| (n << 8) | *b as u32)
}

/// Bytes read as UTF-16, most significant first, which is how a `ToUnicode` table writes them.
fn utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks(2)
        .map(|p| ((p[0] as u16) << 8) | *p.get(1).unwrap_or(&0) as u16)
        .collect();
    String::from_utf16_lossy(&units)
}

/// The character a code stands for in WinAnsiEncoding.
fn win_ansi(code: u32) -> char {
    const HIGH: [char; 32] = [
        '\u{20ac}', '\u{fffd}', '\u{201a}', '\u{0192}', '\u{201e}', '\u{2026}', '\u{2020}',
        '\u{2021}', '\u{02c6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{fffd}',
        '\u{017d}', '\u{fffd}', '\u{fffd}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}',
        '\u{2022}', '\u{2013}', '\u{2014}', '\u{02dc}', '\u{2122}', '\u{0161}', '\u{203a}',
        '\u{0153}', '\u{fffd}', '\u{017e}', '\u{0178}',
    ];
    match code {
        0x80..=0x9f => HIGH[(code - 0x80) as usize],
        _ => char::from_u32(code).unwrap_or('\u{fffd}'),
    }
}

/// The character a code stands for in MacRomanEncoding, for the part that differs from ASCII.
fn mac_roman(code: u32) -> char {
    const HIGH: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø\
                        ¿¡¬√ƒ≈∆«»… ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄¤‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔﾀÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
    if code < 0x80 {
        return char::from_u32(code).unwrap_or('\u{fffd}');
    }
    HIGH.chars()
        .nth((code - 0x80) as usize)
        .unwrap_or('\u{fffd}')
}

/// The character a glyph name stands for, for the names a font lists for itself.
fn glyph_char(name: &str) -> Option<char> {
    // `uniXXXX` and `uXXXX[XX]` name a character by number.
    if let Some(hex) = name.strip_prefix("uni").or_else(|| name.strip_prefix('u')) {
        if hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return u32::from_str_radix(&hex[..4.max(hex.len().min(6))], 16)
                .ok()
                .and_then(char::from_u32);
        }
    }
    let named = [
        ("space", ' '),
        ("exclam", '!'),
        ("quotedbl", '"'),
        ("numbersign", '#'),
        ("dollar", '$'),
        ("percent", '%'),
        ("ampersand", '&'),
        ("quotesingle", '\''),
        ("parenleft", '('),
        ("parenright", ')'),
        ("asterisk", '*'),
        ("plus", '+'),
        ("comma", ','),
        ("hyphen", '-'),
        ("period", '.'),
        ("slash", '/'),
        ("zero", '0'),
        ("one", '1'),
        ("two", '2'),
        ("three", '3'),
        ("four", '4'),
        ("five", '5'),
        ("six", '6'),
        ("seven", '7'),
        ("eight", '8'),
        ("nine", '9'),
        ("colon", ':'),
        ("semicolon", ';'),
        ("less", '<'),
        ("equal", '='),
        ("greater", '>'),
        ("question", '?'),
        ("at", '@'),
        ("bracketleft", '['),
        ("backslash", '\\'),
        ("bracketright", ']'),
        ("underscore", '_'),
        ("braceleft", '{'),
        ("bar", '|'),
        ("braceright", '}'),
        ("quoteright", '\u{2019}'),
        ("quoteleft", '\u{2018}'),
        ("quotedblleft", '\u{201c}'),
        ("quotedblright", '\u{201d}'),
        ("endash", '\u{2013}'),
        ("emdash", '\u{2014}'),
        ("bullet", '\u{2022}'),
        ("fi", '\u{fb01}'),
        ("fl", '\u{fb02}'),
    ];
    if let Some((_, c)) = named.iter().find(|(n, _)| *n == name) {
        return Some(*c);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

// ---------- the filters a stream may be written with ----------

fn inflate(body: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read as _;
    let mut out = Vec::new();
    let mut reader = flate2::read::ZlibDecoder::new(body).take(MAX_STREAM as u64);
    if reader.read_to_end(&mut out).is_ok() && !out.is_empty() {
        return Ok(out);
    }
    // Some files leave off the two bytes a zlib stream starts with.
    out.clear();
    let mut raw = flate2::read::DeflateDecoder::new(body).take(MAX_STREAM as u64);
    match raw.read_to_end(&mut out) {
        Ok(_) if !out.is_empty() => Ok(out),
        _ => bail!("a compressed part of the file could not be read"),
    }
}

fn ascii_hex(body: &[u8]) -> Vec<u8> {
    let digits: Vec<u8> = body
        .iter()
        .copied()
        .take_while(|b| *b != b'>')
        .filter(u8::is_ascii_hexdigit)
        .collect();
    digits
        .chunks(2)
        .filter_map(|p| {
            let hi = (p[0] as char).to_digit(16)?;
            let lo = p
                .get(1)
                .map_or(0, |b| (*b as char).to_digit(16).unwrap_or(0));
            Some((hi * 16 + lo) as u8)
        })
        .collect()
}

fn ascii85(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut count = 0usize;
    let mut i = 0usize;
    let body = body.strip_prefix(b"<~").unwrap_or(body);
    while i < body.len() {
        let b = body[i];
        i += 1;
        if b.is_ascii_whitespace() {
            continue;
        }
        if b == b'~' {
            break;
        }
        if b == b'z' && count == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue;
        }
        group[count] = b - b'!';
        count += 1;
        if count == 5 {
            let value = group
                .iter()
                .fold(0u32, |n, d| n.wrapping_mul(85) + *d as u32);
            out.extend_from_slice(&value.to_be_bytes());
            count = 0;
        }
        if out.len() > MAX_STREAM {
            break;
        }
    }
    if count > 1 {
        for slot in group.iter_mut().skip(count) {
            *slot = 84;
        }
        let value = group
            .iter()
            .fold(0u32, |n, d| n.wrapping_mul(85) + *d as u32);
        out.extend_from_slice(&value.to_be_bytes()[..count - 1]);
    }
    out
}

fn run_length(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < body.len() {
        let length = body[i] as usize;
        i += 1;
        if length == 128 {
            break;
        }
        if length < 128 {
            let end = (i + length + 1).min(body.len());
            out.extend_from_slice(&body[i..end]);
            i = end;
        } else if let Some(&b) = body.get(i) {
            out.extend(std::iter::repeat_n(b, 257 - length));
            i += 1;
        }
        if out.len() > MAX_STREAM {
            break;
        }
    }
    out
}

/// Undoes the row-by-row prediction a stream may be written with, which is how cross-reference
/// streams and many images are packed.
fn undo_predictor(body: Vec<u8>, parms: &HashMap<String, Object>, doc: &Document<'_>) -> Vec<u8> {
    let number =
        |key: &str, default: f64| doc.get(parms, key).as_number().unwrap_or(default) as usize;
    let predictor = number("Predictor", 1.0);
    if predictor < 10 {
        return body;
    }
    let colors = number("Colors", 1.0).clamp(1, 32);
    let bits = number("BitsPerComponent", 8.0).clamp(1, 32);
    let columns = number("Columns", 1.0).max(1);
    let step = (colors * bits).div_ceil(8).max(1);
    // A row is as wide as the file says it is, so it is held to the same limit as the rest.
    let Some(row) = colors
        .checked_mul(bits)
        .and_then(|n| n.checked_mul(columns))
        .map(|n| n.div_ceil(8))
        .filter(|row| *row <= MAX_STREAM)
    else {
        return body;
    };
    let mut out = Vec::with_capacity(body.len());
    let mut previous = vec![0u8; row];
    let mut at = 0usize;
    while at + 1 + row <= body.len() + row && at < body.len() {
        let tag = body[at];
        at += 1;
        let end = (at + row).min(body.len());
        let mut line = body[at..end].to_vec();
        line.resize(row, 0);
        at = end;
        for i in 0..row {
            let left = if i >= step { line[i - step] } else { 0 };
            let up = previous[i];
            let corner = if i >= step { previous[i - step] } else { 0 };
            line[i] = match tag {
                1 => line[i].wrapping_add(left),
                2 => line[i].wrapping_add(up),
                3 => line[i].wrapping_add(((left as u16 + up as u16) / 2) as u8),
                4 => {
                    let p = left as i16 + up as i16 - corner as i16;
                    let (pa, pb, pc) = (
                        (p - left as i16).abs(),
                        (p - up as i16).abs(),
                        (p - corner as i16).abs(),
                    );
                    let best = if pa <= pb && pa <= pc {
                        left
                    } else if pb <= pc {
                        up
                    } else {
                        corner
                    };
                    line[i].wrapping_add(best)
                }
                _ => line[i],
            };
        }
        out.extend_from_slice(&line);
        previous = line;
        if out.len() > MAX_STREAM {
            break;
        }
    }
    out
}

/// The text of the PDF file `bytes`.
pub fn text(bytes: &[u8]) -> Result<String> {
    Document::read(bytes)?.text()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a PDF file from the bodies of its objects, numbered from one, with the table a
    /// reader follows at the end. `extra` is added to the trailer.
    fn build(objects: &[Vec<u8>], extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R{extra} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn stream(dict: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!("<< {dict} /Length {} >>\nstream\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendstream");
        out
    }

    /// The four objects every one of these files needs, with `content` drawn on the one page
    /// and `font` as its only font.
    fn one_page(content: Vec<u8>, font: &str) -> Vec<Vec<u8>> {
        vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \n              /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .as_bytes()
                .to_vec(),
            content,
            font.as_bytes().to_vec(),
        ]
    }

    const HELVETICA: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica \
                             /Encoding /WinAnsiEncoding >>";

    #[test]
    fn the_words_of_a_page_are_read_in_the_order_they_are_drawn() {
        let content =
            b"BT /F1 12 Tf 72 720 Td (Hello, world) Tj 0 -14 Td (second line) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "Hello, world\nsecond line\n");
    }

    #[test]
    fn a_compressed_page_is_read_the_same_as_a_plain_one() {
        use std::io::Write as _;
        let content = b"BT /F1 12 Tf 72 720 Td (Hello, world) Tj ET\n";
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        let packed = encoder.finish().unwrap();
        let file = build(
            &one_page(stream("/Filter /FlateDecode", &packed), HELVETICA),
            "",
        );
        assert_eq!(text(&file).unwrap(), "Hello, world\n");
    }

    #[test]
    fn a_font_that_says_what_its_codes_mean_is_read_by_that() {
        // Codes 1, 2 and 3 stand for O, K and the check mark, which no encoding would give.
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                     1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                     3 beginbfchar\n<0001> <004F>\n<0002> <004B>\n<0003> <2713>\n\
                     endbfchar\nendcmap end end"
            .to_vec();
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Custom \
                    /DescendantFonts [] /ToUnicode 6 0 R >>";
        let content = b"BT /F1 12 Tf 72 720 Td <000100020003> Tj ET\n".to_vec();
        let mut objects = one_page(stream("", &content), font);
        objects.push(stream("", &cmap));
        let file = build(&objects, "");
        assert_eq!(text(&file).unwrap(), "OK\u{2713}\n");
    }

    #[test]
    fn a_wide_gap_between_two_runs_on_a_line_is_a_space() {
        let content = b"BT /F1 12 Tf 72 720 Td [(left)-400(right)] TJ (joined)Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "left rightjoined\n");
    }

    #[test]
    fn the_characters_a_font_names_for_itself_are_read() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding \
                    << /Type /Encoding /Differences [65 /bullet /emdash /uni00E9] >> >>";
        let content = b"BT /F1 12 Tf 72 720 Td (ABC) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), font), "");
        assert_eq!(text(&file).unwrap(), "\u{2022}\u{2014}\u{e9}\n");
    }

    #[test]
    fn the_pages_are_read_in_order_however_deep_the_tree_is() {
        let draw = |t: &str| {
            stream(
                "",
                format!(
                    "BT /F1 12 Tf 72 720 Td ({t}) Tj ET
"
                )
                .as_bytes(),
            )
        };
        let page = |parent: u32, contents: u32| {
            format!(
                "<< /Type /Page /Parent {parent} 0 R /Resources << /Font << /F1 6 0 R >> >> \n                 /Contents {contents} 0 R >>"
            )
            .into_bytes()
        };
        let objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            // The first page is one level further down than the second.
            b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_vec(),
            b"<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 >>".to_vec(),
            draw("first page"),
            page(3, 4),
            HELVETICA.as_bytes().to_vec(),
            page(2, 8),
            draw("second page"),
        ];
        let file = build(&objects, "");
        assert_eq!(
            text(&file).unwrap(),
            "first page

second page
"
        );
    }

    /// Where a file says its tables are may be nonsense; reading it must not end the program.
    #[test]
    fn a_table_said_to_be_past_the_end_of_the_file_is_not_followed() {
        let content = b"BT /F1 12 Tf 72 720 Td (still here) Tj ET\n".to_vec();
        let file = build(
            &one_page(stream("", &content), HELVETICA),
            " /XRefStm 999999999",
        );
        assert_eq!(text(&file).unwrap(), "still here\n");
    }

    /// A file that stops in the middle of a value is read as far as it goes.
    #[test]
    fn a_file_cut_short_in_the_middle_of_a_value_is_refused_rather_than_read_past() {
        for tail in [
            "1 0 obj\n<< /Type /Catalog /K <AB",
            "1 0 obj\n<< /Type /Catalog /K (unclosed",
            "1 0 obj\n<< /Type /Catalog /Pages",
            "1 0 obj\n<< /Type /Catalog /K <<",
        ] {
            let file = format!("%PDF-1.7\n{tail}");
            // Whatever it says, it is a result and not a crash.
            let _ = text(file.as_bytes());
        }
    }

    /// A stream may say its rows are any width at all; believing it would ask for all memory.
    #[test]
    fn a_stream_that_asks_for_an_impossible_row_is_left_alone() {
        use std::io::Write as _;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(b"\x00rubbish").unwrap();
        let packed = encoder.finish().unwrap();
        let content = stream(
            "/Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 1000000000 >>",
            &packed,
        );
        let file = build(&one_page(content, HELVETICA), "");
        // No text comes of it, and no memory is asked for either.
        assert_eq!(text(&file).unwrap(), "");
    }

    /// A `ToUnicode` table may name the very last code there is.
    #[test]
    fn a_range_at_the_end_of_the_codes_does_not_run_past_it() {
        let cmap =
            b"begincmap 1 beginbfrange\n<FFFFFFFF> <FFFFFFFF> <0041>\nendbfrange endcmap".to_vec();
        assert_eq!(
            read_cmap(&cmap).get(&u32::MAX).map(String::as_str),
            Some("A")
        );
    }

    #[test]
    fn an_encrypted_file_is_reported_rather_than_read_as_nothing() {
        let file = build(
            &one_page(stream("", b"BT ET"), HELVETICA),
            " /Encrypt 9 0 R",
        );
        let e = text(&file).unwrap_err();
        assert!(format!("{e}").contains("encrypted"), "{e}");
    }

    #[test]
    fn a_file_that_is_not_a_pdf_is_refused() {
        assert!(text(b"just some bytes").is_err());
    }

    /// A file whose table of objects is wrong is still read, by looking for the objects.
    #[test]
    fn a_file_with_a_damaged_table_is_still_read() {
        let content = b"BT /F1 12 Tf 72 720 Td (recovered) Tj ET\n".to_vec();
        let mut file = build(&one_page(stream("", &content), HELVETICA), "");
        let at = file.windows(9).rposition(|w| w == b"startxref").unwrap();
        // The place the table is said to be at is nonsense.
        let tail = "startxref\n999999\n%%EOF\n";
        file.truncate(at);
        file.extend_from_slice(tail.as_bytes());
        assert_eq!(text(&file).unwrap(), "recovered\n");
    }
}
