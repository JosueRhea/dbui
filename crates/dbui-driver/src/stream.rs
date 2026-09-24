//! Reading a query's every row a page at a time, for an export that is not
//! bound by the editor's row cap.

use crate::error::{DriverError, Result};
use dbui_domain::{ColumnInfo, ResultSet, Value};
use futures_util::{Stream, TryStreamExt as _};

/// Where streamed rows go: the columns, then each page of values.
pub type RowSink<'a> =
    dyn FnMut(&[ColumnInfo], Vec<Vec<Value>>) -> std::result::Result<(), String> + Send + 'a;

/// Rows handed to the sink at a time: enough that the sink's own overhead
/// is not the cost, few enough that memory stays flat on any size of result.
const PAGE: usize = 1_000;

/// Drain `rows`, decoding each page with `build`, into `sink`. Resolves to
/// how many rows went through.
pub(crate) async fn drain<R>(
    rows: impl Stream<Item = std::result::Result<R, sqlx::Error>>,
    sql: &str,
    build: impl Fn(Vec<R>) -> ResultSet,
    sink: &mut RowSink<'_>,
) -> Result<u64> {
    let mut rows = std::pin::pin!(rows);
    let mut page = Vec::with_capacity(PAGE);
    let mut total = 0u64;
    loop {
        let next = rows
            .try_next()
            .await
            .map_err(|error| DriverError::query(sql, &error))?;
        let done = next.is_none();
        if let Some(row) = next {
            page.push(row);
        }
        if page.len() == PAGE || (done && !page.is_empty()) {
            let set = build(std::mem::take(&mut page));
            total += set.rows.len() as u64;
            let values = set.rows.into_iter().map(|row| row.0).collect();
            sink(&set.columns, values).map_err(|message| DriverError::message(sql, message))?;
        }
        if done {
            return Ok(total);
        }
    }
}
