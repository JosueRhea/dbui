//! What the two type-name-dispatched decoders share.
//!
//! Postgres and MySQL both decode a cell by matching on the type name sqlx
//! reports and asking sqlx for the Rust type that name implies. Only the table
//! of names differs, so the attempt-and-fall-through step lives here.

/// Try `Option<T>` at `index`, and hand the caller a `Value` only if sqlx
/// agreed to decode it. `Ok(None)` is a real SQL NULL.
///
/// Expands to a `return`, so a decoder can list several types for one column
/// and have the first that decodes win -- and a type that is named but fails
/// falls through to the caller's own fallback rather than failing the query.
macro_rules! attempt {
    ($row:expr, $index:expr, $ty:ty, $wrap:expr) => {
        match $row.try_get::<Option<$ty>, _>($index) {
            Ok(Some(value)) => return $wrap(value),
            Ok(None) => return Value::Null,
            Err(_) => {}
        }
    };
}

pub(crate) use attempt;
