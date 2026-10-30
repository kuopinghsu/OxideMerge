//! Text file loading/saving with encoding and end-of-line detection.

use anyhow::{Context, Result};
use encoding_rs::Encoding;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    CrLf,
    Lf,
    Cr,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::CrLf => "\r\n",
            Eol::Lf => "\n",
            Eol::Cr => "\r",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Eol::CrLf => "CRLF",
            Eol::Lf => "LF",
            Eol::Cr => "CR",
        }
    }

    pub fn platform() -> Eol {
        if cfg!(windows) {
            Eol::CrLf
        } else {
            Eol::Lf
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextEncoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Other(&'static Encoding),
}

/// Encodings offered in the "reload with encoding" menus.
pub const COMMON_ENCODINGS: &[&str] = &[
    "UTF-8",
    "UTF-16LE",
    "UTF-16BE",
    "windows-1252",
    "ISO-8859-1",
    "ISO-8859-2",
    "windows-1250",
    "windows-1251",
    "KOI8-R",
    "Big5",
    "GBK",
    "gb18030",
    "Shift_JIS",
    "EUC-JP",
    "ISO-2022-JP",
    "EUC-KR",
    "windows-874",
    "windows-1253",
    "windows-1254",
    "windows-1255",
    "windows-1256",
    "windows-1257",
    "windows-1258",
];

impl TextEncoding {
    pub fn name(self) -> String {
        match self {
            TextEncoding::Utf8 => "UTF-8".into(),
            TextEncoding::Utf16Le => "UTF-16LE".into(),
            TextEncoding::Utf16Be => "UTF-16BE".into(),
            TextEncoding::Other(e) => e.name().to_string(),
        }
    }

    pub fn from_label(label: &str) -> Option<TextEncoding> {
        match label.to_ascii_uppercase().as_str() {
            "UTF-8" | "UTF8" => Some(TextEncoding::Utf8),
            "UTF-16LE" | "UTF-16" | "UCS-2" => Some(TextEncoding::Utf16Le),
            "UTF-16BE" => Some(TextEncoding::Utf16Be),
            _ => Encoding::for_label(label.as_bytes()).map(|e| {
                if e == encoding_rs::UTF_8 {
                    TextEncoding::Utf8
                } else if e == encoding_rs::UTF_16LE {
                    TextEncoding::Utf16Le
                } else if e == encoding_rs::UTF_16BE {
                    TextEncoding::Utf16Be
                } else {
                    TextEncoding::Other(e)
                }
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TextDoc {
    pub path: Option<PathBuf>,
    pub lines: Vec<String>,
    pub encoding: TextEncoding,
    pub bom: bool,
    pub eol: Eol,
    pub mixed_eol: bool,
    /// Patch results retain their exact terminators until the user chooses another EOL style.
    pub preserve_eols: bool,
    /// Original terminators of a mixed-EOL document, retained for comparison until normalized.
    pub(crate) original_eols: Vec<Option<Eol>>,
    pub final_newline: bool,
    pub modified: bool,
    pub read_only: bool,
    pub description: Option<String>,
    /// File extension used to pick syntax highlighting when there is no path.
    pub syntax: Option<String>,
}

impl TextDoc {
    pub fn empty(path: Option<PathBuf>) -> Self {
        TextDoc {
            path,
            lines: Vec::new(),
            encoding: TextEncoding::Utf8,
            bom: false,
            eol: Eol::platform(),
            mixed_eol: false,
            preserve_eols: false,
            original_eols: Vec::new(),
            final_newline: true,
            modified: false,
            read_only: false,
            description: None,
            syntax: None,
        }
    }

    pub fn from_text(path: Option<PathBuf>, text: &str) -> Self {
        let mut doc = TextDoc::empty(path);
        let (lines, eol, mixed, nl) = split_lines(text);
        doc.lines = lines;
        if let Some(eol) = eol {
            doc.eol = eol;
        }
        doc.mixed_eol = mixed;
        if mixed {
            doc.original_eols = self::lines(text).map(|(_, eol)| eol).collect();
        }
        doc.final_newline = nl || doc.lines.is_empty();
        doc
    }

    /// Terminator of a displayed line; editing or choosing an EOL style can normalize it.
    pub fn line_eol(&self, line: usize) -> Option<Eol> {
        if line + 1 == self.lines.len() && !self.final_newline {
            None
        } else if self.mixed_eol && self.original_eols.len() == self.lines.len() {
            self.original_eols[line].or(Some(self.eol))
        } else {
            Some(self.eol)
        }
    }

    pub fn title(&self) -> String {
        if let Some(d) = &self.description {
            return d.clone();
        }
        match &self.path {
            Some(p) => p.display().to_string(),
            None => String::from("Untitled"),
        }
    }

    pub fn text(&self) -> String {
        if !self.preserve_eols {
            return join_lines(&self.lines, self.eol, self.final_newline);
        }
        let mut text = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            text.push_str(line);
            if let Some(eol) = self.line_eol(i) {
                text.push_str(eol.as_str());
            }
        }
        text
    }

    pub fn save(&mut self) -> Result<()> {
        let path = self.path.clone().context("no file name")?;
        self.save_as(&path)
    }

    pub fn save_as(&mut self, path: &Path) -> Result<()> {
        self.write(path, super::fsutil::backup_on())
    }

    /// [`Self::save`], keeping the file it replaces as `<name>.bak` with `backup`.
    pub fn save_with(&mut self, backup: bool) -> Result<()> {
        let path = self.path.clone().context("no file name")?;
        self.write(&path, backup)
    }

    fn write(&mut self, path: &Path, backup: bool) -> Result<()> {
        let text = self.text();
        // encoding_rs writes characters a legacy encoding can't hold as HTML entities.
        if let TextEncoding::Other(e) = self.encoding {
            if e.encode(&text).2 {
                anyhow::bail!(crate::i18n::trf(
                    "msg.unmappable",
                    &[
                        ("file", path.display().to_string()),
                        ("encoding", e.name().to_string()),
                    ],
                ));
            }
        }
        let bytes = encode(&text, self.encoding, self.bom);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        super::fsutil::write_atomic_with(path, &bytes, backup)?;
        self.path = Some(path.to_path_buf());
        self.modified = false;
        if !self.preserve_eols {
            self.mixed_eol = false;
        }
        Ok(())
    }
}

pub fn join_lines(lines: &[String], eol: Eol, final_newline: bool) -> String {
    let mut s = lines.join(eol.as_str());
    if final_newline && !lines.is_empty() {
        s.push_str(eol.as_str());
    }
    s
}

/// Heuristic binary detection (NUL bytes in the first 8 KiB, unless UTF-16 with BOM).
pub fn looks_binary(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return false;
    }
    let n = bytes.len().min(8192);
    bytes[..n].contains(&0)
}

pub fn decode(bytes: &[u8], forced: Option<TextEncoding>) -> (String, TextEncoding, bool) {
    let (text, enc, bom) = decode_cow(bytes, forced);
    (text.into_owned(), enc, bom)
}

/// Like `decode`, but borrows `bytes` when they are already valid UTF-8.
pub fn decode_cow(
    bytes: &[u8],
    forced: Option<TextEncoding>,
) -> (Cow<'_, str>, TextEncoding, bool) {
    let (bom_enc, bom_len) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (Some(TextEncoding::Utf8), 3)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        (Some(TextEncoding::Utf16Le), 2)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        (Some(TextEncoding::Utf16Be), 2)
    } else {
        (None, 0)
    };
    let enc = match (forced, bom_enc) {
        (Some(f), _) => f,
        (None, Some(b)) => b,
        (None, None) => {
            if std::str::from_utf8(bytes).is_ok() {
                TextEncoding::Utf8
            } else {
                let mut det = chardetng::EncodingDetector::new();
                det.feed(bytes, true);
                let e = det.guess(None, true);
                TextEncoding::from_label(e.name()).unwrap_or(TextEncoding::Other(e))
            }
        }
    };
    let has_bom = bom_enc == Some(enc);
    let body = if has_bom { &bytes[bom_len..] } else { bytes };
    let text = match enc {
        TextEncoding::Utf8 => String::from_utf8_lossy(body),
        TextEncoding::Utf16Le => encoding_rs::UTF_16LE.decode_without_bom_handling(body).0,
        TextEncoding::Utf16Be => encoding_rs::UTF_16BE.decode_without_bom_handling(body).0,
        TextEncoding::Other(e) => e.decode_without_bom_handling(body).0,
    };
    (text, enc, has_bom)
}

pub fn encode(text: &str, enc: TextEncoding, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 4);
    match enc {
        TextEncoding::Utf8 => {
            if bom {
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
            }
            out.extend_from_slice(text.as_bytes());
        }
        TextEncoding::Utf16Le => {
            if bom {
                out.extend_from_slice(&[0xFF, 0xFE]);
            }
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
        }
        TextEncoding::Utf16Be => {
            if bom {
                out.extend_from_slice(&[0xFE, 0xFF]);
            }
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
        }
        TextEncoding::Other(e) => {
            out.extend_from_slice(&e.encode(text).0);
        }
    }
    out
}

/// Iterates the lines of `s` (without terminators) together with their terminator. A final
/// terminator does not start another, empty line.
pub fn lines(s: &str) -> Lines<'_> {
    Lines { rest: s }
}

pub struct Lines<'a> {
    rest: &'a str,
}

impl<'a> Iterator for Lines<'a> {
    type Item = (&'a str, Option<Eol>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.rest.is_empty() {
            return None;
        }
        let bytes = self.rest.as_bytes();
        let Some(i) = memchr::memchr2(b'\n', b'\r', bytes) else {
            return Some((std::mem::take(&mut self.rest), None));
        };
        let (eol, len) = match (bytes[i], bytes.get(i + 1)) {
            (b'\n', _) => (Eol::Lf, 1),
            (_, Some(b'\n')) => (Eol::CrLf, 2),
            _ => (Eol::Cr, 1),
        };
        let line = &self.rest[..i];
        self.rest = &self.rest[i + len..];
        Some((line, Some(eol)))
    }
}

/// Counts the line terminators of a text.
#[derive(Default)]
pub struct EolCounts {
    crlf: usize,
    lf: usize,
    cr: usize,
}

impl EolCounts {
    pub fn of(s: &str) -> EolCounts {
        let mut counts = EolCounts::default();
        for (_, eol) in lines(s) {
            counts.add(eol);
        }
        counts
    }

    pub fn add(&mut self, eol: Option<Eol>) {
        match eol {
            Some(Eol::CrLf) => self.crlf += 1,
            Some(Eol::Lf) => self.lf += 1,
            Some(Eol::Cr) => self.cr += 1,
            None => {}
        }
    }

    pub fn dominant(&self) -> Option<Eol> {
        let (crlf, lf, cr) = (self.crlf, self.lf, self.cr);
        if crlf == 0 && lf == 0 && cr == 0 {
            None
        } else if crlf >= lf && crlf >= cr {
            Some(Eol::CrLf)
        } else if lf >= cr {
            Some(Eol::Lf)
        } else {
            Some(Eol::Cr)
        }
    }

    pub fn mixed(&self) -> bool {
        [self.crlf, self.lf, self.cr]
            .iter()
            .filter(|&&c| c > 0)
            .count()
            > 1
    }
}

/// Splits text into lines (without terminators).
/// Returns (lines, dominant eol, mixed eols, ends with newline).
pub fn split_lines(s: &str) -> (Vec<String>, Option<Eol>, bool, bool) {
    let mut counts = EolCounts::default();
    let mut final_newline = false;
    let lines = lines(s)
        .map(|(line, eol)| {
            counts.add(eol);
            final_newline = eol.is_some();
            line.to_string()
        })
        .collect();
    (lines, counts.dominant(), counts.mixed(), final_newline)
}

pub fn load_doc(path: &Path, forced: Option<TextEncoding>) -> Result<TextDoc> {
    use std::io::{Read, Seek};
    let file = std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let large = file.metadata()?.len() >= 16 * 1024 * 1024;
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, file);
    if large && forced.is_none_or(|enc| enc == TextEncoding::Utf8) {
        if let Some(doc) = read_utf8_lines(&mut reader, Some(path.to_path_buf()))
            .with_context(|| format!("reading {}", path.display()))?
        {
            return Ok(doc);
        }
        // Invalid UTF-8 or another BOM must retain the existing detection/replacement rules.
        reader.rewind()?;
    }
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(doc_from_bytes(Some(path.to_path_buf()), &bytes, forced))
}

/// Read valid UTF-8 incrementally without retaining a second whole-file byte buffer.
/// None asks the caller to rewind and use the full encoding detector instead.
fn read_utf8_lines(
    reader: &mut impl std::io::BufRead,
    path: Option<PathBuf>,
) -> std::io::Result<Option<TextDoc>> {
    let prefix = reader.fill_buf()?;
    if prefix.starts_with(&[0xff, 0xfe]) || prefix.starts_with(&[0xfe, 0xff]) {
        return Ok(None);
    }
    let bom = prefix.starts_with(&[0xef, 0xbb, 0xbf]);
    if bom {
        reader.consume(3);
    }
    let mut doc = TextDoc::empty(path);
    doc.bom = bom;
    let mut counts = EolCounts::default();
    let mut pending = Vec::new();
    let mut after_cr = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            break;
        }
        // A CR at the preceding buffer boundary may be followed by LF.
        if after_cr {
            after_cr = false;
            if chunk[0] == b'\n' {
                *doc.original_eols.last_mut().unwrap() = Some(Eol::CrLf);
                counts.cr -= 1;
                counts.crlf += 1;
                reader.consume(1);
                continue;
            }
        }
        let end = memchr::memchr2(b'\r', b'\n', chunk);
        let len = end.unwrap_or(chunk.len());
        pending.extend_from_slice(&chunk[..len]);
        let consumed = if let Some(end) = end {
            let Ok(line) = String::from_utf8(std::mem::take(&mut pending)) else {
                return Ok(None);
            };
            let eol = if chunk[end] == b'\r' {
                Eol::Cr
            } else {
                Eol::Lf
            };
            after_cr = eol == Eol::Cr;
            counts.add(Some(eol));
            doc.lines.push(line);
            doc.original_eols.push(Some(eol));
            end + 1
        } else {
            len
        };
        reader.consume(consumed);
    }
    if !pending.is_empty() {
        let Ok(line) = String::from_utf8(pending) else {
            return Ok(None);
        };
        doc.lines.push(line);
        doc.original_eols.push(None);
    }
    doc.final_newline = doc.original_eols.last().is_none_or(Option::is_some);
    doc.mixed_eol = counts.mixed();
    if let Some(eol) = counts.dominant() {
        doc.eol = eol;
    }
    if !doc.mixed_eol {
        doc.original_eols = Vec::new();
    }
    Ok(Some(doc))
}

pub fn doc_from_bytes(
    path: Option<PathBuf>,
    bytes: &[u8],
    forced: Option<TextEncoding>,
) -> TextDoc {
    let (text, enc, bom) = decode(bytes, forced);
    let mut doc = TextDoc::from_text(path, &text);
    doc.encoding = enc;
    doc.bom = bom;
    doc
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Text,
    Binary,
    Image,
    Web,
}

pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "tif", "tiff", "webp", "tga", "pnm", "ppm", "pgm",
    "pbm",
];

pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_image_path(path: &Path) -> bool {
    IMAGE_EXTS.contains(&extension(path).as_str())
}

pub fn detect_kind(path: &Path) -> FileKind {
    if is_image_path(path) {
        return FileKind::Image;
    }
    use std::io::Read;
    let mut buf = vec![0u8; 8192];
    match std::fs::File::open(path) {
        Ok(mut f) => {
            let n = f.read(&mut buf).unwrap_or(0);
            if looks_binary(&buf[..n]) {
                FileKind::Binary
            } else {
                FileKind::Text
            }
        }
        Err(_) => FileKind::Text,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn buffered_utf8_matches_the_existing_decoder_at_buffer_boundaries() {
        use std::io::{BufReader, Cursor};
        for bytes in [
            &b""[..],
            &b"\n"[..],
            &b"\r\n"[..],
            &b"a\rb\rc"[..],
            &b"a\r\nb\nc\rd\r\n"[..],
            "繁體中文\r\nemoji 🦀\nend".as_bytes(),
            "\u{feff}繁體\r\n中文\r\n".as_bytes(),
        ] {
            for capacity in [4, 7, 64] {
                let mut reader = BufReader::with_capacity(capacity, Cursor::new(bytes));
                let doc = super::read_utf8_lines(&mut reader, None).unwrap().unwrap();
                let expected = super::doc_from_bytes(None, bytes, None);
                assert_eq!(doc.lines, expected.lines);
                assert_eq!(doc.encoding, expected.encoding);
                assert_eq!(doc.bom, expected.bom);
                assert_eq!(doc.eol, expected.eol);
                assert_eq!(doc.mixed_eol, expected.mixed_eol);
                assert_eq!(doc.final_newline, expected.final_newline);
                assert_eq!(doc.original_eols, expected.original_eols);
            }
        }
        for bytes in [&b"valid\ninvalid \xff"[..], &b"\xff\xfea\0"[..]] {
            let mut reader = BufReader::new(Cursor::new(bytes));
            assert!(super::read_utf8_lines(&mut reader, None).unwrap().is_none());
        }
    }

    #[test]
    fn large_file_encoding_fallback_rewinds_after_late_invalid_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        let mut bytes = b"plain ascii line\r\n".repeat(1_000_000);
        bytes.extend_from_slice(b"legacy \xe9\r\n");
        std::fs::write(&path, &bytes).unwrap();
        for forced in [None, Some(super::TextEncoding::Utf8)] {
            let actual = super::load_doc(&path, forced).unwrap();
            let expected = super::doc_from_bytes(None, &bytes, forced);
            assert_eq!(actual.lines, expected.lines);
            assert_eq!(actual.encoding, expected.encoding);
            assert_eq!(actual.final_newline, expected.final_newline);
        }
    }

    use super::*;

    #[test]
    fn split_and_join_roundtrip() {
        let s = "a\r\nb\r\nc";
        let (lines, eol, mixed, nl) = split_lines(s);
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert_eq!(eol, Some(Eol::CrLf));
        assert!(!mixed);
        assert!(!nl);
        assert_eq!(join_lines(&lines, Eol::CrLf, nl), s);
    }

    #[test]
    fn lines_keep_their_terminators() {
        let v: Vec<_> = lines("a\r\nb\rc\n\nd").collect();
        assert_eq!(
            v,
            vec![
                ("a", Some(Eol::CrLf)),
                ("b", Some(Eol::Cr)),
                ("c", Some(Eol::Lf)),
                ("", Some(Eol::Lf)),
                ("d", None),
            ]
        );
        assert_eq!(lines("x\r").collect::<Vec<_>>(), vec![("x", Some(Eol::Cr))]);
        let (l, eol, mixed, nl) = split_lines("a\nb\n");
        assert_eq!(
            (l, eol, mixed, nl),
            (vec!["a".into(), "b".into()], Some(Eol::Lf), false, true)
        );
    }

    #[test]
    fn utf16_roundtrip() {
        let bytes = encode("héllo\n", TextEncoding::Utf16Le, true);
        let (text, enc, bom) = decode(&bytes, None);
        assert_eq!(text, "héllo\n");
        assert_eq!(enc, TextEncoding::Utf16Le);
        assert!(bom);
    }

    #[test]
    fn refuses_to_save_characters_the_encoding_cannot_hold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("latin1.txt");
        let mut doc = TextDoc::from_text(Some(path.clone()), "café\n");
        doc.encoding = TextEncoding::Other(encoding_rs::WINDOWS_1252);
        doc.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"caf\xe9\n");
        doc.lines = vec!["→ 日本".into()];
        doc.modified = true;
        assert!(doc.save().is_err());
        assert!(doc.modified);
        assert_eq!(std::fs::read(&path).unwrap(), b"caf\xe9\n");
    }
}
