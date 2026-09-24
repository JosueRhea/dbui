//! The SQLite adapter.
//!
//! The odd one out: there is no server, no host and no user, and the
//! "database" is a path on disk. Everything above this module still holds an
//! `Arc<dyn DatabaseDriver>` and cannot tell.

mod catalog;
mod decode;

use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, QueryToken, RowBatch, RowUpdate};
use crate::sql_build;
use async_trait::async_trait;
use dbui_domain::{
    query, Catalog, Column, ColumnInfo, ConnectionConfig, CreateStatements, DbObject, Driver,
    ForeignKey, Index, ObjectKind, Page, QueryOutcome, QueryResult, QueryStats, ResultSet,
    Row as DomainRow, Schema, SortKey, Table, TableRef, TransactionState, Value,
};
use futures_util::{StreamExt as _, TryStreamExt as _};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::Sqlite;
use sqlx::{AssertSqlSafe, Column as _, Row as _, SqlSafeStr as _, TypeInfo as _};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub struct SqliteDriver {
    pool: SqlitePool,
    server_version: String,
    /// Whether the file's one connection was left inside a transaction by
    /// the last editor statement.
    in_transaction: AtomicBool,
}

impl SqliteDriver {
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let path = config.database.trim();
        if path.is_empty() {
            return Err(DriverError::message(
                "CONNECT",
                "a SQLite connection needs the path to a database file",
            ));
        }

        // `create_if_missing` is deliberately off: a typo in a path should say
        // so, not silently make an empty database and look like it worked.
        let options = SqliteConnectOptions::from_str(path)
            .map_err(|error| DriverError::connect(path, &error))?
            .create_if_missing(false)
            .read_only(config.read_only);

        let pool = SqlitePoolOptions::new()
            // SQLite serialises writers anyway, and a single connection keeps
            // a transaction and the statements around it on the same one.
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(10))
            // Never closed for age or for idling: the one connection is where
            // a `BEGIN` typed in the editor lives, and closing it would roll
            // that back without a word.
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await
            .map_err(|error| DriverError::connect(path, &error))?;

        let version: String = sqlx::query_scalar(catalog::SERVER_VERSION)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| "SQLite".to_string());

        Ok(Self {
            pool,
            server_version: format!("SQLite {version}"),
            in_transaction: AtomicBool::new(false),
        })
    }

    async fn foreign_keys(&self, table: &TableRef) -> Result<Vec<ForeignKey>> {
        let rows = sqlx::query(catalog::FOREIGN_KEYS)
            .bind(&table.name)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(ForeignKey {
                    column: row.try_get::<String, _>("column_name").ok()?,
                    references: TableRef::new(
                        catalog::SCHEMA_NAME,
                        row.try_get::<String, _>("ref_table").ok()?,
                    ),
                    references_column: row.try_get::<String, _>("ref_column").ok()?,
                })
            })
            .collect())
    }
}

#[async_trait]
impl DatabaseDriver for SqliteDriver {
    fn driver(&self) -> Driver {
        Driver::Sqlite
    }

    fn server_version(&self) -> &str {
        &self.server_version
    }

    async fn ping(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|error| DriverError::query("SELECT 1", &error))
    }

    async fn catalog(&self) -> Result<Catalog> {
        let rows = sqlx::query(catalog::RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        let tables = rows
            .iter()
            .filter_map(|row| {
                Some(Table {
                    schema: catalog::SCHEMA_NAME.to_string(),
                    name: row.try_get::<String, _>("relation_name").ok()?,
                    kind: catalog::table_kind(&row.try_get::<String, _>("relation_kind").ok()?),
                })
            })
            .collect();

        let objects = sqlx::query(catalog::TRIGGERS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?
            .iter()
            .filter_map(|row| {
                let name: String = row.try_get("trigger_name").ok()?;
                Some(DbObject {
                    schema: catalog::SCHEMA_NAME.to_string(),
                    key: name.clone(),
                    name,
                    kind: ObjectKind::Trigger,
                    detail: row.try_get::<String, _>("table_name").ok(),
                })
            })
            .collect();

        // One schema, always -- the tree needs a folder to put them in.
        Ok(Catalog {
            schemas: vec![Schema {
                name: catalog::SCHEMA_NAME.to_string(),
                tables,
            }],
            objects,
        })
    }

    async fn create_statements(&self, table: &Table) -> Result<CreateStatements> {
        let create: Vec<String> = sqlx::query_scalar(catalog::CREATE_STATEMENTS)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::query(catalog::CREATE_STATEMENTS, &error))?;
        Ok(CreateStatements {
            create,
            after_data: Vec::new(),
        })
    }

    async fn definition(&self, object: &DbObject) -> Result<String> {
        if object.kind != ObjectKind::Trigger {
            return Err(DriverError::message(
                "",
                format!("SQLite has no {}s", object.kind.label()),
            ));
        }
        let sql: String = sqlx::query_scalar(catalog::TRIGGER_SQL)
            .bind(&object.key)
            .fetch_one(&self.pool)
            .await
            .map_err(|error| DriverError::query(catalog::TRIGGER_SQL, &error))?;
        Ok(format!("{};\n", sql.trim_end().trim_end_matches(';')))
    }

    async fn columns(&self, table: &TableRef) -> Result<Vec<Column>> {
        let keys = self.foreign_keys(table).await.unwrap_or_default();

        let rows = sqlx::query(catalog::COLUMNS)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                let name = row.try_get::<String, _>("column_name").ok()?;
                Some(Column {
                    references: keys.iter().find(|key| key.column == name).cloned(),
                    name,
                    data_type: row.try_get::<String, _>("data_type").unwrap_or_default(),
                    nullable: row.try_get::<i64, _>("not_null").unwrap_or(0) == 0,
                    default: row
                        .try_get::<Option<String>, _>("column_default")
                        .ok()
                        .flatten(),
                    // `pk` is the 1-based position in the key, 0 outside it.
                    is_primary_key: row.try_get::<i64, _>("pk_position").unwrap_or(0) > 0,
                    ordinal: row.try_get::<i64, _>("ordinal").unwrap_or(0) as i32,
                })
            })
            .collect())
    }

    async fn table_rows(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
    ) -> Result<ResultSet> {
        self.table_rows_tracked(table, page, where_clause, order, &QueryToken::new())
            .await
    }

    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64> {
        self.row_count_tracked(table, where_clause, &QueryToken::new())
            .await
    }

    async fn table_rows_tracked(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
        token: &QueryToken,
    ) -> Result<ResultSet> {
        let bound = sql_build::select_page_sql(Driver::Sqlite, table, where_clause, order);
        // Not kept as a prepared statement: `SELECT *` is shaped by the table,
        // and after a column is added PostgreSQL refuses a cached plan whose
        // result type changed -- every later page of the table failed -- and
        // SQLite quietly returns nothing.
        let mut query = sqlx::query(AssertSqlSafe(bound.sql.clone())).persistent(false);
        for value in &bound.binds {
            query = bind_value(query, value);
        }
        query = query.bind(page.probe_limit()).bind(page.offset as i64);

        let mut conn = self.interruptible_connection(&bound.sql, token).await?;
        let tracking = token.track(0);
        let rows = query.fetch_all(&mut *conn).await;
        drop(tracking);
        // Handed back before the backfill, which needs the pool's one
        // connection for itself.
        release_interruptible(conn).await;
        let rows = rows.map_err(|error| DriverError::query(&bound.sql, &error))?;

        let mut set = build_result_set(rows, page.limit as usize);
        self.backfill_columns(&mut set, &bound.sql).await;
        Ok(set)
    }

    async fn row_count_tracked(
        &self,
        table: &TableRef,
        where_clause: &str,
        token: &QueryToken,
    ) -> Result<i64> {
        let bound = sql_build::count_sql(Driver::Sqlite, table, where_clause);
        debug_assert!(bound.binds.is_empty(), "count_sql binds nothing");
        let mut conn = self.interruptible_connection(&bound.sql, token).await?;
        let tracking = token.track(0);
        let count = sqlx::query_scalar(AssertSqlSafe(bound.sql.clone()))
            .fetch_one(&mut *conn)
            .await;
        drop(tracking);
        release_interruptible(conn).await;
        count.map_err(|error| DriverError::query(&bound.sql, &error))
    }

    async fn update_row(
        &self,
        table: &TableRef,
        pk: &[(String, Value)],
        changes: &[(String, Value)],
    ) -> Result<u64> {
        self.apply_changes(
            table,
            &RowBatch::of_updates(vec![RowUpdate {
                pk: pk.to_vec(),
                changes: changes.to_vec(),
            }]),
        )
        .await
    }

    async fn apply_changes(&self, table: &TableRef, batch: &RowBatch) -> Result<u64> {
        if batch.is_empty() {
            return Ok(0);
        }

        let mut statements = Vec::with_capacity(batch.len());
        for row in &batch.inserts {
            statements.push(
                sql_build::insert_sql(Driver::Sqlite, table, &row.values)
                    .map_err(|message| DriverError::message("INSERT", message))?,
            );
        }
        for row in &batch.updates {
            statements.push(
                sql_build::update_sql(Driver::Sqlite, table, &row.changes, &row.pk)
                    .map_err(|message| DriverError::message("UPDATE", message))?,
            );
        }
        for row in &batch.deletes {
            statements.push(
                sql_build::delete_sql(Driver::Sqlite, table, &row.pk)
                    .map_err(|message| DriverError::message("DELETE", message))?,
            );
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|error| DriverError::query("BEGIN", &error))?;

        let mut total = 0u64;
        for bound in &statements {
            let mut query = sqlx::query(AssertSqlSafe(bound.sql.clone()));
            for value in &bound.binds {
                query = bind_value(query, value);
            }
            let done = query
                .execute(&mut *tx)
                .await
                .map_err(|error| DriverError::query(&bound.sql, &error))?;
            total += done.rows_affected();
        }

        tx.commit()
            .await
            .map_err(|error| DriverError::query("COMMIT", &error))?;
        Ok(total)
    }

    async fn indexes(&self, table: &TableRef) -> Result<Vec<Index>> {
        let rows = sqlx::query(catalog::INDEXES)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;
        Ok(crate::port::group_indexes(
            rows.iter()
                .filter_map(|row| {
                    Some((
                        row.try_get::<String, _>("index_name").ok()?,
                        row.try_get::<i64, _>("is_unique").unwrap_or(0) != 0,
                        row.try_get::<String, _>("origin")
                            .is_ok_and(|origin| origin == "pk"),
                        row.try_get::<Option<String>, _>("column_name")
                            .ok()
                            .flatten(),
                    ))
                })
                .collect(),
        ))
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        self.execute_tracked(sql, &QueryToken::new()).await
    }

    fn editor_transaction(&self) -> TransactionState {
        if self.in_transaction.load(Ordering::SeqCst) {
            TransactionState::Open
        } else {
            TransactionState::Idle
        }
    }

    async fn execute_tracked(&self, sql: &str, token: &QueryToken) -> Result<QueryResult> {
        let started = Instant::now();

        let mut conn = self.interruptible_connection(sql, token).await?;
        let tracking = token.track(0);

        // Never kept as a prepared statement (`persistent(false)`): run again
        // after an ALTER TABLE -- here, or from any other session -- a cached
        // plan still has the old columns, which PostgreSQL refuses and SQLite
        // answers with no rows at all.
        let outcome = if query::returns_rows(sql) {
            // One row past the cap says whether there were more; the
            // statement is reset without stepping through the rest.
            sqlx::query(AssertSqlSafe(sql.to_string()))
                .persistent(false)
                .fetch(&mut *conn)
                .take(ResultSet::QUERY_ROW_CAP + 1)
                .try_collect()
                .await
                .map(|rows| QueryOutcome::Rows(build_result_set(rows, ResultSet::QUERY_ROW_CAP)))
        } else {
            sqlx::query(AssertSqlSafe(sql.to_string()))
                .persistent(false)
                .execute(&mut *conn)
                .await
                .map(|done| QueryOutcome::Affected(done.rows_affected()))
        };
        drop(tracking);
        // In-process, so asking costs no round trip: asked every time.
        if let Ok(mut handle) = conn.lock_handle().await {
            // SAFETY: the handle is locked for the duration of the call, and
            // `sqlite3_get_autocommit` only reads the connection's flag.
            let autocommit =
                unsafe { libsqlite3_sys::sqlite3_get_autocommit(handle.as_raw_handle().as_ptr()) };
            self.in_transaction.store(autocommit == 0, Ordering::SeqCst);
        }
        // Handed back before the backfill, which needs the pool's one
        // connection for itself.
        release_interruptible(conn).await;

        let mut outcome = outcome.map_err(|error| DriverError::query(sql, &error))?;
        if let QueryOutcome::Rows(set) = &mut outcome {
            self.backfill_columns(set, sql).await;
        }

        Ok(QueryResult {
            statement: sql.to_string(),
            outcome,
            stats: QueryStats {
                elapsed: started.elapsed(),
            },
        })
    }

    async fn cancel(&self, token: &QueryToken) -> Result<bool> {
        if token.session().is_none() {
            return Ok(false);
        }
        token.interrupt();
        Ok(true)
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}

impl SqliteDriver {
    /// A connection that stops its statement once `token` is told to.
    ///
    /// SQLite runs in-process: there is no server to ask, and dropping the
    /// future does not stop a statement mid-step -- the one connection would
    /// stay busy, and the next query would queue behind it. So the statement
    /// stops itself: SQLite calls the progress handler every so many steps,
    /// and a `false` from it ends the statement there.
    async fn interruptible_connection(
        &self,
        sql: &str,
        token: &QueryToken,
    ) -> Result<PoolConnection<Sqlite>> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|error| DriverError::query(sql, &error))?;
        {
            let token = token.clone();
            conn.lock_handle()
                .await
                .map_err(|error| DriverError::query(sql, &error))?
                .set_progress_handler(1_000, move || !token.should_stop());
        }
        Ok(conn)
    }

    /// See the Postgres adapter's copy: a query that matched nothing carries
    /// no column metadata, and a grid with no headers looks broken.
    async fn backfill_columns(&self, set: &mut ResultSet, sql: &str) {
        use sqlx::{Executor, Statement};

        if !set.columns.is_empty() || !set.rows.is_empty() {
            return;
        }
        let prepared = Executor::prepare(&self.pool, AssertSqlSafe(sql.to_string()).into_sql_str());
        if let Ok(statement) = prepared.await {
            set.columns = statement
                .columns()
                .iter()
                .map(|column| ColumnInfo {
                    name: column.name().to_string(),
                    type_name: column.type_info().name().to_string(),
                })
                .collect();
        }
    }
}

fn build_result_set(rows: Vec<SqliteRow>, keep: usize) -> ResultSet {
    let columns = rows
        .first()
        .map(|row| {
            row.columns()
                .iter()
                .map(|column| ColumnInfo {
                    name: column.name().to_string(),
                    type_name: column.type_info().name().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();

    let truncated = rows.len() > keep;
    let decoded = rows
        .iter()
        .take(keep)
        .map(|row| DomainRow(decode::decode_row(row)))
        .collect();

    ResultSet {
        columns,
        rows: decoded,
        truncated,
    }
}

/// Bind one domain value with its own type.
///
/// SQLite's affinity rules coerce most things, but binding the real type keeps
/// an integer key comparing as an integer rather than as text.
fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>,
    value: &Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments> {
    match value {
        Value::Null | Value::Default => query.bind(Option::<String>::None),
        Value::Bool(flag) => query.bind(*flag),
        Value::Int(number) => query.bind(*number),
        Value::Float(number) => query.bind(*number),
        Value::Bytes(bytes) => query.bind(bytes.clone()),
        other => query.bind(other.to_text()),
    }
}

/// Take the progress handler off and hand the connection back to the pool.
async fn release_interruptible(mut conn: PoolConnection<Sqlite>) {
    if let Ok(mut handle) = conn.lock_handle().await {
        handle.remove_progress_handler();
    }
}
