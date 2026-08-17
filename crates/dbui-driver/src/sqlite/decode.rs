//! Turning SQLite wire values into [`Value`].
//!
//! SQLite has five storage classes -- NULL, INTEGER, REAL, TEXT, BLOB -- and a
//! *declared* column type that is only an affinity. A `datetime` column really
//! holds text, and a `boolean` column really holds 0 or 1. So a value is
//! decoded from the class it is actually stored in, and the declared type is
//! consulted only to decide how to present it.
//!
//! Two consequences worth knowing, both of them the honest reading rather than
//! a limitation to work around:
//!
//! - **There is no exact numeric type.** A `NUMERIC` column stores a REAL or
//!   an INTEGER, so a price arrives as [`Value::Float`]. Reporting it as
//!   [`Value::Decimal`] would claim an exactness the file does not have.
//! - **sqlx reports the declared type only for the spellings it knows.**
//!   `BOOLEAN` and `DATETIME` come through; `NUMERIC` and `JSON` do not, and
//!   those columns decode as the class they are stored in. The structure pane
//!   still shows what the table declared, because that comes from
//!   `pragma_table_info` rather than from the wire.

use crate::adapter;
use dbui_domain::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row as _, TypeInfo as _, ValueRef as _};

pub fn decode_row(row: &SqliteRow) -> Vec<Value> {
    adapter::decode_row::<sqlx::Sqlite>(row, decode_cell)
}

fn decode_cell(row: &SqliteRow, index: usize, declared: &str) -> Value {
    // The *storage class* of this particular value, which is what SQLite
    // actually holds. Asking for an `i64` first would not do: sqlx coerces,
    // so a REAL of 99.95 comes back as 99 and a money column is silently
    // truncated.
    let storage = match row.try_get_raw(index) {
        Ok(raw) if raw.is_null() => return Value::Null,
        Ok(raw) => raw.type_info().name().to_ascii_uppercase(),
        Err(_) => return Value::Unsupported(declared.to_string()),
    };

    // The declared type is only an affinity -- a `datetime` column really
    // holds text -- so it decides presentation, never which class was stored.
    let affinity = declared.to_ascii_uppercase();

    match storage.as_str() {
        "INTEGER" => match row.try_get::<i64, _>(index) {
            // SQLite has no boolean class: a `boolean` column holds 0 or 1,
            // and the declared type is the only thing that says which it
            // meant.
            Ok(number) if affinity.contains("BOOL") => Value::Bool(number != 0),
            Ok(number) => Value::Int(number),
            Err(_) => Value::Unsupported(declared.to_string()),
        },
        // A REAL is an IEEE double however the column was declared, so it is
        // reported as one. See the module note: there is no exact numeric
        // type here to promote it to.
        "REAL" => match row.try_get::<f64, _>(index) {
            Ok(number) => Value::Float(number),
            Err(_) => Value::Unsupported(declared.to_string()),
        },
        "BLOB" => match row.try_get::<Vec<u8>, _>(index) {
            Ok(bytes) => Value::Bytes(bytes),
            Err(_) => Value::Unsupported(declared.to_string()),
        },
        _ => match row.try_get::<String, _>(index) {
            Ok(text) if affinity.contains("JSON") => Value::Json(text),
            Ok(text) if affinity.contains("UUID") => Value::Uuid(text),
            Ok(text) if affinity.contains("DATE") || affinity.contains("TIME") => {
                Value::Temporal(text)
            }
            // Text in a NUMERIC column is exact as written, so it is kept
            // rather than parsed into a float that would round it.
            Ok(text) if affinity.contains("NUMERIC") || affinity.contains("DECIMAL") => {
                Value::Decimal(text)
            }
            Ok(text) => Value::Text(text),
            Err(_) => Value::Unsupported(declared.to_string()),
        },
    }
}

