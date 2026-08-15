//! The port: what any database engine must be able to do.
//!
//! `dbui-app` is written against this trait and never against a concrete
//! engine. Adding SQLite would mean one more implementation here and one more
//! arm in [`crate::connect`] -- and no change at all in the UI.

use crate::error::Result;
use async_trait::async_trait;
use dbui_domain::{
    Catalog, Column, Driver, Page, QueryResult, ResultSet, SortKey, TableRef, Value,
};

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
    /// `where_clause` is freeform SQL after `WHERE` (empty means the whole
    /// table). `order` is what makes the page meaningful: `LIMIT`/`OFFSET`
    /// over an unordered read can return the same row twice and skip another,
    /// so the caller passes the sort plus the key that breaks its ties.
    async fn table_rows(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
    ) -> Result<ResultSet>;

    /// Total rows matching the same WHERE as [`table_rows`].
    async fn row_count(&self, table: &TableRef, where_clause: &str) -> Result<i64>;

    /// Apply a whole batch of edits and deletions in one transaction.
    ///
    /// This is the primitive every write goes through: an editor that stages
    /// changes and commits them together cannot honour "all or nothing" if the
    /// updates and the deletions travel in separate transactions.
    async fn apply_changes(&self, table: &TableRef, batch: &RowBatch) -> Result<u64>;

    /// Update one row identified by primary-key columns.
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

    /// Apply several row updates in one transaction. Any failure rolls all back.
    async fn update_rows(&self, table: &TableRef, rows: &[RowUpdate]) -> Result<u64> {
        self.apply_changes(table, &RowBatch::of_updates(rows.to_vec()))
            .await
    }

    /// Run one statement as typed by the user.
    async fn execute(&self, sql: &str) -> Result<QueryResult>;

    /// Close the pool. Idempotent.
    async fn close(&self);
}

/// One pending row change for [`DatabaseDriver::apply_changes`].
#[derive(Debug, Clone)]
pub struct RowUpdate {
    pub pk: Vec<(String, Value)>,
    pub changes: Vec<(String, Value)>,
}

/// One pending row removal for [`DatabaseDriver::apply_changes`].
#[derive(Debug, Clone)]
pub struct RowDelete {
    pub pk: Vec<(String, Value)>,
}

/// One new row for [`DatabaseDriver::apply_changes`].
///
/// Columns the user never filled in are left out of `values` entirely rather
/// than sent as NULL: leaving them out is what lets a `DEFAULT`, a sequence or
/// a generated column do its job.
#[derive(Debug, Clone)]
pub struct RowInsert {
    pub values: Vec<(String, Value)>,
}

/// Everything one commit writes.
#[derive(Debug, Clone, Default)]
pub struct RowBatch {
    /// Run first, so a row can be inserted and then referred to by the rest
    /// of the same batch.
    pub inserts: Vec<RowInsert>,
    pub updates: Vec<RowUpdate>,
    /// Run after the updates. Staging an edit and a delete on the same row is
    /// the user changing their mind, and in that order the UPDATE is not left
    /// hunting for a row that is already gone.
    pub deletes: Vec<RowDelete>,
}

impl RowBatch {
    pub fn of_updates(updates: Vec<RowUpdate>) -> Self {
        Self {
            inserts: Vec::new(),
            updates,
            deletes: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inserts.is_empty() && self.updates.is_empty() && self.deletes.is_empty()
    }

    pub fn len(&self) -> usize {
        self.inserts.len() + self.updates.len() + self.deletes.len()
    }
}
