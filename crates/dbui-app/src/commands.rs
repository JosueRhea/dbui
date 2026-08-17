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

        let rows = driver.table_rows(&table, page, &where_clause, &order).await?;
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
    use dbui_domain::{ColumnInfo, Driver, QueryStats, Row, Schema, Table, TlsMode};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    /// What a fake driver was asked to do, in the order it was asked.
    #[derive(Default)]
    struct Calls {
        orders: Vec<Vec<SortKey>>,
        where_clauses: Vec<String>,
        statements: Vec<String>,
        batches: Vec<RowBatch>,
    }

    /// A driver that answers out of memory and remembers the questions.
    ///
    /// The use cases are thin on purpose -- what is worth proving about them is
    /// *what they ask the driver for*, and in which order. A real server can
    /// only confirm the answer.
    #[derive(Default)]
    struct FakeDriver {
        columns: Vec<Column>,
        rows: ResultSet,
        catalog: Catalog,
        total_rows: i64,
        /// Set to fail `columns`, `table_rows`, `row_count` or `catalog`.
        columns_error: Option<DriverError>,
        rows_error: Option<DriverError>,
        count_error: Option<DriverError>,
        catalog_error: Option<DriverError>,
        /// `execute` fails for any statement containing this.
        poison: Option<String>,
        calls: Mutex<Calls>,
        closed: AtomicBool,
    }

    impl FakeDriver {
        fn with_columns(columns: Vec<Column>) -> Self {
            Self {
                columns,
                ..Self::default()
            }
        }

        fn calls(&self) -> std::sync::MutexGuard<'_, Calls> {
            self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl DatabaseDriver for FakeDriver {
        fn driver(&self) -> Driver {
            Driver::Postgres
        }

        fn server_version(&self) -> &str {
            "Fake 1.0"
        }

        async fn ping(&self) -> Outcome<()> {
            Ok(())
        }

        async fn catalog(&self) -> Outcome<Catalog> {
            match &self.catalog_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.catalog.clone()),
            }
        }

        async fn columns(&self, _table: &TableRef) -> Outcome<Vec<Column>> {
            match &self.columns_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.columns.clone()),
            }
        }

        async fn table_rows(
            &self,
            _table: &TableRef,
            _page: Page,
            where_clause: &str,
            order: &[SortKey],
        ) -> Outcome<ResultSet> {
            {
                let mut calls = self.calls();
                calls.orders.push(order.to_vec());
                calls.where_clauses.push(where_clause.to_string());
            }
            match &self.rows_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.rows.clone()),
            }
        }

        async fn row_count(&self, _table: &TableRef, _where_clause: &str) -> Outcome<i64> {
            match &self.count_error {
                Some(error) => Err(error.clone()),
                None => Ok(self.total_rows),
            }
        }

        async fn apply_changes(&self, _table: &TableRef, batch: &RowBatch) -> Outcome<u64> {
            self.calls().batches.push(batch.clone());
            Ok(batch.len() as u64)
        }

        async fn execute(&self, sql: &str) -> Outcome<QueryResult> {
            self.calls().statements.push(sql.to_string());
            if let Some(poison) = &self.poison {
                if sql.contains(poison.as_str()) {
                    return Err(DriverError::message(sql, "no"));
                }
            }
            Ok(affected(sql, 1, 5))
        }

        async fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// One shared runtime for every test.
    ///
    /// It is never dropped: a `DbRuntime` owns a tokio runtime, and dropping
    /// one from inside an async test is a panic.
    fn runtime() -> &'static DbRuntime {
        static RUNTIME: OnceLock<DbRuntime> = OnceLock::new();
        RUNTIME.get_or_init(|| DbRuntime::new().expect("runtime"))
    }

    fn users() -> TableRef {
        TableRef::new("public", "users")
    }

    fn column(name: &str, ordinal: i32, is_primary_key: bool) -> Column {
        Column {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_primary_key,
            ordinal,
            references: None,
        }
    }

    fn rows(count: usize, truncated: bool) -> ResultSet {
        ResultSet {
            columns: vec![ColumnInfo {
                name: "id".into(),
                type_name: "int8".into(),
            }],
            rows: vec![Row(vec![Value::Int(1)]); count],
            truncated,
        }
    }

    fn affected(sql: &str, count: u64, ms: u64) -> QueryResult {
        QueryResult {
            statement: sql.to_string(),
            outcome: QueryOutcome::Affected(count),
            stats: QueryStats {
                elapsed: Duration::from_millis(ms),
            },
        }
    }

    fn returning(sql: &str, set: ResultSet, ms: u64) -> QueryResult {
        QueryResult {
            statement: sql.to_string(),
            outcome: QueryOutcome::Rows(set),
            stats: QueryStats {
                elapsed: Duration::from_millis(ms),
            },
        }
    }

    fn batch(rows: Vec<QueryResult>) -> BatchQueryResult {
        let total_elapsed = rows.iter().map(|r| r.stats.elapsed).sum();
        BatchQueryResult {
            results: rows,
            last_rows: None,
            total_elapsed,
        }
    }

    /// The order a page is read in is the user's sort followed by the whole
    /// primary key, and the key arrives in the order the table declares it --
    /// not the order the catalog happened to list the columns in.
    #[tokio::test]
    async fn a_page_is_ordered_by_the_sort_then_the_declared_key() {
        let driver = Arc::new(FakeDriver::with_columns(vec![
            column("id", 2, true),
            column("name", 3, false),
            column("tenant", 1, true),
        ]));
        let runtime = runtime();

        open_table(
            runtime,
            driver.clone(),
            users(),
            Page::first(),
            String::new(),
            None,
        )
        .await
        .expect("task")
        .expect("page");

        open_table(
            runtime,
            driver.clone(),
            users(),
            Page::first(),
            String::new(),
            Some(SortKey::desc("name")),
        )
        .await
        .expect("task")
        .expect("page");

        let calls = driver.calls();
        assert_eq!(
            calls.orders[0],
            vec![SortKey::asc("tenant"), SortKey::asc("id")]
        );
        assert_eq!(
            calls.orders[1],
            vec![
                SortKey::desc("name"),
                SortKey::asc("tenant"),
                SortKey::asc("id")
            ]
        );
    }

    /// The predicate reaches the driver, and the same one is used for the count
    /// -- a filtered page that reported the whole table's size would page past
    /// its own last row.
    #[tokio::test]
    async fn a_page_carries_its_predicate_and_its_total() {
        let driver = Arc::new(FakeDriver {
            columns: vec![column("id", 1, true)],
            rows: rows(2, false),
            total_rows: 17,
            ..FakeDriver::default()
        });

        let contents = open_table(
            runtime(),
            driver.clone(),
            users(),
            Page::first(),
            "id > 3".into(),
            None,
        )
        .await
        .expect("task")
        .expect("page");

        assert_eq!(driver.calls().where_clauses, vec!["id > 3".to_string()]);
        assert_eq!(contents.where_clause, "id > 3");
        assert_eq!(contents.total_rows, Some(17));
        assert_eq!(contents.rows.rows.len(), 2);
        assert!(contents.is_ordered(), "the key orders it");
    }

    /// A server that will not count still shows the rows it did return: the
    /// count feeds a label, and losing the label is not worth losing the page.
    #[tokio::test]
    async fn a_page_survives_a_count_that_fails() {
        let driver = Arc::new(FakeDriver {
            columns: vec![column("id", 1, true)],
            rows: rows(1, false),
            count_error: Some(DriverError::Closed),
            ..FakeDriver::default()
        });

        let contents = open_table(
            runtime(),
            driver,
            users(),
            Page::first(),
            String::new(),
            None,
        )
        .await
        .expect("task")
        .expect("page");

        assert_eq!(contents.total_rows, None);
        assert_eq!(contents.rows.rows.len(), 1);
    }

    /// Columns that cannot be read leave the page unordered rather than
    /// failing it -- but the UI is told, so it does not present unstable paging
    /// as if it were stable.
    #[tokio::test]
    async fn columns_that_cannot_be_read_leave_the_page_unordered() {
        let driver = Arc::new(FakeDriver {
            columns_error: Some(DriverError::Catalog("denied".into())),
            rows: rows(1, false),
            ..FakeDriver::default()
        });

        let contents = open_table(
            runtime(),
            driver.clone(),
            users(),
            Page::first(),
            String::new(),
            None,
        )
        .await
        .expect("task")
        .expect("page");

        assert!(contents.columns.is_empty());
        assert!(driver.calls().orders[0].is_empty());
        assert!(!contents.is_ordered());
    }

    /// A sort the user chose is an order in its own right, key or no key.
    #[tokio::test]
    async fn a_keyless_table_is_ordered_once_the_user_sorts_it() {
        let driver = Arc::new(FakeDriver {
            columns: vec![column("name", 1, false)],
            ..FakeDriver::default()
        });

        let contents = open_table(
            runtime(),
            driver,
            users(),
            Page::first(),
            String::new(),
            Some(SortKey::asc("name")),
        )
        .await
        .expect("task")
        .expect("page");

        assert!(contents.is_ordered());
    }

    /// The read itself failing *is* reported: there is no page to show.
    #[tokio::test]
    async fn a_failed_read_is_reported_rather_than_shown_as_empty() {
        let driver = Arc::new(FakeDriver {
            rows_error: Some(DriverError::message("SELECT", "syntax error")),
            ..FakeDriver::default()
        });

        let outcome = open_table(
            runtime(),
            driver,
            users(),
            Page::first(),
            String::new(),
            None,
        )
        .await
        .expect("task");

        assert!(matches!(outcome, Err(DriverError::Query { .. })));
    }

    #[tokio::test]
    async fn a_refreshed_catalog_comes_back_whole_and_its_failure_is_not_hidden() {
        let catalog = Catalog {
            schemas: vec![Schema {
                name: "public".into(),
                tables: vec![Table {
                    schema: "public".into(),
                    name: "users".into(),
                    kind: TableKind::Table,
                }],
            }],
        };
        let driver = Arc::new(FakeDriver {
            catalog: catalog.clone(),
            ..FakeDriver::default()
        });
        let runtime = runtime();

        let read = refresh_catalog(runtime, driver)
            .await
            .expect("task")
            .expect("catalog");
        assert_eq!(read, catalog);

        let broken = Arc::new(FakeDriver {
            catalog_error: Some(DriverError::Catalog("permission denied".into())),
            ..FakeDriver::default()
        });
        assert!(matches!(
            refresh_catalog(runtime, broken).await.expect("task"),
            Err(DriverError::Catalog(_))
        ));
    }

    #[tokio::test]
    async fn fetching_columns_answers_with_the_table_they_belong_to() {
        let driver = Arc::new(FakeDriver::with_columns(vec![column("id", 1, true)]));
        let runtime = runtime();

        let (table, columns) = fetch_columns(runtime, driver, users())
            .await
            .expect("task")
            .expect("columns");
        assert_eq!(table, users());
        assert_eq!(columns.len(), 1);

        let broken = Arc::new(FakeDriver {
            columns_error: Some(DriverError::Catalog("gone".into())),
            ..FakeDriver::default()
        });
        assert!(fetch_columns(runtime, broken, users())
            .await
            .expect("task")
            .is_err());
    }

    /// One row's edits and a whole batch both arrive as a single batch, which
    /// is the only write primitive the port has -- that is what makes "commit
    /// everything" one transaction.
    #[tokio::test]
    async fn every_write_reaches_the_driver_as_one_batch() {
        let driver = Arc::new(FakeDriver::default());
        let runtime = runtime();
        let pk = vec![("id".to_string(), Value::Int(1))];
        let changes = vec![("name".to_string(), Value::Text("ada".into()))];

        let touched = update_row(
            runtime,
            driver.clone(),
            users(),
            pk.clone(),
            changes.clone(),
        )
        .await
        .expect("task")
        .expect("update");
        assert_eq!(touched, 1);

        update_rows(
            runtime,
            driver.clone(),
            users(),
            vec![
                RowUpdate {
                    pk: pk.clone(),
                    changes: changes.clone(),
                },
                RowUpdate { pk, changes },
            ],
        )
        .await
        .expect("task")
        .expect("update");

        apply_changes(
            runtime,
            driver.clone(),
            users(),
            RowBatch {
                deletes: vec![dbui_driver::RowDelete {
                    pk: vec![("id".to_string(), Value::Int(2))],
                }],
                ..RowBatch::default()
            },
        )
        .await
        .expect("task")
        .expect("delete");

        let calls = driver.calls();
        assert_eq!(calls.batches.len(), 3, "three calls, three batches");
        assert_eq!(calls.batches[0].updates.len(), 1);
        assert_eq!(calls.batches[1].updates.len(), 2);
        assert_eq!(calls.batches[2].deletes.len(), 1);
    }

    /// The statement the UI never composes: quoting the identifier is the
    /// driver's job, and the use case runs what it hands back.
    #[tokio::test]
    async fn truncate_and_drop_run_the_drivers_own_quoted_statement() {
        let driver = Arc::new(FakeDriver::default());
        let runtime = runtime();
        let hostile = TableRef::new("public", "users\"; DROP TABLE users; --");

        truncate_table(runtime, driver.clone(), hostile.clone())
            .await
            .expect("task")
            .expect("truncate");
        drop_relation(runtime, driver.clone(), hostile.clone(), TableKind::View)
            .await
            .expect("task")
            .expect("drop");

        let calls = driver.calls();
        assert_eq!(
            calls.statements[0],
            dbui_driver::truncate_sql(Driver::Postgres, &hostile)
        );
        assert_eq!(
            calls.statements[1],
            dbui_driver::drop_sql(Driver::Postgres, &hostile, TableKind::View)
        );
        assert!(calls.statements[1].contains("VIEW"), "drops it as a view");
    }

    #[tokio::test]
    async fn a_query_is_run_as_typed() {
        let driver = Arc::new(FakeDriver::default());
        let result = run_query(runtime(), driver.clone(), "SELECT 1".into())
            .await
            .expect("task")
            .expect("query");

        assert_eq!(result.statement, "SELECT 1");
        assert_eq!(driver.calls().statements, vec!["SELECT 1".to_string()]);
    }

    /// A batch stops at the first failure: the statements behind it may well
    /// depend on the one that did not run.
    #[tokio::test]
    async fn a_batch_stops_at_the_first_failure() {
        let driver = Arc::new(FakeDriver {
            poison: Some("BOOM".into()),
            ..FakeDriver::default()
        });

        let outcome = run_queries(
            runtime(),
            driver.clone(),
            vec![
                "INSERT INTO t VALUES (1)".into(),
                "BOOM".into(),
                "INSERT INTO t VALUES (2)".into(),
            ],
        )
        .await
        .expect("task");

        assert!(outcome.is_err());
        assert_eq!(
            driver.calls().statements.len(),
            2,
            "the third statement never ran"
        );
    }

    /// The status bar needs the elapsed time of the whole batch, and the grid
    /// needs the last statement that actually produced rows -- which is not
    /// necessarily the last statement.
    #[tokio::test]
    async fn a_batch_totals_its_time_and_keeps_the_last_rows() {
        struct Mixed;

        #[async_trait]
        impl DatabaseDriver for Mixed {
            fn driver(&self) -> Driver {
                Driver::Postgres
            }
            fn server_version(&self) -> &str {
                "Fake 1.0"
            }
            async fn ping(&self) -> Outcome<()> {
                Ok(())
            }
            async fn catalog(&self) -> Outcome<Catalog> {
                Ok(Catalog::default())
            }
            async fn columns(&self, _table: &TableRef) -> Outcome<Vec<Column>> {
                Ok(Vec::new())
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
                Ok(0)
            }
            async fn apply_changes(&self, _table: &TableRef, _batch: &RowBatch) -> Outcome<u64> {
                Ok(0)
            }
            async fn execute(&self, sql: &str) -> Outcome<QueryResult> {
                if sql.starts_with("SELECT") {
                    Ok(returning(sql, rows(3, false), 20))
                } else {
                    Ok(affected(sql, 4, 10))
                }
            }
            async fn close(&self) {}
        }

        let result = run_queries(
            runtime(),
            Arc::new(Mixed),
            vec![
                "SELECT * FROM users".into(),
                "UPDATE users SET name = 'ada'".into(),
            ],
        )
        .await
        .expect("task")
        .expect("batch");

        assert_eq!(result.results.len(), 2);
        assert_eq!(result.total_elapsed, Duration::from_millis(30));
        let last = result
            .last_rows
            .as_ref()
            .expect("a statement returned rows");
        assert_eq!(last.statement, "SELECT * FROM users");
        assert_eq!(
            result.summary(),
            "2 statements · 4 rows affected in 30 ms",
            "the summary describes the last statement, not the kept rows"
        );
    }

    /// The batch summary is the sentence a status bar shows, so every shape of
    /// it has to read as one.
    #[test]
    fn batch_summaries_read_as_sentences() {
        assert_eq!(
            batch(vec![affected("DELETE FROM t", 1, 7)]).summary(),
            "1 statement · 1 row affected in 7 ms"
        );
        assert_eq!(
            batch(vec![returning("SELECT 1", rows(1, false), 3)]).summary(),
            "1 statement · 1 row in 3 ms"
        );
        assert_eq!(
            batch(vec![
                affected("BEGIN", 0, 1),
                returning("SELECT 1", rows(2, true), 4),
            ])
            .summary(),
            "2 statements · 2+ rows in 5 ms",
            "a clipped result set says so rather than under-reporting"
        );
        assert_eq!(
            batch(Vec::new()).summary(),
            "0 statements in 0 ms",
            "an empty batch still has a sentence"
        );
    }

    #[tokio::test]
    async fn disconnecting_closes_the_pool() {
        let driver = Arc::new(FakeDriver::default());
        disconnect(runtime(), driver.clone()).await.expect("task");
        assert!(driver.closed.load(Ordering::SeqCst));
    }

    /// Connecting is the one use case that names a real adapter, so the failure
    /// path is what a test without a server can reach -- and it must come back
    /// as an error rather than a panic.
    #[tokio::test]
    async fn connecting_to_something_that_is_not_there_is_an_error() {
        let mut config = ConnectionConfig::new(dbui_domain::Driver::Sqlite);
        config.database = "/nonexistent/dbui-test/missing.sqlite".into();
        config.tls = TlsMode::Disable;
        let runtime = runtime();

        assert!(connect(runtime, config.clone())
            .await
            .expect("task")
            .is_err());
        assert!(test_connection(runtime, config)
            .await
            .expect("task")
            .is_err());
    }
}
