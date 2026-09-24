//! The use cases, one function each.
//!
//! Each takes what it needs and returns a [`Task`] the UI awaits. They are
//! deliberately free functions rather than methods on [`Workspace`]: the
//! workspace is state the UI mutates when a task *lands*, and a use case must
//! not hold a borrow of it across an await.

use crate::runtime::{DbRuntime, Task};
use dbui_domain::{
    Catalog, Column, ColumnInfo, ConnectionConfig, DbObject, Index, Page, QueryOutcome,
    QueryResult, ResultSet, ServerSession, SortKey, TableKind, TableRef, Value,
};
use dbui_driver::{DatabaseDriver, DriverError, QueryToken, RowBatch, RowUpdate};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

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
///
/// `stop` ends it early -- the tab it was loading into has closed -- at
/// whichever of the three reads it had reached, on the server as well.
pub fn open_table(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    page: Page,
    where_clause: String,
    sort: Option<SortKey>,
    mut stop: Stop,
) -> Task<Outcome<TableContents>> {
    runtime.spawn(async move {
        let driver = driver.as_ref();
        let label = table.qualified();

        // The two that are allowed to fail without failing the load -- a
        // table with no readable columns still has rows, a count that errors
        // still leaves a page -- are still not allowed to swallow a stop.
        let token = QueryToken::new();
        let columns =
            match until_stopped(driver, &label, &token, driver.columns(&table), &mut stop).await {
                Err(error @ DriverError::Cancelled { .. }) => return Err(error),
                columns => columns.unwrap_or_default(),
            };
        let order = dbui_domain::order_for(sort.as_ref(), &key_columns(&columns));

        let token = QueryToken::new();
        let rows = until_stopped(
            driver,
            &label,
            &token,
            driver.table_rows_tracked(&table, page, &where_clause, &order, &token),
            &mut stop,
        )
        .await?;

        let token = QueryToken::new();
        let total_rows = match until_stopped(
            driver,
            &label,
            &token,
            driver.row_count_tracked(&table, &where_clause, &token),
            &mut stop,
        )
        .await
        {
            Err(error @ DriverError::Cancelled { .. }) => return Err(error),
            count => count.ok(),
        };

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

/// Where an export's rows go, a page at a time.
///
/// The app layer reads; the UI decides what the file looks like. Errors are
/// already worded for the status bar.
pub trait PageSink: Send + 'static {
    fn page(&mut self, columns: &[ColumnInfo], rows: Vec<Vec<Value>>) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String>;
}

/// How many rows an export reads per round trip: enough that the round trips
/// are not the cost, few enough that a wide table's page is not the problem.
const EXPORT_PAGE: u32 = 5_000;

/// Read every row of `table` that `where_clause` matches, in `sort` order,
/// and hand them to `sink` as they arrive. Resolves to how many were read.
///
/// Paged by the same ordering the grid uses -- the sort, then the key -- so no
/// row is read twice or skipped between pages. A table with neither key nor
/// sort has no order that holds still between reads, so it is read in one go
/// instead: slower to start, but it is every row exactly once.
pub fn export_table(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
    where_clause: String,
    sort: Option<SortKey>,
    mut sink: impl PageSink,
) -> Task<Result<u64, String>> {
    runtime.spawn(async move {
        let columns = driver.columns(&table).await.unwrap_or_default();
        let keys = key_columns(&columns);
        let order = dbui_domain::order_for(sort.as_ref(), &keys);
        let stable = sort.is_some() || !keys.is_empty();
        let limit = if stable { EXPORT_PAGE } else { u32::MAX - 1 };

        let mut offset = 0u64;
        let mut read = 0u64;
        loop {
            let set = driver
                .table_rows(&table, Page { limit, offset }, &where_clause, &order)
                .await
                .map_err(|error| error.to_string())?;
            let count = set.rows.len() as u64;
            let more = set.truncated;
            sink.page(
                &set.columns,
                set.rows.into_iter().map(|row| row.0).collect(),
            )?;
            read += count;
            offset += count;
            if !more || count == 0 {
                break;
            }
        }
        sink.finish()?;
        Ok(read)
    })
}

/// Write rows already in hand -- a query's result -- through a sink, off the
/// UI thread.
pub fn export_rows(
    runtime: &DbRuntime,
    columns: Vec<ColumnInfo>,
    rows: Vec<Vec<Value>>,
    mut sink: impl PageSink,
) -> Task<Result<u64, String>> {
    runtime.spawn(async move {
        let count = rows.len() as u64;
        sink.page(&columns, rows)?;
        sink.finish()?;
        Ok(count)
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

/// How a run in progress can be ended early: by the user, or by the clock.
///
/// Made alongside the [`StopHandle`] the UI keeps, and handed to the run.
pub struct Stop {
    signal: watch::Receiver<bool>,
    /// The connection's query timeout, per statement. `None` waits for ever.
    timeout: Option<Duration>,
}

/// The UI's end of a [`Stop`]: pressing Stop is [`StopHandle::stop`].
///
/// Dropping it stops nothing, so it is safe to let go of without a second
/// thought: a tab that closes mid-run calls [`stop`](Self::stop) on purpose.
pub struct StopHandle(watch::Sender<bool>);

impl StopHandle {
    pub fn stop(&self) {
        let _ = self.0.send(true);
    }
}

impl Stop {
    /// Whether the run has been told to stop.
    pub fn is_stopped(&self) -> bool {
        *self.signal.borrow()
    }
}

/// A fresh stop signal for one run.
pub fn stop_signal(timeout: Option<Duration>) -> (StopHandle, Stop) {
    let (sender, signal) = watch::channel(false);
    (StopHandle(sender), Stop { signal, timeout })
}

/// How long a told-off statement gets to wind down on its own before it is
/// dropped. A cancelled Postgres or MySQL statement ends with an error of its
/// own a moment after being told; waiting for that hands the connection back
/// to the pool clean instead of mid-conversation.
const WIND_DOWN: Duration = Duration::from_secs(3);

/// Run one statement, ending it early if `stop` fires or its timeout runs
/// out -- on the server as well as here.
async fn run_stoppable(
    driver: &dyn DatabaseDriver,
    sql: &str,
    stop: &mut Stop,
) -> Outcome<QueryResult> {
    let token = QueryToken::new();
    until_stopped(
        driver,
        sql,
        &token,
        driver.execute_tracked(sql, &token),
        stop,
    )
    .await
}

/// Await `run`, a call tracked in `token`, unless `stop` fires or its timeout
/// runs out first -- in which case the server is told through `token`, and
/// the answer is [`DriverError::Cancelled`] or [`DriverError::TimedOut`] for
/// `statement`. A `run` whose token was never tracked is simply dropped.
///
/// A stop that has already fired ends the next call at once, so a load made
/// of several calls in a row stops at whichever one it had reached.
async fn until_stopped<T>(
    driver: &dyn DatabaseDriver,
    statement: &str,
    token: &QueryToken,
    run: impl std::future::Future<Output = Outcome<T>>,
    stop: &mut Stop,
) -> Outcome<T> {
    tokio::pin!(run);

    let pressed = async {
        // A dropped handle is not a press: the run goes on to its end.
        if stop.signal.wait_for(|stopped| *stopped).await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let expired = async {
        match stop.timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending().await,
        }
    };

    let why = tokio::select! {
        result = &mut run => return result,
        _ = pressed => DriverError::Cancelled { statement: statement.to_string() },
        _ = expired => DriverError::TimedOut {
            statement: statement.to_string(),
            seconds: stop.timeout.map(|limit| limit.as_secs()).unwrap_or_default(),
        },
    };

    // Told on the server, it ends by itself; wait for that. Not told --
    // SQLite, or the telling failed -- dropping `run` is what stops it. The
    // telling gets a limit of its own: it borrows a connection from the same
    // pool the stuck statement may have drained, and a server that sits on a
    // `KILL` would otherwise hold the stop hostage to the very thing it stops.
    let told = tokio::time::timeout(WIND_DOWN, driver.cancel(token)).await;
    if matches!(told, Ok(Ok(true))) {
        let _ = tokio::time::timeout(WIND_DOWN, &mut run).await;
    }
    Err(why)
}

/// Run the statement in the editor.
pub fn run_query(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    sql: String,
    mut stop: Stop,
) -> Task<Outcome<QueryResult>> {
    runtime.spawn(async move { run_stoppable(driver.as_ref(), &sql, &mut stop).await })
}

/// One table's indexes, for the structure pane.
pub fn fetch_indexes(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    table: TableRef,
) -> Task<Outcome<Vec<Index>>> {
    runtime.spawn(async move { driver.indexes(&table).await })
}

/// Every client connection on the server, for the activity panel.
pub fn fetch_sessions(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
) -> Task<Outcome<Vec<ServerSession>>> {
    runtime.spawn(async move { driver.server_sessions().await })
}

/// Cancel what a session is running, or (`terminate`) end the session.
pub fn end_session(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    id: i64,
    terminate: bool,
) -> Task<Outcome<()>> {
    runtime.spawn(async move { driver.end_session(id, terminate).await })
}

/// The statement that creates one function, trigger, sequence... for the
/// tree to open in an editor.
pub fn fetch_definition(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    object: DbObject,
) -> Task<Outcome<String>> {
    runtime.spawn(async move { driver.definition(&object).await })
}

/// Run the statements a structure change is made of, in order, stopping at
/// the first that fails. Resolves to how many ran.
///
/// Not in a transaction: MySQL commits DDL implicitly, so wrapping it would
/// promise an all-or-nothing that one of the three engines cannot keep. A
/// change is usually one statement anyway; when it is several, the error
/// says which one stopped it.
pub fn run_ddl(
    runtime: &DbRuntime,
    driver: Arc<dyn DatabaseDriver>,
    statements: Vec<String>,
) -> Task<Outcome<usize>> {
    runtime.spawn(async move {
        for sql in &statements {
            driver.execute(sql).await?;
        }
        Ok(statements.len())
    })
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
    mut stop: Stop,
) -> Task<Outcome<BatchQueryResult>> {
    runtime.spawn(async move {
        let attempted = statements.len();
        let mut results = Vec::with_capacity(attempted);
        let mut last_rows: Option<QueryResult> = None;
        let mut total_elapsed = std::time::Duration::ZERO;
        let mut failure = None;

        for sql in statements {
            // Each statement gets the whole timeout, and a Stop ends the
            // batch where it is: what already ran stays in the results.
            match run_stoppable(driver.as_ref(), &sql, &mut stop).await {
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

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_domain::{ConnectionConfig, Driver};

    const RUNAWAY: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                           SELECT count(*) FROM (SELECT i FROM n LIMIT 5000000000)";

    /// The rows behind [`RUNAWAY`]: any page of them is quick, counting them
    /// is not.
    const RUNAWAY_ROWS: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                                SELECT i FROM n LIMIT 5000000000";

    /// A SQLite file of its own, deleted on drop.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn sqlite(name: &str) -> (Scratch, Arc<dyn DatabaseDriver>) {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-commands-{}-{name}.db", std::process::id()));
        std::fs::File::create(&path).expect("create the database file");
        let mut config = ConnectionConfig::new(Driver::Sqlite);
        config.database = path.to_string_lossy().to_string();
        let driver = dbui_driver::connect(&config).await.expect("connect");
        (Scratch(path), driver)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stop_ends_a_run_as_cancelled() {
        let (_file, driver) = sqlite("stop").await;
        let (handle, mut stop) = stop_signal(None);

        let started = std::time::Instant::now();
        let run = run_stoppable(driver.as_ref(), RUNAWAY, &mut stop);
        let press = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            handle.stop();
        };
        let (result, ()) = tokio::join!(run, press);

        assert!(
            matches!(result, Err(DriverError::Cancelled { .. })),
            "{result:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        driver
            .execute("SELECT 1")
            .await
            .expect("the connection is free");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_timeout_stops_a_run_by_itself() {
        let (_file, driver) = sqlite("timeout").await;
        let (_handle, mut stop) = stop_signal(Some(Duration::from_millis(300)));

        let result = run_stoppable(driver.as_ref(), RUNAWAY, &mut stop).await;

        assert!(
            matches!(result, Err(DriverError::TimedOut { .. })),
            "{result:?}"
        );
        driver
            .execute("SELECT 1")
            .await
            .expect("the connection is free");
    }

    /// A page load whose tab closes is stopped, not left to run: here the
    /// count over an endless view, which would otherwise never come back.
    #[tokio::test(flavor = "multi_thread")]
    async fn stop_ends_a_table_load_as_cancelled() {
        let runtime = DbRuntime::new().expect("runtime");
        let (_file, driver) = sqlite("load").await;
        driver
            .execute(&format!("CREATE VIEW endless AS {RUNAWAY_ROWS}"))
            .await
            .expect("create the view");
        let (handle, stop) = stop_signal(None);

        let started = std::time::Instant::now();
        let load = open_table(
            &runtime,
            driver.clone(),
            TableRef::new("main", "endless"),
            Page {
                limit: 10,
                offset: 0,
            },
            String::new(),
            None,
            stop,
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        handle.stop();
        let result = load.await.expect("the load answers");

        assert!(
            matches!(result, Err(DriverError::Cancelled { .. })),
            "{:?}",
            result.map(|contents| contents.total_rows)
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        driver
            .execute("SELECT 1")
            .await
            .expect("the connection is free");
        // See `export_all`: a runtime is let go of on a blocking thread.
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
    }

    /// Letting go of the handle is not a Stop.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dropped_handle_lets_the_run_finish() {
        let (_file, driver) = sqlite("dropped").await;
        let (handle, mut stop) = stop_signal(None);
        drop(handle);

        let result = run_stoppable(driver.as_ref(), "SELECT 42", &mut stop).await;
        assert!(result.is_ok(), "{result:?}");
    }

    /// Collects what an export hands it.
    struct Collect(std::sync::Arc<std::sync::Mutex<(Vec<i64>, usize, bool)>>);

    impl PageSink for Collect {
        fn page(&mut self, _: &[ColumnInfo], rows: Vec<Vec<Value>>) -> Result<(), String> {
            let mut seen = self.0.lock().unwrap();
            seen.1 += 1;
            for row in rows {
                if let Some(Value::Int(id)) = row.first() {
                    seen.0.push(*id);
                }
            }
            Ok(())
        }
        fn finish(&mut self) -> Result<(), String> {
            self.0.lock().unwrap().2 = true;
            Ok(())
        }
    }

    async fn export_all(
        driver: Arc<dyn DatabaseDriver>,
        table: &str,
    ) -> (u64, Vec<i64>, usize, bool) {
        let runtime = DbRuntime::new().expect("runtime");
        let seen = std::sync::Arc::new(std::sync::Mutex::new((Vec::new(), 0, false)));
        let read = export_table(
            &runtime,
            driver,
            TableRef::new("main", table),
            String::new(),
            None,
            Collect(seen.clone()),
        )
        .await
        .expect("the task ran")
        .expect("the export worked");
        let (ids, pages, finished) = seen.lock().unwrap().clone();
        // A runtime cannot be dropped from inside another one's async
        // context; letting it go on a blocking thread is allowed.
        tokio::task::spawn_blocking(move || drop(runtime))
            .await
            .unwrap();
        (read, ids, pages, finished)
    }

    /// More rows than one page: every one of them, once, in key order.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_keyed_table_exports_across_pages_without_gaps_or_repeats() {
        let (_file, driver) = sqlite("export-keyed").await;
        driver
            .execute("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .await
            .unwrap();
        driver
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 12345) \
                 INSERT INTO t SELECT i FROM n",
            )
            .await
            .unwrap();

        let (read, ids, pages, finished) = export_all(driver, "t").await;
        assert_eq!(read, 12_345);
        assert_eq!(ids, (1..=12_345).collect::<Vec<i64>>());
        assert_eq!(pages, 3, "5000 + 5000 + 2345");
        assert!(finished);
    }

    /// No key and no sort: no order holds still between pages, so it is read
    /// in one go -- and still every row exactly once.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_keyless_table_exports_in_one_read() {
        let (_file, driver) = sqlite("export-keyless").await;
        driver.execute("CREATE TABLE t (n INTEGER)").await.unwrap();
        driver
            .execute(
                "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 7000) \
                 INSERT INTO t SELECT i FROM n",
            )
            .await
            .unwrap();

        let (read, mut ids, pages, _) = export_all(driver, "t").await;
        assert_eq!(read, 7_000);
        assert_eq!(pages, 1);
        ids.sort_unstable();
        assert_eq!(ids, (1..=7_000).collect::<Vec<i64>>());
    }
}
