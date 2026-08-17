//! MySQL wire values -> [`Value`].
//!
//! Same contract as the Postgres decoder: dispatch on the reported type name,
//! degrade to [`Value::Unsupported`] rather than failing the query.

use crate::adapter;
use crate::decode::attempt;
use dbui_domain::Value;
use sqlx::mysql::MySqlRow;
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, JsonValue};
use sqlx::Row;

pub fn decode_row(row: &MySqlRow) -> Vec<Value> {
    adapter::decode_row::<sqlx::MySql>(row, decode_cell)
}

fn decode_cell(row: &MySqlRow, index: usize, reported_type: &str) -> Value {
    let type_name = reported_type.to_ascii_uppercase();
    match type_name.as_str() {
        // MySQL has no real boolean: `BOOLEAN` is an alias for `TINYINT(1)`,
        // and sqlx reports exactly that column as `BOOLEAN` -- there is no way
        // to tell "I meant true/false" from "I meant a one-digit integer".
        //
        // So it stays a number. `TINYINT(1)` can hold 7, and rendering that as
        // `true` would be an editor lying about what is stored; 1 and 0 read
        // fine as booleans anyway, and the structure pane still shows the
        // declared `tinyint(1)`.
        "BOOLEAN" | "TINYINT" => attempt!(row, index, i8, |v| Value::Int(i64::from(v))),
        "TINYINT UNSIGNED" => attempt!(row, index, u8, |v| Value::Int(i64::from(v))),
        "SMALLINT" => attempt!(row, index, i16, |v| Value::Int(i64::from(v))),
        "SMALLINT UNSIGNED" => attempt!(row, index, u16, |v| Value::Int(i64::from(v))),
        "MEDIUMINT" | "INT" | "INTEGER" => attempt!(row, index, i32, |v| Value::Int(i64::from(v))),
        "MEDIUMINT UNSIGNED" | "INT UNSIGNED" => {
            attempt!(row, index, u32, |v| Value::Int(i64::from(v)))
        }
        "BIGINT" => attempt!(row, index, i64, Value::Int),
        // A `BIGINT UNSIGNED` above 2^63 has no `i64` to land in. Keeping it
        // as an exact decimal string beats wrapping it into a negative number.
        "BIGINT UNSIGNED" => {
            attempt!(row, index, u64, |v: u64| match i64::try_from(v) {
                Ok(fits) => Value::Int(fits),
                Err(_) => Value::Decimal(v.to_string()),
            })
        }
        "FLOAT" => attempt!(row, index, f32, |v| Value::Float(f64::from(v))),
        "DOUBLE" => attempt!(row, index, f64, Value::Float),
        "DECIMAL" | "NEWDECIMAL" => {
            attempt!(row, index, BigDecimal, |v: BigDecimal| Value::Decimal(
                v.to_string()
            ))
        }
        "VARCHAR" | "CHAR" | "TEXT" | "TINYTEXT" | "MEDIUMTEXT" | "LONGTEXT" | "ENUM" | "SET" => {
            attempt!(row, index, String, Value::Text)
        }
        "JSON" => {
            attempt!(row, index, JsonValue, |v: JsonValue| Value::Json(
                v.to_string()
            ))
        }
        "BINARY" | "VARBINARY" | "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BIT" => {
            attempt!(row, index, Vec<u8>, Value::Bytes)
        }
        "DATE" => {
            attempt!(row, index, NaiveDate, |v: NaiveDate| Value::Temporal(
                v.to_string()
            ))
        }
        "TIME" => {
            attempt!(row, index, NaiveTime, |v: NaiveTime| Value::Temporal(
                v.to_string()
            ))
        }
        "DATETIME" => {
            attempt!(row, index, NaiveDateTime, |v: NaiveDateTime| Value::Temporal(
                v.format("%Y-%m-%d %H:%M:%S%.f").to_string()
            ))
        }
        // MySQL stores TIMESTAMP in UTC and converts on the way out; sqlx
        // hands it over already anchored, unlike DATETIME.
        "TIMESTAMP" => {
            attempt!(row, index, DateTime<Utc>, |v: DateTime<Utc>| {
                Value::Temporal(v.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string())
            })
        }
        "YEAR" => attempt!(row, index, u16, |v| Value::Int(i64::from(v))),
        _ => {}
    }

    // `TIME` can hold an interval (`838:59:59`, or a negative one) that is not
    // a clock time; text is the only faithful rendering left. This also
    // catches types added by a newer server than this decoder knows about.
    if let Ok(text) = row.try_get::<Option<String>, _>(index) {
        return text.map(Value::Text).unwrap_or(Value::Null);
    }
    if let Ok(bytes) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return bytes.map(Value::Bytes).unwrap_or(Value::Null);
    }

    Value::Unsupported(type_name.to_ascii_lowercase())
}
