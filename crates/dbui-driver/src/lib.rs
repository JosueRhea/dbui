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
mod sessions;
mod sql_build;
mod sqlite;
mod tunnel;

pub use error::{DriverError, Result};
pub use port::{DatabaseDriver, QueryToken, RowBatch, RowDelete, RowInsert, RowUpdate};
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
    if config.uses_ssh() {
        return connect_through_ssh(config).await;
    }
    match config.driver {
        Driver::Postgres => Ok(Arc::new(postgres::PostgresDriver::connect(config).await?)),
        Driver::MySql => Ok(Arc::new(mysql::MySqlDriver::connect(config).await?)),
        Driver::Sqlite => Ok(Arc::new(sqlite::SqliteDriver::connect(config).await?)),
    }
}

/// Open the tunnel, then connect to its local end.
///
/// The adapter keeps the tunnel, so the two share one lifetime: a driver that
/// is dropped takes its `ssh` with it, and there is no tunnel left running for
/// a connection nobody holds.
async fn connect_through_ssh(config: &ConnectionConfig) -> Result<Arc<dyn DatabaseDriver>> {
    let tunnel = tunnel::SshTunnel::open(config).await?;
    let through = tunnel.rewrite(config);
    // The adapter reports the address it dialed, which is now a local port
    // nobody typed. Name the one the user did, and the hop in between.
    let relabel = |error: DriverError| match error {
        DriverError::Connect { message, .. } => DriverError::Connect {
            address: format!(
                "{}:{} via SSH {}",
                config.host,
                config.port,
                config.ssh.summary()
            ),
            message,
        },
        other => other,
    };
    match config.driver {
        Driver::Postgres => {
            let mut driver = postgres::PostgresDriver::connect(&through)
                .await
                .map_err(relabel)?;
            driver.tunnel = Some(tunnel);
            Ok(Arc::new(driver))
        }
        Driver::MySql => {
            let mut driver = mysql::MySqlDriver::connect(&through)
                .await
                .map_err(relabel)?;
            driver.tunnel = Some(tunnel);
            Ok(Arc::new(driver))
        }
        // `uses_ssh` is false for a file; there is nothing to tunnel to.
        Driver::Sqlite => Ok(Arc::new(sqlite::SqliteDriver::connect(config).await?)),
    }
}
