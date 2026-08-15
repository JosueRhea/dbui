//! The MySQL adapter.

mod catalog;
mod decode;

use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, RowBatch, RowUpdate};
use crate::sql_build;
use async_trait::async_trait;
use dbui_domain::{
    query, Catalog, Column, ColumnInfo, ConnectionConfig, Driver, Page, QueryOutcome,
    QueryResult, QueryStats, ResultSet, Row as DomainRow, Schema, SortKey, Table, TableRef,
    TlsMode, Value,
};
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlSslMode};
use sqlx::{AssertSqlSafe, Column as _, Row as _, SqlSafeStr as _, TypeInfo as _};
use std::time::{Duration, Instant};

pub struct MySqlDriver {
    pool: MySqlPool,
    server_version: String,
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

        let pool = MySqlPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options)
            .await
            .map_err(|error| DriverError::connect(&address, &error))?;

        let server_version: String = sqlx::query_scalar(catalog::SERVER_VERSION)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| "MySQL".to_string());

        Ok(Self {
            pool,
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
                    default: row.try_get::<Option<String>, _>("column_default").ok().flatten(),
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
        let bound = sql_build::select_page_sql(Driver::MySql, table, where_clause, order);
        let mut query = sqlx::query(AssertSqlSafe(bound.sql.clone()));
        for value in &bound.binds {
            query = bind_value(query, value);
        }
        query = query
            .bind(page.probe_limit())
            .bind(page.offset as i64);

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::query(&bound.sql, &error))?;

        let mut set = build_result_set(rows, page.limit as usize);
        self.backfill_columns(&mut set, &bound.sql).await;
        Ok(set)
    }

    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64> {
        let bound = sql_build::count_sql(Driver::MySql, table, where_clause);
        // The filter is freeform text spliced into the statement, so a count
        // has no parameters of its own -- the same trust model as the editor.
        debug_assert!(bound.binds.is_empty(), "count_sql binds nothing");
        sqlx::query_scalar(AssertSqlSafe(bound.sql.clone()))
            .fetch_one(&self.pool)
            .await
            .map_err(|error| DriverError::query(&bound.sql, &error))
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

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        let started = Instant::now();

        let outcome = if query::returns_rows(sql) {
            let rows = sqlx::query(AssertSqlSafe(sql.to_string()))
                .fetch_all(&self.pool)
                .await
                .map_err(|error| DriverError::query(sql, &error))?;
            let mut set = build_result_set(rows, usize::MAX);
            self.backfill_columns(&mut set, sql).await;
            QueryOutcome::Rows(set)
        } else {
            let done = sqlx::query(AssertSqlSafe(sql.to_string()))
                .execute(&self.pool)
                .await
                .map_err(|error| DriverError::query(sql, &error))?;
            QueryOutcome::Affected(done.rows_affected())
        };

        Ok(QueryResult {
            statement: sql.to_string(),
            outcome,
            stats: QueryStats {
                elapsed: started.elapsed(),
            },
        })
    }

    async fn close(&self) {
        self.pool.close().await;
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
