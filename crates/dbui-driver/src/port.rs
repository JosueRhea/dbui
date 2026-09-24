//! The port: what any database engine must be able to do.
//!
//! `dbui-app` is written against this trait and never against a concrete
//! engine. Adding SQLite would mean one more implementation here and one more
//! arm in [`crate::connect`] -- and no change at all in the UI.

use crate::error::{DriverError, Result};
use async_trait::async_trait;
use dbui_domain::{
    Catalog, Column, DbObject, Driver, Index, Page, QueryResult, ResultSet, SortKey, TableRef,
    TransactionState, Value,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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

    /// The statement that would create `object` as it stands: a function's
    /// `CREATE OR REPLACE FUNCTION`, a trigger's `CREATE TRIGGER`, and so on.
    /// Opened in an editor, where it can be read, changed and run again.
    async fn definition(&self, object: &DbObject) -> Result<String> {
        Err(DriverError::message(
            "",
            format!(
                "Reading a {}'s definition is not supported on this engine",
                object.kind.label()
            ),
        ))
    }

    /// One table's indexes, by name, each with its columns in index order.
    async fn indexes(&self, table: &TableRef) -> Result<Vec<Index>> {
        let _ = table;
        Ok(Vec::new())
    }

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

    /// [`table_rows`](Self::table_rows), tracked in `token` the way
    /// [`execute_tracked`](Self::execute_tracked) is, so a page load can be
    /// stopped on the server too. The default runs it untracked.
    async fn table_rows_tracked(
        &self,
        table: &TableRef,
        page: Page,
        where_clause: &str,
        order: &[SortKey],
        token: &QueryToken,
    ) -> Result<ResultSet> {
        let _ = token;
        self.table_rows(table, page, where_clause, order).await
    }

    /// [`row_count`](Self::row_count), tracked in `token`. A `COUNT(*)` over
    /// a large table is as often the slow half of a page load as the page is.
    async fn row_count_tracked(
        &self,
        table: &TableRef,
        where_clause: &str,
        token: &QueryToken,
    ) -> Result<i64> {
        let _ = token;
        self.row_count(table, where_clause).await
    }

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

    /// [`execute`](Self::execute), leaving in `token` what [`cancel`] needs
    /// to stop it from elsewhere while it runs.
    ///
    /// The default runs it untracked, for an engine with nothing to record.
    ///
    /// [`cancel`]: Self::cancel
    async fn execute_tracked(&self, sql: &str, token: &QueryToken) -> Result<QueryResult> {
        let _ = token;
        self.execute(sql).await
    }

    /// Ask the server to stop the statement `token` is tracking.
    ///
    /// Dropping the future that awaits a query does not stop it: the server
    /// keeps working -- or keeps waiting on the lock it is stuck behind --
    /// until it next tries to write to a socket that may be minutes away.
    /// This reaches the server on a connection of its own and tells it.
    ///
    /// `Ok(true)` when the server was told, and the statement will now end
    /// by itself, with an error. `Ok(false)` when there was nothing to tell:
    /// a statement not yet started or already finished, or an engine that
    /// runs in-process and stops when its future is dropped -- which is then
    /// the caller's job.
    async fn cancel(&self, token: &QueryToken) -> Result<bool> {
        let _ = token;
        Ok(false)
    }

    /// Whether the SQL editor's session has a transaction open, as of its
    /// last statement. The default is for an engine that cannot tell.
    fn editor_transaction(&self) -> TransactionState {
        TransactionState::Idle
    }

    /// Close the pool. Idempotent.
    async fn close(&self);
}

/// A statement in flight, as seen from outside it.
///
/// Holds the server's id for the session the statement runs on -- a
/// Postgres backend pid, a MySQL connection id -- once
/// [`DatabaseDriver::execute_tracked`] has learned it. Cloned to whoever may
/// want to stop it.
#[derive(Debug, Clone, Default)]
pub struct QueryToken {
    session: Arc<Mutex<Option<u64>>>,
    /// Set by a cancel on an engine that stops a statement from inside --
    /// SQLite, whose progress handler reads it between steps.
    interrupted: Arc<AtomicBool>,
}

impl QueryToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Note the session the statement is about to run on, for as long as the
    /// returned guard lives.
    ///
    /// The guard forgets it again on drop -- when the statement finishes, and
    /// just as much when its future is dropped mid-run. The connection goes
    /// back to the pool after that, and a late cancel aimed at the session
    /// would stop whatever it ran next instead.
    pub(crate) fn track(&self, session: u64) -> Tracking<'_> {
        if let Ok(mut slot) = self.session.lock() {
            *slot = Some(session);
        }
        Tracking(self)
    }

    /// The server session the statement is running on, once known.
    pub fn session(&self) -> Option<u64> {
        self.session.lock().ok().and_then(|slot| *slot)
    }

    pub(crate) fn interrupt(&self) {
        self.interrupted.store(true, Ordering::SeqCst);
    }

    /// Whether the statement this tracks should stop now: it is still
    /// running, and it has been told to.
    ///
    /// Both, not just the flag. A statement whose future was dropped leaves
    /// SQLite's progress handler installed on the connection until the next
    /// tracked run replaces it, and that handler must not go on stopping
    /// whatever runs there next.
    pub(crate) fn should_stop(&self) -> bool {
        self.interrupted.load(Ordering::SeqCst) && self.session().is_some()
    }
}

/// See [`QueryToken::track`].
pub(crate) struct Tracking<'a>(&'a QueryToken);

impl Drop for Tracking<'_> {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.0.session.lock() {
            *slot = None;
        }
    }
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

/// Fold one-row-per-column index listings into [`Index`]es, keeping the
/// order the rows came in -- by index, then by position in it.
pub(crate) fn group_indexes(rows: Vec<(String, bool, bool, Option<String>)>) -> Vec<Index> {
    let mut indexes: Vec<Index> = Vec::new();
    for (name, unique, primary, column) in rows {
        let index = match indexes.iter_mut().position(|index| index.name == name) {
            Some(at) => &mut indexes[at],
            None => {
                indexes.push(Index {
                    name,
                    columns: Vec::new(),
                    unique,
                    primary,
                });
                indexes.last_mut().expect("just pushed")
            }
        };
        // An expression index has no column to name for that part.
        if let Some(column) = column {
            index.columns.push(column);
        }
    }
    indexes
}
