//! The PostgreSQL adapter.

mod catalog;
mod decode;

use crate::adapter;
use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, RowBatch, RowUpdate};
use async_trait::async_trait;
use dbui_domain::{
    Catalog, Column, ConnectionConfig, Driver, ForeignKey, Page, QueryResult, ResultSet, SortKey,
    TableRef, TlsMode, Value,
};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgSslMode};
use sqlx::Row as _;

pub struct PostgresDriver {
    pool: PgPool,
    server_version: String,
}

impl PostgresDriver {
    pub async fn connect(config: &ConnectionConfig) -> Result<Self> {
        let address = format!("{}:{}", config.host, config.port);

        let mut options = PgConnectOptions::new()
            .host(&config.host)
            .port(config.port)
            .username(&config.username)
            .ssl_mode(match config.tls {
                TlsMode::Disable => PgSslMode::Disable,
                TlsMode::Prefer => PgSslMode::Prefer,
                TlsMode::Require => PgSslMode::Require,
            });

        if !config.password.is_empty() {
            options = options.password(&config.password);
        }
        if !config.database.is_empty() {
            options = options.database(&config.database);
        }

        // A read-only connection is enforced by the server, not only by the
        // UI refusing to send writes. Set on every connection the pool opens,
        // because a pool that grows later would otherwise hand back a writable
        // one.
        let read_only = config.read_only;
        let pool = PgPoolOptions::new()
            .max_connections(adapter::MAX_CONNECTIONS)
            .acquire_timeout(adapter::ACQUIRE_TIMEOUT)
            .after_connect(move |conn, _meta| {
                Box::pin(async move {
                    if read_only {
                        sqlx::query("SET default_transaction_read_only = on")
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
            .unwrap_or_else(|_| "PostgreSQL".to_string());

        Ok(Self {
            pool,
            server_version: short_version(&server_version),
        })
    }

    /// Single-column foreign keys on `table`, for the grid's jump arrows.
    async fn foreign_keys(&self, table: &TableRef) -> Result<Vec<ForeignKey>> {
        let rows = sqlx::query(catalog::FOREIGN_KEYS)
            .bind(&table.schema)
            .bind(&table.name)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        Ok(adapter::foreign_keys::<sqlx::Postgres>(&rows, None))
    }
}

#[async_trait]
impl DatabaseDriver for PostgresDriver {
    fn driver(&self) -> Driver {
        Driver::Postgres
    }

    fn server_version(&self) -> &str {
        &self.server_version
    }

    async fn ping(&self) -> Result<()> {
        adapter::ping(&self.pool).await
    }

    async fn catalog(&self) -> Result<Catalog> {
        let schema_rows = sqlx::query(catalog::SCHEMAS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        let relation_rows = sqlx::query(catalog::RELATIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| DriverError::catalog(&error))?;

        Ok(adapter::catalog::<sqlx::Postgres>(
            &schema_rows,
            &relation_rows,
            catalog::table_kind,
        ))
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

        let mut columns: Vec<Column> = rows
            .iter()
            .filter_map(|row| {
                Some(Column {
                    name: row.try_get::<String, _>("column_name").ok()?,
                    data_type: row.try_get::<String, _>("data_type").ok()?,
                    nullable: row.try_get::<bool, _>("is_nullable").unwrap_or(true),
                    default: row
                        .try_get::<Option<String>, _>("column_default")
                        .ok()
                        .flatten(),
                    is_primary_key: row.try_get::<bool, _>("is_primary_key").unwrap_or(false),
                    ordinal: i32::from(row.try_get::<i16, _>("ordinal").unwrap_or(0)),
                    references: None,
                })
            })
            .collect();

        adapter::attach_references(&mut columns, &keys);
        Ok(columns)
    }

    async fn table_rows(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
    ) -> Result<ResultSet> {
        adapter::table_page(
            &self.pool,
            Driver::Postgres,
            table,
            page,
            where_clause,
            order,
            decode::decode_row,
        )
        .await
    }

    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64> {
        adapter::row_count(&self.pool, Driver::Postgres, table, where_clause).await
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
        adapter::apply_changes(&self.pool, Driver::Postgres, table, batch).await
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        adapter::execute(&self.pool, sql, decode::decode_row).await
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}

/// `version()` returns a paragraph; the status bar wants the first three words.
fn short_version(full: &str) -> String {
    full.split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ")
}
