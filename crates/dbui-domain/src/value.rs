//! One cell, after the adapter has decoded it.
//!
//! The grid never sees a driver-native type. Adapters widen everything into
//! [`Value`] so that a `BIGINT` from MySQL and an `int8` from Postgres reach
//! the UI as the same thing, and so the renderer has a closed set to match on.

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fmt;

/// A decoded cell.
///
/// Exact numerics (`NUMERIC`, `DECIMAL`) stay as [`Value::Decimal`] strings
/// rather than becoming `f64`: those columns are usually money, and rounding
/// them to binary floating point on the way to a screen would be a display bug
/// that looks like a data bug.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Uuid(String),
    Json(String),
    /// Date, time, timestamp and interval, already formatted by the adapter.
    Temporal(String),
    Array(Vec<Value>),
    /// A type this build has no decoder for. Carries the engine's own type
    /// name so the grid can say *what* it could not read instead of "?".
    Unsupported(String),
    /// Write-only: `SET col = DEFAULT`. Never produced by a decoder.
    Default,
}

/// Order two cells for a sort the client does itself.
///
/// A query's rows are ordered by its own `ORDER BY`, and the grid will not
/// rewrite someone's SQL to change that -- so sorting a query result reorders
/// the page already in hand, and this is the whole definition of what
/// "ascending" means there.
///
/// Numbers compare as numbers, so `9` comes before `10` rather than after it,
/// and that is the entire reason this is not `to_text().cmp()`. Exact numerics
/// are strings on purpose (see [`Value::Decimal`]) and are read back as `f64`
/// only to be compared; a value too wide for that falls back to comparing the
/// digits, which is still stable and still a total order.
///
/// `NULL` sorts last ascending and first descending, the way Postgres orders
/// it by default -- descending is the exact reverse of ascending, which is
/// what makes a second click on a header mean what it looks like it means.
/// `NaN` sorts above every other number and below `NULL`, which is also what
/// Postgres does with it.
///
/// Mixed types in one column are possible (a `json` column, a union in a
/// query) so kinds that cannot be compared to each other fall back to a fixed
/// order between them. Two rows never compare equal by accident that way, and
/// the sort stays a total order.
pub fn compare(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::Null | Value::Default, Value::Null | Value::Default) => Ordering::Equal,
        (Value::Null | Value::Default, _) => Ordering::Greater,
        (_, Value::Null | Value::Default) => Ordering::Less,
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        (Value::Bytes(a), Value::Bytes(b)) => a.cmp(b),
        (Value::Array(a), Value::Array(b)) => a
            .iter()
            .zip(b.iter())
            .map(|(a, b)| compare(a, b))
            .find(|order| *order != Ordering::Equal)
            .unwrap_or_else(|| a.len().cmp(&b.len())),
        _ => match (as_number(left), as_number(right)) {
            // `partial_cmp` declines only when one side is NaN, and answering
            // "equal" there would make NaN equal to every number while the
            // numbers stay ordered among themselves -- not a total order, and
            // a sort over it comes out shuffled rather than sorted. Postgres
            // ranks NaN above every other float, so do that.
            (Some(a), Some(b)) => a
                .partial_cmp(&b)
                .unwrap_or_else(|| a.is_nan().cmp(&b.is_nan())),
            _ => match sort_rank(left).cmp(&sort_rank(right)) {
                Ordering::Equal => left.to_text().cmp(&right.to_text()),
                order => order,
            },
        },
    }
}

/// The numeric reading of a value, for the columns where digits are the point.
fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Int(number) => Some(*number as f64),
        Value::Float(number) => Some(*number),
        Value::Decimal(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Where a variant sits relative to the others, for the columns that hold
/// more than one. Only reached when two values cannot be compared on their
/// own terms.
fn sort_rank(value: &Value) -> u8 {
    match value {
        Value::Bool(_) => 0,
        Value::Int(_) | Value::Float(_) | Value::Decimal(_) => 1,
        Value::Temporal(_) => 2,
        Value::Text(_) | Value::Uuid(_) => 3,
        Value::Json(_) => 4,
        Value::Array(_) => 5,
        Value::Bytes(_) => 6,
        Value::Unsupported(_) => 7,
        Value::Null | Value::Default => 8,
    }
}

impl Value {
    /// An empty value of whatever variant a column of `declared_type` holds.
    ///
    /// Editing a stored row can read the type off the value already in the
    /// cell. A *new* row has no such value, and sending everything as text is
    /// what makes Postgres refuse `INSERT INTO t (id) VALUES ($1)` against a
    /// bigint column. So the engine's own spelling of the type is matched
    /// instead, coarsely -- the variant only has to be right about which of
    /// this enum's arms the value belongs in.
    pub fn prototype_for(declared_type: &str) -> Value {
        let name = declared_type.to_ascii_lowercase();
        let has = |needle: &str| name.contains(needle);

        // Order matters: `timestamp` contains neither `int` nor `text`, but
        // `bigint` contains `int` and `point` must not.
        if has("bool") {
            return Value::Bool(false);
        }
        if has("json") {
            return Value::Json(String::new());
        }
        if has("uuid") {
            return Value::Uuid(String::new());
        }
        if has("timestamp") || has("date") || has("time") || has("interval") {
            return Value::Temporal(String::new());
        }
        if has("numeric") || has("decimal") || has("money") {
            return Value::Decimal(String::new());
        }
        if has("double") || has("real") || has("float") {
            return Value::Float(0.0);
        }
        if has("serial") || (has("int") && !has("point")) {
            return Value::Int(0);
        }
        if has("bytea") || has("blob") || has("binary") {
            return Value::Bytes(Vec::new());
        }
        Value::Text(String::new())
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// The coarse class the UI styles on: alignment, colour, monospacing.
    ///
    /// Deliberately smaller than the variant set -- the grid wants "is this a
    /// number" (right-align), not "is this an i64 or a decimal".
    pub fn kind(&self) -> ValueKind {
        match self {
            Value::Null => ValueKind::Null,
            Value::Bool(_) => ValueKind::Bool,
            Value::Int(_) | Value::Float(_) | Value::Decimal(_) => ValueKind::Number,
            Value::Text(_) => ValueKind::Text,
            Value::Bytes(_) => ValueKind::Binary,
            Value::Uuid(_) => ValueKind::Uuid,
            Value::Json(_) | Value::Array(_) => ValueKind::Structured,
            Value::Temporal(_) => ValueKind::Temporal,
            Value::Unsupported(_) => ValueKind::Unsupported,
            Value::Default => ValueKind::Unsupported,
        }
    }

    /// The full text of the cell, as the clipboard and the detail pane want it.
    ///
    /// NULL renders empty here, not as the word "NULL": a cell holding the
    /// four characters `NULL` and a cell holding nothing must not copy out the
    /// same way. The grid draws the italic NULL marker itself, off [`kind`].
    ///
    /// [`kind`]: Value::kind
    pub fn to_text(&self) -> String {
        match self {
            Value::Null => String::new(),
            Value::Bool(b) => b.to_string(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => format_float(*f),
            Value::Decimal(s) | Value::Text(s) | Value::Uuid(s) | Value::Json(s) => s.clone(),
            Value::Temporal(s) => s.clone(),
            Value::Bytes(b) => format_bytes(b),
            Value::Array(items) => {
                let inner: Vec<String> = items.iter().map(Value::to_text).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Unsupported(type_name) => format!("<{type_name}>"),
            Value::Default => "DEFAULT".into(),
        }
    }

    /// A single-line, length-capped rendering for a grid cell.
    ///
    /// A row is one line tall, so an embedded newline would otherwise let one
    /// value paint over its neighbours; and a 2 MB JSON blob costs real time to
    /// lay out for the ~60 characters that end up visible.
    pub fn to_cell(&self, max_chars: usize) -> String {
        let text = self.to_text();
        let mut out = String::with_capacity(text.len().min(max_chars) + 1);
        for ch in text.chars() {
            if out.chars().count() >= max_chars {
                out.push('…');
                return out;
            }
            match ch {
                '\n' => out.push('⏎'),
                '\t' => out.push(' '),
                '\r' => {}
                c => out.push(c),
            }
        }
        out
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_text())
    }
}

/// The style class of a [`Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Null,
    Bool,
    Number,
    Text,
    Binary,
    Uuid,
    Temporal,
    Structured,
    Unsupported,
}

impl ValueKind {
    /// Numbers read as columns of digits only when their ones line up.
    pub fn right_aligned(self) -> bool {
        matches!(self, ValueKind::Number)
    }
}

/// Trim the `f64` debug tail without turning integral floats into `2` -- a
/// `float8` column showing `2` and an `int8` column showing `2` should still
/// look different in the grid.
fn format_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f.is_sign_negative() {
            "-Infinity"
        } else {
            "Infinity"
        }
        .into();
    }
    if f == f.trunc() && f.abs() < 1e15 {
        format!("{f:.1}")
    } else {
        let mut s = format!("{f}");
        if s.contains('e') || s.contains('E') {
            s = format!("{f:e}");
        }
        s
    }
}

/// Show binary as a hex preview with its true length, the way a hex viewer
/// would: the first bytes are what identify a blob, and the length is what
/// tells you it is not the whole thing.
fn format_bytes(bytes: &[u8]) -> String {
    const PREVIEW: usize = 16;
    let mut s = String::from("0x");
    for byte in bytes.iter().take(PREVIEW) {
        s.push_str(&format!("{byte:02x}"));
    }
    if bytes.len() > PREVIEW {
        s.push('…');
    }
    s.push_str(&format!(" ({} bytes)", bytes.len()));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_copies_as_nothing_not_as_the_word() {
        assert_eq!(Value::Null.to_text(), "");
        assert_eq!(Value::Text("NULL".into()).to_text(), "NULL");
    }

    #[test]
    fn cells_stay_on_one_line_and_within_budget() {
        let value = Value::Text("a\nb".into());
        assert_eq!(value.to_cell(80), "a⏎b");

        let long = Value::Text("x".repeat(500));
        let cell = long.to_cell(10);
        assert_eq!(cell.chars().count(), 11, "10 chars plus the ellipsis");
        assert!(cell.ends_with('…'));
    }

    #[test]
    fn integral_floats_keep_a_decimal_point() {
        assert_eq!(Value::Float(2.0).to_text(), "2.0");
        assert_eq!(Value::Int(2).to_text(), "2");
    }

    #[test]
    fn decimals_are_not_routed_through_binary_floating_point() {
        let money = Value::Decimal("0.10".into());
        assert_eq!(money.to_text(), "0.10");
        assert_eq!(money.kind(), ValueKind::Number);
    }

    #[test]
    fn bytes_show_a_preview_and_the_real_length() {
        assert_eq!(Value::Bytes(vec![0xde, 0xad]).to_text(), "0xdead (2 bytes)");
        assert!(Value::Bytes(vec![0; 40]).to_text().contains("(40 bytes)"));
    }
}

#[cfg(test)]
mod prototype_tests {
    use super::*;

    /// The bug this fixes: a new row had no stored value to take a type from,
    /// so every column went over as text and Postgres refused the INSERT with
    /// "column is of type bigint but expression is of type text".
    #[test]
    fn integer_columns_are_recognised_in_both_engines_spellings() {
        for name in [
            "bigint",
            "integer",
            "int4",
            "INT",
            "smallint",
            "BIGINT UNSIGNED",
        ] {
            assert_eq!(
                Value::prototype_for(name),
                Value::Int(0),
                "{name} should widen to an integer"
            );
        }
    }

    /// `point` contains "int" and is not a number.
    #[test]
    fn point_is_not_mistaken_for_an_integer() {
        assert_eq!(Value::prototype_for("point"), Value::Text(String::new()));
    }

    #[test]
    fn the_other_families_land_in_their_own_variants() {
        assert_eq!(Value::prototype_for("boolean"), Value::Bool(false));
        assert_eq!(Value::prototype_for("jsonb"), Value::Json(String::new()));
        assert_eq!(Value::prototype_for("uuid"), Value::Uuid(String::new()));
        assert_eq!(
            Value::prototype_for("timestamp with time zone"),
            Value::Temporal(String::new())
        );
        assert_eq!(
            Value::prototype_for("numeric(10,2)"),
            Value::Decimal(String::new())
        );
        assert_eq!(Value::prototype_for("double precision"), Value::Float(0.0));
        assert_eq!(Value::prototype_for("bytea"), Value::Bytes(Vec::new()));
    }

    /// Anything unrecognised is text, which every engine will coerce from.
    #[test]
    fn an_unknown_type_falls_back_to_text() {
        assert_eq!(
            Value::prototype_for("some_custom_enum"),
            Value::Text(String::new())
        );
        assert_eq!(
            Value::prototype_for("character varying(255)"),
            Value::Text(String::new())
        );
    }

    /// A `serial` is an integer column with a sequence behind it.
    #[test]
    fn serial_is_an_integer() {
        assert_eq!(Value::prototype_for("bigserial"), Value::Int(0));
    }

    /// The whole reason this is not `to_text().cmp()`.
    #[test]
    fn numbers_sort_as_numbers_not_as_digits() {
        let mut values = vec![Value::Int(10), Value::Int(9), Value::Int(100)];
        values.sort_by(compare);
        assert_eq!(values, vec![Value::Int(9), Value::Int(10), Value::Int(100)]);
    }

    /// Money stays a string all the way to the screen, and still sorts.
    #[test]
    fn decimals_compare_by_value_and_against_ints() {
        assert_eq!(
            compare(
                &Value::Decimal("9.50".into()),
                &Value::Decimal("10.00".into())
            ),
            Ordering::Less
        );
        assert_eq!(
            compare(&Value::Decimal("2.5".into()), &Value::Int(2)),
            Ordering::Greater
        );
    }

    /// A decimal too wide for `f64` still lands somewhere, and lands there
    /// every time.
    #[test]
    fn an_unparseable_decimal_falls_back_to_its_digits() {
        let huge = Value::Decimal("not a number".into());
        assert_eq!(compare(&huge, &huge), Ordering::Equal);
        assert_eq!(
            compare(&huge, &Value::Decimal("zzz".into())),
            Ordering::Less
        );
    }

    /// Postgres' default: last ascending, and therefore first descending,
    /// because descending is the exact reverse.
    #[test]
    fn null_sorts_last_ascending() {
        let mut values = vec![Value::Null, Value::Int(2), Value::Int(1)];
        values.sort_by(compare);
        assert_eq!(values, vec![Value::Int(1), Value::Int(2), Value::Null]);

        values.sort_by(|a, b| compare(a, b).reverse());
        assert_eq!(values, vec![Value::Null, Value::Int(2), Value::Int(1)]);
    }

    /// A `float8` column can hold `'NaN'`, and a comparator that calls it
    /// equal to every number is not a total order -- sorting one used to come
    /// out shuffled, integers and all.
    #[test]
    fn nan_sorts_above_the_numbers_rather_than_equal_to_them() {
        let mut values = [
            Value::Float(2.),
            Value::Float(f64::NAN),
            Value::Int(1),
            Value::Null,
            Value::Int(3),
        ];
        values.sort_by(compare);
        assert_eq!(
            values[..3],
            [Value::Int(1), Value::Float(2.), Value::Int(3)],
            "the real numbers stay in order"
        );
        assert!(
            matches!(values[3], Value::Float(f) if f.is_nan()),
            "then NaN"
        );
        assert_eq!(values[4], Value::Null, "and NULL is still last");
    }

    /// `DEFAULT` is an absence like `NULL`, and sorts with it.
    #[test]
    fn default_sorts_with_null() {
        assert_eq!(compare(&Value::Default, &Value::Null), Ordering::Equal);
        assert_eq!(compare(&Value::Default, &Value::Int(0)), Ordering::Greater);
    }

    /// Timestamps arrive already formatted, and the adapters format them so
    /// that lexicographic order is chronological order.
    #[test]
    fn temporals_sort_chronologically_as_written() {
        let mut values = [
            Value::Temporal("2026-01-02 00:00:00".into()),
            Value::Temporal("2025-12-31 23:59:59".into()),
        ];
        values.sort_by(compare);
        assert_eq!(values[0], Value::Temporal("2025-12-31 23:59:59".into()));
    }

    /// A column holding more than one kind still gets a total order, so a
    /// sort over it terminates and is repeatable.
    #[test]
    fn mixed_kinds_still_order_totally() {
        let mut values = vec![
            Value::Text("b".into()),
            Value::Int(3),
            Value::Bool(true),
            Value::Null,
            Value::Text("a".into()),
        ];
        values.sort_by(compare);
        assert_eq!(
            values,
            vec![
                Value::Bool(true),
                Value::Int(3),
                Value::Text("a".into()),
                Value::Text("b".into()),
                Value::Null,
            ]
        );
    }

    /// Arrays compare element by element, shorter first when one is a prefix.
    #[test]
    fn arrays_compare_element_by_element() {
        let short = Value::Array(vec![Value::Int(1)]);
        let long = Value::Array(vec![Value::Int(1), Value::Int(0)]);
        assert_eq!(compare(&short, &long), Ordering::Less);
        assert_eq!(
            compare(&long, &Value::Array(vec![Value::Int(2)])),
            Ordering::Less
        );
    }
}
