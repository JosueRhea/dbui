//! Application layer: what the app can do, and what it currently knows.
//!
//! This crate is the whole of the app minus its pixels. It owns the tokio
//! runtime, the use cases, the workspace state and the on-disk connection
//! store -- and it does not depend on GPUI, so all of that can be exercised
//! from a plain `#[test]`.

pub mod commands;
pub mod dump;
pub mod history;
pub mod plan;
pub mod runtime;
pub mod saved;
pub mod session;
pub mod store;
pub mod tableplus;
pub mod updater;
pub mod workspace;

pub use commands::{BatchQueryResult, Outcome, PageSink, TableContents};
pub use history::{History, HistoryEntry};
pub use plan::{Plan, PlanStep};
pub use runtime::{DbRuntime, Task};
pub use saved::{SavedQueries, SavedQuery};
pub use session::{SavedConnectionTab, SavedTab, Session};
pub use tableplus::{import_from_tableplus, ImportReport, TablePlusError};
pub use updater::{Update, UpdateError, Version};
pub use workspace::{ConnectionEntry, ConnectionStatus, Workspace};

// Re-exported so the UI can name the things it renders without depending on
// `dbui-domain` and `dbui-driver` directly. The layer boundary is easier to
// keep when there is exactly one crate to import from.
pub use dbui_domain as domain;
pub use dbui_driver::{DatabaseDriver, DriverError, RowBatch, RowDelete, RowInsert, RowUpdate};

/// Open a connection directly, without going through a [`DbRuntime`].
///
/// The UI never needs this -- every database call it makes goes through the
/// runtime so a query cannot block a frame. It exists so a test above this
/// layer can lay down a fixture, which for SQLite needs no server at all.
pub async fn connect_driver(
    config: &domain::ConnectionConfig,
) -> Result<std::sync::Arc<dyn DatabaseDriver>, DriverError> {
    dbui_driver::connect(config).await
}
