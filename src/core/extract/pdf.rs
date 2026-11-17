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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The most objects the tables of a file may list, all told.
const MAX_OBJECTS: usize = 500_000;
/// The most bytes one stream is decoded into, and the most content one page is read from.
const MAX_STREAM: usize = 128 << 20;
/// The most bytes decoded from the streams of one file all told, so that a small file whose
/// streams are read again and again, or undone through filter after filter, cannot take much
/// longer than a large one.
const MAX_DECODED: usize = 1 << 30;
/// The most filters one stream is passed through.
const MAX_FILTERS: usize = 8;
/// The most drawings of forms (`Do`) followed in one file, and how deep inside one another.
const MAX_FORMS: usize = 10_000;
const MAX_FORM_DEPTH: usize = 8;
/// The most codes one `ToUnicode` table maps.
const MAX_CMAP: usize = 200_000;
/// The most pages read.
const MAX_PAGES: usize = 20_000;
/// The most items kept of one value with everything inside it, so that a short run of bytes
/// such as `0 0 0 …` cannot become a much larger amount of memory.
const MAX_ITEMS: usize = 1 << 18;
/// How many objects may be part way through being read at once. Reading one may need another,
/// such as a stream's length; a file can make those lead back to themselves or on and on.
const MAX_NESTING: usize = 32;

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
    /// How many more items the value being read may keep; see [`MAX_ITEMS`].
    room: usize,
}

impl<'a> Lexer<'a> {
    fn new(bytes: &'a [u8], at: usize) -> Self {
        Lexer {
            bytes,
            // a place a file gives may be past its end
            at: at.min(bytes.len()),
            room: MAX_ITEMS,
        }
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
        if depth == 0 {
            self.room = MAX_ITEMS;
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
                        // Past the room there is, the rest is still read to find the end.
                        _ => match self.object(depth + 1) {
                            Some(o) if self.room > 0 => {
                                self.room -= 1;
                                items.push(o);
                            }
                            Some(_) => {}
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
                    if self.room > 0 {
                        self.room -= 1;
                        map.insert(key, value);
                    }
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
                let first = self.word();
                let value: f64 = first.parse().unwrap_or(0.0);
                // `12 0 R` is a reference to object 12.
                let after = self.at;
                self.skip_space();
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
                self.at = after;
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
    /// How many objects are being read now, one needing the next; see [`MAX_NESTING`].
    nesting: std::cell::Cell<usize>,
    /// Streams of objects already unpacked.
    bundles: std::cell::RefCell<HashMap<u32, Vec<(u32, usize)>>>,
    bundle_bytes: std::cell::RefCell<HashMap<u32, Vec<u8>>>,
    /// Bytes decoded so far; see [`MAX_DECODED`].
    decoded: std::cell::Cell<usize>,
    /// Forms followed so far; see [`MAX_FORMS`].
    forms: std::cell::Cell<usize>,
    /// Fonts already read, by object number, since every page may name the same ones.
    fonts: std::cell::RefCell<HashMap<u32, Rc<Font>>>,
    /// The fingerprints of images already drawn, by object number.
    prints: std::cell::RefCell<HashMap<u32, String>>,
    /// Why some of the content could not be read, the first time it could not.
    unreadable: std::cell::RefCell<Option<String>>,
    cancel: Option<&'a AtomicBool>,
}

impl<'a> Document<'a> {
    pub fn read(bytes: &'a [u8], cancel: Option<&'a AtomicBool>) -> Result<Document<'a>> {
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
            nesting: Default::default(),
            bundles: Default::default(),
            bundle_bytes: Default::default(),
            decoded: Default::default(),
            forms: Default::default(),
            fonts: Default::default(),
            prints: Default::default(),
            unreadable: Default::default(),
            cancel,
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
        let mut seen = HashSet::new();
        while let Some(at) = next {
            if at >= self.bytes.len() || !seen.insert(at) {
                break;
            }
            next = self.read_table(at, &mut seen)?;
        }
        Ok(())
    }

    /// Notes that object `number` is at `place`, unless a newer table said where it is or the
    /// tables already list as many objects as are read.
    fn place(&mut self, number: u32, place: Where) {
        if self.places.len() < MAX_OBJECTS {
            self.places.entry(number).or_insert(place);
        }
    }

    /// One table and the trailer that follows it; the place of the one before it, if any.
    /// `seen` holds the places of the tables read, which any table may point back at.
    fn read_table(&mut self, at: usize, seen: &mut HashSet<usize>) -> Result<Option<usize>> {
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
                    let Some(number) = first.checked_add(i as u32) else {
                        break;
                    };
                    if kind == "n" {
                        if let Ok(o) = offset.parse::<usize>() {
                            self.place(number, Where::At(o));
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
                if (0.0..self.bytes.len() as f64).contains(x) && seen.insert(*x as usize) {
                    let _ = self.read_table(*x as usize, seen);
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
        // A field is a number of at most eight bytes, and every entry at least one byte.
        let widths: Vec<usize> = match dict.get("W") {
            Some(Object::Array(a)) => a
                .iter()
                .map(|o| match o.as_number() {
                    Some(n) if (0.0..=8.0).contains(&n) => Some(n as usize),
                    _ => None,
                })
                .collect::<Option<_>>()
                .unwrap_or_default(),
            _ => return Ok(None),
        };
        if widths.len() < 3 || widths.iter().sum::<usize>() == 0 {
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
        let row = widths.iter().sum::<usize>();
        let mut at = 0usize;
        for (first, count) in ranges {
            for i in 0..count.min(MAX_OBJECTS as u32) {
                if at + row > body.len() || self.places.len() >= MAX_OBJECTS {
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
                let Some(number) = first.checked_add(i) else {
                    break;
                };
                match kind {
                    1 => self.place(number, Where::At(second as usize)),
                    2 => self.place(
                        number,
                        Where::In {
                            stream: second as u32,
                            index: third as usize,
                        },
                    ),
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
        let nesting = self.nesting.get();
        if nesting >= MAX_NESTING {
            return Object::Null;
        }
        // Null while it is read, so that an object reached again from inside itself is
        // nothing rather than another read of the same object, and so on without end.
        self.cache.borrow_mut().insert(number, Object::Null);
        self.nesting.set(nesting + 1);
        let value = self.read_object(number).unwrap_or(Object::Null);
        self.nesting.set(nesting);
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
            entries.push((n, first.saturating_add(offset)));
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

    /// The bytes of a stream whose dictionary may still hold references: a table of the
    /// file's objects, which is not read from a damaged stream, since the objects can be
    /// looked for instead.
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
        self.decode_stream(dict, start, end, true)
    }

    /// Counts `bytes` more decoded against [`MAX_DECODED`], and stops when the work is cancelled.
    fn spend(&self, bytes: usize) -> Result<()> {
        let total = self.decoded.get().saturating_add(bytes);
        self.decoded.set(total);
        if total > MAX_DECODED {
            bail!("{}", too_much());
        }
        if self.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            bail!("cancelled");
        }
        Ok(())
    }

    /// Fails once the decoding budget is spent or the work is cancelled, so that what has been
    /// read is not taken for the whole document.
    fn check(&self) -> Result<()> {
        self.spend(0)
    }

    /// Notes that some of the content could not be read, and why.
    fn unreadable(&self, why: impl FnOnce() -> String) {
        let mut slot = self.unreadable.borrow_mut();
        if slot.is_none() {
            *slot = Some(why());
        }
    }

    /// A stream's bytes with its filters undone; what is left of a damaged one is kept, and
    /// noted as incomplete.
    fn decode(&self, dict: &HashMap<String, Object>, start: usize, end: usize) -> Result<Vec<u8>> {
        self.decode_stream(dict, start, end, false)
    }

    /// A stream's bytes with its filters undone. With `whole`, a damaged stream is an error
    /// rather than what is left of it.
    fn decode_stream(
        &self,
        dict: &HashMap<String, Object>,
        start: usize,
        end: usize,
        whole: bool,
    ) -> Result<Vec<u8>> {
        let raw = self
            .bytes
            .get(start..end)
            .context("stream runs past the end of the file")?;
        self.spend(raw.len())?;
        let filters: Vec<String> = match self.get(dict, "Filter") {
            Object::Name(n) => vec![n],
            Object::Array(a) => a
                .into_iter()
                .filter_map(|o| self.resolve(o).as_name().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        };
        if filters.len() > MAX_FILTERS {
            bail!("a stream has more than {MAX_FILTERS} filters");
        }
        let parms: Vec<Object> = match self.get(dict, "DecodeParms") {
            Object::Array(a) => a.into_iter().map(|o| self.resolve(o)).collect(),
            other => vec![other],
        };
        let mut body = raw.to_vec();
        for (i, filter) in filters.iter().enumerate() {
            body = match filter.as_str() {
                "FlateDecode" | "Fl" => {
                    let (body, all) = inflate(&body)?;
                    if !all && whole {
                        bail!("a compressed part of the file is damaged");
                    }
                    if !all {
                        self.unreadable(|| "a compressed part of the file is damaged".into());
                    }
                    body
                }
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
            self.spend(body.len())?;
        }
        Ok(body)
    }

    /// Every page of the document, in order.
    fn pages(&self) -> Vec<HashMap<String, Object>> {
        self.numbered_pages().into_iter().map(|(_, p)| p).collect()
    }

    /// Every page of the document, in order, with its object number when it has one.
    fn numbered_pages(&self) -> Vec<(Option<u32>, HashMap<String, Object>)> {
        let root = self.resolve(self.trailer.get("Root").cloned().unwrap_or(Object::Null));
        let Some(catalog) = root.as_dict() else {
            return Vec::new();
        };
        const INHERITED: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let pages = catalog.get("Pages").cloned().unwrap_or(Object::Null);
        let number = match pages {
            Object::Ref(n) => Some(n),
            _ => None,
        };
        let mut stack = vec![(number, self.resolve(pages), Rc::new(HashMap::new()))];
        let mut nodes = 0usize;
        while let Some((number, node, inherited)) = stack.pop() {
            nodes += 1;
            if out.len() >= MAX_PAGES || nodes > MAX_OBJECTS {
                self.unreadable(|| "the PDF exceeded its page traversal limit".into());
                break;
            }
            let Some(dict) = node.as_dict() else { continue };
            // What a page inherits from the node above it, shared by every node below.
            let passed = if INHERITED.iter().any(|k| dict.contains_key(*k)) {
                let mut passed = (*inherited).clone();
                for key in INHERITED {
                    if let Some(v) = dict.get(key) {
                        passed.insert(key.to_string(), v.clone());
                    }
                }
                Rc::new(passed)
            } else {
                inherited
            };
            match self.get(dict, "Type").as_name() {
                Some("Page") => {
                    let mut page = dict.clone();
                    for (k, v) in passed.iter() {
                        page.entry(k.clone()).or_insert_with(|| v.clone());
                    }
                    out.push((number, page));
                }
                _ => {
                    let Object::Array(kids) = self.get(dict, "Kids") else {
                        continue;
                    };
                    for kid in kids.into_iter().rev() {
                        let number = match kid {
                            Object::Ref(n) => Some(n),
                            _ => None,
                        };
                        if let Some(n) = number {
                            if !seen.insert(n) {
                                continue;
                            }
                        }
                        stack.push((number, self.resolve(kid), passed.clone()));
                    }
                }
            }
        }
        out
    }

    /// The text of the whole document, a line to a line of the page, without the running
    /// headers and footers, with the byte each page's text starts at.
    pub fn text_pages(&self) -> Result<(String, Vec<usize>)> {
        self.read_text(true)
    }

    /// Like [`Document::text_pages`], leaving out the running headers and footers when
    /// `drop_running` is set.
    fn read_text(&self, drop_running: bool) -> Result<(String, Vec<usize>)> {
        let pages = self.pages();
        if pages.is_empty() {
            bail!("the file lists no pages");
        }
        let mut read = Vec::with_capacity(pages.len());
        let mut used = 0;
        let mut cut = false;
        for (i, page) in pages.iter().enumerate() {
            self.check()?;
            let separator = usize::from(i > 0);
            let Some(room) = super::MAX_OUTPUT.checked_sub(used + separator) else {
                self.unreadable(|| "the PDF has too much decoded text".into());
                break;
            };
            let resources = self.get(page, "Resources");
            let content = self.page_content(page);
            let (display, edges) = self.frame(page);
            let at = (display, i);
            let lines = self.page_text(&content, &resources, 0, room, &mut cut, at);
            if !(cut && lines.is_empty()) {
                used += separator + lines.iter().map(|l| l.text.len() + 1).sum::<usize>();
                read.push((i, lines, edges));
            }
            if cut {
                break;
            }
        }
        // The running headers and footers are left out, as the formatted comparison leaves
        // them out (see [`running_lines`]). Each placed line lends its text to a run while
        // they are found, and gets it back.
        if drop_running {
            // Which line of its page each run was lent by.
            let mut lenders: Vec<Vec<usize>> = Vec::with_capacity(read.len());
            let placed: Vec<(Vec<Run>, Option<Edges>)> = read
                .iter_mut()
                .map(|(_, lines, edges)| {
                    let mut from = Vec::new();
                    let mut runs = Vec::new();
                    for (k, line) in lines.iter_mut().enumerate() {
                        if let Some(mut run) = line.at.take() {
                            run.text = std::mem::take(&mut line.text);
                            runs.push(run);
                            from.push(k);
                        }
                    }
                    lenders.push(from);
                    (runs, *edges)
                })
                .collect();
            let gone = running_lines(&placed, || self.check(), false)?;
            let back = read.iter_mut().zip(placed).zip(gone).zip(lenders);
            for ((((_, lines, _), (runs, _)), gone), from) in back {
                for ((run, gone), k) in runs.into_iter().zip(gone).zip(from) {
                    lines[k].text = run.text;
                    lines[k].dropped = gone;
                }
            }
        }
        let mut out = String::new();
        let mut starts = Vec::with_capacity(read.len());
        for (i, lines, _) in read {
            if i > 0 {
                out.push('\n');
            }
            starts.push(out.len());
            for line in lines.into_iter().filter(|line| !line.dropped) {
                out.push_str(&line.text);
                out.push('\n');
            }
        }
        self.check()?;
        if let Some(why) = self.unreadable.borrow().as_ref() {
            if out.trim().is_empty() {
                bail!("{why}");
            }
            out.push('\n');
            out.push_str(&crate::i18n::trf(
                "msg.extract_incomplete",
                &[("reason", why.clone())],
            ));
            out.push('\n');
        }
        Ok((out, starts))
    }

    /// The content streams of a page, one after another.
    fn page_content(&self, page: &HashMap<String, Object>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut add = |object: Object| {
            if let Object::Stream(dict, start, end) = object {
                match self.decode(&dict, start, end) {
                    Ok(body) if out.len() + body.len() <= MAX_STREAM => {
                        out.extend_from_slice(&body);
                        out.push(b'\n');
                    }
                    Ok(_) => self.unreadable(|| {
                        format!("a page has more than {} MiB of content", MAX_STREAM >> 20)
                    }),
                    Err(e) => self.unreadable(|| e.to_string()),
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

    /// How each font of `resources` turns the codes in the content into characters.
    fn fonts(&self, resources: &Object) -> HashMap<String, Rc<Font>> {
        let Some(resources) = resources.as_dict() else {
            return HashMap::new();
        };
        let fonts = self.get(resources, "Font");
        let Some(fonts) = fonts.as_dict() else {
            return HashMap::new();
        };
        fonts
            .iter()
            .map(|(name, font)| {
                let read = || Rc::new(self.font(self.resolve(font.clone()).as_dict()));
                let font = match font {
                    Object::Ref(n) => {
                        let cached = self.fonts.borrow().get(n).cloned();
                        cached.unwrap_or_else(|| {
                            let font = read();
                            self.fonts.borrow_mut().insert(*n, font.clone());
                            font
                        })
                    }
                    _ => read(),
                };
                (name.clone(), font)
            })
            .collect()
    }

    /// The text of the form `name` of `resources` that the content draws, read as a page is.
    fn form_text(
        &self,
        resources: &Object,
        name: &str,
        depth: usize,
        room: usize,
        cut: &mut bool,
        (ctm, page): (Matrix, usize),
    ) -> Vec<TextLine> {
        let Some(xobjects) = resources.as_dict().map(|r| self.get(r, "XObject")) else {
            return Vec::new();
        };
        let Some(Object::Stream(dict, start, end)) = xobjects.as_dict().map(|x| self.get(x, name))
        else {
            return Vec::new();
        };
        // An image is drawn the same way, and has no text.
        if self.get(&dict, "Subtype").as_name() != Some("Form") {
            return Vec::new();
        }
        if depth >= MAX_FORM_DEPTH || self.forms.get() >= MAX_FORMS {
            self.unreadable(|| "the PDF exceeded its form traversal limit".into());
            return Vec::new();
        }
        self.forms.set(self.forms.get() + 1);
        let body = match self.decode(&dict, start, end) {
            Ok(body) => body,
            Err(e) => {
                self.unreadable(|| e.to_string());
                return Vec::new();
            }
        };
        // Where the form is placed: its own matrix, then where it is drawn.
        let matrix = match self.get(&dict, "Matrix") {
            Object::Array(m) => numbers::<6>(&m).unwrap_or(IDENTITY),
            _ => IDENTITY,
        };
        let own = self.get(&dict, "Resources");
        let resources = if own.as_dict().is_some() {
            &own
        } else {
            resources
        };
        let at = (then(matrix, ctm), page);
        self.page_text(&body, resources, depth + 1, room, cut, at)
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
            match self.decode(&d, start, end) {
                Ok(body) => font.to_unicode = read_cmap(&body),
                Err(e) => self.unreadable(|| e.to_string()),
            }
        }
        font.unmapped = font.two_byte && font.to_unicode.is_empty();
        self.read_weight(dict, &mut font);
        self.read_widths(dict, &mut font);
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
                                // A name that is not known is not the character the
                                // encoding has at its code; `.notdef` names no glyph.
                                if let Some(text) = glyph_text(&name) {
                                    font.differences.insert(code, text);
                                } else if name != ".notdef" {
                                    font.differences.insert(code, '\u{fffd}'.into());
                                }
                                code = code.saturating_add(1);
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

    /// Reads a page's or a form's content, keeping what it draws as text, a line at a time with
    /// where each is drawn; `depth` is how many forms it is inside, and `at` where the content
    /// is placed on the displayed page and which page that is.
    fn page_text(
        &self,
        content: &[u8],
        resources: &Object,
        depth: usize,
        budget: usize,
        cut: &mut bool,
        (start, page): (Matrix, usize),
    ) -> Vec<TextLine> {
        let fonts = self.fonts(resources);
        let mut out = Lines::default();
        let mut ops = Operations::new(content);
        let mut font: Option<&Font> = None;
        let default = Font::default();
        let mut line = String::new();
        // Where the text drawn last on the line ends, the direction its line runs in and its
        // size, as placed on the page or form; a new position starts a line only when it
        // leaves that line or steps back along it, so text drawn a glyph at a time, as Chinese
        // often is, reads as lines, whichever way the line runs.
        let mut text = TextState::default();
        let (mut ctm, mut saved) = (start, Saved::default());
        let mut last: Option<Drawn> = None;
        // Where the line being read starts, ends and how large it is drawn.
        let mut line_at: Option<Run> = None;
        let flush = |line: &mut String,
                     out: &mut Lines,
                     last: &mut Option<Drawn>,
                     line_at: &mut Option<Run>| {
            out.push(line.trim_end(), line_at.take());
            line.clear();
            *last = None;
        };
        while let Some(operator) = ops.next() {
            let operands = &ops.operands;
            match operator.as_str() {
                "Tf" => {
                    if let Some(Object::Name(name)) = operands.iter().rev().nth(1) {
                        font = fonts.get(name).map(Rc::as_ref);
                    }
                    if let Some([size]) = numbers::<1>(operands) {
                        text.size = size;
                    }
                }
                "Do" => {
                    if let Some(Object::Name(name)) = operands.last() {
                        flush(&mut line, &mut out, &mut last, &mut line_at);
                        let room = budget.saturating_sub(out.size);
                        let at = (ctm, page);
                        out.extend(self.form_text(resources, name, depth, room, cut, at));
                    }
                }
                // The font and the text state are part of the graphics state; the text
                // matrices are not.
                "q" => {
                    saved.save((ctm, font, text));
                }
                "Q" => {
                    if let Some((transform, selected_font, state)) = saved.restore() {
                        ctm = transform;
                        font = selected_font;
                        text = text.restored(state);
                    }
                }
                "cm" => {
                    if let Some(m) = numbers::<6>(operands) {
                        ctm = then(m, ctm);
                    }
                }
                "BT" => (text.tm, text.tlm) = (IDENTITY, IDENTITY),
                "Tm" => {
                    if let Some(m) = numbers::<6>(operands) {
                        (text.tm, text.tlm) = (m, m);
                    }
                }
                "Td" | "TD" => {
                    if let Some([tx, ty]) = numbers::<2>(operands) {
                        text.next_line(tx, ty);
                        if operator == "TD" {
                            text.leading = -ty;
                        }
                    }
                }
                "TL" => text.leading = numbers::<1>(operands).map_or(text.leading, |[l]| l),
                "Tc" => text.char_space = numbers::<1>(operands).map_or(0.0, |[n]| n),
                "Tw" => text.word_space = numbers::<1>(operands).map_or(0.0, |[n]| n),
                "Tz" => text.scale = numbers::<1>(operands).map_or(100.0, |[n]| n) / 100.0,
                "T*" => text.next_line(0.0, -text.leading),
                "Tj" | "'" | "\"" | "TJ" => {
                    if operator == "\"" {
                        let number = |k: usize| operands.get(k).and_then(Object::as_number);
                        if let (Some(w), Some(c)) = (number(0), number(1)) {
                            (text.word_space, text.char_space) = (w, c);
                        }
                    }
                    if operator == "'" || operator == "\"" {
                        text.next_line(0.0, -text.leading);
                        flush(&mut line, &mut out, &mut last, &mut line_at);
                    }
                    let place = then(text.tm, ctm);
                    let size = text.size.abs() * place[2].hypot(place[3]);
                    // One byte is kept for a space put before the text, which only text going
                    // on a line already begun can have.
                    let space = usize::from(!line.is_empty());
                    let room = budget.saturating_sub(out.size + line.len() + space);
                    let Some(shown) = font.unwrap_or(&default).show(operands.last(), Some(room))
                    else {
                        self.no_characters();
                        continue;
                    };
                    match last.map(|before| (before.size, before.along(place, size))) {
                        Some((before, Some(gap))) => {
                            let size = size.max(before);
                            if spaced(line.chars().last(), shown.text.chars().next(), gap, size)
                                && !line.ends_with(char::is_whitespace)
                                && !shown.text.starts_with(char::is_whitespace)
                            {
                                line.push(' ');
                            }
                        }
                        _ => flush(&mut line, &mut out, &mut last, &mut line_at),
                    }
                    line.push_str(&shown.text);
                    text.advance(&shown);
                    let end = then(text.tm, ctm);
                    last = Drawn::after(place, end, size);
                    match &mut line_at {
                        Some(at) => {
                            at.end = end[4];
                            at.size = at.size.max(size);
                        }
                        None => {
                            line_at = Some(Run {
                                text: String::new(),
                                bold: false,
                                italic: false,
                                page,
                                y: place[5],
                                x: place[4],
                                end: end[4],
                                size,
                            });
                        }
                    }
                    if shown.cut {
                        self.unreadable(|| "the PDF has too much decoded text".into());
                        *cut = true;
                    }
                }
                _ => {}
            }
            if *cut {
                break;
            }
        }
        flush(&mut line, &mut out, &mut last, &mut line_at);
        out.lines
    }

    /// Notes that a font's codes are glyph numbers, which are not characters.
    fn no_characters(&self) {
        self.unreadable(|| "a font gives no characters for its glyphs".to_string());
    }

    /// How wide `font`'s glyphs are, in thousandths of its size, from its own `Widths` or, for
    /// a composite font, the `W` of the font it is made of.
    fn read_widths(&self, dict: &HashMap<String, Object>, font: &mut Font) {
        let number = |o: Object| self.resolve(o).as_number();
        if font.two_byte {
            let Object::Array(fonts) = self.get(dict, "DescendantFonts") else {
                return;
            };
            let Some(cid) = fonts.first().map(|f| self.resolve(f.clone())) else {
                return;
            };
            let Some(cid) = cid.as_dict() else { return };
            font.default_width = self.get(cid, "DW").as_number().unwrap_or(1000.0);
            let Object::Array(w) = self.get(cid, "W") else {
                return;
            };
            // `c [w1 w2 …]` gives the widths from code c on; `c1 c2 w` one width for a range.
            let mut items = w.into_iter().map(|o| self.resolve(o)).peekable();
            while let Some(first) = items.next() {
                let Some(first) = first.as_number().map(|n| n as u32) else {
                    break;
                };
                match items.next() {
                    Some(Object::Array(ws)) => {
                        for (k, w) in ws.into_iter().enumerate() {
                            if let Some(w) = number(w) {
                                font.widths.insert(first.saturating_add(k as u32), w);
                            }
                            if font.widths.len() >= MAX_CMAP {
                                return;
                            }
                        }
                    }
                    Some(Object::Number(last)) => {
                        let Some(w) = items.next().and_then(|o| o.as_number()) else {
                            break;
                        };
                        let last = (last as u32).min(first.saturating_add(MAX_CMAP as u32));
                        for code in first..=last {
                            font.widths.insert(code, w);
                        }
                        if font.widths.len() >= MAX_CMAP {
                            return;
                        }
                    }
                    _ => break,
                }
            }
        } else {
            let first = self.get(dict, "FirstChar").as_number().unwrap_or(0.0) as u32;
            if let Object::Array(ws) = self.get(dict, "Widths") {
                for (k, w) in ws.into_iter().take(256).enumerate() {
                    if let Some(w) = number(w) {
                        font.widths.insert(first.saturating_add(k as u32), w);
                    }
                }
            }
            let descriptor = self.get(dict, "FontDescriptor");
            if let Some(missing) = descriptor
                .as_dict()
                .and_then(|d| self.get(d, "MissingWidth").as_number())
                .filter(|w| *w > 0.0)
            {
                font.default_width = missing;
            }
        }
    }
}

/// The operations of a content stream one at a time: each operator, with its operands in
/// `operands`. An image written into the content is passed over, since its bytes are not
/// operators.
struct Operations<'c> {
    content: &'c [u8],
    lexer: Lexer<'c>,
    operands: Vec<Object>,
}

impl<'c> Operations<'c> {
    fn new(content: &'c [u8]) -> Self {
        Operations {
            content,
            lexer: Lexer::new(content, 0),
            operands: Vec::new(),
        }
    }

    fn next(&mut self) -> Option<String> {
        self.operands.clear();
        loop {
            self.lexer.skip_space();
            let b = *self.content.get(self.lexer.at)?;
            if matches!(b, b'/' | b'(' | b'[' | b'<' | b'+' | b'-' | b'.') || b.is_ascii_digit() {
                let o = self.lexer.object(0)?;
                if self.operands.len() < 64 {
                    self.operands.push(o);
                }
                continue;
            }
            let operator = self.lexer.word();
            if operator.is_empty() {
                self.lexer.at += 1;
                continue;
            }
            if operator == "BI" {
                self.lexer.at = after_inline_image(self.content, self.lexer.at);
                self.operands.clear();
            }
            return Some(operator);
        }
    }
}

/// What one text-showing operation draws.
struct Shown {
    text: String,
    cut: bool,
    /// How far it moves along the line, in thousandths of the font size, with the steps
    /// between its runs; how many glyphs it draws, and how many of them are single-byte spaces.
    units: f64,
    glyphs: usize,
    spaces: usize,
}

/// How one font's codes become characters.
#[derive(Default, Clone)]
struct Font {
    two_byte: bool,
    /// A composite font without a `ToUnicode` table, whose codes cannot be read as text.
    unmapped: bool,
    to_unicode: HashMap<u32, String>,
    encoding: Option<String>,
    differences: HashMap<u32, String>,
    /// Whether its name or description says it is bold, or slanted.
    bold: bool,
    italic: bool,
    /// How wide each glyph is, in thousandths of the font size, and a glyph it does not list;
    /// 0 when the font says nothing, read as half the size.
    widths: HashMap<u32, f64>,
    default_width: f64,
}

impl Font {
    fn codes(&self, bytes: &[u8]) -> Vec<u32> {
        if self.two_byte {
            bytes
                .chunks(2)
                .map(|p| ((p[0] as u32) << 8) | *p.get(1).unwrap_or(&0) as u32)
                .collect()
        } else {
            bytes.iter().map(|b| *b as u32).collect()
        }
    }

    fn width(&self, code: u32) -> f64 {
        match self.widths.get(&code) {
            Some(w) => *w,
            None if self.default_width > 0.0 => self.default_width,
            None => 500.0,
        }
    }

    /// What a text-showing operation with the string or array `operand` draws: its text,
    /// with a space for a large step forward between runs, and how far it moves. `None` when
    /// its codes are glyph numbers, which are not characters. With no text
    /// budget, measure glyph advances without decoding any text.
    fn show(&self, operand: Option<&Object>, room: Option<usize>) -> Option<Shown> {
        if self.unmapped && room.is_some() {
            return None;
        }
        let mut shown = Shown {
            text: String::new(),
            cut: false,
            units: 0.0,
            glyphs: 0,
            spaces: 0,
        };
        let add = |bytes: &[u8], shown: &mut Shown| {
            for code in self.codes(bytes) {
                if let Some(room) = room {
                    let mut buffer = [0; 4];
                    let text = if let Some(text) = self.to_unicode.get(&code) {
                        text.as_str()
                    } else if let Some(text) = self.differences.get(&code) {
                        text.as_str()
                    } else {
                        let c = match self.encoding.as_deref() {
                            Some("MacRomanEncoding") => mac_roman(code),
                            _ => win_ansi(code),
                        };
                        // A control code is a glyph the font names some other way, such as
                        // a ligature of TeX's fonts; which one is not known.
                        // A tab or a line end is the space it always was.
                        let c = if c.is_control() && !matches!(c, '\t' | '\n' | '\r') {
                            '\u{fffd}'
                        } else {
                            c
                        };
                        c.encode_utf8(&mut buffer)
                    };
                    let remaining = room.saturating_sub(shown.text.len());
                    if text.len() > remaining {
                        shown
                            .text
                            .push_str(&text[..text.floor_char_boundary(remaining)]);
                        shown.cut = true;
                        return;
                    }
                    shown.text.push_str(text);
                }
                shown.units += self.width(code);
                shown.glyphs += 1;
                if !self.two_byte && code == 32 {
                    shown.spaces += 1;
                }
            }
        };
        match operand {
            Some(Object::Text(bytes)) => add(bytes, &mut shown),
            Some(Object::Array(items)) => {
                for item in items {
                    match item {
                        Object::Text(bytes) => add(bytes, &mut shown),
                        Object::Number(n) => {
                            shown.units -= n;
                            // A large step forward between runs is a space.
                            if let Some(room) = room {
                                if *n < -180.0 && !shown.text.ends_with(' ') {
                                    if shown.text.len() >= room {
                                        shown.cut = true;
                                        break;
                                    }
                                    shown.text.push(' ');
                                }
                            }
                        }
                        _ => {}
                    }
                    if shown.cut {
                        break;
                    }
                }
            }
            _ => {}
        }
        Some(shown)
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
                                if out.len() >= MAX_CMAP {
                                    break;
                                }
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
                                let Some(code) = lo.checked_add(i as u32).filter(|&c| c <= hi)
                                else {
                                    break;
                                };
                                if let Object::Text(to) = item {
                                    out.insert(code, utf16be(to));
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
        if out.len() >= MAX_CMAP {
            break;
        }
    }
    out
}

/// Where the content goes on after an inline image whose `BI` ends at `at`: past its `ID`,
/// the image's bytes, and the `EI` that ends them, or the end when there is no `EI`.
fn after_inline_image(content: &[u8], at: usize) -> usize {
    let space = |i: usize| content.get(i).is_none_or(|b| b.is_ascii_whitespace());
    let word_at = |i: usize, word: &[u8]| {
        content[i..].starts_with(word) && (i == 0 || space(i - 1)) && space(i + word.len())
    };
    let Some(data) = (at..content.len()).find(|&i| word_at(i, b"ID")) else {
        return content.len();
    };
    (data + 3..content.len())
        .find(|&i| word_at(i, b"EI"))
        .map_or(content.len(), |i| i + 2)
}

/// Why a file is not read past the work it has asked for.
fn too_much() -> String {
    format!(
        "the file needs more than {} MiB to be decoded",
        MAX_DECODED >> 20
    )
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
                        ¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄¤‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
    if code < 0x80 {
        return char::from_u32(code).unwrap_or('\u{fffd}');
    }
    HIGH.chars()
        .nth((code - 0x80) as usize)
        .unwrap_or('\u{fffd}')
}

/// The names of the characters U+00A0 to U+00FF, in order.
const LATIN1_NAMES: &str = "\
    nbspace exclamdown cent sterling currency yen brokenbar section \
    dieresis copyright ordfeminine guillemotleft logicalnot sfthyphen \
    registered macron degree plusminus twosuperior threesuperior acute mu \
    paragraph periodcentered cedilla onesuperior ordmasculine guillemotright \
    onequarter onehalf threequarters questiondown Agrave Aacute Acircumflex \
    Atilde Adieresis Aring AE Ccedilla Egrave Eacute Ecircumflex \
    Edieresis Igrave Iacute Icircumflex Idieresis Eth Ntilde Ograve \
    Oacute Ocircumflex Otilde Odieresis multiply Oslash Ugrave Uacute \
    Ucircumflex Udieresis Yacute Thorn germandbls agrave aacute \
    acircumflex atilde adieresis aring ae ccedilla egrave eacute \
    ecircumflex edieresis igrave iacute icircumflex idieresis eth ntilde \
    ograve oacute ocircumflex otilde odieresis divide oslash ugrave \
    uacute ucircumflex udieresis yacute thorn ydieresis";

/// The text a glyph name stands for, for the names a font lists for itself: its characters,
/// or the letters of a ligature, which is how a document compares with one that spells them
/// out. A suffix after a `.` names a variant of the same glyph, and names joined by `_` a
/// ligature of them.
fn glyph_text(name: &str) -> Option<String> {
    let name = name.split('.').next().unwrap_or(name);
    if name.contains('_') {
        return name
            .split('_')
            .map(glyph_text)
            .collect::<Option<String>>()
            .filter(|t| !t.is_empty());
    }
    match name {
        "ff" | "ffi" | "ffl" | "fi" | "fl" | "IJ" | "ij" => return Some(name.to_string()),
        "uniFB00" => return Some("ff".into()),
        "uniFB01" => return Some("fi".into()),
        "uniFB02" => return Some("fl".into()),
        "uniFB03" => return Some("ffi".into()),
        "uniFB04" => return Some("ffl".into()),
        _ => {}
    }
    // `uniXXXX`, with one or more groups of four hex digits, names characters by number.
    if let Some(hex) = name.strip_prefix("uni") {
        if !hex.is_empty() && hex.len() % 4 == 0 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return (0..hex.len())
                .step_by(4)
                .map(|i| {
                    u32::from_str_radix(&hex[i..i + 4], 16)
                        .ok()
                        .and_then(char::from_u32)
                })
                .collect();
        }
    }
    glyph_char(name).map(String::from)
}

/// The character a single glyph name stands for.
fn glyph_char(name: &str) -> Option<char> {
    // `uXXXX` to `uXXXXXX` names one character by number.
    if let Some(hex) = name.strip_prefix('u') {
        if (4..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
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
        ("quotesinglbase", '\u{201a}'),
        ("quotedblbase", '\u{201e}'),
        ("guilsinglleft", '\u{2039}'),
        ("guilsinglright", '\u{203a}'),
        ("dagger", '\u{2020}'),
        ("daggerdbl", '\u{2021}'),
        ("ellipsis", '\u{2026}'),
        ("perthousand", '\u{2030}'),
        ("trademark", '\u{2122}'),
        ("minus", '\u{2212}'),
        ("fraction", '\u{2044}'),
        ("Euro", '\u{20ac}'),
        ("florin", '\u{0192}'),
        ("dotlessi", '\u{0131}'),
        ("dotlessj", '\u{0237}'),
        ("OE", '\u{0152}'),
        ("oe", '\u{0153}'),
        ("Scaron", '\u{0160}'),
        ("scaron", '\u{0161}'),
        ("Zcaron", '\u{017d}'),
        ("zcaron", '\u{017e}'),
        ("Ydieresis", '\u{0178}'),
        ("Lslash", '\u{0141}'),
        ("lslash", '\u{0142}'),
        ("grave", '`'),
        ("asciicircum", '^'),
        ("asciitilde", '~'),
        ("circumflex", '\u{02c6}'),
        ("tilde", '\u{02dc}'),
        ("breve", '\u{02d8}'),
        ("dotaccent", '\u{02d9}'),
        ("ring", '\u{02da}'),
        ("ogonek", '\u{02db}'),
        ("hungarumlaut", '\u{02dd}'),
        ("caron", '\u{02c7}'),
    ];
    if let Some((_, c)) = named.iter().find(|(n, _)| *n == name) {
        return Some(*c);
    }
    if let Some(i) = LATIN1_NAMES.split_whitespace().position(|n| n == name) {
        return char::from_u32(0xa0 + i as u32);
    }
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
        _ => None,
    }
}

// ---------- the filters a stream may be written with ----------

/// How the reading of a Flate stream ended.
#[derive(PartialEq)]
enum Inflated {
    /// At the end the stream marks, or at the most bytes a stream is read for.
    Whole,
    /// Out of bytes before the end the stream marks.
    Cut,
    Damaged,
}

/// The bytes of the Flate stream `body` up to where its reading ended, with the two bytes
/// and the checksum of a zlib stream around it when `zlib` is set.
fn inflate_as(body: &[u8], zlib: bool) -> (Vec<u8>, Inflated) {
    let mut reader = flate2::Decompress::new(zlib);
    let mut out = Vec::new();
    loop {
        if out.len() >= MAX_STREAM {
            return (out, Inflated::Whole);
        }
        // Room to write into, so that a step that reads and writes nothing is out of bytes.
        if out.len() == out.capacity() {
            out.reserve(out.len().clamp(32 << 10, 8 << 20));
        }
        let (read, written) = (reader.total_in(), reader.total_out());
        let rest = body.get(read as usize..).unwrap_or(&[]);
        match reader.decompress_vec(rest, &mut out, flate2::FlushDecompress::None) {
            Ok(flate2::Status::StreamEnd) => return (out, Inflated::Whole),
            Ok(_) if reader.total_in() == read && reader.total_out() == written => {
                return (out, Inflated::Cut)
            }
            Ok(_) => {}
            Err(_) => return (out, Inflated::Damaged),
        }
    }
}

/// A Flate stream's bytes, and whether they are all there: a stream damaged or cut short
/// keeps what came before the damage. A stream that holds nothing is read as nothing.
fn inflate(body: &[u8]) -> Result<(Vec<u8>, bool)> {
    if body.is_empty() {
        return Ok((Vec::new(), true));
    }
    let (out, ended) = inflate_as(body, true);
    if ended == Inflated::Whole {
        return Ok((out, true));
    }
    // Some files leave off the two bytes a zlib stream starts with.
    let (raw_out, raw_ended) = inflate_as(body, false);
    if raw_ended == Inflated::Whole && !raw_out.is_empty() {
        return Ok((raw_out, true));
    }
    // A stream cut short of its checksum alone still holds all of its bytes.
    if ended == Inflated::Cut {
        let (data, data_ended) = inflate_as(body.get(2..).unwrap_or(&[]), false);
        if data_ended == Inflated::Whole {
            return Ok((data, true));
        }
    }
    let partial = if raw_out.len() > out.len() {
        raw_out
    } else {
        out
    };
    if partial.is_empty() {
        bail!("a compressed part of the file could not be read");
    }
    Ok((partial, false))
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
    // A row wider than the data is the only one, and its bytes past the data are not there:
    // reading it no wider than the data gives the same bytes without making room for the rest.
    let row = row.min(body.len().saturating_sub(1)).max(1);
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

/// The text of the PDF file `bytes`, given up on once `cancel` is set.
#[cfg(test)]
pub fn text(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<String> {
    Ok(Document::read(bytes, cancel)?.text_pages()?.0)
}

/// The text of the PDF file `bytes` with every line it draws, its running headers and
/// footers included, for tests that read back what a file draws.
#[cfg(test)]
pub(crate) fn text_as_drawn(bytes: &[u8]) -> Result<String> {
    Ok(Document::read(bytes, None)?.read_text(false)?.0)
}

// ---------- the structure of a tagged file ----------
//
// A tagged file says what its content is as well as where it is drawn: a tree of elements
// such as headings, paragraphs, lists and table cells, each leading to the pieces of the pages'
// content drawn for it, which the content marks with numbers (`/P <</MCID 3>> BDC … EMC`).
// Content marked as an artifact, such as a running header or page number, is in no element.

/// The most elements of a structure tree read, and how deep inside one another.
const MAX_ELEMENTS: usize = 200_000;
const MAX_STRUCTURE_DEPTH: usize = 64;

/// A run of text drawn in one font, with the line it is on.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    /// The page it is drawn on, how far up the page its line is, where along the line it
    /// starts and ends, and how large its font is drawn.
    pub page: usize,
    pub y: f64,
    pub x: f64,
    pub end: f64,
    pub size: f64,
}

/// Whether a step of `gap` along a line, with text of `size`, is wide enough to be a space;
/// a step back past where the text was is one too, as from one column to the next.
pub(super) fn gap_is_space(gap: f64, size: f64) -> bool {
    gap > 0.2 * size || gap < -size
}

/// Whether text ending with `before` and text starting with `after`, drawn `gap` apart along
/// one line at `size`, are set apart by a space. Chinese and Japanese are written without
/// spaces, and justified lines of them spread their characters apart, so between two of their
/// characters only a gap of a whole character or more is a space.
pub(super) fn spaced(before: Option<char>, after: Option<char>, gap: f64, size: f64) -> bool {
    if before.is_some_and(is_unspaced) && after.is_some_and(is_unspaced) {
        gap > size || gap < -size
    } else {
        gap_is_space(gap, size)
    }
}

/// Whether `c` belongs to a script written without spaces between words, such as Chinese
/// characters, kana, or the punctuation and full-width forms used with them. Hangul is not,
/// since Korean puts spaces between words.
pub(super) fn is_unspaced(c: char) -> bool {
    matches!(c as u32,
        0x2E80..=0x312F
        | 0x3190..=0x9FFF
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0x20000..=0x3FFFD)
}

/// What one marked piece of a page draws.
#[derive(Default)]
struct Marked {
    runs: Vec<Run>,
    /// The fingerprints of the images drawn.
    images: Vec<String>,
}

/// What an element holds: other elements, and the text drawn for it.
#[derive(Debug)]
pub(super) enum Kid {
    Element(Element),
    Text(Vec<Run>),
}

/// An element of a structure tree.
#[derive(Debug, Default)]
pub(super) struct Element {
    /// Its standard type, such as `P`, `H1`, `L`, `Table` or `TD`; a type the file names for
    /// itself is the standard one its role map gives.
    pub role: String,
    /// The description it gives, as of a figure, and the text it says it stands for.
    pub alt: Option<String>,
    pub actual: Option<String>,
    /// How many rows and columns a table cell spans.
    pub rows: usize,
    pub cols: usize,
    /// Where a link leads.
    pub link: Option<String>,
    /// The fingerprints of the images drawn for it.
    pub images: Vec<String>,
    pub kids: Vec<Kid>,
}

/// A transformation, as the six numbers `a b c d e f` of a PDF matrix.
type Matrix = [f64; 6];
const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// `m` then `n`.
fn then(m: Matrix, n: Matrix) -> Matrix {
    [
        m[0] * n[0] + m[1] * n[2],
        m[0] * n[1] + m[1] * n[3],
        m[2] * n[0] + m[3] * n[2],
        m[2] * n[1] + m[3] * n[3],
        m[4] * n[0] + m[5] * n[2] + n[4],
        m[4] * n[1] + m[5] * n[3] + n[5],
    ]
}

/// The last `N` operands, when they are all numbers.
fn numbers<const N: usize>(stack: &[Object]) -> Option<[f64; N]> {
    let tail = stack.get(stack.len().checked_sub(N)?..)?;
    let mut out = [0.0; N];
    for (o, v) in out.iter_mut().zip(tail) {
        *o = v.as_number()?;
    }
    Some(out)
}

/// A text string of the file: UTF-16 after its byte-order mark, UTF-8 after its own, and
/// otherwise one byte a character.
fn text_string(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xfe, 0xff]) {
        utf16be(rest)
    } else if let Some(rest) = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        String::from_utf8_lossy(rest).into_owned()
    } else {
        bytes.iter().map(|&b| win_ansi(b as u32)).collect()
    }
}

/// Where text is drawn: the text and line matrices, and the state that says how far each
/// glyph moves.
#[derive(Clone, Copy)]
struct TextState {
    tm: Matrix,
    tlm: Matrix,
    leading: f64,
    size: f64,
    char_space: f64,
    word_space: f64,
    /// The horizontal scale, 1 for none.
    scale: f64,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            tm: IDENTITY,
            tlm: IDENTITY,
            leading: 0.0,
            size: 0.0,
            char_space: 0.0,
            word_space: 0.0,
            scale: 1.0,
        }
    }
}

impl TextState {
    /// This state with the graphics state `saved` put back: the font size and the spacing
    /// are part of the graphics state, and the text matrices are not.
    fn restored(self, saved: TextState) -> TextState {
        TextState {
            tm: self.tm,
            tlm: self.tlm,
            ..saved
        }
    }

    fn next_line(&mut self, tx: f64, ty: f64) {
        self.tlm = then([1.0, 0.0, 0.0, 1.0, tx, ty], self.tlm);
        self.tm = self.tlm;
    }

    /// Moves past what `shown` draws.
    fn advance(&mut self, shown: &Shown) {
        let tx = (shown.units / 1000.0 * self.size
            + self.char_space * shown.glyphs as f64
            + self.word_space * shown.spaces as f64)
            * self.scale;
        self.tm = then([1.0, 0.0, 0.0, 1.0, tx, 0.0], self.tm);
    }
}

/// The graphics states a content stream saves with `q` and puts back with `Q`.
struct Saved<T> {
    kept: Vec<T>,
    /// States saved past the most kept, whose `Q`s restore nothing rather than a state
    /// saved before them.
    unsaved: usize,
}

impl<T> Default for Saved<T> {
    fn default() -> Self {
        Saved {
            kept: Vec::new(),
            unsaved: 0,
        }
    }
}

impl<T> Saved<T> {
    /// The most states kept.
    const MOST: usize = 64;

    /// Saves `state`; `false` when it is past the most kept.
    fn save(&mut self, state: T) -> bool {
        if self.kept.len() < Self::MOST {
            self.kept.push(state);
            true
        } else {
            self.unsaved += 1;
            false
        }
    }

    /// The state the matching `q` saved, if it was kept.
    fn restore(&mut self) -> Option<T> {
        if self.unsaved > 0 {
            self.unsaved -= 1;
            None
        } else {
            self.kept.pop()
        }
    }
}

/// A line of text read from a page, with where it is drawn when that is known: its text is
/// left empty, and its baseline, start and end are those of the displayed page.
struct TextLine {
    text: String,
    at: Option<Run>,
    /// Whether it is a running header or footer, left out.
    dropped: bool,
}

/// The lines of text read from a page or form, and how many bytes they take as text.
#[derive(Default)]
struct Lines {
    lines: Vec<TextLine>,
    size: usize,
}

impl Lines {
    /// Adds the line `text` drawn `at`, unless it is empty.
    fn push(&mut self, text: &str, at: Option<Run>) {
        if !text.is_empty() {
            self.size += text.len() + 1;
            self.lines.push(TextLine {
                text: text.to_string(),
                at,
                dropped: false,
            });
        }
    }

    fn extend(&mut self, lines: Vec<TextLine>) {
        for line in lines {
            self.size += line.text.len() + 1;
            self.lines.push(line);
        }
    }
}

/// Where text drawn on a line ends, which way the line runs, and how large the text is.
#[derive(Clone, Copy)]
struct Drawn {
    end: (f64, f64),
    direction: (f64, f64),
    size: f64,
}

impl Drawn {
    /// What is known of text that starts at `start` and ends at `end`, both text matrices
    /// placed on the page; none when the text has no length to tell its direction from and no
    /// size, which leaves the next text to start a line.
    fn after(start: Matrix, end: Matrix, size: f64) -> Option<Self> {
        // The direction of the line is that of the text space's first axis.
        let length = start[0].hypot(start[1]);
        (length > 0.0).then(|| Drawn {
            end: (end[4], end[5]),
            direction: (start[0] / length, start[1] / length),
            size,
        })
    }

    /// How far along this text's line text placed at `place`, drawn at `size`, starts after it
    /// ends, or none when it is on another line: off the line by more than half the size, as
    /// a raised footnote mark or a lowered subscript is not, running another way, or stepping
    /// back along the line past where it was.
    fn along(&self, place: Matrix, size: f64) -> Option<f64> {
        let size = size.max(self.size).max(1.0);
        let length = place[0].hypot(place[1]);
        let same_way = length > 0.0
            && (place[0] * self.direction.0 + place[1] * self.direction.1) / length > 0.99;
        let (dx, dy) = (place[4] - self.end.0, place[5] - self.end.1);
        let (dir_x, dir_y) = self.direction;
        let gap = dx * dir_x + dy * dir_y;
        let off = dx * dir_y - dy * dir_x;
        (same_way && off.abs() <= 0.5 * size && gap >= -size).then_some(gap)
    }
}

/// How a piece of content is marked.
#[derive(Clone, Copy)]
enum Mark {
    /// Drawn for the elements that name this number.
    Id(i64),
    /// Not part of the document's content, such as a page number.
    Artifact,
    /// Marked for some other reason; what is inside belongs to what it is inside.
    Other,
}

/// What a run kept apart from the one before it costs beyond its text, counted against what
/// marked content may keep.
const RUN_COST: usize = 80;

/// What reading one page's marked content keeps track of.
struct Marking<'m> {
    page: usize,
    marks: Vec<Mark>,
    out: &'m mut HashMap<(usize, i64), Marked>,
    /// The bytes of text kept so far, on this page and the pages before it, with
    /// [`RUN_COST`] for each run.
    size: usize,
    /// Collect positioned text independently of marked-content IDs.
    inferred: bool,
    has_image: bool,
    cut: bool,
}

impl Marking<'_> {
    /// Keeps `run` in the marked piece being drawn, joined to the run before it when it goes
    /// on along the same line in the same format.
    fn add(&mut self, mut run: Run) {
        let added = run.text.len();
        if run.text.is_empty() {
            return;
        }
        if self.size + added + RUN_COST > super::MAX_OUTPUT {
            self.cut = true;
            return;
        }
        if self.inferred {
            if [run.x, run.y, run.end, run.size]
                .into_iter()
                .all(f64::is_finite)
            {
                run.text.shrink_to_fit();
                self.out.entry((self.page, 0)).or_default().runs.push(run);
                self.size += added + RUN_COST;
            }
            return;
        }
        let Some(piece) = self.current() else { return };
        let mut separator = 0;
        match piece.runs.last_mut() {
            Some(r)
                if r.bold == run.bold
                    && r.italic == run.italic
                    && r.page == run.page
                    && (r.y - run.y).abs() < 1.0 =>
            {
                let gap = run.x - r.end;
                if spaced(
                    r.text.chars().last(),
                    run.text.chars().next(),
                    gap,
                    r.size.max(run.size),
                ) && !r.text.ends_with(char::is_whitespace)
                    && !run.text.starts_with(char::is_whitespace)
                {
                    r.text.push(' ');
                    separator = 1;
                }
                r.text.push_str(&run.text);
                r.end = run.end;
                r.size = r.size.max(run.size);
            }
            _ => {
                run.text.shrink_to_fit();
                piece.runs.push(run);
                self.size += RUN_COST;
            }
        }
        self.size += added + separator;
    }

    /// The marked piece being drawn, if it is part of the content.
    fn content_id(&self) -> Option<i64> {
        self.marks.iter().rev().find_map(|m| match m {
            Mark::Id(n) => Some(Some(*n)),
            Mark::Artifact => Some(None),
            Mark::Other => None,
        })?
    }

    fn current(&mut self) -> Option<&mut Marked> {
        let id = self.content_id()?;
        Some(self.out.entry((self.page, id)).or_default())
    }
}

impl Document<'_> {
    /// Notes whether `font`'s name or description says it is bold, or slanted.
    fn read_weight(&self, dict: &HashMap<String, Object>, font: &mut Font) {
        if let Some(name) = self.get(dict, "BaseFont").as_name() {
            let name = name.rsplit('+').next().unwrap_or(name).to_ascii_lowercase();
            font.bold = ["bold", "black", "heavy", "semibold", "demi"]
                .iter()
                .any(|w| name.contains(w));
            font.italic = name.contains("italic") || name.contains("oblique");
        }
        // A composite font is described by the font it is made of.
        let descendant = match self.get(dict, "DescendantFonts") {
            Object::Array(fonts) => fonts.first().map(|f| self.resolve(f.clone())),
            _ => None,
        };
        let described = descendant
            .as_ref()
            .and_then(Object::as_dict)
            .unwrap_or(dict);
        if let Some(descriptor) = self.get(described, "FontDescriptor").as_dict() {
            let flags = self.get(descriptor, "Flags").as_number().unwrap_or(0.0) as u32;
            let weight = self
                .get(descriptor, "FontWeight")
                .as_number()
                .unwrap_or(0.0);
            font.italic |= flags & (1 << 6) != 0;
            font.bold |= flags & (1 << 18) != 0 || weight >= 600.0;
        }
    }

    /// The structure tree of a tagged file, with the text drawn for each element; `None` for
    /// a file that has none.
    fn structure(&self) -> Result<Option<Element>> {
        let root = self.resolve(self.trailer.get("Root").cloned().unwrap_or(Object::Null));
        let Some(catalog) = root.as_dict() else {
            return Ok(None);
        };
        let tree = self.get(catalog, "StructTreeRoot");
        let Some(tree) = tree.as_dict() else {
            return Ok(None);
        };
        let pages = self.numbered_pages();
        if pages.is_empty() {
            bail!("the file lists no pages");
        }
        let mut marked = HashMap::new();
        let mut size = 0;
        for (i, (_, page)) in pages.iter().enumerate() {
            self.check()?;
            let resources = self.get(page, "Resources");
            let content = self.page_content(page);
            let mut marking = Marking {
                page: i,
                marks: Vec::new(),
                out: &mut marked,
                size,
                inferred: false,
                has_image: false,
                cut: false,
            };
            self.read_marked(
                &content,
                &resources,
                IDENTITY,
                0,
                &mut marking,
                (None, TextState::default()),
            );
            if marking.cut {
                bail!("the PDF has too much tagged text to compare completely");
            }
            size = marking.size;
        }
        self.check()?;
        let roles = match self.get(tree, "RoleMap") {
            Object::Dict(map) => map
                .iter()
                .filter_map(|(k, v)| {
                    Some((k.clone(), self.resolve(v.clone()).as_name()?.to_string()))
                })
                .collect(),
            _ => HashMap::new(),
        };
        let mut walk = Walk {
            doc: self,
            roles,
            marked,
            page_of: pages
                .iter()
                .enumerate()
                .filter_map(|(i, (n, _))| Some(((*n)?, i)))
                .collect(),
            seen: HashSet::new(),
            count: 0,
        };
        let mut document = Element {
            role: "Document".to_string(),
            ..Default::default()
        };
        walk.add(
            tree.get("K").cloned().unwrap_or(Object::Null),
            None,
            &mut document,
            0,
        );
        self.check()?;
        Ok(Some(document))
    }

    /// Reads a page's or a form's content, keeping the text and images drawn in each marked
    /// piece; `ctm` is where the content is placed, `depth` how many forms it is inside.
    fn read_marked(
        &self,
        content: &[u8],
        resources: &Object,
        ctm: Matrix,
        depth: usize,
        marking: &mut Marking,
        inherited: (Option<&Font>, TextState),
    ) {
        let fonts = self.fonts(resources);
        let properties = resources
            .as_dict()
            .map(|r| self.get(r, "Properties"))
            .unwrap_or(Object::Null);
        let mut ops = Operations::new(content);
        let (mut font, mut text) = inherited;
        (text.tm, text.tlm) = (IDENTITY, IDENTITY);
        let default = Font::default();
        // The placement, with the states saved by `q`.
        let (mut ctm, mut saved) = (ctm, Saved::default());
        let marks_at_start = marking.marks.len();
        let mut count = 0usize;
        while let Some(operator) = ops.next() {
            count += 1;
            // Stops once cancelled or out of budget; the caller then fails.
            if count.is_multiple_of(4096) && self.check().is_err() {
                break;
            }
            let operands = &ops.operands;
            match operator.as_str() {
                "q" => {
                    if !saved.save((ctm, font, text)) && marking.inferred {
                        self.unreadable(|| {
                            "the PDF exceeded its graphics-state stack limit".into()
                        });
                        break;
                    }
                }
                "Q" => {
                    if let Some((transform, selected_font, state)) = saved.restore() {
                        ctm = transform;
                        font = selected_font;
                        text = text.restored(state);
                    }
                }
                "cm" => {
                    if let Some(m) = numbers::<6>(operands) {
                        ctm = then(m, ctm);
                    }
                }
                "BT" => (text.tm, text.tlm) = (IDENTITY, IDENTITY),
                "Tm" => {
                    if let Some(m) = numbers::<6>(operands) {
                        (text.tm, text.tlm) = (m, m);
                    }
                }
                "Td" | "TD" => {
                    if let Some([tx, ty]) = numbers::<2>(operands) {
                        text.next_line(tx, ty);
                        if operator == "TD" {
                            text.leading = -ty;
                        }
                    }
                }
                "TL" => text.leading = numbers::<1>(operands).map_or(text.leading, |[l]| l),
                "Tc" => text.char_space = numbers::<1>(operands).map_or(0.0, |[n]| n),
                "Tw" => text.word_space = numbers::<1>(operands).map_or(0.0, |[n]| n),
                "Tz" => text.scale = numbers::<1>(operands).map_or(100.0, |[n]| n) / 100.0,
                "T*" => text.next_line(0.0, -text.leading),
                "Tf" => {
                    if let Some(Object::Name(name)) = operands.iter().rev().nth(1) {
                        font = fonts.get(name).map(Rc::as_ref);
                    }
                    if let Some([size]) = numbers::<1>(operands) {
                        text.size = size;
                    }
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    if operator == "\"" {
                        // `aw ac string "`: the word and character spacing, then the string.
                        let number = |k: usize| operands.get(k).and_then(Object::as_number);
                        if let (Some(w), Some(c)) = (number(0), number(1)) {
                            (text.word_space, text.char_space) = (w, c);
                        }
                    }
                    if operator == "'" || operator == "\"" {
                        text.next_line(0.0, -text.leading);
                    }
                    let shown_in = font.unwrap_or(&default);
                    let room = (marking.inferred || marking.content_id().is_some())
                        .then(|| super::MAX_OUTPUT.saturating_sub(marking.size + RUN_COST));
                    let Some(shown) = shown_in.show(operands.last(), room) else {
                        self.no_characters();
                        continue;
                    };
                    if shown.cut {
                        marking.cut = true;
                        break;
                    }
                    let place = then(text.tm, ctm);
                    let (x, y) = (place[4], place[5]);
                    let size = text.size.abs() * place[2].hypot(place[3]);
                    text.advance(&shown);
                    let end = then(text.tm, ctm)[4];
                    let run = Run {
                        text: shown.text,
                        bold: shown_in.bold,
                        italic: shown_in.italic,
                        page: marking.page,
                        y,
                        x,
                        end,
                        size,
                    };
                    marking.add(run);
                }
                "BDC" => {
                    let id = match operands.last() {
                        Some(Object::Dict(d)) => d.get("MCID").and_then(Object::as_number),
                        Some(Object::Name(n)) => properties
                            .as_dict()
                            .and_then(|p| self.get(p, n).as_dict().map(|d| self.get(d, "MCID")))
                            .and_then(|id| id.as_number()),
                        _ => None,
                    };
                    let artifact =
                        operands.iter().rev().nth(1).and_then(Object::as_name) == Some("Artifact");
                    if marking.marks.len() < 256 {
                        marking.marks.push(match id {
                            _ if artifact => Mark::Artifact,
                            Some(n) => Mark::Id(n as i64),
                            None => Mark::Other,
                        });
                    }
                }
                "BMC" => {
                    if marking.marks.len() < 256 {
                        let artifact =
                            operands.last().and_then(Object::as_name) == Some("Artifact");
                        marking.marks.push(if artifact {
                            Mark::Artifact
                        } else {
                            Mark::Other
                        });
                    }
                }
                "EMC" => {
                    if marking.marks.len() > marks_at_start {
                        marking.marks.pop();
                    }
                }
                "BI" => marking.has_image = true,
                "Do" => {
                    if let Some(Object::Name(name)) = operands.last() {
                        let name = name.clone();
                        self.draw_marked(resources, &name, ctm, depth, marking, (font, text));
                    }
                }
                _ => {}
            }
        }
        // What a form marks and does not close ends with the form.
        marking.marks.truncate(marks_at_start);
    }

    /// Follows the drawing of the image or form `name`: an image is noted in the piece it is
    /// drawn in, a form is read as a page is.
    fn draw_marked(
        &self,
        resources: &Object,
        name: &str,
        ctm: Matrix,
        depth: usize,
        marking: &mut Marking,
        inherited: (Option<&Font>, TextState),
    ) {
        let xobjects = resources.as_dict().map(|r| self.get(r, "XObject"));
        let entry = xobjects
            .as_ref()
            .and_then(Object::as_dict)
            .and_then(|x| x.get(name))
            .cloned()
            .unwrap_or(Object::Null);
        let number = match entry {
            Object::Ref(n) => Some(n),
            _ => None,
        };
        let Object::Stream(dict, start, end) = self.resolve(entry) else {
            return;
        };
        match self.get(&dict, "Subtype").as_name() {
            Some("Image") => {
                if marking.inferred {
                    marking.has_image = true;
                    return;
                }
                // Only an image drawn as part of the content is told apart, each once.
                let Some(piece) = marking.current() else {
                    return;
                };
                let cached = number.and_then(|n| self.prints.borrow().get(&n).cloned());
                let print = match cached {
                    Some(print) => print,
                    None => {
                        let raw = self.bytes.get(start..end).unwrap_or(&[]);
                        if self.spend(raw.len()).is_err() {
                            return;
                        }
                        let print = super::markdown::fingerprint(raw);
                        if let Some(n) = number {
                            self.prints.borrow_mut().insert(n, print.clone());
                        }
                        print
                    }
                };
                if !piece.images.contains(&print) {
                    piece.images.push(print);
                }
            }
            Some("Form") => {
                if depth >= MAX_FORM_DEPTH || self.forms.get() >= MAX_FORMS {
                    if marking.inferred {
                        self.unreadable(|| "the PDF exceeded its form traversal limit".into());
                    }
                    return;
                }
                self.forms.set(self.forms.get() + 1);
                let body = match self.decode(&dict, start, end) {
                    Ok(body) => body,
                    Err(e) => return self.unreadable(|| e.to_string()),
                };
                let own = self.get(&dict, "Resources");
                let resources = if own.as_dict().is_some() {
                    &own
                } else {
                    resources
                };
                let matrix = match self.get(&dict, "Matrix") {
                    Object::Array(m) if m.len() == 6 => numbers::<6>(&m).unwrap_or(IDENTITY),
                    _ => IDENTITY,
                };
                self.read_marked(
                    &body,
                    resources,
                    then(matrix, ctm),
                    depth + 1,
                    marking,
                    inherited,
                );
            }
            _ => {}
        }
    }
}

/// Reading a structure tree.
struct Walk<'d, 'a> {
    doc: &'d Document<'a>,
    roles: HashMap<String, String>,
    marked: HashMap<(usize, i64), Marked>,
    page_of: HashMap<u32, usize>,
    /// Objects already read, so that a tree leading back into itself is read once.
    seen: HashSet<u32>,
    count: usize,
}

/// The types of element the standard names.
const STANDARD_ROLES: &[&str] = &[
    "Document",
    "DocumentFragment",
    "Part",
    "Art",
    "Sect",
    "Div",
    "BlockQuote",
    "Caption",
    "TOC",
    "TOCI",
    "Index",
    "NonStruct",
    "Private",
    "Aside",
    "Title",
    "FENote",
    "Sub",
    "P",
    "Note",
    "Code",
    "Formula",
    "H",
    "H1",
    "H2",
    "H3",
    "H4",
    "H5",
    "H6",
    "L",
    "LI",
    "Lbl",
    "LBody",
    "Table",
    "TR",
    "TH",
    "TD",
    "THead",
    "TBody",
    "TFoot",
    "Span",
    "Quote",
    "Reference",
    "BibEntry",
    "Link",
    "Annot",
    "Ruby",
    "RB",
    "RT",
    "RP",
    "Warichu",
    "WT",
    "WP",
    "Figure",
    "Form",
    "Em",
    "Strong",
    "Artifact",
];

impl Walk<'_, '_> {
    /// The standard type a type the file names stands for.
    fn role(&self, name: &str) -> String {
        let mut role = name;
        for _ in 0..8 {
            if STANDARD_ROLES.contains(&role) {
                break;
            }
            match self.roles.get(role) {
                Some(next) if next != role => role = next,
                _ => break,
            }
        }
        role.to_string()
    }

    /// The page an object names with `Pg`, or `page` when it names none.
    fn page(&self, dict: &HashMap<String, Object>, page: Option<usize>) -> Option<usize> {
        match dict.get("Pg") {
            Some(Object::Ref(n)) => self.page_of.get(n).copied().or(page),
            _ => page,
        }
    }

    /// Adds what `object`, a kid of `parent` in the tree, holds to `parent`; the kid is on
    /// `page` unless it names its own.
    fn add(&mut self, object: Object, page: Option<usize>, parent: &mut Element, depth: usize) {
        if depth > MAX_STRUCTURE_DEPTH || self.count > MAX_ELEMENTS || self.doc.check().is_err() {
            return;
        }
        self.count += 1;
        if let Object::Ref(n) = object {
            if !self.seen.insert(n) {
                return;
            }
        }
        match self.doc.resolve(object) {
            Object::Number(id) => self.content(page, id as i64, parent),
            Object::Array(kids) => {
                for kid in kids {
                    self.add(kid, page, parent, depth + 1);
                }
            }
            Object::Dict(dict) => match self.doc.get(&dict, "Type").as_name() {
                // Content drawn on a page, possibly another than the element's.
                Some("MCR") => {
                    if dict.contains_key("Stm") {
                        return;
                    }
                    if let Some(id) = self.doc.get(&dict, "MCID").as_number() {
                        self.content(self.page(&dict, page), id as i64, parent);
                    }
                }
                // An annotation, such as the link a link element is for.
                Some("OBJR") => {
                    let annotation = self.doc.get(&dict, "Obj");
                    let Some(annotation) = annotation.as_dict() else {
                        return;
                    };
                    let action = self.doc.get(annotation, "A");
                    if let Some(Object::Text(uri)) =
                        action.as_dict().map(|a| self.doc.get(a, "URI"))
                    {
                        parent.link.get_or_insert_with(|| text_string(&uri));
                    }
                }
                _ => {
                    let Some(name) = self.doc.get(&dict, "S").as_name().map(str::to_string) else {
                        return;
                    };
                    let mut element = Element {
                        role: self.role(&name),
                        alt: self.text(&dict, "Alt"),
                        actual: self.text(&dict, "ActualText"),
                        rows: 1,
                        cols: 1,
                        ..Default::default()
                    };
                    self.spans(&dict, &mut element);
                    let page = self.page(&dict, page);
                    let kids = dict.get("K").cloned().unwrap_or(Object::Null);
                    self.add(kids, page, &mut element, depth + 1);
                    parent.kids.push(Kid::Element(element));
                }
            },
            _ => {}
        }
    }

    /// Adds the content marked `id` on `page` to `parent`.
    fn content(&mut self, page: Option<usize>, id: i64, parent: &mut Element) {
        let Some(page) = page else { return };
        let Some(marked) = self.marked.remove(&(page, id)) else {
            return;
        };
        for print in marked.images {
            if !parent.images.contains(&print) {
                parent.images.push(print);
            }
        }
        if !marked.runs.is_empty() {
            parent.kids.push(Kid::Text(marked.runs));
        }
    }

    fn text(&self, dict: &HashMap<String, Object>, key: &str) -> Option<String> {
        match self.doc.get(dict, key) {
            Object::Text(bytes) => Some(text_string(&bytes)).filter(|t| !t.trim().is_empty()),
            _ => None,
        }
    }

    /// Reads how many rows and columns a table cell spans from its attributes.
    fn spans(&self, dict: &HashMap<String, Object>, element: &mut Element) {
        let attributes = match self.doc.get(dict, "A") {
            Object::Array(items) => items.into_iter().map(|a| self.doc.resolve(a)).collect(),
            one => vec![one],
        };
        for a in attributes {
            let Some(a) = a.as_dict() else { continue };
            let span = |key| {
                self.doc
                    .get(a, key)
                    .as_number()
                    .map(|n| (n as usize).clamp(1, 1000))
            };
            if let Some(n) = span("RowSpan") {
                element.rows = n;
            }
            if let Some(n) = span("ColSpan") {
                element.cols = n;
            }
        }
    }
}

/// The PDF's structure and whether it was inferred from positioned text.
pub(super) fn structure(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<(Element, bool)> {
    let doc = Document::read(bytes, cancel)?;
    let Some(tree) = doc.structure()? else {
        return Ok((doc.infer_structure()?, true));
    };
    if let Some(why) = doc.unreadable.borrow().as_ref() {
        if tree.kids.is_empty() {
            bail!("{why}");
        }
    }
    Ok((tree, false))
}

impl Document<'_> {
    /// How `page` is turned to be displayed, and how far up the displayed page its bottom and
    /// top edges are, when its box says.
    fn frame(&self, page: &HashMap<String, Object>) -> (Matrix, Option<Edges>) {
        // PDF page rotation is clockwise. The page-origin translation is irrelevant because
        // positions are only compared with one another and with the edges, turned alike.
        let rotation = self.get(page, "Rotate").as_number().unwrap_or(0.0);
        let display = match rotation.rem_euclid(360.0) {
            90.0 => [0.0, -1.0, 1.0, 0.0, 0.0, 0.0],
            180.0 => [-1.0, 0.0, 0.0, -1.0, 0.0, 0.0],
            270.0 => [0.0, 1.0, -1.0, 0.0, 0.0, 0.0],
            _ => IDENTITY,
        };
        let edges = ["CropBox", "MediaBox"].into_iter().find_map(|key| {
            let Object::Array(corners) = self.get(page, key) else {
                return None;
            };
            let [x0, y0, x1, y1] = numbers::<4>(&corners).filter(|_| corners.len() == 4)?;
            let ys = [(x0, y0), (x0, y1), (x1, y0), (x1, y1)]
                .map(|(x, y)| display[1] * x + display[3] * y);
            let low = ys.into_iter().fold(f64::INFINITY, f64::min);
            let high = ys.into_iter().fold(f64::NEG_INFINITY, f64::max);
            (high - low > 1.0 && low.is_finite() && high.is_finite()).then_some((low, high))
        });
        (display, edges)
    }

    fn infer_structure(&self) -> Result<Element> {
        let mut tree = Element {
            role: "Document".into(),
            ..Element::default()
        };
        let mut drawn = HashMap::new();
        let mut size = 0;
        let mut pages = Vec::new();
        for (i, page) in self.pages().iter().enumerate() {
            self.check()?;
            let mut marking = Marking {
                page: i,
                marks: Vec::new(),
                out: &mut drawn,
                size,
                inferred: true,
                has_image: false,
                cut: false,
            };
            let (display, edges) = self.frame(page);
            self.read_marked(
                &self.page_content(page),
                &self.get(page, "Resources"),
                display,
                0,
                &mut marking,
                (None, TextState::default()),
            );
            if marking.cut {
                bail!("the PDF has too much text to infer its formatting");
            }
            size = marking.size;
            let has_image = marking.has_image;
            let page = drawn.remove(&(i, 0));
            if has_image
                && page.as_ref().is_none_or(|page| {
                    page.runs
                        .iter()
                        .all(|run| run.text.trim().is_empty() || is_icon(&run.text))
                })
            {
                return Err(NoTextLayer(format!(
                    "PDF page {} contains an image but no extractable text",
                    i + 1
                ))
                .into());
            }
            if let Some(page) = page {
                pages.push((page.runs, edges));
            }
        }
        self.check()?;
        if let Some(why) = self.unreadable.borrow().as_ref() {
            bail!("{why}");
        }
        drop_running_lines(&mut pages, || self.check())?;
        let bodies: Vec<Option<f64>> = pages.iter().map(|(runs, _)| body_size(runs)).collect();
        let page = Page::of(&pages, &bodies);
        for ((runs, _), body) in pages.into_iter().zip(bodies) {
            self.check()?;
            let start = tree.kids.len();
            if let Some(body) = body {
                infer_page(runs, body, &mut tree.kids);
            }
            if let (Some(page), Some(before)) = (
                &page,
                start.checked_sub(1).filter(|_| start < tree.kids.len()),
            ) {
                if goes_on(&tree.kids[before], &tree.kids[start], page) {
                    if let (Kid::Element(next), Some(Kid::Element(paragraph))) =
                        (tree.kids.remove(start), tree.kids.get_mut(before))
                    {
                        if let (Some(Kid::Text(runs)), [Kid::Text(more)]) =
                            (paragraph.kids.last_mut(), next.kids.as_slice())
                        {
                            runs.extend_from_slice(more);
                        }
                    }
                }
            }
        }
        Ok(tree)
    }
}

/// Whether `text` is only characters of the private use areas, where icon fonts put their
/// pictures, such as the sign beside a note. Text can use them too, as a character a font
/// defines for a name does, so only a line of them set apart from the text is left out as an
/// icon (see [`infer_page`]).
fn is_icon(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text.chars().all(
            |c| matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x10_0000..=0x10_FFFD),
        )
}

/// Why a PDF file, or a page of it, has no text to read, as a scan without a text layer has
/// none: nothing gone wrong in reading it, but nothing for the formatted view to show until
/// the file is given a text layer, as by OCR.
#[derive(Debug)]
pub struct NoTextLayer(pub String);

impl std::fmt::Display for NoTextLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NoTextLayer {}

/// The size most of a page's visible characters are drawn at, if it has any.
fn body_size(runs: &[Run]) -> Option<f64> {
    // Weight the median by visible characters so splitting a text operation
    // into several runs does not change the inferred body size.
    let mut sizes: Vec<_> = runs
        .iter()
        .filter_map(|run| {
            let count = run.text.chars().filter(|c| !c.is_whitespace()).count();
            (count > 0).then_some((run.size, count))
        })
        .collect();
    sizes.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    let mut middle = sizes.iter().map(|(_, count)| count).sum::<usize>() / 2;
    sizes.into_iter().find_map(|(size, count)| {
        if middle < count {
            Some(size)
        } else {
            middle -= count;
            None
        }
    })
}

/// How much of a page's height, from its top or its bottom edge, a running header or footer
/// may be drawn in.
const EDGE_BAND: f64 = 0.1;

/// How far up a displayed page its bottom and top edges are.
type Edges = (f64, f64);

/// One line of visible text on a page.
struct Line {
    /// The indices of its runs in the page's runs.
    runs: Vec<usize>,
    /// Its lowest and highest baselines.
    low: f64,
    high: f64,
    /// Its text with whitespace left out and each number written as `#`, for finding
    /// repeated running text.
    shape: String,
    /// The numbers in it, in order.
    numbers: Vec<String>,
    page_number: bool,
    /// The largest size it is drawn at.
    size: f64,
}

/// The lines of visible text on a page, from the top down, grouped by baseline as
/// [`infer_page`] groups them.
fn page_lines(runs: &[Run]) -> Vec<Line> {
    let mut order: Vec<usize> = (0..runs.len())
        .filter(|&i| !runs[i].text.trim().is_empty())
        .collect();
    order.sort_by(|&a, &b| runs[b].y.total_cmp(&runs[a].y));
    let mut order = order.into_iter().peekable();
    let mut lines = Vec::new();
    while let Some(first) = order.next() {
        let high = runs[first].y;
        let mut members = vec![first];
        while let Some(next) = order.next_if(|&next| high - runs[next].y < 1.0) {
            members.push(next);
        }
        members.sort_by(|&a, &b| runs[a].x.total_cmp(&runs[b].x));
        let mut shape = String::new();
        let mut numbers: Vec<String> = Vec::new();
        let mut in_number = false;
        for c in members
            .iter()
            .flat_map(|&i| runs[i].text.chars().chain([' ']))
            .flat_map(char::to_lowercase)
        {
            if c.is_numeric() {
                if !in_number {
                    shape.push('#');
                    numbers.push(String::new());
                }
                numbers.last_mut().into_iter().for_each(|n| n.push(c));
                in_number = true;
            } else {
                in_number = false;
                if !c.is_whitespace() {
                    shape.push(c);
                }
            }
        }
        lines.push(Line {
            low: members
                .iter()
                .map(|&i| runs[i].y)
                .fold(f64::INFINITY, f64::min),
            high,
            size: members.iter().map(|&i| runs[i].size).fold(0.0, f64::max),
            runs: members,
            page_number: is_page_number(&shape),
            shape,
            numbers,
        });
    }
    lines
}

/// Whether a line's [`Line::shape`] says no more than which page it is on, as "3", "- 3 -",
/// "Page 3 of 10" or "第 3 頁" do.
fn is_page_number(key: &str) -> bool {
    let key = key.trim_matches(|c: char| "-–—()[]|·•.,:".contains(c));
    [
        "#",
        "page#",
        "page#of#",
        "#of#",
        "#/#",
        "p#",
        "p.#",
        "pg#",
        "pg.#",
        "第#頁",
        "第#页",
        "第#頁共#頁",
        "第#页共#页",
        "#頁",
        "#页",
        "頁#",
        "页#",
    ]
    .contains(&key)
}

/// The value of a number written in decimal digits, half or full width.
fn number_value(number: &str) -> Option<u64> {
    number.chars().try_fold(0_u64, |value, c| {
        let digit = c
            .to_digit(10)
            .or_else(|| ('０'..='９').contains(&c).then(|| c as u32 - '０' as u32))?;
        value.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

/// Where a line is drawn on a page with the given edges, as running text keeps it from page to
/// page: how far below the top edge, as a positive number, or above the bottom edge, as a
/// negative one, whichever edge is nearer. Measuring from the edge rather than from the foot of
/// the page keeps a header in the same place on pages of different heights.
fn edge_place(line: &Line, (low, high): Edges) -> f64 {
    if high - line.high < line.high - low {
        high - line.high
    } else {
        low - line.high
    }
}

/// Whether `a`, drawn on page `pa` at [`edge_place`] `at_a`, and `b`, on page `pb` at `at_b`,
/// are the same running text: they are drawn in the same place on their pages, and read the
/// same but for numbers that count up with the pages, as a page number does. A header or footer
/// keeps its place on every page, while a body line that happens to read the same, such as the
/// `}` that closes a code listing near the bottom of two pages, rarely falls in the same place.
///
/// Design decision (chosen by the project owner, 2026-11-14); do not narrow it again:
/// running headers and footers are left out of the inferred comparison *whole*, numbers
/// included, so that only the body is compared. A footer such as
/// "OxideMerge 使用者指南 | 版本 0.9.9 104" must be dropped on every page even though it holds
/// a version. An earlier review fix kept every repeated line that held any number; that
/// brought such footers back into the body text and split paragraphs at every page break,
/// which is the bug this code exists to fix. The accepted cost is that a change inside a
/// header or footer, such as "Revision 12" becoming "Revision 13" or "Draft" becoming
/// "Final", is not shown. What keeps body text safe is the rest of [`drop_running_lines`]:
/// only lines in the edge band, no larger than the body text, with nothing but other such
/// lines between them and the edge, and here, numbers that either stay the same or count up
/// exactly with the pages ("Step 1" on one page and "Step 5" on the next is kept).
fn same_running_text(
    (pa, a, at_a): (usize, &Line, f64),
    (pb, b, at_b): (usize, &Line, f64),
) -> bool {
    pa != pb
        && (at_a - at_b).abs() < 1.0
        && a.shape == b.shape
        && a.numbers.iter().zip(&b.numbers).all(|(m, n)| {
            m == n
                || number_value(m)
                    .zip(number_value(n))
                    .is_some_and(|(m, n)| n.wrapping_sub(m) == (pb as u64).wrapping_sub(pa as u64))
        })
}

/// Leaves out the running headers and footers of the pages, each with the bottom and top of
/// the page when they are known: the lines at the top or bottom edge of a page that say only
/// which page it is, or that say what a line at the edge of another page says, but for a page
/// number that counts up with the pages. In a file of more than one page, a line that says only
/// which page it is must count with the pages as the page numbers that repeat do. Only lines
/// with nothing but other such lines between them and the edge are left out, and none drawn
/// larger than the page's body text, so a page's body is kept whole.
fn drop_running_lines(
    pages: &mut [(Vec<Run>, Option<Edges>)],
    check: impl Fn() -> Result<()>,
) -> Result<()> {
    let gone = running_lines(pages, check, true)?;
    for ((runs, _), gone) in pages.iter_mut().zip(gone) {
        let mut gone = gone.into_iter();
        runs.retain(|_| !gone.next().unwrap_or(false));
    }
    Ok(())
}

/// Which runs of each page [`drop_running_lines`] leaves out, a flag for each run. A line
/// that holds only a number at the edge of the one page of a file is taken for its page number
/// only when `lone_numbers` is set, since such a line can as well be a value, such as the total
/// that ends an invoice.
fn running_lines(
    pages: &[(Vec<Run>, Option<Edges>)],
    check: impl Fn() -> Result<()>,
    lone_numbers: bool,
) -> Result<Vec<Vec<bool>>> {
    let lines = pages
        .iter()
        .map(|(runs, _)| {
            check()?;
            Ok(page_lines(runs))
        })
        .collect::<Result<Vec<_>>>()?;
    let at_edge = |line: &Line, (low, high): Edges| {
        let band = (high - low) * EDGE_BAND;
        line.low >= high - band || line.high <= low + band
    };
    // The number of each page in the document, counting the pages without text that
    // `pages` leaves out, so that a page number counts up across a blank page too.
    let numbers: Vec<usize> = pages
        .iter()
        .enumerate()
        .map(|(p, (runs, _))| runs.first().map_or(p, |run| run.page))
        .collect();
    // The lines drawn at an edge, by what they read with their numbers left out, in page
    // order and from the top down on each page.
    let mut shapes: HashMap<&str, Vec<(usize, usize, f64)>> = HashMap::new();
    for (p, (page, (_, edges))) in lines.iter().zip(pages.iter()).enumerate() {
        check()?;
        let Some(edges) = *edges else { continue };
        for (l, line) in page.iter().enumerate() {
            if at_edge(line, edges) {
                let entry = (p, l, edge_place(line, edges));
                shapes.entry(line.shape.as_str()).or_default().push(entry);
            }
        }
    }
    // A line repeats when it is the same running text as a line of its shape on the page
    // before or after, among those that have one. A page can hold more than one line of a
    // shape, such as the same header and footer, so each is compared with every one on the
    // neighbouring page; only the outermost few count, since only the lines at an edge are
    // left out, which keeps this to a few comparisons a page however many lines there are.
    const OUTERMOST: usize = 4;
    let mut repeated = HashSet::new();
    for group in shapes.values() {
        check()?;
        let on_pages: Vec<&[(usize, usize, f64)]> = group.chunk_by(|a, b| a.0 == b.0).collect();
        let outermost = |page: &[(usize, usize, f64)]| {
            let inner = page.len().saturating_sub(OUTERMOST).max(OUTERMOST);
            let mut ends = page.to_vec();
            ends.drain(OUTERMOST.min(ends.len())..inner.min(ends.len()));
            ends
        };
        for pair in on_pages.windows(2) {
            for &(pa, la, at_a) in &outermost(pair[0]) {
                for &(pb, lb, at_b) in &outermost(pair[1]) {
                    let a = (numbers[pa], &lines[pa][la], at_a);
                    if same_running_text(a, (numbers[pb], &lines[pb][lb], at_b)) {
                        repeated.insert((pa, la));
                        repeated.insert((pb, lb));
                    }
                }
            }
        }
    }
    // How far the printed page numbers are from the page's place in the document, as the
    // page numbers that repeat from page to page show. In a file of more than one page, a
    // line that holds only a number is a page number only when it counts with the pages this
    // way, so that a lone number at the edge of the body, such as the last cell of a table, is
    // kept; a file of one page has nothing to count against.
    let page_offset = |p: usize, line: &Line| {
        let printed = line.numbers.first().and_then(|n| number_value(n))?;
        i64::try_from(printed)
            .ok()?
            .checked_sub(i64::try_from(numbers[p]).ok()?)
    };
    let mut offsets: HashSet<i64> = repeated
        .iter()
        .filter(|&&(p, l)| lines[p][l].page_number)
        .filter_map(|&(p, l)| page_offset(p, &lines[p][l]))
        .collect();
    // When only one page of the file has a line at its edge that could be a page number, as
    // a file numbered only after its first page does, there is nothing for it to repeat; it is
    // a page number when it counts the pages from the first, or from the one after it, as a
    // file whose cover goes unnumbered does. With such lines on several pages that do not
    // count together, they are body text, such as numbers that end a table on each page.
    let numbered = lines
        .iter()
        .zip(pages.iter())
        .filter(|(lines, (_, edges))| {
            edges.is_some_and(|edges| lines.iter().any(|l| l.page_number && at_edge(l, edges)))
        })
        .count();
    if offsets.is_empty() && numbered <= 1 {
        offsets.extend([0, 1]);
    }
    let single = lone_numbers && pages.iter().filter(|(_, edges)| edges.is_some()).count() <= 1;
    let mut flags = Vec::with_capacity(pages.len());
    for (p, ((runs, edges), lines)) in pages.iter().zip(&lines).enumerate() {
        check()?;
        let mut gone = vec![false; runs.len()];
        let (Some(edges), Some(body)) = (*edges, body_size(runs)) else {
            flags.push(gone);
            continue;
        };
        let running = |(l, line): &(usize, &Line)| {
            at_edge(line, edges)
                && line.size <= body * 1.1
                && (repeated.contains(&(p, *l))
                    || line.page_number
                        && (single || page_offset(p, line).is_some_and(|o| offsets.contains(&o))))
        };
        let top = lines.iter().enumerate().take_while(running).count();
        let bottom = lines
            .iter()
            .enumerate()
            .skip(top)
            .rev()
            .take_while(running)
            .count();
        let dropped: Vec<&Line> = lines[..top]
            .iter()
            .chain(&lines[lines.len() - bottom..])
            .collect();
        for line in &dropped {
            for &i in &line.runs {
                gone[i] = true;
            }
        }
        // The spaces drawn on a line that is left out go with it.
        for (i, run) in runs.iter().enumerate() {
            if run.text.trim().is_empty() {
                let at = dropped.partition_point(|line| line.low - 1.0 >= run.y);
                gone[i] = dropped.get(at).is_some_and(|line| run.y < line.high + 1.0);
            }
        }
        flags.push(gone);
    }
    Ok(flags)
}

/// The runs of an inferred paragraph.
fn paragraph_runs(kid: &Kid) -> Option<&[Run]> {
    let Kid::Element(e) = kid else { return None };
    match e.kids.as_slice() {
        [Kid::Text(runs)] if e.role == "P" => Some(runs),
        _ => None,
    }
}

/// Where the line of `runs` starts and how large it is drawn.
fn line_of<'a>(runs: impl Iterator<Item = &'a Run>) -> (f64, f64) {
    runs.filter(|run| !run.text.trim().is_empty())
        .fold((f64::INFINITY, 0.0_f64), |(x, size), run| {
            (x.min(run.x), size.max(run.size))
        })
}

/// Where the text of a page is usually written, measured from the edges of each page so that
/// pages of different sizes, or turned on their side, compare alike: how far below the top edge
/// its first line is drawn, how far above the bottom edge the last line of a filled page is,
/// and where along the line it starts on even and on odd pages, the same for both unless facing
/// pages are seen to have mirrored margins.
struct Page {
    top: f64,
    bottom: f64,
    margins: [f64; 2],
    /// The bottom and top edges of each page of the document, by its place in the document.
    edges: HashMap<usize, Edges>,
}

/// How far below the top and above the bottom edge of its page a run is drawn.
#[derive(Clone, Copy)]
struct OnPage {
    below_top: f64,
    above_bottom: f64,
}

/// How many even and how many odd pages it takes to tell that facing pages have mirrored
/// margins; with fewer, a page that holds only an indented block, such as the end of a code
/// listing, would be taken for a page with a wider margin.
const FACING_PAGES: usize = 3;

impl Page {
    /// How the text of `pages`, whose body text is drawn at the sizes `bodies` says, is usually
    /// written, if any of them has text.
    fn of(pages: &[(Vec<Run>, Option<Edges>)], bodies: &[Option<f64>]) -> Option<Self> {
        let edges: HashMap<usize, Edges> = pages
            .iter()
            .filter_map(|(runs, edges)| Some((runs.first()?.page, (*edges)?)))
            .collect();
        let mut page = Page {
            top: 0.0,
            bottom: 0.0,
            margins: [0.0; 2],
            edges,
        };
        // How far below its top edge each page's text starts, where it starts along the line,
        // by whether it is an even or an odd page of the document, and how far above its
        // bottom edge its lowest line of body text is: footnotes and footers, drawn smaller,
        // can be lower than a filled page goes.
        let mut tops = Vec::new();
        let mut bottoms = Vec::new();
        let mut lefts: [Vec<f64>; 2] = [Vec::new(), Vec::new()];
        for ((runs, _), body) in pages.iter().zip(bodies) {
            let mut visible = runs.iter().filter(|run| !run.text.trim().is_empty());
            let (Some(first), Some(body)) = (visible.next(), *body) else {
                continue;
            };
            let (top, left) = visible.fold((first, first.x), |(top, left), run| {
                (if run.y > top.y { run } else { top }, left.min(run.x))
            });
            let Some(place) = page.place(top) else {
                continue;
            };
            lefts[first.page % 2].push(left);
            tops.push(place.below_top);
            let lowest = runs
                .iter()
                .filter(|run| !run.text.trim().is_empty() && (run.size - body).abs() <= body * 0.15)
                .filter_map(|run| Some(page.place(run)?.above_bottom))
                .reduce(f64::min);
            bottoms.extend(lowest);
        }
        let median = |mut values: Vec<f64>| {
            values.sort_by(f64::total_cmp);
            values.get(values.len() / 2).copied()
        };
        // Of two middle values, the nearer the top, as a page that starts lower is the one
        // that may hold a new block.
        tops.sort_by(f64::total_cmp);
        page.top = *tops.get(tops.len().saturating_sub(1) / 2)?;
        // The lowest tenth is left out, so that one page with a line of body text below where
        // its text block ends, such as a footer that is not repeated, does not stand for every
        // page.
        bottoms.sort_by(f64::total_cmp);
        page.bottom = bottoms.get(bottoms.len() / 10).copied()?;
        let [even, odd] = lefts;
        if even.len().min(odd.len()) >= FACING_PAGES {
            page.margins = [median(even).unwrap_or(0.0), median(odd).unwrap_or(0.0)];
        }
        Some(page)
    }

    /// Where `run` is drawn on its page. When no page of the file has known edges, every
    /// position is taken as it is drawn; when only some do, a page without them is not
    /// measured, since its positions cannot be compared with theirs.
    fn place(&self, run: &Run) -> Option<OnPage> {
        let (low, high) = match self.edges.get(&run.page) {
            Some(&edges) => edges,
            None if self.edges.is_empty() => (0.0, 0.0),
            None => return None,
        };
        Some(OnPage {
            below_top: high - run.y,
            above_bottom: run.y - low,
        })
    }
}

/// Whether the paragraph `next`, first on its page, goes on with `before`, last on the page
/// before, where the text is written as `page` says: the last line of `before` does not end a
/// sentence, `next` starts at the same place and size as that line, measured from the margin of
/// its page so that facing pages with mirrored margins still match, and its first line is drawn
/// at the top of the page's text rather than below it.
///
/// This is the test [`infer_page`] uses to join one line to the next within a page, with the
/// gap between them read from where the next page's text starts: a block that goes on over a
/// page break starts at the top, while a new block set apart by space, such as the next code
/// listing or table row with its padding, starts lower. So a paragraph or a code listing reads
/// the same whether or not a page break falls inside it. Whether the last line runs to the
/// right margin is not asked of a line that does not end a sentence: a line of code, or a line
/// that ends a list item, never does, and asking split such blocks at every page break.
///
/// A last line that ends a sentence goes on only when the page broke between two sentences of
/// one paragraph, which shows in two ways (see [`fills_its_page`]): the line runs to the right
/// edge of the paragraph, since the last line of a paragraph usually stops short of it, and no
/// further line fits below it on its page, since a paragraph that goes on fills its page to the
/// bottom. A paragraph whose own last line happens to do both cannot be told apart from that,
/// and is read as going on.
fn goes_on(before: &Kid, next: &Kid, page: &Page) -> bool {
    let (Some(before), Some(next)) = (paragraph_runs(before), paragraph_runs(next)) else {
        return false;
    };
    let (Some(last), Some(first)) = (before.last(), next.first()) else {
        return false;
    };
    let last_line = || {
        before
            .iter()
            .rev()
            .take_while(|run| run.page == last.page && run.y == last.y)
    };
    let first_line = next
        .iter()
        .take_while(|run| run.page == first.page && run.y == first.y);
    let ends_sentence = last_line()
        .flat_map(|run| run.text.chars().rev())
        .find(|c| !c.is_whitespace() && !"\"'”’」』)）]］".contains(*c))
        .is_some_and(|c| ".!?。！？．:：;；".contains(c));
    let (last_x, last_size) = line_of(last_line());
    let (Some(last_place), Some(first_place)) = (page.place(last), page.place(first)) else {
        return false;
    };
    if ends_sentence && !fills_its_page(before, (last, last_place), last_size, page) {
        return false;
    }
    let (next_x, next_size) = line_of(first_line);
    let size = last_size.max(next_size);
    let indent = |x: f64, run: &Run| x - page.margins[run.page % 2];
    size > 0.0
        && first_place.below_top <= page.top + 0.25 * size
        && (indent(last_x, last) - indent(next_x, first)).abs() <= size
        && (last_size - next_size).abs() <= size * 0.15
}

/// Whether the line of `last`, drawn at `size` and at `place` as the last line of the paragraph
/// `runs`, leaves no room on its page: it ends within a character of the right edge the
/// paragraph's other lines on that page reach, and one more line, as far below it as it is
/// below the line above, would fall below where the last line of a filled page is drawn, as
/// `page` says. A paragraph with one line on the page has neither an edge to reach nor a step
/// between lines, so it never fills its page. Only the lines on that page are looked at, which
/// also keeps a block that goes on over many pages from being read again at each of them.
fn fills_its_page(runs: &[Run], (last, place): (&Run, OnPage), size: f64, page: &Page) -> bool {
    let on_last_line = |run: &&Run| run.y == last.y;
    let start = runs.partition_point(|run| run.page < last.page);
    let visible = || {
        runs[start..]
            .iter()
            .filter(|run| !run.text.trim().is_empty())
    };
    let edge = visible()
        .filter(|run| !on_last_line(run))
        .map(|run| run.end)
        .fold(f64::NEG_INFINITY, f64::max);
    let end = visible()
        .filter(on_last_line)
        .map(|run| run.end)
        .fold(f64::NEG_INFINITY, f64::max);
    let above = visible()
        .filter(|run| run.y > last.y + 1.0)
        .map(|run| run.y)
        .fold(f64::INFINITY, f64::min);
    size > 0.0
        && edge.is_finite()
        && end >= edge - size
        && above.is_finite()
        && place.above_bottom - (above - last.y) < page.bottom - 0.25 * size
}

/// How many characters a line drawn between two lines of a paragraph may hold and still be
/// read as part of them, however wide it is; a wider one may still be, when it is narrow next
/// to the line above (see [`infer_page`]).
const MAX_BETWEEN: usize = 4;

/// One line of a page as [`infer_page`] reads it: the runs on one baseline, sorted along it,
/// and, measured from its visible runs other than a drop cap, where it starts, how large and
/// how wide it is drawn and how many characters it holds.
struct Row {
    y: f64,
    runs: Vec<Run>,
    /// A first letter drawn over several lines beside the start of a paragraph, taken out of
    /// `runs`.
    cap: Option<Run>,
    x: f64,
    size: f64,
    min_size: f64,
    width: f64,
    chars: usize,
}

impl Row {
    fn new(y: f64, runs: Vec<Run>) -> Self {
        Row::build(y, runs, true)
    }

    /// The row with what was taken for a drop cap put back in its place on the line.
    fn without_cap(self) -> Self {
        let mut runs = self.runs;
        runs.extend(self.cap);
        runs.sort_by(|a, b| a.x.total_cmp(&b.x));
        Row::build(self.y, runs, false)
    }

    fn build(y: f64, mut runs: Vec<Run>, may_cap: bool) -> Self {
        let visible = |run: &&Run| !run.text.trim().is_empty();
        // What may be a drop cap: a letter or digit, first on the line, drawn at least twice
        // as large as the rest of it. A bullet drawn large is no letter; whether a letter is a
        // drop cap is told by the line below it (see [`infer_page`]).
        let cap = runs.iter().position(|run| visible(&run)).filter(|&first| {
            let rest = runs[first + 1..]
                .iter()
                .filter(visible)
                .map(|run| run.size)
                .fold(0.0, f64::max);
            may_cap && rest > 0.0 && is_cap(&runs[first]) && runs[first].size >= 2.0 * rest
        });
        let cap = cap.map(|i| runs.remove(i));
        let own: Vec<&Run> = runs.iter().filter(visible).collect();
        let x = own.iter().map(|run| run.x).fold(f64::INFINITY, f64::min);
        let end = own
            .iter()
            .map(|run| run.end)
            .fold(f64::NEG_INFINITY, f64::max);
        let (min_size, size) = own
            .iter()
            .fold((f64::INFINITY, 0.0_f64), |(min, max), run| {
                (min.min(run.size), max.max(run.size))
            });
        let chars = own
            .iter()
            .flat_map(|run| run.text.chars())
            .filter(|c| !c.is_whitespace())
            .count();
        Row {
            y,
            runs,
            cap,
            x,
            size,
            min_size,
            width: (end - x).max(0.0),
            chars,
        }
    }
}

/// Whether `run` could be a drop cap: one or two letters or digits.
fn is_cap(run: &Run) -> bool {
    let text = run.text.trim();
    (1..=2).contains(&text.chars().count()) && text.chars().all(char::is_alphanumeric)
}

/// Puts the drop cap `cap` of `paragraph` before its first word, with nothing between them.
fn place_cap(paragraph: &mut Vec<Run>, mut cap: Run) {
    if let Some(first) = paragraph.iter().find(|run| !run.text.trim().is_empty()) {
        let width = cap.end - cap.x;
        (cap.page, cap.y, cap.end, cap.x) = (first.page, first.y, first.x, first.x - width);
    }
    paragraph.insert(0, cap);
}

/// Approximate a single-column page in top-to-bottom order. Group nearby baselines,
/// then join lines with similar size and indentation into paragraphs.
/// `body_size` is the size most of the page's visible text is drawn at.
fn infer_page(mut runs: Vec<Run>, body_size: f64, blocks: &mut Vec<Kid>) {
    runs.sort_by(|a, b| b.y.total_cmp(&a.y));
    let (spaces, visible): (Vec<_>, Vec<_>) =
        runs.into_iter().partition(|run| run.text.trim().is_empty());
    let mut spaces = spaces.into_iter().peekable();
    let mut runs = visible.into_iter().peekable();
    let mut rows = Vec::new();
    while let Some(first) = runs.next() {
        let y = first.y;
        let mut line = vec![first];
        while runs.peek().is_some_and(|next| y - next.y < 1.0) {
            line.extend(runs.next());
        }
        while let Some(space) = spaces.peek().filter(|space| space.y > y - 1.0) {
            let space_y = space.y;
            if runs
                .peek()
                .is_some_and(|next| (next.y - space_y).abs() < (y - space_y).abs())
            {
                break;
            }
            line.extend(spaces.next().filter(|space| space.y - y < 1.0));
        }
        // Runs grouped on this line share the baseline when assembled as Markdown.
        for run in &mut line {
            run.y = y;
        }
        line.sort_by(|a, b| a.x.total_cmp(&b.x));
        rows.push(Row::new(y, line));
    }
    let emit = |blocks: &mut Vec<Kid>, role: &str, runs: Vec<Run>| {
        if !runs.is_empty() {
            blocks.push(Kid::Element(Element {
                role: role.into(),
                kids: vec![Kid::Text(runs)],
                ..Element::default()
            }));
        }
    };
    let mut paragraph: Vec<Run> = Vec::new();
    // The baseline, start, size and width of the paragraph's last line.
    let mut last_line: Option<(f64, f64, f64, f64)> = None;
    // Where the lines of the paragraph start below its drop cap, if it has one.
    let mut cap_x: Option<f64> = None;
    // Runs of a line drawn between two lines that belong with the next one.
    let mut carry: Vec<Run> = Vec::new();
    // A drop cap sits beside the first lines of its paragraph, and the line below it goes back
    // to where the cap starts; a large letter the next line does not go back to, such as a
    // list's number, stays on its line. A mark drawn between the lines, such as a raised
    // footnote mark, is passed over in looking for that line.
    let caps: Vec<bool> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            row.cap.as_ref().is_some_and(|cap| {
                let mark = |next: &Row| {
                    (next.chars <= MAX_BETWEEN || next.width <= 0.25 * row.width)
                        && (next.size - row.size).abs() > row.size * 0.15
                };
                rows[i + 1..]
                    .iter()
                    .find(|next| !mark(next))
                    .is_some_and(|next| (next.x - cap.x).abs() <= row.size.max(next.size))
            })
        })
        .collect();
    let mut rows = rows.into_iter().zip(caps).peekable();
    while let Some((mut row, cap)) = rows.next() {
        if row.cap.is_some() && !cap {
            row = row.without_cap();
        }
        let next = rows.peek().map(|(next, _)| (next.y, next.size));
        let y = row.y;
        let heading = body_size > 0.0 && row.min_size >= body_size * 1.25;
        let icon = row
            .runs
            .iter()
            .all(|run| run.text.trim().is_empty() || is_icon(&run.text));
        // A line drawn between two lines of a paragraph that are no further apart than one
        // line is from the next, such as the icon beside a note, a large warning sign or a
        // raised footnote mark, takes no line of its own: it is short, or narrow beside the
        // line above, drawn larger or smaller than that line or as an icon, and neither ends
        // the paragraph nor starts a heading; text drawn at the size of the lines around it,
        // such as the wrapped text of a table cell set a little off their baselines, is a
        // line of its own. It goes with
        // whichever of the two lines it is nearer, in its place along that line, as a raised
        // mark goes after its word; an icon there is left out, as is one drawn at heading size.
        let between = last_line
            .filter(|_| row.cap.is_none())
            .zip(next)
            .filter(|&((py, _, ps, pw), (ny, ns))| {
                (row.chars <= MAX_BETWEEN || row.width <= 0.25 * pw)
                    && (icon || (row.size - ps).abs() > ps * 0.15)
                    && py - ny <= 1.65 * ps.max(ns)
            })
            .map(|((py, ..), (ny, _))| (py, ny));
        if icon && (between.is_some() || heading) {
            continue;
        }
        // A drop cap drawn on a baseline of its own, between two lines of the paragraph and
        // to their left.
        let own_cap = between.is_some()
            && last_line.is_some_and(|(_, px, ps, _)| row.x < px - ps && row.size >= 2.0 * ps)
            && row
                .runs
                .iter()
                .filter(|run| !run.text.trim().is_empty())
                .count()
                == 1
            && row.runs.iter().any(is_cap);
        if own_cap {
            let cap = row.runs.into_iter().find(is_cap);
            if let Some(cap) = cap {
                cap_x = Some(cap.x);
                place_cap(&mut paragraph, cap);
            }
            continue;
        }
        if let Some((py, ny)) = between {
            if py - y <= y - ny {
                for run in &mut row.runs {
                    run.y = py;
                }
                let start = paragraph
                    .iter()
                    .rposition(|run| run.y != py)
                    .map_or(0, |i| i + 1);
                paragraph.extend(row.runs);
                paragraph[start..].sort_by(|a, b| a.x.total_cmp(&b.x));
            } else {
                for run in &mut row.runs {
                    run.y = ny;
                }
                carry.extend(row.runs);
            }
            continue;
        }
        if !carry.is_empty() {
            row.runs.append(&mut carry);
            row.runs.sort_by(|a, b| a.x.total_cmp(&b.x));
        }
        let (x, size) = (row.x, row.size);
        if heading
            || last_line.is_some_and(|(py, px, ps, _)| {
                py - y > 1.65 * ps.max(size)
                    || ((px - x).abs() > size && !cap_x.is_some_and(|cx| (cx - x).abs() <= size))
                    || (ps - size).abs() > size * 0.15
            })
        {
            emit(blocks, "P", std::mem::take(&mut paragraph));
            cap_x = None;
        }
        if heading {
            row.runs.extend(row.cap.take());
            row.runs.sort_by(|a, b| a.x.total_cmp(&b.x));
            emit(blocks, "H1", row.runs);
            last_line = None;
            continue;
        }
        paragraph.extend(row.runs);
        if let Some(cap) = row.cap {
            // The drop cap is the first letter of the paragraph.
            cap_x = Some(cap.x);
            place_cap(&mut paragraph, cap);
        }
        last_line = Some((y, x, size, row.width));
    }
    paragraph.append(&mut carry);
    emit(blocks, "P", paragraph);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(bytes: &[u8]) -> Result<String> {
        super::text(bytes, None)
    }

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

    /// A tagged file with a heading, a paragraph with a bold word on its second line, a
    /// table whose cells span rows and columns, a numbered list, a figure and a page number.
    fn tagged_file() -> Vec<u8> {
        let content = b"/Artifact BMC BT /F1 9 Tf 300 30 Td (Page 1) Tj ET EMC\n\
            /H1 <</MCID 0>> BDC BT /F2 18 Tf 72 720 Td (Plan) Tj ET EMC\n\
            /P <</MCID 1>> BDC BT /F1 12 Tf 72 700 Td (Some words and) Tj ET EMC\n\
            /Span <</MCID 2>> BDC BT /F2 12 Tf 72 686 Td (bold) Tj ET EMC\n\
            /Span <</MCID 3>> BDC BT /F1 12 Tf 100 686 Td ( ones.) Tj ET EMC\n\
            /TH <</MCID 4>> BDC BT /F1 12 Tf 72 600 Td (Head) Tj ET EMC\n\
            /TD <</MCID 5>> BDC BT /F1 12 Tf 72 580 Td (A) Tj ET EMC\n\
            /TD <</MCID 6>> BDC BT /F1 12 Tf 200 580 Td (x) Tj ET EMC\n\
            /TD <</MCID 7>> BDC BT /F1 12 Tf 200 560 Td (y) Tj ET EMC\n\
            /Lbl <</MCID 8>> BDC BT /F1 12 Tf 72 500 Td (1.) Tj ET EMC\n\
            /LBody <</MCID 9>> BDC BT /F1 12 Tf 90 500 Td (first) Tj ET EMC\n\
            /Lbl <</MCID 10>> BDC BT /F1 12 Tf 72 486 Td (2.) Tj ET EMC\n\
            /LBody <</MCID 11>> BDC BT /F1 12 Tf 90 486 Td (second) Tj ET EMC\n";
        let cell = |tag: &str, id: u32, attrs: &str| format!("<< /S /{tag} /K {id}{attrs} >>");
        let document = format!(
            "<< /Type /StructElem /S /Document /Pg 3 0 R /K [\
             << /S /H1 /K 0 >>\
             << /S /Standard /K [1 << /S /Span /K 2 >> << /S /Span /K 3 >>] >>\
             << /S /Table /K [<< /S /TR /K [{}] >> << /S /TR /K [{} {}] >> << /S /TR /K [{}] >>] >>\
             << /S /L /K [<< /S /LI /K [{} {}] >> << /S /LI /K [{} {}] >>] >>\
             << /S /Figure /Alt (A tree) >>] >>",
            cell("TH", 4, " /A << /O /Table /ColSpan 2 >>"),
            cell("TD", 5, " /A [<< /O /Layout >> << /O /Table /RowSpan 2 >>]"),
            cell("TD", 6, ""),
            cell("TD", 7, ""),
            cell("Lbl", 8, ""),
            cell("LBody", 9, ""),
            cell("Lbl", 10, ""),
            cell("LBody", 11, ""),
        );
        build(
            &[
                b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                  /Resources << /Font << /F1 5 0 R /F2 8 0 R >> >> /Contents 4 0 R >>"
                    .to_vec(),
                stream("", content),
                HELVETICA.as_bytes().to_vec(),
                b"<< /Type /StructTreeRoot /K 7 0 R /RoleMap << /Standard /P >> >>".to_vec(),
                document.into_bytes(),
                b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold \
                  /Encoding /WinAnsiEncoding >>"
                    .to_vec(),
            ],
            "",
        )
    }

    fn blocks(file: &[u8]) -> Result<Vec<String>> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tagged.pdf");
        std::fs::write(&path, file).unwrap();
        Ok(crate::core::extract::pdf_blocks(&path, None)?
            .0
            .into_iter()
            .map(|b| format!("{}{}", "  ".repeat(b.depth), b.text))
            .collect())
    }

    #[test]
    fn a_tagged_file_is_read_as_the_blocks_its_structure_names() {
        assert_eq!(
            blocks(&tagged_file()).unwrap(),
            [
                "# Plan",
                "Some words and **bold** ones.",
                "| Head | < |\n| --- | --- |\n| A | x |\n| ^ | y |",
                "1. first",
                "2. second",
                "*Image: A tree*",
            ]
        );
    }

    /// A tagged file with one page drawing `content`, whose document element holds `kids`;
    /// the page names `/F1` (Helvetica), `/F2` (Helvetica-Bold) and the images and forms of
    /// `xobjects`, and `extra` are objects 9 on.
    fn tagged_with(content: &[u8], kids: &str, xobjects: &str, extra: Vec<Vec<u8>>) -> Vec<u8> {
        let mut objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << \
                 /Font << /F1 5 0 R /F2 8 0 R >> /XObject << {xobjects} >> >> /Contents 4 0 R >>"
            )
            .into_bytes(),
            stream("", content),
            HELVETICA.as_bytes().to_vec(),
            b"<< /Type /StructTreeRoot /K 7 0 R >>".to_vec(),
            format!("<< /Type /StructElem /S /Document /Pg 3 0 R /K [{kids}] >>").into_bytes(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec(),
        ];
        objects.extend(extra);
        build(&objects, "")
    }

    #[test]
    fn words_placed_apart_on_a_line_are_read_with_a_space_between_them() {
        // Each word placed on its own, with no space drawn, as some programs write them; a
        // small step back inside a word, as for kerning, is no space.
        let content = b"/P <</MCID 0>> BDC BT /F1 12 Tf 72 700 Td (Some) Tj 40 0 Td (words) Tj \
            40 0 Td [(ke) 30 (rned)] TJ ET EMC\n";
        assert_eq!(
            blocks(&tagged_with(content, "<< /S /P /K 0 >>", "", vec![])).unwrap(),
            ["Some words kerned"]
        );
    }

    #[test]
    fn the_placement_saved_past_the_most_kept_is_restored_as_it_was() {
        // `q` sets the placement aside, `cm` moves it; 70 more `q` and `Q` must leave it moved,
        // so that the second half of the word is on the first half's line.
        let nested = format!("{}{}", "q ".repeat(70), "Q ".repeat(70));
        let content = format!(
            "/P <</MCID 0>> BDC q 1 0 0 1 0 -50 cm BT /F1 12 Tf 72 750 Td (Hel) Tj ET {nested} \
             BT /F1 12 Tf 90 750 Td (lo) Tj ET Q EMC\n"
        );
        assert_eq!(
            blocks(&tagged_with(
                content.as_bytes(),
                "<< /S /P /K 0 >>",
                "",
                vec![]
            ))
            .unwrap(),
            ["Hello"]
        );
    }

    #[test]
    fn links_figures_captions_and_nested_lists_are_read_from_the_structure() {
        let content = b"/Link <</MCID 0>> BDC BT /F1 12 Tf 72 700 Td (the site) Tj ET EMC\n\
            /Figure <</MCID 1>> BDC q 10 0 0 10 72 600 cm /Im1 Do Q EMC\n\
            /Lbl <</MCID 2>> BDC BT /F1 12 Tf 72 500 Td (\x95) Tj ET EMC\n\
            /LBody <</MCID 3>> BDC BT /F1 12 Tf 90 500 Td (outer) Tj ET EMC\n\
            /LBody <</MCID 4>> BDC BT /F1 12 Tf 108 486 Td (inner) Tj ET EMC\n\
            /TD <</MCID 5>> BDC BT /F1 12 Tf 72 400 Td (cell) Tj ET EMC\n\
            /Caption <</MCID 6>> BDC BT /F1 12 Tf 72 380 Td (Table 1. Totals) Tj ET EMC\n";
        let kids = "<< /S /P /K [<< /S /Link /K [0 << /Type /OBJR /Obj 9 0 R >>] >>] >>\
            << /S /Figure /Alt (A tree) /K 1 >>\
            << /S /L /K [<< /S /LI /K [<< /S /Lbl /K 2 >> << /S /LBody /K [3 \
               << /S /L /K [<< /S /LI /K [<< /S /LBody /K 4 >>] >>] >>] >>] >>] >>\
            << /S /Table /K [<< /S /TR /K [<< /S /TD /K 5 >>] >> << /S /Caption /K 6 >>] >>";
        let file = tagged_with(
            content,
            kids,
            "/Im1 10 0 R",
            vec![
                b"<< /Type /Annot /Subtype /Link /A << /S /URI /URI (https://example.com/a) >> >>"
                    .to_vec(),
                stream(
                    "/Type /XObject /Subtype /Image /Width 1 /Height 1 /BitsPerComponent 8 \
                     /ColorSpace /DeviceGray",
                    b"x",
                ),
            ],
        );
        let print = super::super::markdown::fingerprint(b"x");
        assert_eq!(
            blocks(&file).unwrap(),
            [
                "[the site](https://example.com/a)".to_string(),
                format!("*Image: A tree ({print})*"),
                "- outer".to_string(),
                "  - inner".to_string(),
                "| cell |\n| --- |".to_string(),
                "Table 1. Totals".to_string(),
            ]
        );
    }

    #[test]
    fn headings_take_their_level_from_their_sections_inside_a_type_the_file_names() {
        // `Story` is no standard type and is not mapped to one, yet it holds blocks; a bold
        // paragraph is bold as its style is, and a bold word in another is set apart.
        let content = b"/H <</MCID 0>> BDC BT /F1 18 Tf 72 700 Td (Top) Tj ET EMC\n\
            /H <</MCID 1>> BDC BT /F1 14 Tf 72 670 Td (Sub) Tj ET EMC\n\
            /P <</MCID 2>> BDC BT /F2 12 Tf 72 640 Td (All bold) Tj ET EMC\n\
            /P <</MCID 3>> BDC BT /F1 12 Tf 72 620 Td (One ) Tj /F2 12 Tf (bold) Tj \
            /F1 12 Tf ( word) Tj ET EMC\n";
        let kids = "<< /S /Story /K [<< /S /Sect /K [<< /S /H /K 0 >> \
            << /S /Sect /K [<< /S /H /K 1 >>] >>] >> << /S /P /K 2 >> << /S /P /K 3 >>] >>";
        assert_eq!(
            blocks(&tagged_with(content, kids, "", vec![])).unwrap(),
            ["# Top", "## Sub", "All bold", "One **bold** word"]
        );
    }

    #[test]
    fn inferred_comparison_reports_an_image_only_page_after_a_searchable_cover() {
        let mut objects = one_page(stream("", b"BT /F1 12 Tf (cover) Tj ET"), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec();
        objects.push(b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".to_vec());
        objects.push(stream("", b"BI /W 1 /H 1 /CS /G /BPC 8 ID x\nEI\n"));
        let error = blocks(&build(&objects, "")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("page 2 contains an image but no extractable text"),
            "{error:#}"
        );
    }

    #[test]
    fn inferred_comparison_reports_graphics_state_overflow() {
        let mut content = b"BT /F1 12 Tf (cover) Tj ET\n".to_vec();
        content.extend_from_slice(&b"q\n".repeat(65));
        content.extend_from_slice(b"BT /F1 30 Tf ET Q BT (body) Tj ET\n");
        content.extend_from_slice(&b"Q\n".repeat(64));
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        let error = blocks(&file).unwrap_err();
        assert!(
            error.to_string().contains("graphics-state stack limit"),
            "{error:#}"
        );
    }

    #[test]
    fn inferred_text_inherits_the_callers_font_inside_a_form() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding \
                    << /Type /Encoding /Differences [65 /uni005A] >> >>";
        let mut objects = one_page(
            stream("", b"BT /F1 12 Tf ET /Fm1 Do BT 72 706 Td (A) Tj ET"),
            font,
        );
        objects[2] = b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> \
                       /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>"
            .to_vec();
        objects.push(stream(
            "/Type /XObject /Subtype /Form /Resources << /Font << /F2 7 0 R >> >>",
            b"BT 72 720 Td (A) Tj /F2 30 Tf ET",
        ));
        objects.push(HELVETICA.as_bytes().to_vec());
        assert_eq!(blocks(&build(&objects, "")).unwrap(), ["Z Z"]);
    }

    #[test]
    fn excluded_tagged_text_does_not_consume_the_retained_text_budget() {
        let bytes = build(&one_page(stream("", b""), HELVETICA), "");
        let doc = Document::read(&bytes, None).unwrap();
        let mut retained = HashMap::new();
        let mut marking = Marking {
            page: 0,
            marks: Vec::new(),
            out: &mut retained,
            size: super::super::MAX_OUTPUT - RUN_COST - 4,
            inferred: false,
            has_image: false,
            cut: false,
        };
        let content = b"BT /F1 12 Tf /Artifact BMC (excluded) Tj EMC
                        /P <</MCID 0>> BDC (kept) Tj EMC ET";
        doc.read_marked(
            content,
            &Object::Null,
            IDENTITY,
            0,
            &mut marking,
            (None, TextState::default()),
        );
        assert!(!marking.cut);
        let run = &retained[&(0, 0)].runs[0];
        assert_eq!(run.text, "kept");
        assert!(run.x > 0.0, "excluded glyphs still advance the text matrix");
    }

    #[test]
    fn text_showing_stops_when_a_unicode_mapping_exceeds_the_remaining_budget() {
        let mut font = Font::default();
        font.to_unicode.insert(u32::from(b'A'), "ééé".to_string());
        let text = Object::Text(b"AA".to_vec());
        assert_eq!(font.show(Some(&text), Some(12)).unwrap().text, "éééééé");
        let partial = font.show(Some(&text), Some(11)).unwrap();
        assert!(partial.cut);
        assert_eq!(partial.text, "ééééé");
        let measured = font.show(Some(&text), None).unwrap();
        assert!(measured.text.is_empty());
        assert_eq!(measured.glyphs, 2);
    }

    #[test]
    fn plain_pdf_page_map_omits_blank_pages_after_the_exact_limit() {
        let mut content = b"BT /F1 12 Tf (".to_vec();
        content.resize(content.len() + super::super::MAX_OUTPUT - 1, b'a');
        content.extend_from_slice(b") Tj ET");
        let mut objects = one_page(stream("", &content), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R 6 0 R 7 0 R] /Count 3 >>".to_vec();
        objects.extend((0..2).map(|_| b"<< /Type /Page /Parent 2 0 R >>".to_vec()));
        let file = build(&objects, "");
        let (text, starts) = Document::read(&file, None).unwrap().text_pages().unwrap();
        assert_eq!(starts, [0]);
        assert_eq!(
            text.bytes().take_while(|&byte| byte == b'a').count(),
            super::super::MAX_OUTPUT - 1
        );
        assert!(text.contains("the PDF has too much decoded text"));
    }

    #[test]
    fn plain_pdf_page_map_omits_a_page_with_no_retained_text() {
        let mut content = b"BT /F1 12 Tf (".to_vec();
        content.resize(content.len() + super::super::MAX_OUTPUT - 1, b'a');
        content.extend_from_slice(b") Tj ET");
        let mut objects = one_page(stream("", &content), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec();
        objects.push(b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".to_vec());
        objects.push(stream("", b"BT (omitted second page) Tj ET"));
        let file = build(&objects, "");
        let (text, starts) = Document::read(&file, None).unwrap().text_pages().unwrap();
        assert_eq!(starts, [0]);
        assert!(!text.contains("omitted second page"));
        assert!(text.contains("the PDF has too much decoded text"));
    }

    #[test]
    fn plain_pdf_text_keeps_the_prefix_and_skips_pages_after_truncation() {
        let mut content = b"BT /F1 12 Tf (".to_vec();
        content.resize(content.len() + crate::core::extract::MAX_OUTPUT + 1, b'a');
        content.extend_from_slice(b") Tj ET");
        let mut objects = one_page(stream("", b"/Fm1 Do"), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R 7 0 R] /Count 2 >>".to_vec();
        objects[2] = b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>".to_vec();
        objects.push(stream("/Subtype /Form", &content));
        objects.push(b"<< /Type /Page /Parent 2 0 R /Contents 8 0 R >>".to_vec());
        objects.push(stream("", b"BT (second page) Tj ET"));
        let file = build(&objects, "");
        let (extracted, starts) = Document::read(&file, None).unwrap().text_pages().unwrap();
        assert_eq!(starts, [0]);
        assert!(!extracted.contains("second page"));
        assert!(extracted.starts_with("aaaaaaaa"));
        assert_eq!(
            extracted.bytes().take_while(|byte| *byte == b'a').count(),
            crate::core::extract::MAX_OUTPUT
        );
        assert!(extracted.contains("the PDF has too much decoded text"));
    }

    #[test]
    fn inferred_whitespace_joins_the_nearest_baseline() {
        let content = b"BT /F1 12 Tf 1 0 0 1 72 720 Tm (Upper) Tj
                        1 0 0 1 80 719 Tm ( ) Tj
                        1 0 0 1 72 718.8 Tm (Lower) Tj ET";
        let file = build(&one_page(stream("", content), HELVETICA), "");
        let (tree, _) = super::structure(&file, None).unwrap();
        let Kid::Element(paragraph) = &tree.kids[0] else {
            panic!("expected paragraph")
        };
        let Kid::Text(runs) = &paragraph.kids[0] else {
            panic!("expected runs")
        };
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<Vec<_>>(),
            ["Upper", "Lower", " "]
        );
        assert_eq!(runs[1].y, runs[2].y);
    }

    #[test]
    fn inferred_lines_are_anchored_only_by_visible_text() {
        let content = b"BT /F1 20 Tf 1 0 0 1 60 720.8 Tm ( ) Tj
                        1 0 0 1 72 720 Tm (Hello) Tj
                        1 0 0 1 150 719.5 Tm (world) Tj
                        /F1 12 Tf 1 0 0 1 72 680 Tm (Longer body text.) Tj ET";
        let file = build(&one_page(stream("", content), HELVETICA), "");
        assert_eq!(
            blocks(&file).unwrap(),
            ["# Hello world", "Longer body text."]
        );
    }

    #[test]
    fn inferred_comparison_rejects_markdown_expansion_past_the_output_limit() {
        let mut content = b"BT /F1 12 Tf (".to_vec();
        content.resize(
            content.len() + crate::core::extract::MAX_OUTPUT / 2 + 1,
            b'*',
        );
        content.extend_from_slice(b") Tj ET BT /F1 12 Tf 72 720 Td (other paragraph) Tj ET");
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        let error = blocks(&file).unwrap_err();
        assert!(
            error.to_string().contains("too much formatted text"),
            "{error:#}"
        );
    }

    #[test]
    fn inferred_headings_are_independent_of_how_an_exporter_splits_runs() {
        let formatted = |heading| {
            let content = format!(
                "BT /F1 20 Tf 72 720 Td {heading} ET
                 BT /F1 12 Tf 72 700 Td (First body line) Tj
                 0 -14 Td (Second body line) Tj ET"
            );
            blocks(&build(
                &one_page(stream("", content.as_bytes()), HELVETICA),
                "",
            ))
            .unwrap()
        };
        let whole = formatted("(Heading) Tj");
        assert_eq!(whole, ["# Heading", "First body line Second body line"]);
        assert_eq!(formatted("(H) Tj (ead) Tj (ing) Tj"), whole);
        assert_eq!(formatted("(Heading) Tj /F1 12 Tf ( ) Tj"), whole);
    }

    #[test]
    fn inferred_comparison_rejects_a_truncated_page_tree() {
        let mut objects = one_page(stream("", b"BT /F1 12 Tf (text) Tj ET"), HELVETICA);
        let mut kids = (6..6 + MAX_PAGES)
            .map(|number| format!("{number} 0 R "))
            .collect::<String>();
        kids.push_str("3 0 R");
        objects[1] =
            format!("<< /Type /Pages /Kids [{kids}] /Count {} >>", MAX_PAGES + 1).into_bytes();
        // Use empty pages so the page limit is reached before the text budget.
        objects.extend((0..MAX_PAGES).map(|_| b"<< /Type /Page /Parent 2 0 R >>".to_vec()));
        let error = blocks(&build(&objects, "")).unwrap_err();
        assert!(
            error.to_string().contains("page traversal limit"),
            "{error:#}"
        );
    }

    #[test]
    fn inferred_text_restores_font_and_size_after_a_saved_graphics_state() {
        let content = b"BT /F1 12 Tf 72 720 Td (First) Tj ET
                        q BT /F2 30 Tf 72 650 Td (Heading) Tj ET Q
                        BT 72 706 Td (second) Tj ET";
        let mut objects = one_page(stream("", content), HELVETICA);
        objects[2] =
            b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >>
                       /Contents 4 0 R >>"
                .to_vec();
        objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>".to_vec());
        assert_eq!(
            blocks(&build(&objects, "")).unwrap(),
            ["First second", "# Heading"]
        );
    }

    #[test]
    fn inferred_reading_order_uses_the_inherited_page_rotation() {
        // In raw coordinates these runs share X and progress along Y. After
        // clockwise page rotation they form one horizontal line.
        let content = b"BT /F1 12 Tf 0 1 -1 0 72 140 Tm (world) Tj
                        0 1 -1 0 72 100 Tm (Hello) Tj ET";
        let mut objects = one_page(stream("", content), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R] /Count 1 /Rotate 90 >>".to_vec();
        assert_eq!(blocks(&build(&objects, "")).unwrap(), ["Hello world"]);
    }

    #[test]
    fn inferred_comparison_rejects_pages_that_exceed_the_form_budget() {
        let mut content = b"BT /F1 12 Tf 72 720 Td (page) Tj ET\n".to_vec();
        content.extend_from_slice(&b"/Fm1 Do\n".repeat(MAX_FORMS + 1));
        let mut objects = one_page(stream("", &content), HELVETICA);
        objects[2] = b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> \
                       /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>"
            .to_vec();
        objects.push(stream("/Type /XObject /Subtype /Form", b"q Q"));
        let error = blocks(&build(&objects, "")).unwrap_err();
        assert!(
            error.to_string().contains("form traversal limit"),
            "{error:#}"
        );
    }

    #[test]
    fn a_file_without_tags_infers_headings_and_paragraphs_in_page_order() {
        let content = b"BT /F1 12 Tf 72 686 Td (second line.) Tj ET
                        BT /F1 20 Tf 72 720 Td (Heading) Tj ET
                        BT /F1 12 Tf 72 700 Td (First line and) Tj ET
                        BT /F1 12 Tf 72 655 Td (Next paragraph.) Tj ET";
        let file = build(&one_page(stream("", content), HELVETICA), "");
        assert!(super::structure(&file, None).unwrap().1);
        assert_eq!(
            blocks(&file).unwrap(),
            [
                "# Heading",
                "First line and second line.",
                "Next paragraph."
            ]
        );
    }

    /// A file without tags whose pages draw `contents`, each on a letter-sized page, in
    /// Helvetica as `/F1`.
    fn pages_drawing(contents: &[String]) -> Vec<u8> {
        pages_drawing_in(contents, "")
    }

    /// Like [`pages_drawing`], with `inherited` added to the node above the pages.
    fn pages_drawing_in(contents: &[String], inherited: &str) -> Vec<u8> {
        pages_drawing_sized(contents, inherited, &[])
    }

    /// Like [`pages_drawing_in`], with each page as high as `heights` says, or 792 points.
    fn pages_drawing_sized(contents: &[String], inherited: &str, heights: &[u32]) -> Vec<u8> {
        let each: Vec<(u32, &str)> = heights.iter().map(|&height| (height, "")).collect();
        pages_drawing_each(contents, inherited, &each)
    }

    /// Like [`pages_drawing_in`], with each page as high as `each` says, or 792 points, and the
    /// entries it gives added to the page.
    fn pages_drawing_each(contents: &[String], inherited: &str, each: &[(u32, &str)]) -> Vec<u8> {
        let count = contents.len();
        let font = 3 + 2 * count;
        let kids = (0..count)
            .map(|i| format!("{} 0 R ", 3 + i))
            .collect::<String>();
        let mut objects = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            format!("<< /Type /Pages /Kids [{kids}] /Count {count} {inherited} >>").into_bytes(),
        ];
        for i in 0..count {
            objects.push(
                format!(
                    "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 {}] {} \
                     /Resources << /Font << /F1 {font} 0 R >> >> /Contents {} 0 R >>",
                    each.get(i).map_or(792, |page| page.0),
                    each.get(i).map_or("", |page| page.1),
                    3 + count + i
                )
                .into_bytes(),
            );
        }
        for content in contents {
            objects.push(stream("", content.as_bytes()));
        }
        objects.push(HELVETICA.as_bytes().to_vec());
        build(&objects, "")
    }

    #[test]
    fn inferred_pages_leave_out_their_running_headers_and_footers() {
        let contents: Vec<String> = (1..=3)
            .map(|n| {
                format!(
                    "BT /F1 9 Tf 72 760 Td (Quarterly Report) Tj ET
                     BT /F1 9 Tf 450 760 Td (Draft) Tj ET
                     BT /F1 12 Tf 72 700 Td (Body of page {n}.) Tj ET
                     BT /F1 12 Tf 72 400 Td (Said on every page.) Tj ET
                     BT /F1 9 Tf 280 30 Td (Page {n} of 3) Tj ET"
                )
            })
            .collect();
        let file = pages_drawing(&contents);
        assert!(super::structure(&file, None).unwrap().1);
        assert_eq!(
            blocks(&file).unwrap(),
            [
                "Body of page 1.",
                "Said on every page.",
                "Body of page 2.",
                "Said on every page.",
                "Body of page 3.",
                "Said on every page.",
            ]
        );
    }

    #[test]
    fn the_text_of_one_page_keeps_a_lone_number_at_its_edge() {
        let content = "BT /F1 12 Tf 72 700 Td (Total due:) Tj ET
                       BT /F1 12 Tf 72 40 Td (1250) Tj ET";
        let (text, _) = Document::read(&pages_drawing(&[content.to_string()]), None)
            .unwrap()
            .text_pages()
            .unwrap();
        assert_eq!(text, "Total due:\n1250\n");
    }

    #[test]
    fn the_text_of_pages_leaves_out_their_running_headers_and_footers() {
        let contents: Vec<String> = (1..=3)
            .map(|n| {
                format!(
                    "BT /F1 9 Tf 72 760 Td (Quarterly Report) Tj 378 0 Td (Draft) Tj ET
                     BT /F1 12 Tf 72 700 Td (Body of page {n}.) Tj ET
                     BT /F1 12 Tf 72 400 Td (Said on every page.) Tj ET
                     BT /F1 9 Tf 280 30 Td (Page {n} of 3) Tj ET"
                )
            })
            .collect();
        let (text, starts) = Document::read(&pages_drawing(&contents), None)
            .unwrap()
            .text_pages()
            .unwrap();
        assert_eq!(
            text,
            concat!(
                "Body of page 1.\nSaid on every page.\n\n",
                "Body of page 2.\nSaid on every page.\n\n",
                "Body of page 3.\nSaid on every page.\n",
            )
        );
        assert_eq!(starts, [0, 37, 74]);
    }

    #[test]
    fn a_code_listing_split_by_a_page_break_is_read_as_one_block() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Some text that comes first.) Tj ET
             BT /F1 10 Tf 90 660 Td ([merge-tools]) Tj 0 -12 Td (tool.args = -o x) Tj ET"
                .to_string(),
            "BT /F1 10 Tf 90 700 Td (tool.checkchanged = true) Tj ET
             BT /F1 12 Tf 72 660 Td (Text after.) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "Some text that comes first.",
                r"\[merge-tools\] tool.args = -o x tool.checkchanged = true",
                "Text after.",
            ]
        );
    }

    #[test]
    fn a_block_set_lower_on_the_next_page_starts_anew() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Some text that comes first.) Tj ET
             BT /F1 10 Tf 90 660 Td ([extensions]) Tj 0 -12 Td (extdiff =) Tj ET"
                .to_string(),
            "BT /F1 10 Tf 90 688 Td ([extdiff]) Tj 0 -12 Td (cmd.tool = tool) Tj ET
             BT /F1 12 Tf 72 640 Td (Text after.) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "Some text that comes first.",
                r"\[extensions\] extdiff =",
                r"\[extdiff\] cmd.tool = tool",
                "Text after.",
            ]
        );
    }

    #[test]
    fn a_paragraph_that_runs_on_to_the_next_page_is_read_as_one() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (words words words) Tj 0 -14 Td (words words words) Tj ET
             BT /F1 9 Tf 300 30 Td (1) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (goes on here and) Tj 0 -14 Td (ends.) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (A new paragraph.) Tj ET
             BT /F1 9 Tf 300 30 Td (3) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "words words words words words words goes on here and ends.",
                "A new paragraph.",
            ]
        );
    }

    #[test]
    fn a_page_break_between_two_sentences_of_a_paragraph_is_read_as_one() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (words words words) Tj 0 -14 Td (words words ends.) Tj ET
             BT /F1 9 Tf 300 30 Td (1) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (Short.) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (A new paragraph.) Tj ET
             BT /F1 9 Tf 300 30 Td (3) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "words words words words words ends. Short.",
                "A new paragraph.",
            ]
        );
    }

    #[test]
    fn a_full_line_that_ends_a_paragraph_with_room_below_it_starts_anew() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (words words words) Tj 0 -14 Td (words words ends.) Tj ET
             BT /F1 9 Tf 300 30 Td (1) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (other words here) Tj 0 -14 Td (other words here) Tj
             0 -14 Td (other words here) Tj 0 -14 Td (the end.) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "words words words words words ends.",
                "other words here other words here other words here the end.",
            ]
        );
    }

    #[test]
    fn a_body_line_that_reads_the_same_at_the_bottom_of_two_pages_is_kept() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (fn a() {) Tj 0 -626 Td (}) Tj ET
             BT /F1 9 Tf 300 30 Td (1) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 690 Td (fn b() {) Tj 0 -630 Td (}) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["fn a() {", "}", "fn b() {", "}"]
        );
    }

    #[test]
    fn a_lone_number_that_does_not_count_the_pages_is_kept() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Total:) Tj 0 -660 Td (42) Tj ET
             BT /F1 9 Tf 300 30 Td (1) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 690 Td (More.) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (End.) Tj ET
             BT /F1 9 Tf 300 740 Td (3) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Total:", "42", "More.", "End."]
        );
    }

    #[test]
    fn a_paragraph_goes_on_across_facing_pages_with_mirrored_margins() {
        let page = |x: u32, text: &str| {
            format!("BT /F1 12 Tf {x} 700 Td ({text}) Tj 0 -14 Td (and on and) Tj ET")
        };
        let words = ["one", "two", "three", "four", "five", "six"];
        let contents: Vec<String> = words
            .iter()
            .enumerate()
            .map(|(n, word)| page(if n % 2 == 0 { 90 } else { 54 }, word))
            .collect();
        let read = words.map(|word| format!("{word} and on and")).join(" ");
        assert_eq!(blocks(&pages_drawing(&contents)).unwrap(), [read]);
    }

    #[test]
    fn a_crop_box_set_above_the_pages_marks_their_edges() {
        let contents: Vec<String> = (1..=2)
            .map(|n| {
                format!(
                    "BT /F1 12 Tf 72 600 Td (Text {n}.) Tj ET
                     BT /F1 9 Tf 72 120 Td (Guide.) Tj ET"
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing_in(&contents, "/CropBox [0 100 612 692]")).unwrap(),
            ["Text 1.", "Text 2."]
        );
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Text 1.", "Guide.", "Text 2.", "Guide."]
        );
    }

    #[test]
    fn a_header_keeps_its_place_on_pages_of_different_heights() {
        let heights = [792, 842, 792];
        let contents: Vec<String> = heights
            .iter()
            .enumerate()
            .map(|(n, height)| {
                format!(
                    "BT /F1 9 Tf 72 {} Td (Report) Tj ET
                     BT /F1 12 Tf 72 600 Td (Text {}.) Tj ET",
                    height - 40,
                    n + 1
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing_sized(&contents, "", &heights)).unwrap(),
            ["Text 1.", "Text 2.", "Text 3."]
        );
    }

    #[test]
    fn the_only_page_number_of_a_file_is_left_out() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Dear reader.) Tj ET".to_string(),
            "BT /F1 12 Tf 72 700 Td (Yours.) Tj ET
             BT /F1 9 Tf 300 30 Td (2) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Dear reader.", "Yours."]
        );
    }

    #[test]
    fn a_listing_goes_on_to_a_page_it_fills_in_a_short_file() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Run this:) Tj ET
             BT /F1 12 Tf 90 600 Td (a = 1) Tj 0 -14 Td (b = 2) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 90 700 Td (c = 3) Tj 0 -14 Td (d = 4) Tj ET".to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Run this:", "a = 1 b = 2 c = 3 d = 4"]
        );
    }

    #[test]
    fn a_smaller_footnote_does_not_set_where_a_filled_page_ends() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Intro.) Tj
             0 -586 Td (words words words) Tj 0 -14 Td (words words ends.) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (Short.) Tj 0 -600 Td (Last.) Tj ET".to_string(),
            "BT /F1 12 Tf 72 700 Td (The tail.) Tj ET
             BT /F1 8 Tf 72 40 Td (Note.) Tj ET"
                .to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            [
                "Intro.",
                "words words words words words ends. Short.",
                "Last.",
                "The tail.",
                "Note.",
            ]
        );
    }

    #[test]
    fn a_paragraph_goes_on_to_a_page_of_another_height() {
        let heights = [842, 792, 842];
        let lines = [
            ("alpha beta", "gamma and"),
            ("delta and", "more and"),
            ("last of", "it."),
        ];
        let contents: Vec<String> = heights
            .iter()
            .zip(lines)
            .map(|(height, (one, two))| {
                format!(
                    "BT /F1 12 Tf 72 {} Td ({one}) Tj 0 -14 Td ({two}) Tj ET",
                    height - 62
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing_sized(&contents, "", &heights)).unwrap(),
            ["alpha beta gamma and delta and more and last of it."]
        );
    }

    #[test]
    fn a_page_turned_on_its_side_does_not_set_where_a_filled_page_ends() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Intro.) Tj
             0 -586 Td (words words words) Tj 0 -14 Td (words words ends.) Tj ET"
                .to_string(),
            "BT /F1 12 Tf 72 700 Td (Short.) Tj 0 -600 Td (Last.) Tj ET".to_string(),
            "BT /F1 12 Tf 72 500 Td (Table.) Tj ET".to_string(),
        ];
        let each = [(792, ""), (792, ""), (792, "/Rotate 90")];
        assert_eq!(
            blocks(&pages_drawing_each(&contents, "", &each)).unwrap(),
            [
                "Intro.",
                "words words words words words ends. Short.",
                "Last.",
                "Table.",
            ]
        );
    }

    #[test]
    fn numbers_ending_several_pages_that_do_not_count_them_are_kept() {
        let contents = [
            "BT /F1 12 Tf 72 700 Td (Row.) Tj 0 -660 Td (1) Tj ET".to_string(),
            "BT /F1 12 Tf 72 690 Td (Row two.) Tj 0 -650 Td (7) Tj ET".to_string(),
        ];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Row.", "1", "Row two.", "7"]
        );
    }

    #[test]
    fn one_low_line_of_body_text_does_not_set_where_a_filled_page_ends() {
        let mut contents = vec!["BT /F1 12 Tf 72 700 Td (Intro.) Tj
             0 -586 Td (words words words) Tj 0 -14 Td (words words ends.) Tj ET"
            .to_string()];
        contents.extend(
            (0..10).map(|_| "BT /F1 12 Tf 72 700 Td (Short.) Tj 0 -600 Td (Last.) Tj ET".into()),
        );
        contents.push("BT /F1 12 Tf 72 700 Td (The end.) Tj 0 -660 Td (Appendix A) Tj ET".into());
        let mut expected = vec![
            "Intro.",
            "words words words words words ends. Short.",
            "Last.",
        ];
        for _ in 1..10 {
            expected.extend(["Short.", "Last."]);
        }
        expected.extend(["The end.", "Appendix A"]);
        assert_eq!(blocks(&pages_drawing(&contents)).unwrap(), expected);
    }

    #[test]
    fn a_page_number_still_counts_across_a_blank_page() {
        let page = |n: u32| {
            format!(
                "BT /F1 12 Tf 72 700 Td (Text {n}.) Tj ET
                 BT /F1 9 Tf 72 30 Td (Guide | Version 0.9.9 | {n}) Tj ET"
            )
        };
        let contents = [page(9), String::new(), page(11)];
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Text 9.", "Text 11."]
        );
    }

    #[test]
    fn the_same_header_and_footer_are_left_out_on_every_page() {
        let contents: Vec<String> = (1..=2)
            .map(|n| {
                format!(
                    "BT /F1 9 Tf 72 770 Td (Confidential) Tj ET
                     BT /F1 12 Tf 72 700 Td (Text {n}.) Tj ET
                     BT /F1 9 Tf 72 30 Td (Confidential) Tj ET"
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Text 1.", "Text 2."]
        );
    }

    #[test]
    fn inferred_edge_lines_keep_numbers_that_do_not_count_the_pages() {
        let contents: Vec<String> = [1, 5]
            .iter()
            .map(|n| {
                format!(
                    "BT /F1 10 Tf 72 765 Td (Step {n}.) Tj ET
                     BT /F1 12 Tf 72 700 Td (Text {n}.) Tj ET"
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Step 1.", "Text 1.", "Step 5.", "Text 5."]
        );
    }

    /// Guards the design decision on [`same_running_text`]: a footer is left out whole even
    /// when it holds numbers other than the page number.
    #[test]
    fn a_footer_with_a_version_and_a_page_number_is_left_out() {
        let contents: Vec<String> = (103..=105)
            .map(|n| {
                format!(
                    "BT /F1 12 Tf 72 700 Td (Text {n}.) Tj ET
                     BT /F1 9 Tf 72 30 Td (Guide | Version 0.9.9) Tj ET
                     BT /F1 9 Tf 500 30 Td ({n}) Tj ET"
                )
            })
            .collect();
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["Text 103.", "Text 104.", "Text 105."]
        );
    }

    #[test]
    fn a_single_inferred_page_loses_only_its_bare_page_number() {
        let content = b"BT /F1 12 Tf 72 760 Td (Title line near the top) Tj ET
                        BT /F1 12 Tf 72 700 Td (Page 2 is cited in the body.) Tj ET
                        BT /F1 12 Tf 72 400 Td (7) Tj ET
                        BT /F1 12 Tf 72 50 Td (Last words near the bottom.) Tj ET
                        BT /F1 10 Tf 300 30 Td (- 7 -) Tj ET";
        let file = build(&one_page(stream("", content), HELVETICA), "");
        assert_eq!(
            blocks(&file).unwrap(),
            [
                "Title line near the top",
                "Page 2 is cited in the body.",
                "7",
                "Last words near the bottom.",
            ]
        );
    }

    #[test]
    fn a_structure_tree_that_leads_back_into_itself_is_read_once() {
        let file = build(
            &[
                b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 6 0 R >>".to_vec(),
                b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
                b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> \
                  /Contents 4 0 R >>"
                    .to_vec(),
                stream(
                    "",
                    b"/P <</MCID 0>> BDC BT /F1 12 Tf 72 720 Td (Once) Tj ET EMC\n",
                ),
                HELVETICA.as_bytes().to_vec(),
                b"<< /Type /StructTreeRoot /K 7 0 R >>".to_vec(),
                b"<< /S /P /Pg 3 0 R /K [0 7 0 R] >>".to_vec(),
            ],
            "",
        );
        assert_eq!(blocks(&file).unwrap(), ["Once"]);
    }

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
    fn chinese_text_that_goes_on_to_the_next_line_is_joined_without_a_space() {
        // Codes 1 to 4 stand for 檔, 案, A and B.
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                     1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                     4 beginbfchar\n<0001> <6A94>\n<0002> <6848>\n<0003> <0041>\n\
                     <0004> <0042>\nendbfchar\nendcmap end end"
            .to_vec();
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Custom \
                    /DescendantFonts [] /ToUnicode 6 0 R >>";
        let content = b"BT /F1 12 Tf 72 700 Td <000300040001> Tj ET
                        BT /F1 12 Tf 72 686 Td <0002> Tj ET
                        BT /F1 12 Tf 72 672 Td <0003> Tj ET"
            .to_vec();
        let mut objects = one_page(stream("", &content), font);
        objects.push(stream("", &cmap));
        assert_eq!(blocks(&build(&objects, "")).unwrap(), ["AB檔案 A"]);
    }

    #[test]
    fn a_large_icon_beside_a_note_does_not_split_it() {
        // Codes 1 to 3 stand for A, B and the info sign of an icon font.
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                     1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                     3 beginbfchar\n<0001> <0041>\n<0002> <0042>\n<0003> <F05A>\n\
                     endbfchar\nendcmap end end"
            .to_vec();
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Custom \
                    /DescendantFonts [] /ToUnicode 6 0 R >>";
        let content = b"BT /F1 12 Tf 126 700 Td <0001> Tj ET
                        BT /F1 24 Tf 72 694 Td <0003> Tj ET
                        BT /F1 12 Tf 126 686 Td <0002> Tj ET"
            .to_vec();
        let mut objects = one_page(stream("", &content), font);
        objects.push(stream("", &cmap));
        assert_eq!(blocks(&build(&objects, "")).unwrap(), ["A B"]);
    }

    /// A font whose codes 1 to 4 stand for A, B, the info sign of an icon font and a character
    /// a font defines for a name, both in the private use area, and 5 for a warning sign.
    fn with_private_characters(content: &[u8]) -> Vec<u8> {
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                     1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                     5 beginbfchar\n<0001> <0041>\n<0002> <0042>\n<0003> <F05A>\n\
                     <0004> <E3C1>\n<0005> <26A0>\nendbfchar\nendcmap end end"
            .to_vec();
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Custom \
                    /DescendantFonts [] /ToUnicode 6 0 R >>";
        let mut objects = one_page(stream("", content), font);
        objects.push(stream("", &cmap));
        build(&objects, "")
    }

    #[test]
    fn a_private_character_within_a_line_is_kept() {
        let content = b"BT /F1 12 Tf 72 700 Td <000100040002> Tj ET";
        assert_eq!(
            blocks(&with_private_characters(content)).unwrap(),
            ["A\u{e3c1}B"]
        );
    }

    #[test]
    fn a_large_sign_between_the_lines_of_a_paragraph_does_not_split_it() {
        let content = b"BT /F1 12 Tf 126 700 Td <0001> Tj ET
                        BT /F1 24 Tf 72 694 Td <0005> Tj ET
                        BT /F1 12 Tf 126 686 Td <0002> Tj ET";
        assert_eq!(
            blocks(&with_private_characters(content)).unwrap(),
            ["\u{26a0} A B"]
        );
    }

    #[test]
    fn a_raised_footnote_mark_is_read_after_its_word() {
        // Helvetica without widths is half its size wide a character: "as shown by Smith"
        // ends at 72 + 17 * 6 = 174, where the mark starts, and the mark ends at 177.5.
        let content = "BT /F1 12 Tf 72 714 Td (the line before) Tj ET
                       BT /F1 12 Tf 72 700 Td (as shown by Smith) Tj ET
                       BT /F1 7 Tf 174 704 Td (1) Tj ET
                       BT /F1 12 Tf 177.5 700 Td ( in 2020) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["the line before as shown by Smith1 in 2020"]
        );
    }

    #[test]
    fn a_raised_mark_of_several_characters_is_read_after_its_word() {
        let content = "BT /F1 12 Tf 72 714 Td (the line before) Tj ET
                       BT /F1 12 Tf 72 700 Td (as shown by Smith) Tj ET
                       BT /F1 7 Tf 174 704 Td (\\(12a\\)) Tj ET
                       BT /F1 12 Tf 191.5 700 Td ( in 2020) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["the line before as shown by Smith(12a) in 2020"]
        );
    }

    #[test]
    fn a_wide_line_between_two_lines_is_a_line_of_its_own() {
        let content = "BT /F1 12 Tf 72 700 Td (short) Tj ET
                       BT /F1 12 Tf 200 694 Td (a much longer middle line) Tj ET
                       BT /F1 12 Tf 72 686 Td (next) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["short", "a much longer middle line", "next"]
        );
    }

    #[test]
    fn a_short_cell_at_the_size_of_the_text_between_two_lines_is_its_own_line() {
        let content = "BT /F1 12 Tf 72 700 Td (first cell) Tj ET
                       BT /F1 12 Tf 200 694 Td (or x) Tj ET
                       BT /F1 12 Tf 72 686 Td (second cell) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["first cell", "or x", "second cell"]
        );
    }

    #[test]
    fn large_list_numbers_stay_with_their_items() {
        let content = "BT /F1 12 Tf 72 700 Td (Steps:) Tj ET
                       BT /F1 24 Tf 72 680 Td (1) Tj ET BT /F1 12 Tf 100 680 Td (first step) Tj ET
                       BT /F1 24 Tf 72 662 Td (2) Tj ET BT /F1 12 Tf 100 662 Td (second) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["Steps:", "1 first step 2 second"]
        );
    }

    #[test]
    fn a_drop_cap_is_found_past_a_mark_below_its_line() {
        // "the lazy dog and" ends at 110 + 16 * 6 = 206, where the lowered mark starts.
        let content = "BT /F1 12 Tf 110 700 Td (he quick brown) Tj 0 -14 Td (fox jumps over) Tj
                       0 -14 Td (the lazy dog and) Tj ET
                       BT /F1 36 Tf 72 672 Td (T) Tj ET
                       BT /F1 7 Tf 206 668 Td (2) Tj ET
                       BT /F1 12 Tf 72 658 Td (runs away.) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["The quick brown fox jumps over the lazy dog and2 runs away."]
        );
    }

    #[test]
    fn a_drop_cap_on_a_baseline_of_its_own_is_read_first() {
        let content = "BT /F1 12 Tf 110 700 Td (he quick brown) Tj 0 -14 Td (fox jumps over) Tj
                       0 -14 Td (the lazy dog and) Tj ET
                       BT /F1 36 Tf 72 679 Td (T) Tj ET
                       BT /F1 12 Tf 72 658 Td (runs away.) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["The quick brown fox jumps over the lazy dog and runs away."]
        );
    }

    #[test]
    fn a_drop_cap_is_read_as_the_first_letter_of_its_paragraph() {
        let content = "BT /F1 12 Tf 110 700 Td (he quick brown) Tj 0 -14 Td (fox jumps over) Tj
                       0 -14 Td (the lazy dog and) Tj ET
                       BT /F1 36 Tf 72 672 Td (T) Tj ET
                       BT /F1 12 Tf 72 658 Td (runs away.) Tj ET";
        assert_eq!(
            blocks(&pages_drawing(&[content.to_string()])).unwrap(),
            ["The quick brown fox jumps over the lazy dog and runs away."]
        );
    }

    #[test]
    fn an_image_page_with_only_an_icon_is_reported() {
        let mut objects = one_page(stream("", b"BT /F1 12 Tf (cover) Tj ET"), HELVETICA);
        objects[1] = b"<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 2 >>".to_vec();
        objects.push(
            b"<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 8 0 R >> >> \
              /Contents 7 0 R >>"
                .to_vec(),
        );
        objects.push(stream(
            "",
            b"BI /W 1 /H 1 /CS /G /BPC 8 ID x\nEI\nBT /F1 24 Tf 72 700 Td <0001> Tj ET",
        ));
        objects.push(
            b"<< /Type /Font /Subtype /Type0 /BaseFont /Custom /DescendantFonts [] \
              /ToUnicode 9 0 R >>"
                .to_vec(),
        );
        objects.push(stream(
            "",
            b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
              1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
              1 beginbfchar\n<0001> <F05A>\nendbfchar\nendcmap end end",
        ));
        let error = blocks(&build(&objects, "")).unwrap_err();
        assert!(error
            .to_string()
            .contains("page 2 contains an image but no extractable text"));
    }

    #[test]
    fn a_page_without_known_edges_is_not_joined_to_its_neighbours() {
        let contents = ["alpha and", "beta and", "gamma."]
            .map(|text| format!("BT /F1 12 Tf 72 700 Td ({text}) Tj ET"));
        assert_eq!(
            blocks(&pages_drawing_sized(&contents, "", &[792, 0, 792])).unwrap(),
            ["alpha and", "beta and", "gamma."]
        );
        assert_eq!(
            blocks(&pages_drawing(&contents)).unwrap(),
            ["alpha and beta and gamma."]
        );
    }

    #[test]
    fn glyphs_placed_one_at_a_time_on_a_line_read_as_one_line() {
        // Helvetica without widths is 6 points wide a character at 12 points.
        let content = b"BT /F1 12 Tf 72 700 Td (A) Tj 6 0 Td (B) Tj 6 0 Td (C) Tj
                        30 0 Td (next) Tj ET
                        BT /F1 12 Tf 72 686 Td (below) Tj ET"
            .to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "ABC next\nbelow\n");
    }

    #[test]
    fn text_running_up_the_page_reads_as_lines() {
        let content = b"BT /F1 12 Tf 0 1 -1 0 300 100 Tm (A) Tj 0 1 -1 0 300 106 Tm (B) Tj
                        0 1 -1 0 300 112 Tm (C) Tj 0 1 -1 0 286 100 Tm (D) Tj ET"
            .to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "ABC\nD\n");
    }

    #[test]
    fn a_lowered_subscript_stays_on_its_line() {
        let content = b"BT /F1 12 Tf 72 700 Td (H) Tj 6 -4 Td (2) Tj 6 4 Td (O) Tj ET".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "H2O\n");
    }

    #[test]
    fn chinese_spread_along_a_justified_line_gets_no_spaces() {
        // Codes 1 and 2 stand for 檔 and 案, a whole size wide; the second is drawn a quarter
        // of the size after the first ends.
        let cmap = b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n\
                     1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
                     2 beginbfchar\n<0001> <6A94>\n<0002> <6848>\nendbfchar\nendcmap end end"
            .to_vec();
        let font = "<< /Type /Font /Subtype /Type0 /BaseFont /Custom \
                    /DescendantFonts [] /ToUnicode 6 0 R >>";
        let content = b"BT /F1 12 Tf 72 700 Td <0001> Tj 15 0 Td <0002> Tj ET".to_vec();
        let mut objects = one_page(stream("", &content), font);
        objects.push(stream("", &cmap));
        let file = build(&objects, "");
        assert_eq!(text(&file).unwrap(), "檔案\n");
        assert_eq!(blocks(&file).unwrap(), ["檔案"]);
    }

    #[test]
    fn lines_placed_by_the_graphics_transform_stay_apart() {
        let content = b"q 1 0 0 1 72 700 cm BT /F1 12 Tf (one) Tj ET Q
                        q 1 0 0 1 72 686 cm BT /F1 12 Tf (two) Tj ET Q"
            .to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "one\ntwo\n");
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
        // Where each page starts, as found by the read that compares the file, is kept while
        // the file is unchanged: a report asks for it without reading the file again.
        use crate::core::extract::{pdf_pages, Kind};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("two.pdf");
        std::fs::write(&path, &file).unwrap();
        Kind::Pdf.read(&path, None).unwrap();
        assert_eq!(pdf_pages(&path, None).unwrap(), (vec![0, 2], 3));
        let written = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, vec![b'x'; file.len()]).unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(written).unwrap();
        drop(f);
        assert_eq!(pdf_pages(&path, None).unwrap(), (vec![0, 2], 3));
        // A file written since is read again.
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(pdf_pages(&path, None).is_err());
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

    /// A content stream drawing `words`, whose `/Length` is `length` rather than a number.
    fn stream_with_length(length: &str, words: &str) -> Vec<u8> {
        let mut out = format!("<< /Length {length} >>\nstream\n").into_bytes();
        out.extend_from_slice(format!("BT /F1 12 Tf 72 720 Td ({words}) Tj ET\n").as_bytes());
        out.extend_from_slice(b"endstream");
        out
    }

    /// Reading a stream reads its length, which may be the stream itself.
    #[test]
    fn a_stream_whose_length_is_itself_is_read_to_its_end() {
        let file = build(
            &one_page(stream_with_length("4 0 R", "still read"), HELVETICA),
            "",
        );
        assert_eq!(text(&file).unwrap(), "still read\n");
    }

    /// Or another stream, whose length is another, and so on: following each would run out
    /// of stack long before the file runs out of objects.
    #[test]
    fn a_long_chain_of_lengths_is_not_followed_to_its_end() {
        let mut objects = one_page(stream_with_length("6 0 R", "still read"), HELVETICA);
        let chain = 20_000;
        for i in 0..chain {
            let next = 6 + i + 1;
            objects.push(format!("<< /Length {next} 0 R >>\nstream\nx\nendstream").into_bytes());
        }
        let file = build(&objects, "");
        assert_eq!(text(&file).unwrap(), "still read\n");
    }

    #[test]
    fn a_value_keeps_no_more_items_than_there_is_room_for_and_is_still_read_to_its_end() {
        let many = MAX_ITEMS + 10;
        let mut source = b"[".to_vec();
        source.extend(b"0 ".repeat(many));
        source.extend_from_slice(b"] /after");
        let mut lexer = Lexer::new(&source, 0);
        match lexer.object(0) {
            Some(Object::Array(items)) => assert_eq!(items.len(), MAX_ITEMS),
            other => panic!("{other:?}"),
        }
        assert_eq!(lexer.object(0), Some(Object::Name("after".into())));

        let mut source = b"<<".to_vec();
        for i in 0..many {
            source.extend(format!("/k{i} 0 ").bytes());
        }
        source.extend_from_slice(b">> /after");
        let mut lexer = Lexer::new(&source, 0);
        match lexer.object(0) {
            Some(Object::Dict(map)) => assert_eq!(map.len(), MAX_ITEMS),
            other => panic!("{other:?}"),
        }
        assert_eq!(lexer.object(0), Some(Object::Name("after".into())));
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

    /// Puts `value` in place of the first `0000000000` written into `file` after `after`.
    fn patch(file: &mut [u8], after: &[u8], value: usize) {
        let from = file.windows(after.len()).position(|w| w == after).unwrap();
        let at = from
            + file[from..]
                .windows(10)
                .position(|w| w == b"0000000000")
                .unwrap();
        file[at..at + 10].copy_from_slice(format!("{value:010}").as_bytes());
    }

    /// The place of object `number` in `file`.
    fn place_of(file: &[u8], number: usize) -> usize {
        let head = format!("\n{number} 0 obj");
        file.windows(head.len())
            .position(|w| w == head.as_bytes())
            .unwrap()
            + 1
    }

    /// A table may name another to read with it, and that one may be itself or past the end.
    #[test]
    fn a_table_that_names_itself_or_a_place_past_the_end_is_read_once() {
        let content = b"BT /F1 12 Tf 72 720 Td (still here) Tj ET\n".to_vec();
        let mut file = build(
            &one_page(stream("", &content), HELVETICA),
            " /XRefStm 0000000000 /Prev 0000000000",
        );
        let table = file.windows(5).rposition(|w| w == b"xref\n").unwrap();
        patch(&mut file, b"/XRefStm", table);
        let past = file.len() + 10;
        patch(&mut file, b"/Prev", past);
        assert_eq!(text(&file).unwrap(), "still here\n");
    }

    /// A table written as a stream says how wide its fields are; widths no number can have, or
    /// none at all, are not believed.
    #[test]
    fn a_table_stream_with_impossible_field_widths_is_not_believed() {
        for widths in [
            "[9 9 9]",
            "[0 0 0]",
            "[1 2]",
            "[1 -2 1]",
            "[1 99999999999 1]",
        ] {
            let content = b"BT /F1 12 Tf 72 720 Td (still here) Tj ET\n".to_vec();
            let mut objects = one_page(stream("", &content), HELVETICA);
            objects.push(stream(
                &format!("/Type /XRef /Size 7 /W {widths} /Index [0 4294967295]"),
                &[1u8; 64],
            ));
            let mut file = build(&objects, " /XRefStm 0000000000");
            let at = place_of(&file, 6);
            patch(&mut file, b"/XRefStm", at);
            assert_eq!(text(&file).unwrap(), "still here\n", "{widths}");
        }
    }

    /// The first words of the note saying some of a file could not be read.
    fn incomplete_note() -> String {
        let note = crate::i18n::tr("msg.extract_incomplete");
        note.split("{reason}").next().unwrap().to_string()
    }

    /// Words that cannot be read are said to be missing, rather than left out as if the page
    /// had none; a file none of whose words can be read is refused.
    #[test]
    fn content_that_cannot_be_read_is_reported_rather_than_left_out() {
        let page = "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> \
                    /Contents [4 0 R 6 0 R] >>";
        let mut objects = one_page(
            stream("", b"BT /F1 12 Tf 72 720 Td (readable) Tj ET\n"),
            HELVETICA,
        );
        objects[2] = page.as_bytes().to_vec();
        objects.push(stream("/Filter /FlateDecode", b"not compressed at all"));
        let read = text(&build(&objects, "")).unwrap();
        assert!(read.starts_with("readable\n"), "{read}");
        assert!(read.contains(&incomplete_note()), "{read}");

        let only = one_page(
            stream("/Filter /FlateDecode", b"not compressed at all"),
            HELVETICA,
        );
        assert!(text(&build(&only, "")).is_err());
    }

    /// A composite font without a table of the characters its glyphs are writes glyph numbers,
    /// which are not text.
    #[test]
    fn a_composite_font_without_characters_is_reported_rather_than_read_as_rubbish() {
        let content = b"BT /F1 12 Tf 72 720 Td (\x00\x41\x00\x42) Tj ET\n".to_vec();
        let composite = "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H >>";
        assert!(text(&build(&one_page(stream("", &content), composite), "")).is_err());
    }

    /// A page may draw a form, which has words of its own, and the form may draw itself.
    #[test]
    fn the_words_of_a_form_a_page_draws_are_read_once_however_it_draws_itself() {
        let page = "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> \
                    /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>";
        let mut objects = one_page(
            stream("", b"BT /F1 12 Tf 72 720 Td (page) Tj ET /Fm1 Do\n"),
            HELVETICA,
        );
        objects[2] = page.as_bytes().to_vec();
        objects.push(stream(
            "/Type /XObject /Subtype /Form /Resources << /Font << /F1 5 0 R >> \
             /XObject << /Fm1 6 0 R >> >>",
            b"BT /F1 12 Tf (in form) Tj ET /Fm1 Do\n",
        ));
        let read = text(&build(&objects, "")).unwrap();
        assert!(read.starts_with("page\nin form\n"), "{read}");
        assert_eq!(
            read.matches("in form").count(),
            MAX_FORM_DEPTH,
            "a form drawing itself is followed only so deep"
        );
    }

    /// An image may be written into the content, and its bytes may look like anything.
    #[test]
    fn an_image_written_into_the_content_is_not_read_as_words() {
        let content =
            b"BI /W 9 /H 1 /CS /G /BPC 8 ID (fake) Tj\nEI BT /F1 12 Tf 72 720 Td (after) Tj ET\n"
                .to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "after\n");
    }

    /// A range of a `ToUnicode` table may cover every code there is.
    #[test]
    fn a_table_of_characters_keeps_no_more_codes_than_there_is_room_for() {
        let cmap = b"begincmap 1 beginbfrange\n<00000000> <FFFFFFFF> <0041>\nendbfrange \
                     1 beginbfrange\n<FFFFFFF0> <FFFFFFFF> [<0041> <0042>]\nendbfrange endcmap"
            .to_vec();
        assert!(read_cmap(&cmap).len() <= MAX_CMAP);
    }

    fn zlib(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    /// A compressed stream that holds nothing is read as nothing, not as damage; one damaged
    /// only at its end keeps what came before the damage, and says that some is missing.
    #[test]
    fn an_empty_compressed_stream_is_read_as_nothing_and_a_damaged_one_keeps_its_start() {
        let page = "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> >> \
                    /Contents [4 0 R 6 0 R] >>";
        let mut objects = one_page(
            stream("", b"BT /F1 12 Tf 72 720 Td (readable) Tj ET\n"),
            HELVETICA,
        );
        objects[2] = page.as_bytes().to_vec();
        objects.push(stream("/Filter /FlateDecode", &zlib(b"")));
        assert_eq!(text(&build(&objects, "")).unwrap(), "readable\n");

        let mut packed = zlib(b"BT /F1 12 Tf 72 720 Td (kept) Tj ET\n");
        let last = packed.len() - 1;
        packed[last] ^= 0xff;
        let damaged = one_page(stream("/Filter /FlateDecode", &packed), HELVETICA);
        let read = text(&build(&damaged, "")).unwrap();
        assert!(read.starts_with("kept\n"), "{read}");
        assert!(read.contains(&incomplete_note()), "{read}");

        // A stream cut short of its checksum still holds all of its bytes.
        let mut packed = zlib(b"BT /F1 12 Tf 72 720 Td (all there) Tj ET\n");
        packed.truncate(packed.len() - 4);
        let cut = one_page(stream("/Filter /FlateDecode", &packed), HELVETICA);
        let read = text(&build(&cut, "")).unwrap();
        assert!(read.starts_with("all there\n"), "{read}");
    }

    /// A predictor row the parameters make far wider than the stream is read no wider than
    /// the stream, the same bytes as before.
    #[test]
    fn a_predictor_row_wider_than_its_stream_is_read_as_wide_as_the_stream() {
        let objects = [b"<< /Columns 20000000 /Predictor 12 >>".to_vec()];
        let file = build(&objects, "");
        let doc = Document::read(&file, None).unwrap();
        let Object::Dict(parms) = doc.object(1) else {
            panic!("no parameters")
        };
        // Up from a zero row, then the bytes as they are.
        let body = vec![2u8, 5, 6, 7];
        let out = undo_predictor(body, &parms, &doc);
        assert_eq!(out.len(), 3);
        assert_eq!(out, [5, 6, 7]);
    }

    /// pdfLaTeX names its ligatures and accented letters in `Differences`, without a
    /// `ToUnicode` table; a code no name or encoding gives a character for is not read as a
    /// control character.
    #[test]
    fn ligatures_and_other_glyph_names_a_font_lists_are_read_as_their_letters() {
        let font = "<< /Type /Font /Subtype /Type1 /BaseFont /CMR10 /Encoding \
                    << /Type /Encoding /Differences [11 /ff /ffi /ffl 16 /dotlessi \
                    /uni00660069 /eacute /germandbls /ellipsis /minus /f_l /a.sc /u1F600] >> >>";
        let content =
            b"BT /F1 12 Tf 72 720 Td (e\\013ect \\014x \\015y \\020\\021\\022\\023\\024\\025\\026\\027\\030) Tj ET\n"
                .to_vec();
        let file = build(&one_page(stream("", &content), font), "");
        assert_eq!(
            text(&file).unwrap(),
            "effect ffix ffly \u{131}fi\u{e9}\u{df}\u{2026}\u{2212}fla\u{1f600}\n"
        );

        assert_eq!(LATIN1_NAMES.split_whitespace().count(), 96);
        assert_eq!(glyph_char("ydieresis"), Some('\u{ff}'));

        let content = b"BT /F1 12 Tf 72 720 Td (a\\013b) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "a\u{fffd}b\n");
    }

    /// The font and its size are part of the graphics state, which `Q` restores.
    #[test]
    fn the_font_chosen_inside_a_saved_graphics_state_is_put_back_after_it() {
        let page = "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> \
                    /Contents 4 0 R >>";
        let content = b"BT /F1 12 Tf ET q BT /F2 12 Tf 72 700 Td (A) Tj ET Q \
                        BT 72 600 Td (A) Tj ET\n"
            .to_vec();
        let mut objects = one_page(stream("", &content), HELVETICA);
        objects[2] = page.as_bytes().to_vec();
        objects.push(
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Custom /Encoding \
              << /Type /Encoding /Differences [65 /bullet] >> >>"
                .to_vec(),
        );
        assert_eq!(text(&build(&objects, "")).unwrap(), "\u{2022}\nA\n");
    }

    /// Forms past the most a file may draw are said to be missing.
    #[test]
    fn forms_drawn_past_the_most_followed_are_reported() {
        let page = "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 5 0 R >> \
                    /XObject << /Fm1 6 0 R >> >> /Contents 4 0 R >>";
        let mut content = b"BT /F1 12 Tf 72 720 Td (page) Tj ET\n".to_vec();
        content.extend_from_slice(&b"/Fm1 Do\n".repeat(MAX_FORMS + 5));
        let mut objects = one_page(stream("", &content), HELVETICA);
        objects[2] = page.as_bytes().to_vec();
        objects.push(stream(
            "/Type /XObject /Subtype /Form /Resources << /Font << /F1 5 0 R >> >>",
            b"BT /F1 12 Tf (x) Tj ET\n",
        ));
        let read = text(&build(&objects, "")).unwrap();
        assert!(
            read.contains(&incomplete_note()),
            "{}",
            &read[read.len() - 200..]
        );
    }

    /// A compressed stream that runs out before its end keeps what it has, and says that some
    /// is missing; a tab in a string stays the space it is.
    #[test]
    fn a_compressed_stream_cut_short_in_its_data_is_said_to_be_incomplete() {
        let mut content = b"BT /F1 12 Tf 72 720 Td 14 TL\n".to_vec();
        for i in 0..400 {
            content.extend_from_slice(format!("(line {i} {}) Tj T*\n", i * 7919 % 1000).as_bytes());
        }
        content.extend_from_slice(b"ET\n");
        let mut packed = zlib(&content);
        packed.truncate(packed.len() / 2);
        let cut = one_page(stream("/Filter /FlateDecode", &packed), HELVETICA);
        let read = text(&build(&cut, "")).unwrap();
        assert!(read.starts_with("line 0 "), "{read}");
        assert!(!read.contains("line 399 "), "{read}");
        assert!(read.contains(&incomplete_note()), "{read}");

        let tabbed = b"BT /F1 12 Tf 72 720 Td (a\\tb) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &tabbed), HELVETICA), "");
        assert_eq!(text(&file).unwrap(), "a\tb\n");
    }

    /// MacRoman has 128 characters above ASCII; 0xCA is a no-break space and 0xF0 the Apple
    /// logo, which Unicode keeps in its private area.
    #[test]
    fn mac_roman_gives_the_characters_its_table_names() {
        assert_eq!(mac_roman(0xCA), '\u{a0}');
        assert_eq!(mac_roman(0xF0), '\u{f8ff}');
        assert_eq!(mac_roman(0xFF), '\u{2c7}');
        assert_eq!(mac_roman(0x80), '\u{c4}');
        // 0xFF is the last of exactly 128.
        assert_eq!(mac_roman(0x100), '\u{fffd}');
    }

    /// Reading stops once it is no longer wanted.
    #[test]
    fn a_cancelled_read_stops() {
        let content = b"BT /F1 12 Tf 72 720 Td (words) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        let cancel = AtomicBool::new(true);
        assert!(super::text(&file, Some(&cancel)).is_err());
    }
}
