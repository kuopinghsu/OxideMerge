//! CSV/TSV helpers for table compare mode.

use std::path::Path;

pub fn delimiter_for(path: &Path) -> Option<char> {
    match super::text::extension(path).as_str() {
        "csv" => Some(','),
        "tsv" | "tab" => Some('\t'),
        _ => None,
    }
}

/// Splits one line into cells, honoring double quotes.
pub fn split_row(line: &str, delim: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' && cur.is_empty() {
            in_quotes = true;
        } else if c == delim {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    cells.push(cur);
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split() {
        assert_eq!(
            split_row("a,\"b,c\",\"d\"\"e\"", ','),
            vec!["a", "b,c", "d\"e"]
        );
    }
}

/// A logical record and its original physical lines/byte spans. Raw source is retained so
/// changing a cell never rewrites quoting or terminators in unrelated fields.
#[derive(Clone, Debug)]
pub struct Record {
    pub lines: std::ops::Range<usize>,
    pub cells: Vec<String>,
    fields: Vec<std::ops::Range<usize>>,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub source: String,
    pub records: Vec<Record>,
}

/// Reconstructs the actual terminators, even when ordinary text editing normalizes EOLs.
pub fn source(doc: &super::text::TextDoc) -> String {
    let mut source = String::new();
    for (i, line) in doc.lines.iter().enumerate() {
        source.push_str(line);
        if let Some(eol) = doc.line_eol(i) {
            source.push_str(eol.as_str());
        }
    }
    source
}

impl Table {
    pub fn parse(source: String, delim: char) -> Result<Self, String> {
        if ![',', '\t', ';', '|'].contains(&delim) {
            return Err("Unsupported delimiter".into());
        }
        let bytes = source.as_bytes();
        let mut records = Vec::new();
        let (mut i, mut line, mut start_line) = (0, 0, 0);
        let (mut fields, mut cells) = (Vec::new(), Vec::new());
        while i < bytes.len() {
            let start = i;
            let quoted = bytes[i] == b'"';
            let mut value = String::new();
            if quoted {
                i += 1;
                let mut closed = false;
                while i < bytes.len() {
                    if bytes[i] == b'"' {
                        i += 1;
                        if bytes.get(i) == Some(&b'"') {
                            value.push('"');
                            i += 1;
                        } else {
                            closed = true;
                            break;
                        }
                    } else {
                        let ch = source[i..].chars().next().unwrap();
                        if ch == '\r' {
                            line += 1;
                            value.push(ch);
                            i += 1;
                            if bytes.get(i) == Some(&b'\n') {
                                value.push('\n');
                                i += 1;
                            }
                        } else {
                            if ch == '\n' {
                                line += 1;
                            }
                            value.push(ch);
                            i += ch.len_utf8();
                        }
                    }
                }
                if !closed {
                    return Err(format!(
                        "Unclosed quoted field in record {}",
                        records.len() + 1
                    ));
                }
                if i < bytes.len() && ![delim as u8, b'\r', b'\n'].contains(&bytes[i]) {
                    return Err(format!(
                        "Unexpected character after quoted field in record {}",
                        records.len() + 1
                    ));
                }
            } else {
                while i < bytes.len() && ![delim as u8, b'\r', b'\n'].contains(&bytes[i]) {
                    let ch = source[i..].chars().next().unwrap();
                    if ch == '"' {
                        return Err(format!("Unexpected quote in record {}", records.len() + 1));
                    }
                    value.push(ch);
                    i += ch.len_utf8();
                }
            }
            fields.push(start..i);
            cells.push(value);
            if bytes.get(i) == Some(&(delim as u8)) {
                i += 1;
                // A delimiter at EOF still introduces a final empty field.
                if i == bytes.len() {
                    fields.push(i..i);
                    cells.push(String::new());
                } else {
                    continue;
                }
            }
            records.push(Record {
                lines: start_line..line + 1,
                cells: std::mem::take(&mut cells),
                fields: std::mem::take(&mut fields),
            });
            if bytes.get(i) == Some(&b'\r') {
                i += 1;
                if bytes.get(i) == Some(&b'\n') {
                    i += 1;
                }
            } else if bytes.get(i) == Some(&b'\n') {
                i += 1;
            }
            line += 1;
            start_line = line;
        }
        Ok(Self { source, records })
    }

    pub fn replace_cell(
        &self,
        record: usize,
        column: usize,
        value: &str,
        delim: char,
    ) -> Result<String, String> {
        let record = self.records.get(record).ok_or("Record no longer exists")?;
        let span = record.fields.get(column).ok_or("Column no longer exists")?;
        let quoted =
            self.source[span.clone()].starts_with('"') || value.contains([delim, '"', '\r', '\n']);
        let replacement = if quoted {
            format!("\"{}\"", value.replace('"', "\"\""))
        } else {
            value.to_string()
        };
        let mut source = self.source.clone();
        source.replace_range(span.clone(), &replacement);
        Ok(source)
    }

    /// Copy only the selected physical lines. The caller validates the resulting table
    /// before committing it; unrelated fields and other differences stay untouched.
    pub fn copy_lines(
        &self,
        lines: std::ops::Range<usize>,
        source: &Table,
        source_lines: std::ops::Range<usize>,
        eol: &str,
    ) -> String {
        let span = |table: &Table, range: std::ops::Range<usize>| {
            let mut offsets = vec![0];
            let mut offset = 0;
            for (line, ending) in super::text::lines(&table.source) {
                offset += line.len() + ending.map_or(0, |e| e.as_str().len());
                offsets.push(offset);
            }
            offsets
                .get(range.start)
                .copied()
                .unwrap_or(table.source.len())
                ..offsets
                    .get(range.end)
                    .copied()
                    .unwrap_or(table.source.len())
        };
        let destination = span(self, lines);
        let mut value = source.source[span(source, source_lines)].to_string();
        if !value.is_empty() {
            if destination.end < self.source.len() && !value.ends_with(['\r', '\n']) {
                value.push_str(eol);
            }
            if destination.start > 0 && !self.source[..destination.start].ends_with(['\r', '\n']) {
                value.insert_str(0, eol);
            }
        }
        let mut result = self.source.clone();
        result.replace_range(destination, &value);
        result
    }

    pub fn keys(&self, ignore_eol: bool) -> Vec<String> {
        self.records
            .iter()
            .map(|r| {
                let cells: Vec<String> = r
                    .cells
                    .iter()
                    .map(|c| {
                        if ignore_eol {
                            c.replace("\r\n", "\n").replace('\r', "\n")
                        } else {
                            c.clone()
                        }
                    })
                    .collect();
                serde_json::to_string(&cells).unwrap()
            })
            .collect()
    }
}

#[cfg(test)]
mod record_tests {
    use super::*;
    #[test]
    fn block_merges_copy_selected_lines_and_insert_separators() {
        let a = Table::parse("id,note\n1,\"a\nb\nc\"\n2,z\n".into(), ',').unwrap();
        let b = Table::parse("id,note\n1,\"a\nc\"\n2,z\n".into(), ',').unwrap();
        assert_eq!(b.copy_lines(2..2, &a, 2..3, "\n"), a.source);
        let a = Table::parse("b\n".into(), ',').unwrap();
        let b = Table::parse("a".into(), ',').unwrap();
        assert_eq!(b.copy_lines(1..1, &a, 0..1, "\n"), "a\nb\n");
        let a = Table::parse("a\n".into(), ',').unwrap();
        let b = Table::parse("a\nb\n".into(), ',').unwrap();
        assert_eq!(b.copy_lines(1..2, &a, 1..1, "\n"), "a\n");
    }

    #[test]
    fn edits_multiline_unicode_and_preserves_other_fields_and_endings() {
        let source = "id,note,other\r\n1,\"hello\r\n世界\",\"unchanged\"\n2,last,\r\n";
        let table = Table::parse(source.into(), ',').unwrap();
        assert_eq!(table.records.len(), 3);
        assert_eq!(table.records[1].lines, 1..3);
        assert_eq!(table.records[1].cells[1], "hello\r\n世界");
        let updated = table.replace_cell(1, 1, "a,\"b\"\nnext", ',').unwrap();
        assert_eq!(
            updated,
            "id,note,other\r\n1,\"a,\"\"b\"\"\nnext\",\"unchanged\"\n2,last,\r\n"
        );
        assert_eq!(
            Table::parse(updated, ',').unwrap().records[1].cells[1],
            "a,\"b\"\nnext"
        );
    }
    #[test]
    fn empty_fields_eof_and_malformed_records() {
        for (source, expected) in [
            ("", 0),
            ("\n", 1),
            ("a,", 1),
            ("a,\n", 1),
            ("\"a\n\nb\",x", 1),
        ] {
            assert_eq!(
                Table::parse(source.into(), ',').unwrap().records.len(),
                expected
            );
        }
        assert_eq!(
            Table::parse("a,".into(), ',').unwrap().records[0].cells,
            ["a", ""]
        );
        for source in ["\"unterminated", "\"ok\"extra,x", "a\"b,x"] {
            assert!(Table::parse(source.into(), ',').is_err());
        }
        assert_eq!(
            Table::parse("a\t\"b\tc\"\r\n".into(), '\t')
                .unwrap()
                .records[0]
                .cells,
            ["a", "b\tc"]
        );
    }
}
