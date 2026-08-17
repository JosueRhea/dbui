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
/// while another never appears at all. A column read that fails therefore
/// takes the whole open with it rather than being defaulted away: rows read
/// without a key look like an ordinary page while paging past them silently
/// repeats and skips them, and the grid would offer edits it has no key to
/// write.
pub fn open_table(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    page: Page,
    where_clause: String,
    sort: Option<SortKey>,
) -> Task<Outcome<TableContents>> {
    runtime.spawn(async move {
        let columns = driver.columns(&table).await?;
        let order = dbui_domain::order_for(sort.as_ref(), &key_columns(&columns));

        let rows = driver.table_rows(&table, page, &where_clause, &order).await?;
        // The count is the one part of the read allowed to fail on its own: on
        // a large table it is the slowest of the three statements, and rows the
        // user can already see are worth more than the total above them. `None`
        // renders as an unknown count rather than as zero.
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

/// Run several statements in order. Stops on the first error.
///
/// The UI shows the last result that returned rows and a batch summary for the
/// status bar.
pub fn run_queries(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    statements: Vec<String>,
) -> Task<Outcome<BatchQueryResult>> {
    runtime.spawn(async move {
        let mut results = Vec::with_capacity(statements.len());
        let mut last_rows: Option<QueryResult> = None;
        let mut total_elapsed = std::time::Duration::ZERO;

        for sql in statements {
            let result = driver.execute(&sql).await?;
            total_elapsed += result.stats.elapsed;
            if matches!(result.outcome, QueryOutcome::Rows(_)) {
                last_rows = Some(result.clone());
            }
            results.push(result);
        }

        Ok(BatchQueryResult {
            results,
            last_rows,
            total_elapsed,
        })
    })
}

/// Outcome of running more than one statement.
pub struct BatchQueryResult {
    pub results: Vec<QueryResult>,
    /// Last statement that produced a row set, if any.
    pub last_rows: Option<QueryResult>,
    pub total_elapsed: std::time::Duration,
}

impl BatchQueryResult {
    /// One-line status for a finished batch.
    pub fn summary(&self) -> String {
        let n = self.results.len();
        let ms = self.total_elapsed.as_secs_f64() * 1000.0;
        let stmt = if n == 1 { "statement" } else { "statements" };
        if let Some(last) = self.results.last() {
            match &last.outcome {
                QueryOutcome::Rows(set) => {
                    let plural = if set.rows.len() == 1 { "row" } else { "rows" };
                    let truncated = if set.truncated { "+" } else { "" };
                    format!(
                        "{n} {stmt} · {}{} {plural} in {ms:.0} ms",
                        set.rows.len(),
                        truncated
                    )
                }
                QueryOutcome::Affected(count) => {
                    let plural = if *count == 1 { "row" } else { "rows" };
                    format!("{n} {stmt} · {count} {plural} affected in {ms:.0} ms")
                }
            }
        } else {
            format!("{n} {stmt} in {ms:.0} ms")
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use dbui_domain::{Driver, QueryStats};
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    /// A driver whose catalog answers are whatever the test says they are.
    struct StubDriver {
        columns: Outcome<Vec<Column>>,
        row_count: Outcome<i64>,
    }

    impl StubDriver {
        /// A table with one primary-key column and seven rows.
        fn healthy() -> Self {
            Self {
                columns: Ok(vec![Column {
                    name: "id".into(),
                    data_type: "integer".into(),
                    nullable: false,
                    default: None,
                    is_primary_key: true,
                    ordinal: 1,
                    references: None,
                }]),
                row_count: Ok(7),
            }
        }
    }

    #[async_trait]
    impl DatabaseDriver for StubDriver {
        fn driver(&self) -> Driver {
            Driver::Sqlite
        }

        fn server_version(&self) -> &str {
            "stub"
        }

        async fn ping(&self) -> Outcome<()> {
            Ok(())
        }

        async fn catalog(&self) -> Outcome<Catalog> {
            Ok(Catalog::default())
        }

        async fn columns(&self, _table: &TableRef) -> Outcome<Vec<Column>> {
            self.columns.clone()
        }

        async fn table_rows(
            &self,
            _table: &TableRef,
            _page: Page,
            _where_clause: &str,
            _order: &[SortKey],
        ) -> Outcome<ResultSet> {
            Ok(ResultSet::default())
        }

        async fn row_count(&self, _table: &TableRef, _where_clause: &str) -> Outcome<i64> {
            self.row_count.clone()
        }

        async fn apply_changes(&self, _table: &TableRef, _batch: &RowBatch) -> Outcome<u64> {
            Ok(0)
        }

        async fn execute(&self, sql: &str) -> Outcome<QueryResult> {
            Ok(QueryResult {
                statement: sql.to_string(),
                outcome: QueryOutcome::Affected(0),
                stats: QueryStats {
                    elapsed: std::time::Duration::ZERO,
                },
            })
        }

        async fn close(&self) {}
    }

    fn open(driver: StubDriver) -> Outcome<TableContents> {
        let runtime = DbRuntime::new().expect("runtime");
        let task = open_table(
            &runtime,
            Arc::new(driver),
            TableRef::new("main", "widgets"),
            Page::first(),
            String::new(),
            None,
        );
        wait_for(task).expect("the task was dropped")
    }

    /// Wait for a [`Task`] on the test thread: the work itself already runs on
    /// the runtime's own threads, so this only has to watch for the answer.
    fn wait_for<T>(mut task: Task<T>) -> Option<T> {
        loop {
            let mut cx = Context::from_waker(Waker::noop());
            match Pin::new(&mut task).poll(&mut cx) {
                Poll::Ready(output) => return output,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    #[test]
    fn a_table_whose_columns_will_not_read_fails_rather_than_opening_keyless() {
        let error = DriverError::Catalog("permission denied for schema main".into());
        let opened = open(StubDriver {
            columns: Err(error.clone()),
            ..StubDriver::healthy()
        });
        // Defaulting the columns away would open the table unordered and
        // uneditable, with nothing on screen to say why.
        assert_eq!(opened.err(), Some(error));
    }

    #[test]
    fn a_count_that_will_not_read_still_opens_the_page_it_counts() {
        let opened = open(StubDriver {
            row_count: Err(DriverError::Catalog("count timed out".into())),
            ..StubDriver::healthy()
        })
        .expect("the page itself read");
        assert_eq!(opened.total_rows, None);
        assert!(opened.is_ordered(), "the key read, so the page is ordered");
    }

    #[test]
    fn a_healthy_read_carries_the_count_and_the_columns() {
        let opened = open(StubDriver::healthy()).expect("read");
        assert_eq!(opened.total_rows, Some(7));
        assert_eq!(opened.columns.len(), 1);
    }
}
