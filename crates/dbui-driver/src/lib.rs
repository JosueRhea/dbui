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
mod sql_build;

pub use error::{DriverError, Result};
pub use port::{DatabaseDriver, RowUpdate};

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
    }
}
