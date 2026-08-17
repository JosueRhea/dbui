//! The SQLite adapter.
//!
//! The odd one out: there is no server, no host and no user, and the
//! "database" is a path on disk. Everything above this module still holds an
//! `Arc<dyn DatabaseDriver>` and cannot tell.

mod catalog;
mod decode;

use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, RowBatch, RowUpdate};
use crate::sql_build;
use async_trait::async_trait;
use dbui_domain::{
    query, Catalog, Column, ColumnInfo, ConnectionConfig, Driver, ForeignKey, Page, QueryOutcome,
    QueryResult, QueryStats, ResultSet, Row as DomainRow, Schema, SortKey, Table, TableRef, Value,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{AssertSqlSafe, Column as _, Row as _, SqlSafeStr as _, TypeInfo as _};
use std::str::FromStr;
use std::time::{Duration, Instant};

pub struct SqliteDriver {
    pool: SqlitePool,
    server_version: String,
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

        // One schema, always -- the tree needs a folder to put them in.
        Ok(Catalog {
            schemas: vec![Schema {
                name: catalog::SCHEMA_NAME.to_string(),
                tables,
            }],
        })
    }

    async fn columns(&self, table: &TableRef) -> Result<Vec<Column>> {
        let keys = self.foreign_keys(table).await.unwrap_or_default();

        let rows = sqlx::query(catalog::COLUMNS)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        // A row that will not decode fails the whole read: a dropped column is
        // a column missing from the structure pane, and a defaulted key flag is
        // an unordered page the grid will not let anyone edit -- neither with a
        // word on screen to say so.
        rows.iter()
            .map(|row| {
                let catalog = |error: sqlx::Error| DriverError::catalog(&error);
                let name = row.try_get::<String, _>("column_name").map_err(catalog)?;
                Ok(Column {
                    references: keys.iter().find(|key| key.column == name).cloned(),
                    name,
                    data_type: row.try_get::<String, _>("data_type").map_err(catalog)?,
                    nullable: row.try_get::<i64, _>("not_null").map_err(catalog)? == 0,
                    // Only cosmetic, so still best effort.
                    default: row
                        .try_get::<Option<String>, _>("column_default")
                        .ok()
                        .flatten(),
                    // `pk` is the 1-based position in the key, 0 outside it.
                    is_primary_key: row.try_get::<i64, _>("pk_position").map_err(catalog)? > 0,
                    ordinal: row.try_get::<i64, _>("ordinal").map_err(catalog)? as i32,
                })
            })
            .collect()
    }

    async fn table_rows(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
    ) -> Result<ResultSet> {
        let bound = sql_build::select_page_sql(Driver::Sqlite, table, where_clause, order);
        let mut query = sqlx::query(AssertSqlSafe(bound.sql.clone()));
        for value in &bound.binds {
            query = bind_value(query, value);
        }
        query = query.bind(page.probe_limit()).bind(page.offset as i64);

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::query(&bound.sql, &error))?;

        let mut set = build_result_set(rows, page.limit as usize);
        self.backfill_columns(&mut set, &bound.sql).await;
        Ok(set)
    }

    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64> {
        let bound = sql_build::count_sql(Driver::Sqlite, table, where_clause);
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

impl SqliteDriver {
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
