//! Application layer: what the app can do, and what it currently knows.
//!
//! This crate is the whole of the app minus its pixels. It owns the tokio
//! runtime, the use cases, the workspace state and the on-disk connection
//! store -- and it does not depend on GPUI, so all of that can be exercised
//! from a plain `#[test]`.

pub mod commands;
pub mod runtime;
pub mod store;
pub mod workspace;

pub use commands::{Outcome, TableContents};
pub use runtime::{DbRuntime, Task};
pub use workspace::{ConnectionEntry, ConnectionStatus, Workspace};

// Re-exported so the UI can name the things it renders without depending on
// `dbui-domain` and `dbui-driver` directly. The layer boundary is easier to
// keep when there is exactly one crate to import from.
pub use dbui_domain as domain;
pub use dbui_driver::{DatabaseDriver, DriverError, RowUpdate};
