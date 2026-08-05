//! The bridge between the UI thread and the database.
//!
//! GPUI renders on the main thread and its executor is not `Send`; sqlx needs a
//! tokio reactor. Rather than try to make one drive the other, this holds a
//! tokio runtime on its own threads and passes results back over a oneshot
//! channel. The UI awaits that channel inside `cx.spawn`, so a query in flight
//! never blocks a frame.
//!
//! Every database call in the app goes through [`DbRuntime::spawn`]. Nothing
//! else is allowed to block the main thread on I/O.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::sync::oneshot;

/// Owns the tokio runtime for the process.
#[derive(Clone)]
pub struct DbRuntime {
    inner: Arc<tokio::runtime::Runtime>,
}

impl DbRuntime {
    pub fn new() -> std::io::Result<Self> {
        let inner = tokio::runtime::Builder::new_multi_thread()
            // Two threads is plenty for a GUI's query load, and it keeps the
            // idle footprint honest on a laptop.
            .worker_threads(2)
            .thread_name("dbui-db")
            .enable_all()
            .build()?;

        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    /// Run `future` on the database runtime; await the returned [`Task`] on the
    /// UI thread.
    pub fn spawn<T>(&self, future: impl Future<Output = T> + Send + 'static) -> Task<T>
    where
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.inner.spawn(async move {
            let output = future.await;
            // A closed receiver means the UI dropped the task -- the window
            // closed, or the view was replaced. Not an error; the work simply
            // has nowhere to go.
            let _ = tx.send(output);
        });
        Task { receiver: rx }
    }
}

/// A unit of database work in flight.
///
/// Resolves to `None` if the work was dropped before it finished -- a panic in
/// the task, or a runtime shutting down. Callers treat that as "no answer" and
/// leave the UI as it was.
pub struct Task<T> {
    receiver: oneshot::Receiver<T>,
}

impl<T> Future for Task<T> {
    type Output = Option<T>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.receiver).poll(cx).map(Result::ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_crosses_back_to_the_caller() {
        let runtime = DbRuntime::new().expect("runtime");
        let task = runtime.spawn(async { 2 + 2 });
        // Blocking is a test convenience; the app awaits this on GPUI's
        // executor, from `cx.spawn`.
        let answer = runtime.inner.block_on(task);
        assert_eq!(answer, Some(4));
    }

    #[test]
    fn a_panicking_task_yields_no_answer_instead_of_unwinding_the_caller() {
        let runtime = DbRuntime::new().expect("runtime");
        let task = runtime.spawn(async { panic!("boom") });
        let answer: Option<()> = runtime.inner.block_on(task);
        assert_eq!(answer, None);
    }
}
