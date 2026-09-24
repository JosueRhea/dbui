//! Connections whose server session is already known.
//!
//! A statement that may have to be stopped has to know which server session
//! it runs on: that id is what `pg_cancel_backend` and `KILL QUERY` take.
//! Neither sqlx connection says, so learning it costs a round trip of its own
//! -- and paid on every page load, that is a slower grid on a remote server
//! for the sake of a cancel almost nobody presses.
//!
//! So it is paid once per connection instead. A connection is taken out of
//! the pool the first time it is needed here, asked its id, and kept in this
//! cache from then on with the id beside it. It comes back only after a
//! statement ended with the server's own answer -- a result, or an error the
//! server sent -- and never after one that was dropped mid-flight, which
//! leaves the connection in the middle of a conversation nobody will finish.
//!
//! A kept connection used in the last half minute goes straight back to work,
//! skipping even the liveness ping the pool itself sends on every acquire.
//!
//! One connection is set apart from the rest: the console, which every
//! statement typed into the SQL editor runs on. A session carries state --
//! an open transaction above all, but also `SET`s and temporary tables -- and
//! a `BEGIN` in one run means nothing if the `UPDATE` in the next lands on a
//! different connection. Kept apart, the user's transaction also never leaks
//! into the page loads that borrow the other connections, and the console is
//! never closed for being quiet, which would roll that transaction back.

use dbui_domain::TransactionState;
use sqlx::{Connection, Database, Pool};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How many idle connections are kept. More than the tabs that realistically
/// load at once; the rest are closed as they come back.
const KEEP: usize = 4;

/// Idle for less than this, a connection is used without asking first.
const FRESH: Duration = Duration::from_secs(30);

/// Idle for longer than this, it is closed rather than trusted: servers and
/// the proxies in front of them drop quiet connections on timers of their own.
const STALE: Duration = Duration::from_secs(5 * 60);

type SessionFuture<'c> = Pin<Box<dyn Future<Output = Result<u64, sqlx::Error>> + Send + 'c>>;

type StateFuture<'c> = Pin<Box<dyn Future<Output = TransactionState> + Send + 'c>>;

/// How an engine names the session a connection is, and says whether it is
/// in a transaction.
pub(crate) trait SessionId: Database {
    fn session_id(conn: &mut Self::Connection) -> SessionFuture<'_>;

    /// Asked of the server: sqlx tracks the transactions it opened itself,
    /// not a `BEGIN` somebody typed. An engine that cannot tell says `Idle`.
    fn transaction_state(conn: &mut Self::Connection) -> StateFuture<'_>;

    /// Tell `session`, from a connection out of `pool`, to stop what it is
    /// running. Best-effort: a statement that already ended ignores it.
    fn stop_session(
        pool: &Pool<Self>,
        session: u64,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    /// Whether a statement stopped that way leaves an open transaction
    /// usable. PostgreSQL's does not: the transaction is failed from there on.
    const STOP_KEEPS_TRANSACTION: bool;
}

/// Why the console could not be had.
pub(crate) enum ConsoleError {
    /// It went away with a transaction open. See [`DriverError::TransactionLost`].
    ///
    /// [`DriverError::TransactionLost`]: crate::DriverError::TransactionLost
    TransactionLost,
    Sql(sqlx::Error),
}

impl ConsoleError {
    pub(crate) fn for_statement(self, sql: &str) -> crate::DriverError {
        match self {
            ConsoleError::TransactionLost => crate::DriverError::TransactionLost {
                statement: sql.to_string(),
            },
            ConsoleError::Sql(error) => crate::DriverError::query(sql, &error),
        }
    }
}

struct Idle<DB: Database> {
    conn: DB::Connection,
    session: u64,
    since: Instant,
}

/// The cache itself. One per driver, beside its pool.
pub(crate) struct Sessions<DB: Database> {
    idle: Mutex<Vec<Idle<DB>>>,
    /// The editor's own connection, while it is not running anything.
    console: Mutex<Option<Idle<DB>>>,
    /// What the console's session was last seen doing with transactions.
    console_state: Mutex<TransactionState>,
    /// The console went away while `console_state` said a transaction was
    /// open. Taken by the next console acquire, which refuses.
    lost: AtomicBool,
}

impl<DB: Database> Sessions<DB> {
    /// Whether the editor's session has a transaction open, as last seen.
    pub(crate) fn console_transaction(&self) -> TransactionState {
        self.console_state
            .lock()
            .map(|state| *state)
            .unwrap_or_default()
    }

    pub(crate) fn set_console_transaction(&self, state: TransactionState) {
        if let Ok(mut current) = self.console_state.lock() {
            *current = state;
        }
    }

    /// Whether the console went away with a transaction open since this was
    /// last asked. Asking clears it: the loss is reported once.
    pub(crate) fn take_lost(&self) -> bool {
        self.lost.swap(false, Ordering::SeqCst)
    }

    /// The console connection is gone. Whatever transaction it held went with
    /// it, which the next run has to hear about.
    fn console_gone(&self) {
        if self.console_transaction() != TransactionState::Idle {
            self.lost.store(true, Ordering::SeqCst);
        }
        self.set_console_transaction(TransactionState::Idle);
    }
}

impl<DB: SessionId> Sessions<DB> {
    pub(crate) fn new() -> Self {
        Self {
            idle: Mutex::new(Vec::new()),
            console: Mutex::new(None),
            console_state: Mutex::new(TransactionState::Idle),
            lost: AtomicBool::new(false),
        }
    }

    /// The console connection: the same session the editor used last, so
    /// whatever it left open is still open. See the module docs.
    ///
    /// Not closed for age the way the others are -- that is exactly the
    /// rollback this exists to avoid. Only a connection that no longer
    /// answers is replaced, and then there is nothing left to keep.
    ///
    /// If it went away with a transaction open -- here, or under the
    /// statement before -- this refuses, once, rather than carrying on in a
    /// fresh session as if the transaction were still there.
    pub(crate) async fn acquire_console(
        &self,
        pool: &Pool<DB>,
    ) -> Result<Lease<'_, DB>, ConsoleError> {
        let kept = self.console.lock().ok().and_then(|mut slot| slot.take());
        if let Some(mut idle) = kept {
            if idle.since.elapsed() <= FRESH || idle.conn.ping().await.is_ok() {
                return Ok(Lease {
                    conn: Some(idle.conn),
                    session: idle.session,
                    home: self,
                    console: true,
                });
            }
            self.console_gone();
        }
        if self.lost.swap(false, Ordering::SeqCst) {
            return Err(ConsoleError::TransactionLost);
        }
        let mut lease = self.fresh(pool).await.map_err(ConsoleError::Sql)?;
        lease.console = true;
        Ok(lease)
    }

    /// A connection and its session id, from the cache if one is there and
    /// still good, otherwise fresh out of `pool`.
    pub(crate) async fn acquire(&self, pool: &Pool<DB>) -> Result<Lease<'_, DB>, sqlx::Error> {
        loop {
            let Some(mut idle) = self.idle.lock().ok().and_then(|mut idle| idle.pop()) else {
                break;
            };
            let quiet = idle.since.elapsed();
            if quiet > STALE {
                let _ = idle.conn.close().await;
                continue;
            }
            if quiet > FRESH && idle.conn.ping().await.is_err() {
                continue;
            }
            return Ok(Lease {
                conn: Some(idle.conn),
                session: idle.session,
                home: self,
                console: false,
            });
        }
        self.fresh(pool).await
    }

    /// A connection straight out of `pool`, with its session id learned.
    async fn fresh(&self, pool: &Pool<DB>) -> Result<Lease<'_, DB>, sqlx::Error> {
        // Detached rather than held: the pool's own capacity stays for the
        // untracked calls, and this one is ours to keep or close.
        let mut conn = pool.acquire().await?.detach();
        let session = DB::session_id(&mut conn).await?;
        Ok(Lease {
            conn: Some(conn),
            session,
            home: self,
            console: false,
        })
    }

    /// Close everything kept. For when the pool itself is closing.
    pub(crate) async fn close(&self) {
        let idle = self
            .idle
            .lock()
            .map(|mut idle| std::mem::take(&mut *idle))
            .unwrap_or_default();
        let console = self.console.lock().ok().and_then(|mut slot| slot.take());
        for entry in idle.into_iter().chain(console) {
            let _ = entry.conn.close().await;
        }
    }
}

/// Whether a connection is still fit for another statement after `outcome`.
///
/// Yes after a result or an error the server sent -- a syntax error, a
/// cancelled statement -- because the server finished talking. No after an
/// I/O or protocol failure, and no after the server's own word that the
/// connection is over: SQLSTATE class 08 (connection exception), or 57P01 to
/// 57P03 (administrator shutdown, crash, cannot connect now), which
/// PostgreSQL sends just before it hangs up.
pub(crate) fn survives<T>(outcome: &Result<T, sqlx::Error>) -> bool {
    match outcome {
        Ok(_) => true,
        Err(sqlx::Error::Database(error)) => !error
            .code()
            .is_some_and(|code| code.starts_with("08") || code.starts_with("57P")),
        Err(_) => false,
    }
}

/// A connection out of [`Sessions`] for one statement.
///
/// Dropped without [`settle`](Self::settle) -- the statement's future was
/// dropped mid-flight -- the connection is closed, not kept.
pub(crate) struct Lease<'a, DB: Database> {
    conn: Option<DB::Connection>,
    session: u64,
    home: &'a Sessions<DB>,
    /// Goes back to the console slot rather than among the idle.
    console: bool,
}

impl<DB: SessionId> Lease<'_, DB> {
    /// A result was cut short at the row cap with the server still sending
    /// the rest. Tell it to stop, then read what is left off the wire so the
    /// connection is ready for the next statement -- without the stop, that
    /// is every remaining row, and the next statement waits for all of them.
    ///
    /// Not stopped when the stop would cost an open transaction; then the
    /// rest is read and thrown away. `false` when the connection did not come
    /// back ready, and should be closed rather than kept.
    pub(crate) async fn cut_short(&mut self, pool: &Pool<DB>) -> bool {
        let in_transaction =
            self.console && self.home.console_transaction() != TransactionState::Idle;
        if !in_transaction || DB::STOP_KEEPS_TRANSACTION {
            DB::stop_session(pool, self.session).await;
        }
        // The first wait ends at the stopped statement's error; the second
        // is a clean round trip. A stop that landed late, on the first ping
        // instead of the statement, costs only that ping.
        for _ in 0..2 {
            if self.conn().ping().await.is_ok() {
                return true;
            }
        }
        false
    }

    /// After an editor statement: ask the session whether a transaction is
    /// open, when the statement could have changed that or one already was.
    pub(crate) async fn note_transaction(&mut self, sql: &str) {
        if !self.console {
            return;
        }
        let open = self.home.console_transaction() != TransactionState::Idle;
        if open || dbui_domain::may_change_transaction(sql) {
            let state = DB::transaction_state(self.conn()).await;
            self.home.set_console_transaction(state);
        }
    }
}

/// A lease dropped unsettled closes its connection -- see the type's docs --
/// and for the console that closes whatever transaction was open on it.
impl<DB: Database> Drop for Lease<'_, DB> {
    fn drop(&mut self) {
        if self.console && self.conn.is_some() {
            self.home.console_gone();
        }
    }
}

impl<DB: Database> Lease<'_, DB> {
    /// The server's id for the session this connection is.
    pub(crate) fn session(&self) -> u64 {
        self.session
    }

    pub(crate) fn conn(&mut self) -> &mut DB::Connection {
        self.conn
            .as_mut()
            .expect("a lease holds its connection until settled")
    }

    /// Hand the connection back, or close it, by how its statement ended.
    ///
    /// Kept when [`survives`] says the connection is ready for the next
    /// statement; closed otherwise.
    ///
    /// Call it only once whatever was tracking the session has let go, or a
    /// late cancel aimed at this statement stops the next one on it instead.
    pub(crate) fn settle<T>(mut self, outcome: &Result<T, sqlx::Error>) {
        let reusable = survives(outcome);
        let Some(conn) = self.conn.take() else {
            return;
        };
        if !reusable {
            if self.console {
                self.home.console_gone();
            }
            return;
        }
        let idle = Idle {
            conn,
            session: self.session,
            since: Instant::now(),
        };
        if self.console {
            // Only one editor runs at a time; should a second console lease
            // ever come back to a full slot, the one already there stays.
            if let Ok(mut slot) = self.home.console.lock() {
                slot.get_or_insert(idle);
            }
            return;
        }
        if let Ok(mut kept) = self.home.idle.lock() {
            if kept.len() < KEEP {
                kept.push(idle);
            }
        }
    }
}
