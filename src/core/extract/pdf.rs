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

    /// A stream's bytes with its filters undone.
    fn decode(&self, dict: &HashMap<String, Object>, start: usize, end: usize) -> Result<Vec<u8>> {
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
        const INHERITED: [&str; 3] = ["Resources", "MediaBox", "Rotate"];
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

    /// The text of the whole document, a line to a line of the page, with the byte each page's
    /// text starts at.
    pub fn text_pages(&self) -> Result<(String, Vec<usize>)> {
        let pages = self.pages();
        if pages.is_empty() {
            bail!("the file lists no pages");
        }
        let mut out = String::new();
        let mut starts = Vec::with_capacity(pages.len());
        for (i, page) in pages.iter().enumerate() {
            self.check()?;
            if i > 0 {
                out.push('\n');
            }
            starts.push(out.len());
            let resources = self.get(page, "Resources");
            let content = self.page_content(page);
            out.push_str(&self.page_text(&content, &resources, 0));
            if out.len() > super::MAX_OUTPUT {
                break;
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
    fn form_text(&self, resources: &Object, name: &str, depth: usize) -> String {
        if depth >= MAX_FORM_DEPTH || self.forms.get() >= MAX_FORMS {
            return String::new();
        }
        self.forms.set(self.forms.get() + 1);
        let Some(xobjects) = resources.as_dict().map(|r| self.get(r, "XObject")) else {
            return String::new();
        };
        let Some(Object::Stream(dict, start, end)) = xobjects.as_dict().map(|x| self.get(x, name))
        else {
            return String::new();
        };
        // An image is drawn the same way, and has no text.
        if self.get(&dict, "Subtype").as_name() != Some("Form") {
            return String::new();
        }
        let body = match self.decode(&dict, start, end) {
            Ok(body) => body,
            Err(e) => {
                self.unreadable(|| e.to_string());
                return String::new();
            }
        };
        let own = self.get(&dict, "Resources");
        let resources = if own.as_dict().is_some() {
            &own
        } else {
            resources
        };
        self.page_text(&body, resources, depth + 1)
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

    /// Reads a page's or a form's content, keeping what it draws as text; `depth` is how many
    /// forms it is inside.
    fn page_text(&self, content: &[u8], resources: &Object, depth: usize) -> String {
        let fonts = self.fonts(resources);
        let mut out = String::new();
        let mut ops = Operations::new(content);
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
        while let Some(operator) = ops.next() {
            let operands = &ops.operands;
            match operator.as_str() {
                "Tf" => {
                    if let Some(Object::Name(name)) = operands.iter().rev().nth(1) {
                        font = fonts.get(name).map(Rc::as_ref);
                    }
                }
                "Do" => {
                    if let Some(Object::Name(name)) = operands.last() {
                        flush(&mut line, &mut out);
                        out.push_str(&self.form_text(resources, name, depth));
                    }
                }
                "Tj" | "'" | "\"" | "TJ" => {
                    if operator == "'" || operator == "\"" {
                        flush(&mut line, &mut out);
                    }
                    match font.unwrap_or(&default).show(operands.last()) {
                        Some(shown) => line.push_str(&shown.text),
                        None => self.no_characters(),
                    }
                }
                // A new position or text matrix starts a line.
                "Td" | "TD" | "T*" | "ET" | "Tm" => flush(&mut line, &mut out),
                _ => {}
            }
            if out.len() > super::MAX_OUTPUT {
                break;
            }
        }
        flush(&mut line, &mut out);
        out
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
                continue;
            }
            return Some(operator);
        }
    }
}

/// What one text-showing operation draws.
struct Shown {
    text: String,
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
    differences: HashMap<u32, char>,
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
    /// its codes are glyph numbers, which are not characters.
    fn show(&self, operand: Option<&Object>) -> Option<Shown> {
        if self.unmapped {
            return None;
        }
        let mut shown = Shown {
            text: String::new(),
            units: 0.0,
            glyphs: 0,
            spaces: 0,
        };
        let add = |bytes: &[u8], shown: &mut Shown| {
            shown.text.push_str(&self.decode(bytes));
            for code in self.codes(bytes) {
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
                            if *n < -180.0 && !shown.text.ends_with(' ') {
                                shown.text.push(' ');
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        Some(shown)
    }

    fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::new();
        for code in self.codes(bytes) {
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

/// The text of the PDF file `bytes`, given up on once `cancel` is set.
#[cfg(test)]
pub fn text(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<String> {
    Ok(Document::read(bytes, cancel)?.text_pages()?.0)
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
}

impl Marking<'_> {
    /// Keeps `run` in the marked piece being drawn, joined to the run before it when it goes
    /// on along the same line in the same format.
    fn add(&mut self, mut run: Run) {
        let added = run.text.len();
        if run.text.is_empty() || self.size + added + RUN_COST > super::MAX_OUTPUT {
            return;
        }
        let Some(piece) = self.current() else { return };
        match piece.runs.last_mut() {
            Some(r)
                if r.bold == run.bold
                    && r.italic == run.italic
                    && r.page == run.page
                    && (r.y - run.y).abs() < 1.0 =>
            {
                if gap_is_space(run.x - r.end, r.size.max(run.size))
                    && !r.text.ends_with(char::is_whitespace)
                    && !run.text.starts_with(char::is_whitespace)
                {
                    r.text.push(' ');
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
        self.size += added;
    }

    /// The marked piece being drawn, if it is part of the content.
    fn current(&mut self) -> Option<&mut Marked> {
        let id = self.marks.iter().rev().find_map(|m| match m {
            Mark::Id(n) => Some(Some(*n)),
            Mark::Artifact => Some(None),
            Mark::Other => None,
        })??;
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
            };
            self.read_marked(&content, &resources, IDENTITY, 0, &mut marking);
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
    ) {
        let fonts = self.fonts(resources);
        let properties = resources
            .as_dict()
            .map(|r| self.get(r, "Properties"))
            .unwrap_or(Object::Null);
        let mut ops = Operations::new(content);
        let mut font: Option<&Font> = None;
        let default = Font::default();
        // The placement, with the states saved by `q`; a `q` past the most kept is counted,
        // so that its `Q` restores nothing rather than a state saved before it.
        let (mut ctm, mut saved, mut unsaved) = (ctm, Vec::new(), 0usize);
        let mut text = TextState::default();
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
                "q" if saved.len() < 64 => saved.push(ctm),
                "q" => unsaved += 1,
                "Q" if unsaved > 0 => unsaved -= 1,
                "Q" => ctm = saved.pop().unwrap_or(ctm),
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
                    let Some(shown) = shown_in.show(operands.last()) else {
                        self.no_characters();
                        continue;
                    };
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
                "Do" => {
                    if let Some(Object::Name(name)) = operands.last() {
                        let name = name.clone();
                        self.draw_marked(resources, &name, ctm, depth, marking);
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
                self.read_marked(&body, resources, then(matrix, ctm), depth + 1, marking);
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

/// The structure tree of the tagged PDF file `bytes`, with the text drawn for each element.
/// A file without one is refused.
pub(super) fn structure(bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<Element> {
    let doc = Document::read(bytes, cancel)?;
    let tree = doc.structure()?.context(
        "the PDF file is not tagged, so it says nothing of its headings, lists or tables",
    )?;
    if let Some(why) = doc.unreadable.borrow().as_ref() {
        if tree.kids.is_empty() {
            bail!("{why}");
        }
    }
    Ok(tree)
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
    fn a_file_without_structure_is_refused_for_the_formatted_view() {
        let content = b"BT /F1 12 Tf 72 720 Td (Hello) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        let why = format!("{:#}", blocks(&file).unwrap_err());
        assert!(why.contains("not tagged"), "{why}");
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

    /// Reading stops once it is no longer wanted.
    #[test]
    fn a_cancelled_read_stops() {
        let content = b"BT /F1 12 Tf 72 720 Td (words) Tj ET\n".to_vec();
        let file = build(&one_page(stream("", &content), HELVETICA), "");
        let cancel = AtomicBool::new(true);
        assert!(super::text(&file, Some(&cancel)).is_err());
    }
}
