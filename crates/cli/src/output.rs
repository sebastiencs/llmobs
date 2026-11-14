//! Terminal output shared by the query, traces and projects commands.

use std::io::{self, Read, Write};

use chrono::{DateTime, SecondsFormat};
use serde_json::Value;

/// Where a command reads and writes. `main` passes the real stdin, stdout and stderr; tests
/// pass byte buffers.
pub(crate) struct Io<'a> {
    /// Results.
    pub(crate) out: &'a mut dyn Write,
    /// Notes (row counts, truncation, next-page cursors). Never results.
    pub(crate) err: &'a mut dyn Write,
    /// The statement for `query --file -`.
    pub(crate) input: &'a mut dyn Read,
    /// Whether stdout is a terminal. Parquet refuses to write binary to one.
    pub(crate) stdout_is_terminal: bool,
}

/// One table column.
pub(crate) struct Column {
    /// Header text.
    pub(crate) name: String,
    /// Right-align the cells (numbers).
    pub(crate) right: bool,
}

/// Most characters a cell shows. Longer text keeps its first 59 characters plus `…`.
pub(crate) const CELL_MAX_CHARS: usize = 60;

/// Writes an aligned table: a header line, a rule line, then one line per row.
pub(crate) fn render_table(
    out: &mut dyn Write,
    columns: &[Column],
    rows: &[Vec<String>],
) -> io::Result<()> {
    if columns.is_empty() {
        return Ok(());
    }
    let headers: Vec<String> = columns
        .iter()
        .map(|column| cut(&clean(&column.name)))
        .collect();
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|index| cut(&clean(row.get(index).map_or("", String::as_str))))
                .collect()
        })
        .collect();
    let widths: Vec<usize> = (0..columns.len())
        .map(|index| {
            cells
                .iter()
                .map(|row| row[index].chars().count())
                .chain(std::iter::once(headers[index].chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    write_line(out, columns, &widths, &headers)?;
    let rules: Vec<String> = widths.iter().map(|&width| "-".repeat(width)).collect();
    write_line(out, columns, &widths, &rules)?;
    for row in &cells {
        write_line(out, columns, &widths, row)?;
    }
    Ok(())
}

fn write_line(
    out: &mut dyn Write,
    columns: &[Column],
    widths: &[usize],
    cells: &[String],
) -> io::Result<()> {
    let mut line = String::new();
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            line.push_str("  ");
        }
        let padding = widths[index].saturating_sub(cell.chars().count());
        if columns[index].right {
            line.extend(std::iter::repeat_n(' ', padding));
            line.push_str(cell);
        } else {
            line.push_str(cell);
            line.extend(std::iter::repeat_n(' ', padding));
        }
    }
    writeln!(out, "{}", line.trim_end_matches(' '))
}

/// Cuts text to [`CELL_MAX_CHARS`] characters, the last one `…`.
fn cut(text: &str) -> String {
    if text.chars().count() <= CELL_MAX_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(CELL_MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// Every `char` for which `char::is_control()` is true becomes one space. Nothing else changes.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// Cell text of one JSON value.
pub(crate) fn json_cell(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.to_owned(),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// `2026-01-01T00:00:00.000000000Z` for a nanosecond timestamp; empty when it is out of range.
pub(crate) fn format_time(ns: i64) -> String {
    if ns < 0 {
        return String::new();
    }
    let seconds = ns.div_euclid(1_000_000_000);
    let nanos = ns.rem_euclid(1_000_000_000);
    let Ok(subsec) = u32::try_from(nanos) else {
        return String::new();
    };
    DateTime::from_timestamp(seconds, subsec)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Nanos, true))
        .unwrap_or_default()
}

/// One note line: `1 row` or `<n> rows`, then `; <more>` when given.
pub(crate) fn write_rows_note(
    err: &mut dyn Write,
    rows: usize,
    more: Option<&str>,
) -> io::Result<()> {
    let count = if rows == 1 {
        "1 row".to_owned()
    } else {
        format!("{rows} rows")
    };
    match more {
        Some(text) => writeln!(err, "{count}; {text}"),
        None => writeln!(err, "{count}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(columns: &[(&str, bool)], rows: &[&[&str]]) -> String {
        let columns: Vec<Column> = columns
            .iter()
            .map(|(name, right)| Column {
                name: (*name).to_owned(),
                right: *right,
            })
            .collect();
        let rows: Vec<Vec<String>> = rows
            .iter()
            .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
            .collect();
        let mut out = Vec::new();
        render_table(&mut out, &columns, &rows).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn table_aligns_numbers_and_trims() {
        assert_eq!(
            table(
                &[("name", false), ("n", true)],
                &[&["chat", "15"], &["summarize", "7"]]
            ),
            "name        n\n---------  --\nchat       15\nsummarize   7\n"
        );
    }

    #[test]
    fn table_cleans_and_cuts_cells_and_headers() {
        let long = "x".repeat(70);
        let out = table(&[("h", false)], &[&[long.as_str()]]);
        let cell = format!("{}…", "x".repeat(59));
        assert_eq!(out, format!("h\n-{}\n{cell}\n", "-".repeat(59)));

        assert_eq!(table(&[("h", false)], &[&["a\tb\nc"]]), "h\n-----\na b c\n");
        assert_eq!(table(&[("x\u{1b}y", false)], &[]), "x y\n---\n");
        let header = "h".repeat(61);
        assert_eq!(
            table(&[(header.as_str(), false)], &[]),
            format!("{}…\n{}\n", "h".repeat(59), "-".repeat(60))
        );
    }

    #[test]
    fn table_edge_cases() {
        assert_eq!(table(&[("h", false)], &[]), "h\n-\n");
        assert_eq!(render_table_to_string(&[], &[]), "");
    }

    fn render_table_to_string(columns: &[Column], rows: &[Vec<String>]) -> String {
        let mut out = Vec::new();
        render_table(&mut out, columns, rows).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn json_cell_renders_each_kind() {
        assert_eq!(json_cell(&serde_json::json!(null)), "NULL");
        assert_eq!(json_cell(&serde_json::json!(true)), "true");
        assert_eq!(json_cell(&serde_json::json!(15)), "15");
        assert_eq!(json_cell(&serde_json::json!(0.000_024)), "0.000024");
        assert_eq!(json_cell(&serde_json::json!(0.000_003_6)), "3.6e-6");
        assert_eq!(json_cell(&serde_json::json!(15.0)), "15.0");
        assert_eq!(json_cell(&serde_json::json!("x")), "x");
        assert_eq!(json_cell(&serde_json::json!([1, "a"])), r#"[1,"a"]"#);
        assert_eq!(
            json_cell(&serde_json::json!({"k": {"n": [1, 2]}})),
            r#"{"k":{"n":[1,2]}}"#
        );
    }

    #[test]
    fn rows_note_counts() {
        let mut err = Vec::new();
        write_rows_note(&mut err, 1, None).unwrap();
        write_rows_note(&mut err, 0, None).unwrap();
        write_rows_note(&mut err, 3, Some("more")).unwrap();
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "1 row\n0 rows\n3 rows; more\n"
        );
    }
}
