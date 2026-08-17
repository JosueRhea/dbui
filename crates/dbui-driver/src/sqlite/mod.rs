//! The SQLite adapter.
//!
//! The odd one out: there is no server, no host and no user, and the
//! "database" is a path on disk. Everything above this module still holds an
//! `Arc<dyn DatabaseDriver>` and cannot tell.

mod catalog;
mod decode;

use crate::adapter;
use crate::error::{DriverError, Result};
use crate::port::{DatabaseDriver, RowBatch, RowUpdate};
use async_trait::async_trait;
use dbui_domain::{
    Catalog, Column, ConnectionConfig, Driver, ForeignKey, Page, QueryResult, ResultSet, Schema,
    SortKey, Table, TableRef, Value,
};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use sqlx::Row as _;
use std::str::FromStr;

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
            .acquire_timeout(adapter::ACQUIRE_TIMEOUT)
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

        // SQLite has one schema, so its query has no `ref_schema` to read.
        Ok(adapter::foreign_keys::<sqlx::Sqlite>(
            &rows,
            Some(catalog::SCHEMA_NAME),
        ))
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
        adapter::ping(&self.pool).await
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
        adapter::table_page(
            &self.pool,
            Driver::Sqlite,
            table,
            page,
            where_clause,
            order,
            decode::decode_row,
        )
        .await
    }

    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64> {
        adapter::row_count(&self.pool, Driver::Sqlite, table, where_clause).await
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
        adapter::apply_changes(&self.pool, Driver::Sqlite, table, batch).await
    }

    async fn execute(&self, sql: &str) -> Result<QueryResult> {
        adapter::execute(&self.pool, sql, decode::decode_row).await
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}
