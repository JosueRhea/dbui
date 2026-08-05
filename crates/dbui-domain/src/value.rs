//! One cell, after the adapter has decoded it.
//!
//! The grid never sees a driver-native type. Adapters widen everything into
//! [`Value`] so that a `BIGINT` from MySQL and an `int8` from Postgres reach
//! the UI as the same thing, and so the renderer has a closed set to match on.

use serde::{Deserialize, Serialize};
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

impl Value {
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
        return if f.is_sign_negative() { "-Infinity" } else { "Infinity" }.into();
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
