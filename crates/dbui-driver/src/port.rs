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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DriverError;
    use dbui_domain::TableRef;
    use std::sync::Mutex;

    /// A driver with nothing but the batch primitive implemented, so what the
    /// provided methods do with it is visible.
    #[derive(Default)]
    struct Recorder {
        batches: Mutex<Vec<RowBatch>>,
    }

    #[async_trait]
    impl DatabaseDriver for Recorder {
        fn driver(&self) -> Driver {
            Driver::Sqlite
        }

        fn server_version(&self) -> &str {
            "0"
        }

        async fn ping(&self) -> Result<()> {
            Ok(())
        }

        async fn catalog(&self) -> Result<Catalog> {
            Ok(Catalog::default())
        }

        async fn columns(&self, _table: &TableRef) -> Result<Vec<Column>> {
            Ok(Vec::new())
        }

        async fn table_rows(
            &self,
            _table: &TableRef,
            _page: Page,
            _where_clause: &str,
            _order: &[SortKey],
        ) -> Result<ResultSet> {
            Ok(ResultSet::default())
        }

        async fn row_count(&self, _table: &TableRef, _where_clause: &str) -> Result<i64> {
            Ok(0)
        }

        async fn apply_changes(&self, _table: &TableRef, batch: &RowBatch) -> Result<u64> {
            self.batches.lock().unwrap().push(batch.clone());
            Ok(batch.len() as u64)
        }

        async fn execute(&self, sql: &str) -> Result<QueryResult> {
            Err(DriverError::message(sql, "not implemented"))
        }

        async fn close(&self) {}
    }

    fn update(id: i64) -> RowUpdate {
        RowUpdate {
            pk: vec![("id".to_string(), Value::Int(id))],
            changes: vec![("name".to_string(), Value::Text("ada".into()))],
        }
    }

    /// Both single-row helpers exist for convenience only: an adapter that
    /// implements the batch primitive gets them for free, and they must arrive
    /// as *one* batch so the transaction still covers the lot.
    #[tokio::test]
    async fn the_single_row_helpers_are_one_batch_each() {
        let driver = Recorder::default();
        let table = TableRef::new("main", "users");
        let row = update(1);

        let touched = driver
            .update_row(&table, &row.pk, &row.changes)
            .await
            .expect("update");
        assert_eq!(touched, 1);

        let touched = driver
            .update_rows(&table, &[update(1), update(2), update(3)])
            .await
            .expect("update");
        assert_eq!(touched, 3);

        let batches = driver.batches.lock().unwrap();
        assert_eq!(batches.len(), 2, "one call to the primitive each");
        assert_eq!(batches[0].updates.len(), 1);
        assert_eq!(batches[0].updates[0].pk, row.pk);
        assert_eq!(batches[0].updates[0].changes, row.changes);
        assert_eq!(batches[1].updates.len(), 3);
        assert!(
            batches
                .iter()
                .all(|b| b.inserts.is_empty() && b.deletes.is_empty()),
            "an update batch touches nothing else"
        );
    }

    /// `is_empty` is what stops a commit with nothing staged from opening a
    /// transaction, so it has to account for all three lists.
    #[test]
    fn a_batch_is_empty_only_when_all_three_lists_are() {
        assert!(RowBatch::default().is_empty());
        assert_eq!(RowBatch::default().len(), 0);

        let batch = RowBatch {
            inserts: vec![RowInsert {
                values: vec![("name".to_string(), Value::Default)],
            }],
            updates: vec![update(1), update(2)],
            deletes: vec![RowDelete {
                pk: vec![("id".to_string(), Value::Int(9))],
            }],
        };
        assert!(!batch.is_empty());
        assert_eq!(batch.len(), 4, "every list counts");

        let deletes_only = RowBatch {
            deletes: vec![RowDelete {
                pk: vec![("id".to_string(), Value::Int(9))],
            }],
            ..RowBatch::default()
        };
        assert!(!deletes_only.is_empty(), "deletions alone are still work");

        let updates = RowBatch::of_updates(vec![update(1)]);
        assert_eq!(updates.len(), 1);
        assert!(updates.inserts.is_empty() && updates.deletes.is_empty());
    }
}
