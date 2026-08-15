//! Database access: one port, two adapters.
//!
//! Everything engine-specific lives here -- connection options, introspection
//! SQL, type decoding, error phrasing. The layer above receives an
//! `Arc<dyn DatabaseDriver>` and cannot tell Postgres from MySQL except by
//! asking.

mod error;
mod mysql;
mod port;
mod postgres;
mod sqlite;
mod sql_build;

pub use error::{DriverError, Result};
pub use port::{DatabaseDriver, RowBatch, RowDelete, RowInsert, RowUpdate};
// Statements the UI offers to run but does not compose: quoting an identifier
// is this crate's job, and there is a test that a hostile table name cannot
// break out of one.
pub use sql_build::{drop_sql, truncate_sql};

use dbui_domain::{ConnectionConfig, Driver};
use std::sync::Arc;

/// Open a connection, picking the adapter from the config.
///
/// The only place in the codebase that names a concrete adapter. Everything
/// downstream holds the trait object.
pub async fn connect(config: &ConnectionConfig) -> Result<Arc<dyn DatabaseDriver>> {
    match config.driver {
        Driver::Postgres => Ok(Arc::new(postgres::PostgresDriver::connect(config).await?)),
        Driver::MySql => Ok(Arc::new(mysql::MySqlDriver::connect(config).await?)),
        Driver::Sqlite => Ok(Arc::new(sqlite::SqliteDriver::connect(config).await?)),
    }
}
