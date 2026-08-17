//! Postgres wire values -> [`Value`].
//!
//! Dispatch is on the type name sqlx reports rather than on the OID, because
//! the name is what the grid header shows anyway and it keeps this table
//! readable.
//!
//! A type this build does not know decodes to [`Value::Unsupported`] carrying
//! its name, and one that *is* known but fails to decode falls through to the
//! same place. Neither aborts the query: a result set with one odd column
//! should still show the other forty.

use crate::adapter;
use crate::decode::attempt;
use dbui_domain::Value;
use sqlx::postgres::types::Oid;
use sqlx::postgres::PgRow;
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, Decimal as RustDecimal, JsonValue, Uuid};
use sqlx::Row;

/// Decode every column of one row.
pub fn decode_row(row: &PgRow) -> Vec<Value> {
    adapter::decode_row::<sqlx::Postgres>(row, decode_cell)
}

fn decode_cell(row: &PgRow, index: usize, reported_type: &str) -> Value {
    let type_name = reported_type.to_ascii_uppercase();
    match type_name.as_str() {
        "BOOL" => attempt!(row, index, bool, Value::Bool),
        "INT2" => attempt!(row, index, i16, |v| Value::Int(i64::from(v))),
        "INT4" => attempt!(row, index, i32, |v| Value::Int(i64::from(v))),
        "INT8" => attempt!(row, index, i64, Value::Int),
        // Postgres has no unsigned integers, so sqlx implements no `u32`
        // decode; `oid` gets its own newtype instead.
        "OID" => attempt!(row, index, Oid, |v: Oid| Value::Int(i64::from(v.0))),
        "FLOAT4" => attempt!(row, index, f32, |v| Value::Float(f64::from(v))),
        "FLOAT8" => attempt!(row, index, f64, Value::Float),
        // `rust_decimal` first, `BigDecimal` second, and the order matters.
        //
        // Postgres stores numerics in base-10000 groups, and sqlx's BigDecimal
        // conversion reports a scale rounded up to a multiple of four digits:
        // `numeric(10,2)` holding 0.10 comes back as "0.1000". Same number,
        // wrong column -- a money field should show the scale it was declared
        // with. `rust_decimal` honours the wire's display scale, so it renders
        // "0.10".
        //
        // It caps at 28 significant digits though, and Postgres numerics go to
        // 131072, so BigDecimal stays as the fallback for the ones that do not
        // fit. Those are rare and being off by trailing zeros beats refusing to
        // show the value.
        "NUMERIC" | "MONEY" => {
            attempt!(row, index, RustDecimal, |v: RustDecimal| Value::Decimal(
                v.to_string()
            ));
            attempt!(row, index, BigDecimal, |v: BigDecimal| Value::Decimal(
                v.to_string()
            ))
        }
        "TEXT" | "VARCHAR" | "CHAR" | "BPCHAR" | "NAME" | "CITEXT" | "XML" => {
            attempt!(row, index, String, Value::Text)
        }
        "UUID" => attempt!(row, index, Uuid, |v: Uuid| Value::Uuid(v.to_string())),
        "JSON" | "JSONB" => {
            attempt!(row, index, JsonValue, |v: JsonValue| Value::Json(
                v.to_string()
            ))
        }
        "BYTEA" => attempt!(row, index, Vec<u8>, Value::Bytes),
        "TIMESTAMP" => {
            attempt!(row, index, NaiveDateTime, |v: NaiveDateTime| Value::Temporal(
                v.format("%Y-%m-%d %H:%M:%S%.f").to_string()
            ))
        }
        "TIMESTAMPTZ" => {
            attempt!(row, index, DateTime<Utc>, |v: DateTime<Utc>| {
                Value::Temporal(v.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string())
            })
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
        // Arrays of the types worth carrying.
        //
        // sqlx names these by element with a `[]` suffix (`TEXT[]`), not by
        // the `_text` spelling `pg_type` uses internally -- a live test caught
        // that the hard way, via a `text[]` column that decoded to Unsupported.
        "BOOL[]" => attempt!(row, index, Vec<bool>, |v: Vec<bool>| array(v, Value::Bool)),
        "INT2[]" => {
            attempt!(row, index, Vec<i16>, |v: Vec<i16>| array(v, |n| Value::Int(
                i64::from(n)
            )))
        }
        "INT4[]" => {
            attempt!(row, index, Vec<i32>, |v: Vec<i32>| array(v, |n| Value::Int(
                i64::from(n)
            )))
        }
        "INT8[]" => attempt!(row, index, Vec<i64>, |v: Vec<i64>| array(v, Value::Int)),
        "FLOAT4[]" => {
            attempt!(row, index, Vec<f32>, |v: Vec<f32>| array(v, |n| Value::Float(
                f64::from(n)
            )))
        }
        "FLOAT8[]" => attempt!(row, index, Vec<f64>, |v: Vec<f64>| array(v, Value::Float)),
        "TEXT[]" | "VARCHAR[]" | "BPCHAR[]" | "CHAR[]" | "NAME[]" => {
            attempt!(row, index, Vec<String>, |v: Vec<String>| array(v, Value::Text))
        }
        "UUID[]" => {
            attempt!(row, index, Vec<Uuid>, |v: Vec<Uuid>| array(v, |u: Uuid| {
                Value::Uuid(u.to_string())
            }))
        }
        "NUMERIC[]" => {
            attempt!(row, index, Vec<RustDecimal>, |v: Vec<RustDecimal>| array(
                v,
                |d: RustDecimal| Value::Decimal(d.to_string())
            ))
        }
        _ => {}
    }

    // Two last chances before giving up: many Postgres types (enums, domains,
    // `interval`, `inet`, ranges) have a text representation sqlx will hand
    // over as a String, and a genuine NULL in an unknown column is still a
    // NULL worth showing as one.
    if let Ok(text) = row.try_get::<Option<String>, _>(index) {
        return text.map(Value::Text).unwrap_or(Value::Null);
    }
    if let Ok(None) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return Value::Null;
    }

    Value::Unsupported(type_name.to_ascii_lowercase())
}

fn array<T>(items: Vec<T>, mut wrap: impl FnMut(T) -> Value) -> Value {
    Value::Array(items.into_iter().map(&mut wrap).collect())
}
