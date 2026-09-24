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

use sqlx::{Connection, Database, Pool};
use std::future::Future;
use std::pin::Pin;
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

/// How an engine names the session a connection is.
pub(crate) trait SessionId: Database {
    fn session_id(conn: &mut Self::Connection) -> SessionFuture<'_>;
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
}

impl<DB: SessionId> Sessions<DB> {
    pub(crate) fn new() -> Self {
        Self {
            idle: Mutex::new(Vec::new()),
            console: Mutex::new(None),
        }
    }

    /// The console connection: the same session the editor used last, so
    /// whatever it left open is still open. See the module docs.
    ///
    /// Not closed for age the way the others are -- that is exactly the
    /// rollback this exists to avoid. Only a connection that no longer
    /// answers is replaced, and then there is nothing left to keep.
    pub(crate) async fn acquire_console(
        &self,
        pool: &Pool<DB>,
    ) -> Result<Lease<'_, DB>, sqlx::Error> {
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
        }
        let mut lease = self.fresh(pool).await?;
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
    /// Kept after a result or an error the server sent -- a syntax error, a
    /// cancelled statement -- because the server finished talking and the
    /// connection is ready for the next one. Closed after anything else: an
    /// I/O or protocol failure says nothing good about what is left of it.
    ///
    /// Call it only once whatever was tracking the session has let go, or a
    /// late cancel aimed at this statement stops the next one on it instead.
    pub(crate) fn settle<T>(mut self, outcome: &Result<T, sqlx::Error>) {
        let reusable = matches!(outcome, Ok(_) | Err(sqlx::Error::Database(_)));
        let Some(conn) = self.conn.take() else {
            return;
        };
        if !reusable {
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
