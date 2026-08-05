//! Errors, phrased for the person who typed the query.
//!
//! These strings reach a status bar, not a log file. When the server has an
//! opinion -- a syntax error with a position, a failed authentication -- its
//! own words are the most useful thing available, so they are what gets kept.

use std::fmt;

pub type Result<T> = std::result::Result<T, DriverError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DriverError {
    #[error("Could not connect to {address}: {message}")]
    Connect { address: String, message: String },

    #[error("{message}")]
    Query {
        statement: String,
        message: String,
        /// The engine's SQLSTATE, when it gave one.
        code: Option<String>,
    },

    #[error("Could not read the database catalog: {0}")]
    Catalog(String),

    #[error("The connection is closed")]
    Closed,
}

impl DriverError {
    pub fn connect(address: impl fmt::Display, source: &sqlx::Error) -> Self {
        DriverError::Connect {
            address: address.to_string(),
            message: describe(source),
        }
    }

    pub fn query(statement: impl Into<String>, source: &sqlx::Error) -> Self {
        DriverError::Query {
            statement: statement.into(),
            message: describe(source),
            code: sqlx_code(source),
        }
    }

    pub fn catalog(source: &sqlx::Error) -> Self {
        DriverError::Catalog(describe(source))
    }

    pub fn message(statement: impl Into<String>, message: impl Into<String>) -> Self {
        DriverError::Query {
            statement: statement.into(),
            message: message.into(),
            code: None,
        }
    }
}

/// Unwrap a `sqlx::Error` down to the sentence worth showing.
///
/// `sqlx::Error`'s own `Display` wraps the server's message in scaffolding
/// ("error returned from database: ..."), which is noise on a status bar. When
/// the server spoke, quote the server.
fn describe(error: &sqlx::Error) -> String {
    match error {
        sqlx::Error::Database(db) => db.message().to_string(),
        sqlx::Error::PoolTimedOut => {
            "Timed out waiting for a connection from the pool".to_string()
        }
        sqlx::Error::PoolClosed => "The connection pool is closed".to_string(),
        sqlx::Error::RowNotFound => "No rows returned".to_string(),
        // An I/O failure's own message is usually terse ("connection refused")
        // and the layer above supplies the address it applies to.
        sqlx::Error::Io(io) => io.to_string(),
        other => other.to_string(),
    }
}

fn sqlx_code(error: &sqlx::Error) -> Option<String> {
    match error {
        sqlx::Error::Database(db) => db.code().map(|code| code.into_owned()),
        _ => None,
    }
}
