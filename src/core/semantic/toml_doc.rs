use super::{span, Node};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use toml::de::{DeTable, DeValue};

/// A date as year, month and day.
type Date = (i64, u8, u8);
/// A time as minutes of the day, second and nanosecond.
type Time = (i64, u8, u32);

/// Values in the shape of JSON: tables are objects, arrays arrays, and member names `["key"]`,
/// so semantic paths, array keys and impact rules work the same. Numbers are compared by
/// value, so `1_000` equals `1000`, `0x10` equals `16` and `1` equals `1.0`.
pub fn parse(source: &str) -> Result<Node> {
    if source.len() > super::MAX_CONFIG_BYTES {
        bail!("TOML input exceeds 4 MiB");
    }
    let root = DeTable::parse(source).map_err(|e| match e.span() {
        Some(at) => {
            anyhow::anyhow!("TOML line {}: {}", line_of(source, at.start), e.message())
        }
        None => anyhow::anyhow!("TOML: {}", e.message()),
    })?;
    let range = root.span();
    let mut budget = 100_000;
    table(source, root.get_ref(), range, 0, &mut budget)
}

fn line_of(source: &str, at: usize) -> usize {
    let before = &source.as_bytes()[..at.min(source.len())];
    before.iter().filter(|&&b| b == b'\n').count() + 1
}

fn table(
    source: &str,
    members: &DeTable,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    let mut n = start(source, range, depth, budget)?;
    n.kind = "object".into();
    for (key, value) in members {
        n.children.insert(
            format!("[{}]", serde_json::to_string(key.get_ref().as_ref())?),
            node(source, value.get_ref(), value.span(), depth + 1, budget)?,
        );
    }
    // A `[table]` is placed at its header and a dotted table at its key: it runs on to its
    // last member.
    if let Some(end) = n.children.values().map(|c| c.span.bytes.end).max() {
        n.span.bytes.end = n.span.bytes.end.max(end);
    }
    Ok(n)
}

fn start(
    source: &str,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    if depth > 64 || *budget == 0 {
        bail!("TOML nesting/node limit exceeded");
    }
    *budget -= 1;
    let end = range.end.min(source.len());
    Ok(Node {
        kind: String::new(),
        value: None,
        children: BTreeMap::new(),
        span: span(source, range.start.min(end), end),
    })
}

fn node(
    source: &str,
    value: &DeValue,
    range: std::ops::Range<usize>,
    depth: usize,
    budget: &mut usize,
) -> Result<Node> {
    if let DeValue::Table(members) = value {
        return table(source, members, range, depth, budget);
    }
    let at = range.start;
    let mut n = start(source, range, depth, budget)?;
    let (kind, value) = match value {
        DeValue::String(s) => ("string", Some(serde_json::to_string(s.as_ref())?)),
        DeValue::Integer(i) => {
            let digits = i.as_str().replace('_', "");
            match i128::from_str_radix(&digits, i.radix()) {
                Ok(v) if i64::try_from(v).is_ok() => ("number", Some(v.to_string())),
                _ => bail!(
                    "TOML line {}: integer {} is out of range",
                    line_of(source, at),
                    i.as_str()
                ),
            }
        }
        DeValue::Float(f) => ("number", Some(super::float_literal(f.as_str()))),
        DeValue::Boolean(b) => ("bool", Some(b.to_string())),
        DeValue::Datetime(d) => {
            let date = d.date.map(|d| (i64::from(d.year), d.month, d.day));
            let time = d.time.map(|t| {
                (
                    i64::from(t.hour) * 60 + i64::from(t.minute),
                    t.second.unwrap_or(0),
                    t.nanosecond.unwrap_or(0),
                )
            });
            let offset = d.offset.map(|o| offset_minutes(&o.to_string()));
            ("datetime", Some(datetime_text(date, time, offset)))
        }
        DeValue::Array(items) => {
            n.kind = "array".into();
            for (i, item) in items.iter().enumerate() {
                n.children.insert(
                    format!("[{i}]"),
                    node(source, item.get_ref(), item.span(), depth + 1, budget)?,
                );
            }
            return Ok(n);
        }
        DeValue::Table(_) => unreachable!("tables are handled above"),
    };
    n.kind = kind.into();
    n.value = value;
    Ok(n)
}

/// The text a date-time compares by: seconds always written, fractions without trailing
/// zeros, and one with an offset as the same moment in UTC, so `07:32Z`, `07:32:00.0+00:00`
/// and `09:32:00+02:00` are equal.
fn datetime_text(mut date: Option<Date>, mut time: Option<Time>, offset: Option<i64>) -> String {
    let mut zone = "";
    if let (Some((y, m, day)), Some((minutes, s, ns)), Some(shift)) = (date, time, offset) {
        let total = days_from_civil(y, m, day) * 1440 + minutes - shift;
        let (y, m, day) = civil_from_days(total.div_euclid(1440));
        date = Some((y, m, day));
        time = Some((total.rem_euclid(1440), s, ns));
        zone = "Z";
    }
    let mut out = String::new();
    if let Some((y, m, day)) = date {
        out.push_str(&format!("{y:04}-{m:02}-{day:02}"));
    }
    if let Some((minutes, s, ns)) = time {
        if !out.is_empty() {
            out.push('T');
        }
        out.push_str(&format!("{:02}:{:02}:{s:02}", minutes / 60, minutes % 60));
        if ns > 0 {
            out.push_str(format!(".{ns:09}").trim_end_matches('0'));
        }
    }
    out.push_str(zone);
    out
}

/// The minutes east of UTC of an offset written `Z` or `±HH:MM`.
fn offset_minutes(text: &str) -> i64 {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let mut parts = text
        .trim_start_matches(['+', '-'])
        .split(':')
        .map(|p| p.parse::<i64>().unwrap_or(0));
    let (hours, minutes) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    sign * (hours * 60 + minutes)
}

/// Days since 1970-01-01 of a date of the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: u8, d: u8) -> i64 {
    let (m, d) = (i64::from(m), i64::from(d));
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let year = y - era * 400;
    let day_of_year = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let day_of_era = year * 365 + year / 4 - year / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The date `days` after 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year + year / 4 - year / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let d = (day_of_year - (153 * mp + 2) / 5 + 1) as u8;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    (year + era * 400 + i64::from(m <= 2), m, d)
}
