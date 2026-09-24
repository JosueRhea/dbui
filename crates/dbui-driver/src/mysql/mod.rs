//! The MySQL adapter.

mod catalog;
mod decode;

use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, QueryToken, RowBatch, RowUpdate};
use crate::sessions::{Lease, SessionId, Sessions};
use crate::sql_build;
use async_trait::async_trait;
use dbui_domain::{
    query, Catalog, Column, ColumnInfo, ConnectionConfig, Driver, ForeignKey, Index, Page,
    QueryOutcome, QueryResult, QueryStats, ResultSet, Row as DomainRow, Schema, SortKey, Table,
    TableRef, TlsMode, TransactionState, Value,
};
use futures_util::{StreamExt as _, TryStreamExt as _};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};
use sqlx::{AssertSqlSafe, Column as _, Row as _, SqlSafeStr as _, TypeInfo as _};
use std::time::{Duration, Instant};

pub struct MySqlDriver {
    pool: MySqlPool,
    server_version: String,
    /// Connections kept with their session ids, for statements that may have
    /// to be cancelled. See `sessions`.
    sessions: Sessions<sqlx::MySql>,
}

impl MySqlDriver {
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let address = format!("{}:{}", config.host, config.port);

        let mut options = MySqlConnectOptions::new()
            .host(&config.host)
            .port(config.port)
            .username(&config.username)
            .ssl_mode(match config.tls {
                TlsMode::Disable => MySqlSslMode::Disabled,
                TlsMode::Prefer => MySqlSslMode::Preferred,
                TlsMode::Require => MySqlSslMode::Required,
            });

        if !config.password.is_empty() {
            options = options.password(&config.password);
        }
        // Unlike Postgres, MySQL is content to connect with no database
        // selected -- which is what you want when browsing a whole server.
        if !config.database.is_empty() {
            options = options.database(&config.database);
        }

        // See the Postgres adapter: read-only is a server-side setting, put on
        // every connection the pool opens rather than trusted to the UI.
        // Statements run in autocommit, so each is its own transaction and
        // this applies to all of them.
        let read_only = config.read_only;
        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .after_connect(move |conn, _meta| {
                Box::pin(async move {
                    if read_only {
                        sqlx::query("SET SESSION transaction_read_only = 1")
                            .execute(&mut *conn)
                            .await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|error| DriverError::connect(&address, &error))?;

        let server_version: String = sqlx::query_scalar(catalog::SERVER_VERSION)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| "MySQL".to_string());

        Ok(Self {
            pool,
            sessions: Sessions::new(),
            server_version: format!("MySQL {server_version}"),
        })
    }

    /// See the Postgres adapter's copy of this: a query that matched nothing
    /// carries no column metadata, and a grid with no headers looks broken.
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

impl MySqlDriver {
    /// A connection for one tracked statement, from the driver's cache of
    /// connections whose session id is already known. See `sessions`.
    async fn lease(&self, sql: &str) -> Result<Lease<'_, sqlx::MySql>> {
        self.sessions
            .acquire(&self.pool)
            .await
            .map_err(|error| DriverError::query(sql, &error))
    }

    /// Single-column foreign keys on `table`, for the grid's jump arrows.
    async fn foreign_keys(&self, table: &TableRef) -> Result<Vec<ForeignKey>> {
        let rows = sqlx::query(catalog::FOREIGN_KEYS)
            .bind(&table.schema)
            .bind(&table.name)
            .bind(&table.schema)
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
                        row.try_get::<String, _>("ref_schema").ok()?,
                        row.try_get::<String, _>("ref_table").ok()?,
                    ),
                    references_column: row.try_get::<String, _>("ref_column").ok()?,
                })
            })
            .collect())
    }
}

#[async_trait]
impl DatabaseDriver for MySqlDriver {
    fn driver(&self) -> Driver {
        Driver::MySql
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
        let schema_rows = sqlx::query(catalog::SCHEMAS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        let mut schemas: Vec<Schema> = schema_rows
            .iter()
            .filter_map(|row| row.try_get::<String, _>("schema_name").ok())
            .map(|name| Schema {
                name,
                tables: Vec::new(),
            })
            .collect();

        let relation_rows = sqlx::query(catalog::RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        for row in &relation_rows {
            let (Ok(schema_name), Ok(name), Ok(kind)) = (
                row.try_get::<String, _>("schema_name"),
                row.try_get::<String, _>("relation_name"),
                row.try_get::<String, _>("relation_kind"),
            ) else {
                continue;
            };

            let table = Table {
                schema: schema_name.clone(),
                name,
                kind: catalog::table_kind(&kind),
            };

            match schemas.iter_mut().find(|s| s.name == schema_name) {
                Some(schema) => schema.tables.push(table),
                None => schemas.push(Schema {
                    name: schema_name,
                    tables: vec![table],
                }),
            }
        }

        Ok(Catalog { schemas })
    }

    async fn columns(&self, table: &TableRef) -> Result<Vec<Column>> {
        // Best effort: a server that will not answer this still gives back a
        // perfectly usable structure pane, just without the arrows.
        let keys = self.foreign_keys(table).await.unwrap_or_default();

        let rows = sqlx::query(catalog::COLUMNS)
            .bind(&table.schema)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(Column {
                    name: row.try_get::<String, _>("column_name").ok()?,
                    data_type: row.try_get::<String, _>("data_type").ok()?,
                    // information_schema answers with the strings "YES"/"NO".
                    nullable: row
                        .try_get::<String, _>("is_nullable")
                        .map(|flag| flag.eq_ignore_ascii_case("YES"))
                        .unwrap_or(true),
                    default: row
                        .try_get::<Option<String>, _>("column_default")
                        .ok()
                        .flatten(),
                    // "PRI" marks a primary-key member; "UNI" and "MUL" are
                    // other index kinds and are not what the grid highlights.
                    is_primary_key: row
                        .try_get::<String, _>("column_key")
                        .map(|key| key == "PRI")
                        .unwrap_or(false),
                    ordinal: row
                        .try_get::<u32, _>("ordinal")
                        .map(|n| n as i32)
                        .unwrap_or(0),
                    references: None,
                })
            })
            .map(|mut column: Column| {
                column.references = keys.iter().find(|key| key.column == column.name).cloned();
                column
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
        let bound = sql_build::select_page_sql(Driver::MySql, table, where_clause, order);
        // Not kept as a prepared statement: `SELECT *` is shaped by the table,
        // and after a column is added PostgreSQL refuses a cached plan whose
        // result type changed -- every later page of the table failed -- and
        // SQLite quietly returns nothing.
        let mut query = sqlx::query(AssertSqlSafe(bound.sql.clone())).persistent(false);
        for value in &bound.binds {
            query = bind_value(query, value);
        }
        query = query.bind(page.probe_limit()).bind(page.offset as i64);

        let mut lease = self.lease(&bound.sql).await?;
        let tracking = token.track(lease.session());
        let rows = query.fetch_all(lease.conn()).await;
        drop(tracking);
        lease.settle(&rows);
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
        let bound = sql_build::count_sql(Driver::MySql, table, where_clause);
        // The filter is freeform text spliced into the statement, so a count
        // has no parameters of its own -- the same trust model as the editor.
        debug_assert!(bound.binds.is_empty(), "count_sql binds nothing");
        let mut lease = self.lease(&bound.sql).await?;
        let tracking = token.track(lease.session());
        let count = sqlx::query_scalar(AssertSqlSafe(bound.sql.clone()))
            .fetch_one(lease.conn())
            .await;
        drop(tracking);
        lease.settle(&count);
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
                sql_build::insert_sql(Driver::MySql, table, &row.values)
                    .map_err(|message| DriverError::message("INSERT", message))?,
            );
        }
        for row in &batch.updates {
            statements.push(
                sql_build::update_sql(Driver::MySql, table, &row.changes, &row.pk)
                    .map_err(|message| DriverError::message("UPDATE", message))?,
            );
        }
        for row in &batch.deletes {
            statements.push(
                sql_build::delete_sql(Driver::MySql, table, &row.pk)
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
            .bind(&table.schema)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;
        Ok(crate::port::group_indexes(
            rows.iter()
                .filter_map(|row| {
                    let name = row.try_get::<String, _>("index_name").ok()?;
                    let primary = name == "PRIMARY";
                    Some((
                        name,
                        row.try_get::<i64, _>("non_unique").unwrap_or(1) == 0,
                        primary,
                        row.try_get::<Option<String>, _>("column_name")
                            .ok()
                            .flatten(),
                    ))
                })
                .collect(),
        ))
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        self.run_statement(sql, &QueryToken::new(), false).await
    }

    async fn execute_tracked(&self, sql: &str, token: &QueryToken) -> Result<QueryResult> {
        // The editor's statements, which share one session across runs.
        self.run_statement(sql, token, true).await
    }

    async fn cancel(&self, token: &QueryToken) -> Result<bool> {
        let Some(session) = token.session() else {
            return Ok(false);
        };
        // `KILL` takes a literal, not a parameter. The id is a number this
        // driver read back from the server itself, so there is nothing here
        // for a statement to be smuggled in through.
        let kill = format!("KILL QUERY {session}");
        sqlx::query(AssertSqlSafe(kill.clone()))
            .execute(&self.pool)
            .await
            .map_err(|error| DriverError::query(kill, &error))?;
        Ok(true)
    }

    fn editor_transaction(&self) -> TransactionState {
        self.sessions.console_transaction()
    }

    async fn close(&self) {
        self.sessions.close().await;
        self.pool.close().await;
    }
}

impl MySqlDriver {
    /// One statement as typed, on the console connection when `console` --
    /// see `sessions` -- or on any kept one otherwise. Either way on a
    /// connection held for the whole statement, so the session noted in
    /// `token` is the session the statement runs on.
    async fn run_statement(
        &self,
        sql: &str,
        token: &QueryToken,
        console: bool,
    ) -> Result<QueryResult> {
        let mut lease = if console {
            self.sessions
                .acquire_console(&self.pool)
                .await
                .map_err(|error| error.for_statement(sql))?
        } else {
            self.sessions
                .acquire(&self.pool)
                .await
                .map_err(|error| DriverError::query(sql, &error))?
        };
        let tracking = token.track(lease.session());
        let was_open = console && self.sessions.console_transaction() != TransactionState::Idle;

        let started = Instant::now();

        enum Ran {
            Rows(Vec<sqlx::mysql::MySqlRow>),
            Affected(u64),
        }
        // Never kept as a prepared statement (`persistent(false)`): run again
        // after an ALTER TABLE -- here, or from any other session -- a cached
        // plan still has the old columns, which PostgreSQL refuses and SQLite
        // answers with no rows at all.
        let ran = if query::returns_rows(sql) {
            // One row past the cap says whether there were more. The rest are
            // never decoded or kept, and `cut_short` stops the server sending
            // them.
            sqlx::query(AssertSqlSafe(sql.to_string()))
                .persistent(false)
                .fetch(lease.conn())
                .take(ResultSet::QUERY_ROW_CAP + 1)
                .try_collect()
                .await
                .map(Ran::Rows)
        } else {
            // The text protocol, not a prepared statement: MySQL refuses
            // `BEGIN`, `START TRANSACTION`, `LOCK TABLES` and a good deal more
            // as prepared statements ("not supported in the prepared statement
            // protocol yet"), and a statement that returns no rows loses
            // nothing by going as text.
            sqlx::raw_sql(AssertSqlSafe(sql.to_string()))
                .execute(lease.conn())
                .await
                .map(|done| Ran::Affected(done.rows_affected()))
        };
        drop(tracking);

        let cut_short =
            matches!(&ran, Ok(Ran::Rows(rows)) if rows.len() > ResultSet::QUERY_ROW_CAP);
        let ready = !cut_short || lease.cut_short(&self.pool).await;
        if ready && crate::sessions::survives(&ran) {
            lease.note_transaction(sql).await;
        }
        if ready {
            lease.settle(&ran);
        } else {
            drop(lease);
        }
        // The connection died under a statement inside a transaction: say
        // that, rather than the bare I/O error, and say it now.
        if was_open && self.sessions.take_lost() {
            return Err(DriverError::TransactionLost {
                statement: sql.to_string(),
            });
        }

        let outcome = match ran.map_err(|error| DriverError::query(sql, &error))? {
            Ran::Rows(rows) => {
                let mut set = build_result_set(rows, ResultSet::QUERY_ROW_CAP);
                self.backfill_columns(&mut set, sql).await;
                QueryOutcome::Rows(set)
            }
            Ran::Affected(count) => QueryOutcome::Affected(count),
        };

        Ok(QueryResult {
            statement: sql.to_string(),
            outcome,
            stats: QueryStats {
                elapsed: started.elapsed(),
            },
        })
    }
}

fn build_result_set(rows: Vec<sqlx::mysql::MySqlRow>, keep: usize) -> ResultSet {
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
/// MySQL coerces a string parameter into most column types on its own, but
/// binding what the value actually is skips that guesswork -- and keeps the
/// two adapters saying the same thing about the same batch.
fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    value: &Value,
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
    match value {
        Value::Null | Value::Default => query.bind(Option::<String>::None),
        Value::Bool(flag) => query.bind(*flag),
        Value::Int(number) => query.bind(*number),
        Value::Float(number) => query.bind(*number),
        Value::Bytes(bytes) => query.bind(bytes.clone()),
        other => query.bind(other.to_text()),
    }
}

impl SessionId for sqlx::MySql {
    fn session_id(
        conn: &mut Self::Connection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::result::Result<u64, sqlx::Error>> + Send + '_>,
    > {
        Box::pin(async move {
            let session: u64 = sqlx::query_scalar("SELECT CONNECTION_ID()")
                .fetch_one(conn)
                .await?;
            Ok(session as u64)
        })
    }
    fn transaction_state(
        conn: &mut Self::Connection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TransactionState> + Send + '_>> {
        Box::pin(async move {
            // MySQL keeps no session variable for it; the performance schema
            // does, on by default since 8.0. Without it -- or on MariaDB,
            // which has no PS_CURRENT_THREAD_ID -- this cannot tell.
            let asked = sqlx::raw_sql(
                "SELECT COUNT(*) FROM performance_schema.events_transactions_current \
                 WHERE THREAD_ID = PS_CURRENT_THREAD_ID() AND STATE = 'ACTIVE'",
            )
            .fetch_one(conn)
            .await;
            match asked.map(|row| row.try_get::<i64, _>(0)) {
                Ok(Ok(open)) if open > 0 => TransactionState::Open,
                _ => TransactionState::Idle,
            }
        })
    }

    fn stop_session(
        pool: &sqlx::Pool<Self>,
        session: u64,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            // A literal, as in `cancel`: the id is a number the server gave.
            let kill = format!("KILL QUERY {session}");
            let _ = sqlx::query(AssertSqlSafe(kill)).execute(pool).await;
        })
    }

    /// `KILL QUERY` ends the statement and leaves the transaction open.
    const STOP_KEEPS_TRANSACTION: bool = true;
}
