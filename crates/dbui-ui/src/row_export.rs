//! Turning selected rows into text for the clipboard.
//!
//! Three shapes, because three different places are being pasted into: a
//! spreadsheet wants tab-separated columns, a script wants JSON, and a psql
//! session wants statements it can run. All three are pure functions over a
//! result set, so all three are unit tested.

use dbui_app::domain::{ColumnInfo, Driver, TableRef, Value};

/// What a copy produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFormat {
    /// Tab-separated with a header row -- what a spreadsheet expects.
    Tsv,
    /// An array of objects, keyed by column name.
    Json,
    /// One `INSERT` per row, ready to replay elsewhere.
    Insert,
}

impl RowFormat {
    pub fn label(self) -> &'static str {
        match self {
            RowFormat::Tsv => "Copy as TSV",
            RowFormat::Json => "Copy as JSON",
            RowFormat::Insert => "Copy as INSERT",
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
        let cells: Vec<String> = row.iter().map(|value| escape_cell(&cell_text(value))).collect();
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
fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        other => other.to_text(),
    }
}

fn json(columns: &[ColumnInfo], values: &[Vec<Value>]) -> String {
    let rows: Vec<serde_json::Value> = values
        .iter()
        .map(|row| {
            let mut object = serde_json::Map::new();
            for (index, column) in columns.iter().enumerate() {
                object.insert(
                    column.name.clone(),
                    row.get(index).map(json_value).unwrap_or(serde_json::Value::Null),
                );
            }
            serde_json::Value::Object(object)
        })
        .collect();
    serde_json::to_string_pretty(&serde_json::Value::Array(rows)).unwrap_or_default()
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
fn sql_literal(value: &Value) -> String {
    match value {
        Value::Null | Value::Default => "NULL".to_string(),
        Value::Bool(flag) => if *flag { "TRUE" } else { "FALSE" }.to_string(),
        Value::Int(number) => number.to_string(),
        Value::Float(number) => number.to_string(),
        Value::Decimal(text) => text.clone(),
        other => format!("'{}'", other.to_text().replace('\'', "''")),
    }
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
            &[vec![Value::Text("two\tparts".into())], vec![Value::Text("two\nlines".into())]],
        );
        assert!(out.contains("\"two\tparts\""), "got: {out:?}");
        assert!(out.contains("\"two\nlines\""), "got: {out:?}");
    }

    #[test]
    fn a_quote_inside_a_cell_is_doubled() {
        let out = tsv(&columns(&["note"]), &[vec![Value::Text("say \"hi\"".into())]]);
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
        assert!(out.contains("'O''Brien''); DROP TABLE t; --'"), "got: {out}");
    }

    /// A query result has no one table to name; the statement still has to be
    /// something a person can fix up rather than a syntax error.
    #[test]
    fn a_result_with_no_table_still_produces_a_statement() {
        let out = inserts(&columns(&["a"]), &[vec![Value::Int(1)]], Driver::MySql, None);
        assert_eq!(out, "INSERT INTO `table` (`a`) VALUES (1);\n");
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
}
