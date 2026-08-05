//! The port: what any database engine must be able to do.
//!
//! `dbui-app` is written against this trait and never against a concrete
//! engine. Adding SQLite would mean one more implementation here and one more
//! arm in [`crate::connect`] -- and no change at all in the UI.

use crate::error::Result;
use async_trait::async_trait;
use dbui_domain::{Catalog, Column, Driver, Page, QueryResult, ResultSet, TableRef, Value};

/// A live connection to one server.
///
/// `Send + Sync` because the UI holds it in an `Arc` on the main thread and
/// every call runs on the shared tokio runtime.
#[async_trait]
pub trait DatabaseDriver: Send + Sync {
    /// Which engine this is -- the callers that generate SQL need it for
    /// identifier quoting.
    fn driver(&self) -> Driver;

    /// The server's reported version, cached at connect time.
    fn server_version(&self) -> &str;

    /// Round-trip the connection to prove it is still there.
    async fn ping(&self) -> Result<()>;

    /// Every schema and table visible to this user.
    async fn catalog(&self) -> Result<Catalog>;

    /// The columns of one table, in declaration order.
    async fn columns(&self, table: &TableRef) -> Result<Vec<Column>>;

    /// One page of a table's rows.
    ///
    /// `where_clause` is freeform SQL after `WHERE` (empty means the whole table).
    async fn table_rows(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
    ) -> Result<ResultSet>;

    /// Total rows matching the same WHERE as [`table_rows`].
    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64>;

    /// Update one row identified by primary-key columns.
    async fn update_row(
        &self,
        table: &TableRef,
        pk: &[(String, Value)],
        changes: &[(String, Value)],
    ) -> Result<u64>;

    /// Apply several row updates in one transaction. Any failure rolls all back.
    async fn update_rows(&self, table: &TableRef, rows: &[RowUpdate]) -> Result<u64>;

    /// Run one statement as typed by the user.
    async fn execute(&self, sql: &str) -> Result<QueryResult>;

    /// Close the pool. Idempotent.
    async fn close(&self);
}

/// One pending row change for [`DatabaseDriver::update_rows`].
#[derive(Debug, Clone)]
pub struct RowUpdate {
    pub pk: Vec<(String, Value)>,
    pub changes: Vec<(String, Value)>,
}
