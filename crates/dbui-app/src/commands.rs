//! The use cases, one function each.
//!
//! Each takes what it needs and returns a [`Task`] the UI awaits. They are
//! deliberately free functions rather than methods on [`Workspace`]: the
//! workspace is state the UI mutates when a task *lands*, and a use case must
//! not hold a borrow of it across an await.

use crate::runtime::{DbRuntime, Task};
use dbui_domain::{
    Catalog, Column, ConnectionConfig, Page, QueryOutcome, QueryResult, ResultSet, SortKey,
    TableKind, TableRef, Value,
};
use dbui_driver::{DatabaseDriver, DriverError, RowBatch, RowUpdate};
use std::sync::Arc;

pub type Outcome<T> = Result<T, DriverError>;

/// Open a connection and read its catalog in one trip.
pub fn connect(
    runtime: &DbRuntime,
    config: ConnectionConfig,
) -> Task<Outcome<(Arc<dyn DatabaseDriver>, Catalog)>> {
    runtime.spawn(async move {
        let driver = dbui_driver::connect(&config).await?;
        let catalog = driver.catalog().await?;
        Ok((driver, catalog))
    })
}

/// Re-read the tree for an already-open connection.
pub fn refresh_catalog(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
) -> Task<Outcome<Catalog>> {
    runtime.spawn(async move { driver.catalog().await })
}

/// A page of a table's rows, plus its columns and total size.
///
/// The columns are read *first*, because their primary key is what the page is
/// ordered by. An unordered `LIMIT`/`OFFSET` is not pagination: the engine may
/// return rows in any order it likes, so the same row can appear on two pages
/// while another never appears at all.
pub fn open_table(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    page: Page,
    where_clause: String,
    sort: Option<SortKey>,
) -> Task<Outcome<TableContents>> {
    runtime.spawn(async move {
        let columns = driver.columns(&table).await.unwrap_or_default();
        let order = dbui_domain::order_for(sort.as_ref(), &key_columns(&columns));

        let rows = driver
            .table_rows(&table, page, &where_clause, &order)
            .await?;
        let total_rows = driver.row_count(&table, &where_clause).await.ok();

        Ok(TableContents {
            table,
            page,
            where_clause,
            sort,
            rows,
            columns,
            total_rows,
        })
    })
}

/// The primary-key columns, in the order the table declares them.
fn key_columns(columns: &[Column]) -> Vec<String> {
    let mut key: Vec<&Column> = columns.iter().filter(|c| c.is_primary_key).collect();
    key.sort_by_key(|column| column.ordinal);
    key.into_iter().map(|column| column.name.clone()).collect()
}

/// Everything the table view shows at once.
pub struct TableContents {
    pub table: TableRef,
    pub page: Page,
    pub where_clause: String,
    /// The sort this page was read with, echoed back so the header can draw
    /// its arrow against the data actually on screen.
    pub sort: Option<SortKey>,
    pub rows: ResultSet,
    pub columns: Vec<Column>,
    pub total_rows: Option<i64>,
}

impl TableContents {
    /// Whether this page is in a defined order at all.
    ///
    /// False for a keyless table or view with no sort chosen: there is nothing
    /// to order by that is guaranteed cheap, so the read is left unordered and
    /// the UI says so rather than pretending the paging is stable.
    pub fn is_ordered(&self) -> bool {
        self.sort.is_some() || self.columns.iter().any(|column| column.is_primary_key)
    }
}

/// Persist edits to one row identified by its primary key.
pub fn update_row(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    pk: Vec<(String, Value)>,
    changes: Vec<(String, Value)>,
) -> Task<Outcome<u64>> {
    runtime.spawn(async move { driver.update_row(&table, &pk, &changes).await })
}

/// Persist several row edits in one transaction (all commit or all roll back).
pub fn update_rows(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    rows: Vec<RowUpdate>,
) -> Task<Outcome<u64>> {
    runtime.spawn(async move { driver.update_rows(&table, &rows).await })
}

/// Commit a whole staged batch -- edits and deletions -- in one transaction.
///
/// One call rather than two so "commit everything" means what it says: a
/// delete that fails takes the edits down with it instead of leaving the table
/// half-written.
pub fn apply_changes(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    batch: RowBatch,
) -> Task<Outcome<u64>> {
    runtime.spawn(async move { driver.apply_changes(&table, &batch).await })
}

/// `TRUNCATE` a table. The statement is built by the driver, which is what
/// quotes the identifier.
pub fn truncate_table(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
) -> Task<Outcome<QueryResult>> {
    let sql = dbui_driver::truncate_sql(driver.driver(), &table);
    runtime.spawn(async move { driver.execute(&sql).await })
}

pub fn drop_relation(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    kind: TableKind,
) -> Task<Outcome<QueryResult>> {
    let sql = dbui_driver::drop_sql(driver.driver(), &table, kind);
    runtime.spawn(async move { driver.execute(&sql).await })
}

/// Run the statement in the editor.
pub fn run_query(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    sql: String,
) -> Task<Outcome<QueryResult>> {
    runtime.spawn(async move { driver.execute(&sql).await })
}

/// Load columns for one table (SQL autocomplete cache).
pub fn fetch_columns(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
) -> Task<Outcome<(TableRef, Vec<Column>)>> {
    runtime.spawn(async move {
        let columns = driver.columns(&table).await?;
        Ok((table, columns))
    })
}

/// Run several statements in order, stopping at the first that fails.
///
/// The failure is carried back *inside* the batch rather than as the whole
/// call's `Err`. A run of five statements that fails on the third really did
/// run the first two, and returning only the error threw their results away --
/// which left the user looking at an empty grid and a one-line footer, with no
/// way to tell how far the batch got.
pub fn run_queries(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    statements: Vec<String>,
) -> Task<Outcome<BatchQueryResult>> {
    runtime.spawn(async move {
        let attempted = statements.len();
        let mut results = Vec::with_capacity(attempted);
        let mut last_rows: Option<QueryResult> = None;
        let mut total_elapsed = std::time::Duration::ZERO;
        let mut failure = None;

        for sql in statements {
            match driver.execute(&sql).await {
                Ok(result) => {
                    total_elapsed += result.stats.elapsed;
                    if matches!(result.outcome, QueryOutcome::Rows(_)) {
                        last_rows = Some(result.clone());
                    }
                    results.push(result);
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }

        Ok(BatchQueryResult {
            results,
            last_rows,
            total_elapsed,
            failure,
            attempted,
        })
    })
}

/// Outcome of running one or more statements.
pub struct BatchQueryResult {
    /// Every statement that ran, in order. Stops short of `attempted` when one
    /// of them failed.
    pub results: Vec<QueryResult>,
    /// Last statement that produced a row set, if any.
    pub last_rows: Option<QueryResult>,
    pub total_elapsed: std::time::Duration,
    /// Why the run stopped early, if it did.
    pub failure: Option<DriverError>,
    /// How many statements were sent, counting the one that failed.
    pub attempted: usize,
}

impl BatchQueryResult {
    /// One-line status for a finished batch.
    ///
    /// A lone statement speaks for itself: "1 statement ·" in front of its own
    /// verdict is a prefix that says nothing.
    pub fn summary(&self) -> String {
        let n = self.results.len();
        let ms = self.total_elapsed.as_secs_f64() * 1000.0;
        let stmt = if n == 1 { "statement" } else { "statements" };
        match self.results.last() {
            Some(last) if n == 1 => format!("{} in {ms:.0} ms", last.verdict()),
            Some(last) => format!("{n} {stmt} · {} in {ms:.0} ms", last.verdict()),
            None => format!("{n} {stmt} in {ms:.0} ms"),
        }
    }

    /// Whether any statement that *ran* changed the shape of the database, and
    /// so left the schema tree describing something that is no longer there.
    pub fn changed_the_catalog(&self) -> bool {
        self.results
            .iter()
            .any(|result| dbui_domain::statement::describe(&result.statement).changes_catalog)
    }
}

/// Dial a config without keeping the connection -- the "Test" button.
pub fn test_connection(runtime: &DbRuntime, config: ConnectionConfig) -> Task<Outcome<String>> {
    runtime.spawn(async move {
        let driver = dbui_driver::connect(&config).await?;
        driver.ping().await?;
        let version = driver.server_version().to_string();
        driver.close().await;
        Ok(version)
    })
}

/// Close a pool without blocking the UI on it.
pub fn disconnect(runtime: &DbRuntime, driver: Arc<dyn DatabaseDriver>) -> Task<()> {
    runtime.spawn(async move { driver.close().await })
}
