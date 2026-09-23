//! Turning rows into text -- for the clipboard, and for files.
//!
//! Three shapes for the clipboard, because three different places are being
//! pasted into: a spreadsheet wants tab-separated columns, a script wants
//! JSON, and a psql session wants statements it can run. A file adds CSV, the
//! one every other tool reads. All of them are pure functions over a result
//! set, so all of them are unit tested; [`ExportWriter`] streams the same
//! shapes to disk a page at a time.

use dbui_app::domain::{ColumnInfo, Driver, TableRef, Value};
use std::collections::{HashMap, HashSet};

/// What a copy produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFormat {
    /// Tab-separated with a header row -- what a spreadsheet expects.
    Tsv,
    /// An array of objects, keyed by column name.
    Json,
    /// One `INSERT` per row, ready to replay elsewhere.
    Insert,
    /// Comma-separated with a header row -- for files other tools will open.
    Csv,
}

impl RowFormat {
    pub fn label(self) -> &'static str {
        match self {
            RowFormat::Tsv => "Copy as TSV",
            RowFormat::Json => "Copy as JSON",
            RowFormat::Insert => "Copy as INSERT",
            RowFormat::Csv => "Copy as CSV",
        }
    }

    /// The file extension an export in this shape is saved with.
    pub fn extension(self) -> &'static str {
        match self {
            RowFormat::Tsv => "tsv",
            RowFormat::Json => "json",
            RowFormat::Insert => "sql",
            RowFormat::Csv => "csv",
        }
    }
}

/// Render `rows` (indices into `values`) in `format`.
///
/// `table` is only needed for `Insert`; a query result has no single table to
/// name, so the caller passes what it has and the statement says `table` when
/// it has nothing better.
pub fn render(
    format: RowFormat,
    columns: &[ColumnInfo],
    values: &[Vec<Value>],
    driver: Driver,
    table: Option<&TableRef>,
) -> String {
    match format {
        RowFormat::Tsv => tsv(columns, values),
        RowFormat::Json => json(columns, values),
        RowFormat::Insert => inserts(columns, values, driver, table),
        RowFormat::Csv => csv(columns, values),
    }
}

fn csv(columns: &[ColumnInfo], values: &[Vec<Value>]) -> String {
    let mut out = csv_line(columns.iter().map(|column| column.name.clone()));
    for row in values {
        out.push_str(&csv_line(row.iter().map(cell_text)));
    }
    out
}

/// One CSV record, newline included. RFC 4180 quoting: a field holding a
/// comma, a quote or a line break is wrapped in quotes, with its own quotes
/// doubled.
fn csv_line(fields: impl Iterator<Item = String>) -> String {
    let mut line = fields
        .map(|field| {
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    line.push('\n');
    line
}

/// Rows written to a file as they arrive, a page at a time.
///
/// A whole table can be far more than fits comfortably in memory, so nothing
/// here holds more than the page in hand. `keep` narrows and orders the
/// columns to the ones the grid shows, by name; `None` writes them all.
pub struct ExportWriter<W: std::io::Write> {
    out: W,
    format: RowFormat,
    driver: Driver,
    table: Option<TableRef>,
    keep: Option<Vec<String>>,
    /// Where each kept column sits in the rows being written. Worked out from
    /// the first page's columns.
    picks: Option<Vec<usize>>,
    started: bool,
    pub rows: u64,
}

impl<W: std::io::Write> ExportWriter<W> {
    pub fn new(
        out: W,
        format: RowFormat,
        driver: Driver,
        table: Option<TableRef>,
        keep: Option<Vec<String>>,
    ) -> Self {
        Self {
            out,
            format,
            driver,
            table,
            keep,
            picks: None,
            started: false,
            rows: 0,
        }
    }

    pub fn page(&mut self, columns: &[ColumnInfo], rows: &[Vec<Value>]) -> std::io::Result<()> {
        let picks = self.picks.get_or_insert_with(|| match &self.keep {
            Some(names) => names
                .iter()
                .filter_map(|name| columns.iter().position(|column| &column.name == name))
                .collect(),
            None => (0..columns.len()).collect(),
        });
        let columns: Vec<ColumnInfo> = picks.iter().map(|&at| columns[at].clone()).collect();
        let rows: Vec<Vec<Value>> = rows
            .iter()
            .map(|row| {
                picks
                    .iter()
                    .map(|&at| row.get(at).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();

        let first = !self.started;
        self.started = true;
        match self.format {
            RowFormat::Csv => {
                if first {
                    self.out
                        .write_all(csv_line(columns.iter().map(|c| c.name.clone())).as_bytes())?;
                }
                for row in &rows {
                    self.out
                        .write_all(csv_line(row.iter().map(cell_text)).as_bytes())?;
                }
            }
            RowFormat::Tsv => {
                let text = tsv(&columns, &rows);
                // The header is the first line of every page's rendering;
                // only the first page keeps it.
                let body = if first {
                    text.as_str()
                } else {
                    text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
                };
                self.out.write_all(body.as_bytes())?;
            }
            RowFormat::Insert => {
                let text = inserts(&columns, &rows, self.driver, self.table.as_ref());
                self.out.write_all(text.as_bytes())?;
            }
            RowFormat::Json => {
                // One array across every page: the brackets go on the ends
                // of the file, and a comma between pages as well as rows.
                self.out.write_all(if first { b"[" } else { b"" })?;
                for (index, row) in rows.iter().enumerate() {
                    let mut object = serde_json::Map::new();
                    for (column, value) in columns.iter().zip(row) {
                        object.insert(column.name.clone(), json_value(value));
                    }
                    let separator = if self.rows == 0 && index == 0 {
                        "\n  "
                    } else {
                        ",\n  "
                    };
                    self.out.write_all(separator.as_bytes())?;
                    let item = serde_json::to_string(&serde_json::Value::Object(object))
                        .unwrap_or_default();
                    self.out.write_all(item.as_bytes())?;
                }
            }
        }
        self.rows += rows.len() as u64;
        Ok(())
    }

    /// Close off the file: JSON's array, and whatever is still buffered.
    pub fn finish(&mut self) -> std::io::Result<()> {
        if self.format == RowFormat::Json {
            let tail: &[u8] = match (self.started, self.rows) {
                (false, _) => b"[]\n",
                (true, 0) => b"]\n",
                (true, _) => b"\n]\n",
            };
            self.out.write_all(tail)?;
        }
        self.out.flush()
    }
}

fn tsv(columns: &[ColumnInfo], values: &[Vec<Value>]) -> String {
    let mut out = String::new();
    out.push_str(
        &columns
            .iter()
            .map(|column| escape_cell(&column.name))
            .collect::<Vec<_>>()
            .join("\t"),
    );
    out.push('\n');
    for row in values {
        let cells: Vec<String> = row
            .iter()
            .map(|value| escape_cell(&cell_text(value)))
            .collect();
        out.push_str(&cells.join("\t"));
        out.push('\n');
    }
    out
}

/// A tab or a newline inside a value would end the cell or the row.
///
/// Spreadsheets read a quoted field the way CSV does, so quoting is what keeps
/// a multi-line address in one cell instead of spread over four rows.
fn escape_cell(text: &str) -> String {
    if text.contains('\t') || text.contains('\n') || text.contains('\r') || text.contains('"') {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// NULL is the empty cell in TSV: a spreadsheet has no other way to say it,
/// and the literal word would come back as the four-letter string.
pub(crate) fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.to_text(),
    }
}

fn json(columns: &[ColumnInfo], values: &[Vec<Value>]) -> String {
    // The keys are a property of the columns, not of a row, so they are built
    // once rather than re-derived for every row of the result.
    let keys = json_keys(columns);
    let rows: Vec<serde_json::Value> = values
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (index, key) in keys.iter().enumerate() {
                object.insert(
                    key.clone(),
                    row.get(index)
                        .map(json_value)
                        .unwrap_or(serde_json::Value::Null),
                );
            }
            serde_json::Value::Object(object)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap_or_default()
}

/// One JSON key per column, even when two columns share a name.
///
/// Column names are not unique -- `SELECT a.*, b.*` brings back two `id`s --
/// but object keys are, so keying rows on the raw name let the second `id`
/// overwrite the first and dropped a whole column from the export with nothing
/// in the output to say so. TSV and INSERT both keep all of them.
///
/// The first column to use a name keeps it, so a result with no duplicates is
/// exported exactly as before; later ones take `_2`, `_3`, and so on. The
/// counter skips any name a real column already answers to, so `id, id_2, id`
/// cannot have the disambiguated key land on top of the column it collides
/// with.
fn json_keys(columns: &[ColumnInfo]) -> Vec<String> {
    let mut taken: HashSet<String> = columns.iter().map(|column| column.name.clone()).collect();
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let mut keys = Vec::with_capacity(columns.len());
    for column in columns {
        let count = seen.entry(column.name.as_str()).or_insert(0);
        *count += 1;
        if *count == 1 {
            keys.push(column.name.clone());
            continue;
        }
        let mut suffix = *count;
        let mut candidate = format!("{}_{suffix}", column.name);
        while !taken.insert(candidate.clone()) {
            suffix += 1;
            candidate = format!("{}_{suffix}", column.name);
        }
        keys.push(candidate);
    }
    keys
}

/// Numbers stay numbers and JSON columns stay structured; everything else is
/// a string. Pasting `{"a":1}` back as the *string* `"{\"a\":1}"` is the kind
/// of round-trip that quietly corrupts a fixture.
fn json_value(value: &Value) -> serde_json::Value {
    match value {
        Value::Null | Value::Default => serde_json::Value::Null,
        Value::Bool(flag) => serde_json::Value::Bool(*flag),
        Value::Int(number) => serde_json::Value::from(*number),
        Value::Float(number) => serde_json::Number::from_f64(*number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Json(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| serde_json::Value::String(text.clone()))
        }
        other => serde_json::Value::String(other.to_text()),
    }
}

fn inserts(
    columns: &[ColumnInfo],
    values: &[Vec<Value>],
    driver: Driver,
    table: Option<&TableRef>,
) -> String {
    let target = table
        .map(|table| table.quoted(driver))
        .unwrap_or_else(|| driver.quote_identifier("table"));
    let names: Vec<String> = columns
        .iter()
        .map(|column| driver.quote_identifier(&column.name))
        .collect();

    let mut out = String::new();
    for row in values {
        let literals: Vec<String> = columns
            .iter()
            .enumerate()
            .map(|(index, _)| {
                row.get(index)
                    .map(sql_literal)
                    .unwrap_or_else(|| "NULL".to_string())
            })
            .collect();
        out.push_str(&format!(
            "INSERT INTO {target} ({}) VALUES ({});\n",
            names.join(", "),
            literals.join(", ")
        ));
    }
    out
}

/// A value as SQL text.
///
/// This is the one place in the codebase that interpolates a *value* rather
/// than binding it -- the output is text for a human to read and run, not a
/// statement this app executes. Quotes are still doubled, so a pasted string
/// cannot end its own literal.
pub fn sql_literal(value: &Value) -> String {
    match value {
        Value::Null | Value::Default => "NULL".to_string(),
        Value::Bool(flag) => if *flag { "TRUE" } else { "FALSE" }.to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Decimal(text) => text.clone(),
        other => format!("'{}'", other.to_text().replace('\'', "''")),
    }
}

/// Rows read back off the clipboard.
pub struct PastedRows {
    /// The header line: column names, in the order the cells come in.
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// Parse tab-separated text back into rows.
///
/// The exact inverse of [`tsv`], including its quoting: a cell holding a tab
/// or a newline was written quoted, and has to be read back as one cell rather
/// than as the start of a new column or row. `None` when the text is not
/// shaped like a table at all.
pub fn parse_tsv(text: &str) -> Option<PastedRows> {
    parse_delimited(text, '\t')
}

/// Parse a CSV file's text into rows: the first record is the header.
///
/// A byte-order mark is dropped first -- Excel writes one, and left in place
/// it becomes part of the first column's name, which then matches nothing.
pub fn parse_csv(text: &str) -> Option<PastedRows> {
    parse_delimited(text.strip_prefix('\u{feff}').unwrap_or(text), ',')
}

fn parse_delimited(text: &str, separator: char) -> Option<PastedRows> {
    let mut records = split_records(text, separator);
    if records.len() < 2 {
        // A header and at least one row. A lone line is a value someone
        // copied from somewhere else, not a table.
        return None;
    }

    let columns = records.remove(0);
    if columns.iter().all(|name| name.trim().is_empty()) {
        return None;
    }

    let width = columns.len();
    let mut rows = Vec::with_capacity(records.len());
    for mut record in records {
        // A spreadsheet often drops trailing empty cells; a record with more
        // cells than headers is something else entirely and is refused rather
        // than silently misaligned.
        if record.len() > width {
            return None;
        }
        record.resize(width, String::new());
        rows.push(record);
    }

    if rows.is_empty() {
        return None;
    }
    Some(PastedRows { columns, rows })
}

/// Split delimited text into records of fields, honouring quoted cells.
fn split_records(text: &str, separator: char) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                // A doubled quote inside a quoted cell is one quote.
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }

        match ch {
            '"' if field.is_empty() => quoted = true,
            c if c == separator => record.push(std::mem::take(&mut field)),
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            '\r' => {}
            other => field.push(other),
        }
    }

    // Whatever is left over is the last record, unless the text ended on a
    // newline and there is nothing after it.
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    fn columns(names: &[&str]) -> Vec<ColumnInfo> {
        names
            .iter()
            .map(|name| ColumnInfo {
                name: (*name).to_string(),
                type_name: "text".into(),
            })
            .collect()
    }

    #[test]
    fn tsv_leads_with_a_header_and_one_line_per_row() {
        let out = tsv(
            &columns(&["id", "name"]),
            &[
                vec![Value::Int(1), Value::Text("Ada".into())],
                vec![Value::Int(2), Value::Text("Grace".into())],
            ],
        );
        assert_eq!(out, "id\tname\n1\tAda\n2\tGrace\n");
    }

    /// A tab or newline inside a value would end the cell or the row, so a
    /// spreadsheet would read one address as four rows.
    #[test]
    fn a_value_containing_a_tab_or_newline_is_quoted() {
        let out = tsv(
            &columns(&["note"]),
            &[
                vec![Value::Text("two\tparts".into())],
                vec![Value::Text("two\nlines".into())],
            ],
        );
        assert!(out.contains("\"two\tparts\""), "got: {out:?}");
        assert!(out.contains("\"two\nlines\""), "got: {out:?}");
    }

    #[test]
    fn a_quote_inside_a_cell_is_doubled() {
        let out = tsv(
            &columns(&["note"]),
            &[vec![Value::Text("say \"hi\"".into())]],
        );
        assert!(out.contains("\"say \"\"hi\"\"\""), "got: {out:?}");
    }

    /// NULL is an empty cell, not the word.
    #[test]
    fn null_is_an_empty_tsv_cell() {
        let out = tsv(&columns(&["a", "b"]), &[vec![Value::Null, Value::Int(2)]]);
        assert_eq!(out, "a\tb\n\t2\n");
    }

    #[test]
    fn json_keeps_numbers_and_structure() {
        let out = json(
            &columns(&["id", "meta", "name"]),
            &[vec![
                Value::Int(7),
                Value::Json(r#"{"a":1}"#.into()),
                Value::Text("Ada".into()),
            ]],
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let row = &parsed[0];
        assert_eq!(row["id"], serde_json::json!(7), "a number stays a number");
        assert_eq!(
            row["meta"],
            serde_json::json!({"a": 1}),
            "a json column stays an object, not a string of one"
        );
        assert_eq!(row["name"], serde_json::json!("Ada"));
    }

    #[test]
    fn json_writes_null_for_a_null() {
        let out = json(&columns(&["a"]), &[vec![Value::Null]]);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(parsed[0]["a"].is_null());
    }

    /// `SELECT a.*, b.*` where both sides have an `id` used to come back with
    /// three keys for four columns: the second `id` overwrote the first and
    /// `a.id` was gone, with nothing in the output to say so.
    #[test]
    fn json_keeps_every_column_when_two_share_a_name() {
        let out = json(
            &columns(&["id", "name", "id", "label"]),
            &[vec![
                Value::Int(1),
                Value::Text("Ada".into()),
                Value::Int(5),
                Value::Text("x".into()),
            ]],
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let row = parsed[0].as_object().expect("an object per row");
        assert_eq!(row.len(), 4, "one key per column, got: {out}");
        assert_eq!(row["id"], serde_json::json!(1), "the first id survives");
        assert_eq!(row["id_2"], serde_json::json!(5));
        assert_eq!(row["name"], serde_json::json!("Ada"));
        assert_eq!(row["label"], serde_json::json!("x"));
    }

    /// The suffix has to land on a name no column already answers to, or the
    /// disambiguation collides exactly where it was meant to help.
    #[test]
    fn json_skips_a_suffix_a_real_column_already_uses() {
        let out = json(
            &columns(&["id", "id_2", "id"]),
            &[vec![Value::Int(1), Value::Int(2), Value::Int(3)]],
        );
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid JSON");
        let row = parsed[0].as_object().expect("an object per row");
        assert_eq!(row.len(), 3, "one key per column, got: {out}");
        assert_eq!(row["id"], serde_json::json!(1));
        assert_eq!(
            row["id_2"],
            serde_json::json!(2),
            "the real id_2 column keeps its own name"
        );
        assert_eq!(row["id_3"], serde_json::json!(3));
    }

    /// The three formats have to agree on how many columns a result has. TSV
    /// and INSERT already keep all four, so JSON is pinned to the same count
    /// rather than the other two being loosened to match it.
    #[test]
    fn tsv_and_inserts_keep_every_column_when_two_share_a_name() {
        let columns = columns(&["id", "name", "id", "label"]);
        let values = vec![vec![
            Value::Int(1),
            Value::Text("Ada".into()),
            Value::Int(5),
            Value::Text("x".into()),
        ]];
        assert_eq!(
            tsv(&columns, &values),
            "id\tname\tid\tlabel\n1\tAda\t5\tx\n"
        );
        assert_eq!(
            inserts(
                &columns,
                &values,
                Driver::Postgres,
                Some(&TableRef::new("s", "t"))
            ),
            "INSERT INTO \"s\".\"t\" (\"id\", \"name\", \"id\", \"label\") VALUES (1, 'Ada', 5, 'x');\n"
        );
    }

    #[test]
    fn insert_statements_name_the_table_and_every_column() {
        let out = inserts(
            &columns(&["id", "name"]),
            &[vec![Value::Int(1), Value::Text("Ada".into())]],
            Driver::Postgres,
            Some(&TableRef::new("public", "people")),
        );
        assert_eq!(
            out,
            "INSERT INTO \"public\".\"people\" (\"id\", \"name\") VALUES (1, 'Ada');\n"
        );
    }

    /// The output is text a person runs, so a quote in a value must not end
    /// the literal it sits in.
    #[test]
    fn a_quote_in_a_value_cannot_end_its_literal() {
        let out = inserts(
            &columns(&["name"]),
            &[vec![Value::Text("O'Brien'); DROP TABLE t; --".into())]],
            Driver::Postgres,
            Some(&TableRef::new("s", "t")),
        );
        assert!(
            out.contains("'O''Brien''); DROP TABLE t; --'"),
            "got: {out}"
        );
    }

    /// A query result has no one table to name; the statement still has to be
    /// something a person can fix up rather than a syntax error.
    #[test]
    fn a_result_with_no_table_still_produces_a_statement() {
        let out = inserts(
            &columns(&["a"]),
            &[vec![Value::Int(1)]],
            Driver::MySql,
            None,
        );
        assert_eq!(out, "INSERT INTO `table` (`a`) VALUES (1);\n");
    }

    // -- reading rows back ------------------------------------------------

    /// The property that matters: whatever copy writes, paste reads back.
    #[test]
    fn tsv_round_trips_through_the_clipboard() {
        let columns = columns(&["id", "name", "note"]);
        let values = vec![
            vec![
                Value::Int(1),
                Value::Text("Ada".into()),
                Value::Text("two\tparts".into()),
            ],
            vec![Value::Int(2), Value::Null, Value::Text("two\nlines".into())],
        ];

        let written = tsv(&columns, &values);
        let read = parse_tsv(&written).expect("it reads back");

        assert_eq!(read.columns, vec!["id", "name", "note"]);
        assert_eq!(read.rows[0], vec!["1", "Ada", "two\tparts"]);
        assert_eq!(
            read.rows[1],
            vec!["2", "", "two\nlines"],
            "a NULL comes back as the empty cell it was written as"
        );
    }

    #[test]
    fn a_quote_inside_a_cell_survives_the_round_trip() {
        let written = tsv(
            &columns(&["note"]),
            &[vec![Value::Text("say \"hi\"".into())]],
        );
        let read = parse_tsv(&written).expect("reads back");
        assert_eq!(read.rows[0][0], "say \"hi\"");
    }

    /// A spreadsheet often drops trailing empty cells.
    #[test]
    fn a_short_row_is_padded_to_the_header() {
        let read = parse_tsv("a\tb\tc\n1\t2\n").expect("reads back");
        assert_eq!(read.rows[0], vec!["1", "2", ""]);
    }

    /// More cells than headers is something other than a table, and pasting it
    /// would put values under the wrong columns.
    #[test]
    fn a_row_wider_than_the_header_is_refused() {
        assert!(parse_tsv("a\tb\n1\t2\t3\n").is_none());
    }

    /// One line is a value copied from somewhere else, not a table.
    #[test]
    fn a_single_line_is_not_a_table() {
        assert!(parse_tsv("just some text").is_none());
        assert!(parse_tsv("").is_none());
        assert!(parse_tsv("a\tb\n").is_none(), "a header with no rows");
    }

    #[test]
    fn booleans_and_decimals_are_written_unquoted() {
        let out = inserts(
            &columns(&["ok", "amount"]),
            &[vec![Value::Bool(true), Value::Decimal("1.50".into())]],
            Driver::Postgres,
            Some(&TableRef::new("s", "t")),
        );
        assert!(out.contains("VALUES (TRUE, 1.50)"), "got: {out}");
    }

    #[test]
    fn csv_quotes_only_what_needs_it() {
        let text = render(
            RowFormat::Csv,
            &columns(&["id", "note"]),
            &[
                vec![Value::Int(1), Value::Text("plain".into())],
                vec![Value::Int(2), Value::Text("a, \"quoted\"\nline".into())],
                vec![Value::Int(3), Value::Null],
            ],
            Driver::Postgres,
            None,
        );
        assert_eq!(
            text,
            "id,note\n1,plain\n2,\"a, \"\"quoted\"\"\nline\"\n3,\n"
        );
        // And reading it back gives the same cells.
        let parsed = parse_csv(&text).expect("a table");
        assert_eq!(parsed.rows[1], vec!["2", "a, \"quoted\"\nline"]);
    }

    #[test]
    fn a_json_export_across_pages_is_one_array() {
        let mut out = Vec::new();
        let mut writer = ExportWriter::new(&mut out, RowFormat::Json, Driver::Postgres, None, None);
        let cols = columns(&["id"]);
        writer
            .page(&cols, &[vec![Value::Int(1)], vec![Value::Int(2)]])
            .unwrap();
        writer.page(&cols, &[]).unwrap();
        writer.page(&cols, &[vec![Value::Int(3)]]).unwrap();
        writer.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
        assert_eq!(parsed, serde_json::json!([{"id": 1}, {"id": 2}, {"id": 3}]));
    }

    #[test]
    fn an_empty_json_export_is_an_empty_array() {
        let mut out = Vec::new();
        let mut writer = ExportWriter::new(&mut out, RowFormat::Json, Driver::Postgres, None, None);
        writer.page(&columns(&["id"]), &[]).unwrap();
        writer.finish().unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("valid JSON");
        assert_eq!(parsed, serde_json::json!([]));
    }

    #[test]
    fn an_export_keeps_the_grids_columns_in_the_grids_order() {
        let mut out = Vec::new();
        let mut writer = ExportWriter::new(
            &mut out,
            RowFormat::Csv,
            Driver::Postgres,
            None,
            Some(vec!["name".into(), "id".into()]),
        );
        let cols = columns(&["id", "secret", "name"]);
        writer
            .page(
                &cols,
                &[vec![
                    Value::Int(1),
                    Value::Text("x".into()),
                    Value::Text("Ada".into()),
                ]],
            )
            .unwrap();
        writer
            .page(
                &cols,
                &[vec![
                    Value::Int(2),
                    Value::Text("y".into()),
                    Value::Text("Bo".into()),
                ]],
            )
            .unwrap();
        writer.finish().unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "name,id\nAda,1\nBo,2\n");
    }

    #[test]
    fn a_csv_with_a_byte_order_mark_reads_its_first_header_clean() {
        let parsed = parse_csv("\u{feff}id,name\r\n1,Ada\r\n").expect("a table");
        assert_eq!(parsed.columns, vec!["id", "name"]);
        assert_eq!(parsed.rows, vec![vec!["1", "Ada"]]);
    }
}
