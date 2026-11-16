//! CSV/TSV helpers for table compare mode.

use std::path::Path;

pub fn delimiter_for(path: &Path) -> Option<char> {
    match super::text::extension(path).as_str() {
        "csv" => Some(','),
        "tsv" | "tab" => Some('\t'),
        _ => None,
    }
}

/// The delimiter of the records `doc` holds, when they are a table. A spreadsheet OxideMerge
/// reads itself is comma separated records, so it is compared as a table too; the records of
/// each of several sheets follow a heading that names the sheet. What a converter the user configured writes is
/// not known, so it is compared as text.
pub fn delimiter_of_doc(
    doc: &super::text::TextDoc,
    opts: &super::diff::DiffOptions,
) -> Option<char> {
    let path = doc.path.as_deref()?;
    if let Some(d) = file_delimiter(doc) {
        return Some(d);
    }
    let read_here = matches!(
        super::unpacker::find(opts, path),
        Some(super::unpacker::Converter::BuiltIn(
            super::extract::Kind::Spreadsheet
        ))
    );
    read_here.then_some(',')
}

/// The delimiter of the CSV or TSV file `doc` holds, read from its name and, for a `.csv`, its
/// first line.
pub fn file_delimiter(doc: &super::text::TextDoc) -> Option<char> {
    match delimiter_for(doc.path.as_deref()?)? {
        ',' => Some(sniff_csv(&doc.lines)),
        d => Some(d),
    }
}

/// How many lines of a `.csv` file are looked at to tell its delimiter.
const SNIFF_LINES: usize = 20;

/// The delimiter of a `.csv` file: what an Excel `sep=` line names, else the one of `,`, `;`,
/// a tab and `|` that splits its first lines into the same number of fields most often, at
/// least two; a comma when none does. A comma in the name of a column of a file separated by
/// semicolons, as a spreadsheet set to a language that writes decimal commas saves it, does
/// not make it a comma separated file.
fn sniff_csv(lines: &[String]) -> char {
    let lines: Vec<&String> = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .take(SNIFF_LINES)
        .collect();
    let Some(first) = lines.first() else {
        return ',';
    };
    // Not trimmed: the delimiter it names can be a tab.
    if let Some(sep) = first.strip_prefix("sep=") {
        if let Some(d) = [',', ';', '\t', '|']
            .into_iter()
            .find(|&d| sep == d.to_string())
        {
            return d;
        }
    }
    // Fewer than all lines may agree: a cell with a line break in it spreads its record over
    // lines that each read as fewer fields.
    let score = |d: char| {
        let fields = split_row(first, d).len();
        let agree = lines
            .iter()
            .filter(|l| split_row(l, d).len() == fields)
            .count();
        (fields >= 2).then_some((agree, fields))
    };
    [',', ';', '\t', '|']
        .into_iter()
        .filter_map(|d| Some((score(d)?, d)))
        // The first of the best, so a comma wins a tie.
        .fold(
            None,
            |best: Option<((usize, usize), char)>, (s, d)| match best {
                Some((b, _)) if b >= s => best,
                _ => Some((s, d)),
            },
        )
        .map_or(',', |(_, d)| d)
}

/// One record of `cells` separated by `delim`, each quoted when it has to be.
pub fn encode_record(cells: &[String], delim: char) -> String {
    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(delim);
        }
        if cell.contains([delim, '"', '\r', '\n']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out
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
    /// A field of the record has a quote in it without starting with one, such as `55" screen`.
    stray_quote: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Table {
    pub source: String,
    pub records: Vec<Record>,
    /// Byte spans of stored fields hidden by merges, in source order.
    covered_fields: Vec<std::ops::Range<usize>>,
    /// What separates the fields of a record.
    pub delim: char,
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
        let mut stray_quote = false;
        // An Excel `sep=;` line names the delimiter; it is not a record of the table, nor its
        // header row.
        let sep = format!("sep={delim}");
        if let Some(rest) = source.strip_prefix(sep.as_str()) {
            let eol = match rest.as_bytes() {
                [] => Some(0),
                [b'\r', b'\n', ..] => Some(2),
                [b'\r' | b'\n', ..] => Some(1),
                _ => None,
            };
            if let Some(eol) = eol {
                i = sep.len() + eol;
                (line, start_line) = (1, 1);
            }
        }
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
                // A quote inside a field that does not start with one is part of its value, as
                // a spreadsheet reads `55" screen` or `a, "b"` (a space before the quote).
                while i < bytes.len() && ![delim as u8, b'\r', b'\n'].contains(&bytes[i]) {
                    let ch = source[i..].chars().next().unwrap();
                    stray_quote |= ch == '"';
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
                stray_quote: std::mem::take(&mut stray_quote),
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
        Ok(Self {
            source,
            records,
            covered_fields: Vec::new(),
            delim,
        })
    }

    /// Applies merge geometry to table values while retaining the original source and spans.
    pub fn with_spreadsheet_merges(mut self, merges: &super::extract::TableMerges) -> Self {
        self.add_merge_rows(merges);
        if merges.is_empty() {
            return self;
        }
        for (index, record) in self.records.iter_mut().enumerate() {
            if record.cells.is_empty() || !merges.covers_row(index) {
                continue;
            }
            let mut cleared = false;
            for (column, cell) in record.cells.iter_mut().enumerate() {
                if !cell.is_empty()
                    && merges
                        .covering((index, index), column)
                        .is_some_and(|merge| {
                            index != merge.first_record() || column != merge.first.1
                        })
                {
                    cell.clear();
                    self.covered_fields.push(record.fields[column].clone());
                    cleared = true;
                }
            }
            if cleared {
                while record.cells.len() > 1 && record.cells.last().is_some_and(String::is_empty) {
                    record.cells.pop();
                }
                if record.cells.len() < record.fields.len() {
                    let tail = record.fields[record.cells.len()].start - 1
                        ..record.fields.last().unwrap().end;
                    let keep = self
                        .covered_fields
                        .partition_point(|field| field.end <= tail.start);
                    self.covered_fields.truncate(keep);
                    self.covered_fields.push(tail);
                }
            }
        }
        self
    }

    /// Blank worksheet rows beyond the last written record have no physical text or fields.
    fn add_merge_rows(&mut self, merges: &super::extract::TableMerges) {
        if merges.padding().is_empty() {
            return;
        }
        let mut written = std::mem::take(&mut self.records).into_iter();
        let mut records = Vec::<Record>::new();
        for range in merges.padding() {
            let before = range.start - records.len();
            if before > written.len() {
                // The extraction can truncate a worksheet before the range starts.
                break;
            }
            records.extend(written.by_ref().take(before));
            let line = records.last().map_or(0, |r| r.lines.end);
            records.resize_with(range.end, || Record {
                lines: line..line,
                cells: Vec::new(),
                fields: Vec::new(),
                stray_quote: false,
            });
        }
        records.extend(written);
        self.records = records;
    }

    pub fn replace_cell(
        &self,
        record: usize,
        column: usize,
        value: &str,
        delim: char,
    ) -> Result<String, String> {
        self.replace_cells(record, &[(column, value)], delim)
    }

    /// The source with several cells of one record replaced at once.
    pub fn replace_cells(
        &self,
        record: usize,
        cells: &[(usize, &str)],
        delim: char,
    ) -> Result<String, String> {
        let record = self.records.get(record).ok_or("Record no longer exists")?;
        // A record shorter than the table is made long enough for the cells set past its end.
        let end = record.fields.last().ok_or("Column no longer exists")?.end;
        let mut added: Vec<&str> = Vec::new();
        for &(column, value) in cells {
            if column >= record.fields.len() {
                let at = column - record.fields.len();
                if added.len() <= at {
                    added.resize(at + 1, "");
                }
                added[at] = value;
            }
        }
        let mut spans = cells
            .iter()
            .filter(|&&(column, _)| column < record.fields.len())
            .map(|&(column, value)| (record.fields[column].clone(), value))
            .collect::<Vec<_>>();
        // Last first, so the spans before stay where they are.
        spans.sort_unstable_by_key(|(span, _)| std::cmp::Reverse(span.start));
        spans.dedup_by_key(|(span, _)| span.start);
        let mut source = self.source.clone();
        if !added.is_empty() {
            let tail: Vec<String> = added.iter().map(|v| v.to_string()).collect();
            let text = format!("{delim}{}", encode_record(&tail, delim));
            source.insert_str(end, &text);
        }
        for (span, value) in spans {
            let quoted = self.source[span.clone()].starts_with('"')
                || value.contains([delim, '"', '\r', '\n']);
            let replacement = if quoted {
                format!("\"{}\"", value.replace('"', "\"\""))
            } else {
                value.to_string()
            };
            source.replace_range(span, &replacement);
        }
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
        if source.delim != self.delim {
            // Raw lines would bring the other delimiter along: the records are written again
            // with this table's, whole, each with this table's line ending, in place of the
            // whole records the lines are part of here.
            let value: String = source
                .records
                .iter()
                .filter(|r| r.lines.start < source_lines.end && r.lines.end > source_lines.start)
                .map(|r| encode_record(&r.cells, self.delim) + eol)
                .collect();
            let lines = self.whole_records(lines);
            return self.splice(span(self, lines), value, eol);
        }
        let destination = span(self, lines);
        let from = span(source, source_lines);
        let first = source
            .covered_fields
            .partition_point(|field| field.end <= from.start);
        let mut value = String::new();
        let mut at = from.start;
        for field in &source.covered_fields[first..] {
            if field.start >= from.end {
                break;
            }
            value.push_str(&source.source[at..field.start.max(at)]);
            at = field.end.min(from.end);
        }
        value.push_str(&source.source[at..from.end]);
        self.splice(destination, value, eol)
    }

    /// The physical lines of the records that `lines` is part of; an empty range inside a
    /// record moves to the end of that record.
    fn whole_records(&self, lines: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let inside = |r: &&Record| {
            if lines.is_empty() {
                r.lines.start < lines.start && lines.start < r.lines.end
            } else {
                r.lines.start < lines.end && lines.start < r.lines.end
            }
        };
        let mut found = self.records.iter().filter(inside).peekable();
        let Some(first) = found.peek().map(|r| r.lines.clone()) else {
            return lines;
        };
        let last = found.last().map_or(first.end, |r| r.lines.end);
        if lines.is_empty() {
            last..last
        } else {
            first.start.min(lines.start)..last.max(lines.end)
        }
    }

    /// The source with the bytes `destination` replaced by `value`, with a line ending added
    /// where `value` would otherwise run into the line before or after it.
    fn splice(&self, destination: std::ops::Range<usize>, mut value: String, eol: &str) -> String {
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

    /// The source text of each record with a stray quote, as [`Record`] says.
    fn stray_quote_records(&self) -> std::collections::HashSet<&str> {
        self.records
            .iter()
            .filter(|r| r.stray_quote)
            .filter_map(|r| Some(&self.source[r.fields.first()?.start..r.fields.last()?.end]))
            .collect()
    }

    /// Whether this table, the result of copying lines of `source` into `before`, reads them
    /// as they were read: a record with a quote inside a field that does not start with one
    /// must have been such a record on one side already. Otherwise the copy took part of a
    /// quoted cell with a line break in it, such as only its closing line `b",x`, which a
    /// lenient reading would take for a value with a quote in it.
    pub fn copy_is_sound(&self, before: &Table, source: &Table) -> bool {
        let known: std::collections::HashSet<&str> = before
            .stray_quote_records()
            .into_iter()
            .chain(source.stray_quote_records())
            .collect();
        self.stray_quote_records().is_subset(&known)
    }

    /// Physical lines occupied by a record range, or its insertion point when empty.
    pub fn record_lines(&self, records: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let start = self.records.get(records.start).map_or_else(
            || self.records.last().map_or(0, |r| r.lines.end),
            |r| r.lines.start,
        );
        let end = if records.is_empty() {
            start
        } else {
            self.records[records.end - 1].lines.end
        };
        start..end
    }

    pub fn keys(&self, ignore_eol: bool) -> Vec<String> {
        self.records
            .iter()
            .map(|r| {
                if r.cells.is_empty() {
                    // A virtual worksheet row compares like a written blank record.
                    return r#"[""]"#.into();
                }
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
    fn a_quote_inside_an_unquoted_field_is_part_of_its_value() {
        let table = Table::parse("item,size\nTV,55\" screen\na, \"b,c\"\n".into(), ',').unwrap();
        assert_eq!(table.records[1].cells, ["TV", "55\" screen"]);
        // A space before the quote: the field is not quoted, so its comma separates fields.
        assert_eq!(table.records[2].cells, ["a", " \"b", "c\""]);
        let edited = table.replace_cell(1, 1, "65\" screen", ',').unwrap();
        assert_eq!(edited, "item,size\nTV,\"65\"\" screen\"\na, \"b,c\"\n");
    }

    #[test]
    fn a_csv_delimiter_is_read_from_its_first_line() {
        let lines = |s: &str| s.lines().map(str::to_string).collect::<Vec<_>>();
        assert_eq!(sniff_csv(&lines("a,b\n1,2")), ',');
        assert_eq!(sniff_csv(&lines("a;b;c\n1,5;2;3")), ';');
        assert_eq!(sniff_csv(&lines("\"x;y\",b\n")), ',');
        assert_eq!(sniff_csv(&lines("a\tb\n")), '\t');
        assert_eq!(sniff_csv(&lines("sep=|\na|b\n")), '|');
        assert_eq!(sniff_csv(&lines("\nonly\n")), ',');
        // A comma in a column name of a file separated by semicolons.
        assert_eq!(sniff_csv(&lines("id;Amount, EUR\n1;5\n2;6")), ';');
        // Decimal commas in the values of a semicolon file.
        assert_eq!(sniff_csv(&lines("a;b;c\n1,5;2;3\n4;5,5;6")), ';');
        // A semicolon in a value of a comma file.
        assert_eq!(sniff_csv(&lines("a,b\nx;y,1\nz,2")), ',');
        assert_eq!(sniff_csv(&lines("a|b|c\n1|2|3")), '|');
        assert_eq!(sniff_csv(&lines("sep=\t\na\tb")), '\t');
    }

    #[test]
    fn an_excel_sep_line_is_not_a_record() {
        let table = Table::parse("sep=;\r\nid;v\n1;2\n".into(), ';').unwrap();
        assert_eq!(table.records.len(), 2);
        assert_eq!(table.records[0].cells, ["id", "v"]);
        assert_eq!(table.records[0].lines, 1..2);
        assert_eq!(
            table.replace_cell(1, 1, "3", ';').unwrap(),
            "sep=;\r\nid;v\n1;3\n"
        );
        // Another delimiter's line is an ordinary record.
        let other = Table::parse("sep=,\nid;v\n".into(), ';').unwrap();
        assert_eq!(other.records[0].cells, ["sep=,"]);
    }

    #[test]
    fn records_copied_between_delimiters_are_written_with_the_destination_s() {
        let tsv = Table::parse("id\tnote\n1\ta,b\n".into(), '\t').unwrap();
        let csv = Table::parse("id,note\n1,x\n".into(), ',').unwrap();
        assert_eq!(
            csv.copy_lines(1..2, &tsv, 1..2, "\n"),
            "id,note\n1,\"a,b\"\n"
        );
        // A line inside a cell with line breaks replaces the whole record it is part of.
        let src = Table::parse("id\tnote\n1\t\"p\nq\nr\"\n".into(), '\t').unwrap();
        let dst = Table::parse("id,note\n1,\"p\nX\nr\"\n2,z\n".into(), ',').unwrap();
        let copied = dst.copy_lines(2..3, &src, 2..3, "\n");
        assert_eq!(copied, "id,note\n1,\"p\nq\nr\"\n2,z\n");
        // An insertion point inside a record moves to its end.
        assert_eq!(
            dst.copy_lines(2..2, &src, 0..1, "\n"),
            "id,note\n1,\"p\nX\nr\"\nid,note\n2,z\n"
        );
        // A record on several lines is copied whole.
        let multi = Table::parse("id\tnote\n1\t\"p\nq\"\n".into(), '\t').unwrap();
        assert_eq!(
            csv.copy_lines(1..2, &multi, 2..3, "\n"),
            "id,note\n1,\"p\nq\"\n"
        );
    }

    #[test]
    fn setting_a_cell_past_the_end_of_a_record_lengthens_it() {
        let table = Table::parse("a,b,c\n1\r\n2,x\n".into(), ',').unwrap();
        assert_eq!(
            table.replace_cell(1, 2, "z", ',').unwrap(),
            "a,b,c\n1,,z\r\n2,x\n"
        );
        assert_eq!(
            table
                .replace_cells(2, &[(1, "y"), (3, "q,r"), (2, "")], ',')
                .unwrap(),
            "a,b,c\n1\r\n2,y,,\"q,r\"\n"
        );
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
        for source in ["\"unterminated", "\"ok\"extra,x"] {
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
