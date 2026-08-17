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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    /// The status bar shows the error's `Display`, and each variant has to read
    /// as a sentence on its own -- the UI adds no context of its own.
    #[test]
    fn every_error_reads_as_a_sentence() {
        let connect = DriverError::Connect {
            address: "localhost:5432".into(),
            message: "connection refused".into(),
        };
        assert_eq!(
            connect.to_string(),
            "Could not connect to localhost:5432: connection refused"
        );

        assert_eq!(
            DriverError::message("SELECT 1", "syntax error at or near \"1\"").to_string(),
            "syntax error at or near \"1\"",
            "a query error is the server's own words, unwrapped"
        );
        assert_eq!(
            DriverError::Catalog("permission denied".into()).to_string(),
            "Could not read the database catalog: permission denied"
        );
        assert_eq!(DriverError::Closed.to_string(), "The connection is closed");
    }

    /// A hand-built query error keeps the statement for the editor to point at,
    /// and has no SQLSTATE because no server produced it.
    #[test]
    fn a_message_error_keeps_the_statement_and_no_code() {
        let error = DriverError::message("DELETE FROM t", "read-only connection");
        let DriverError::Query {
            statement,
            message,
            code,
        } = error
        else {
            panic!("a message is a query error");
        };
        assert_eq!(statement, "DELETE FROM t");
        assert_eq!(message, "read-only connection");
        assert_eq!(code, None);
    }

    /// The variants sqlx raises before the server is ever reached are worded
    /// here rather than passed through: `sqlx::Error`'s own text wraps them in
    /// scaffolding that means nothing to the person who typed the query.
    #[test]
    fn pool_failures_are_reworded_and_carry_no_sqlstate() {
        assert_eq!(
            DriverError::catalog(&sqlx::Error::PoolTimedOut),
            DriverError::Catalog("Timed out waiting for a connection from the pool".into())
        );
        assert_eq!(
            DriverError::catalog(&sqlx::Error::PoolClosed),
            DriverError::Catalog("The connection pool is closed".into())
        );
        assert_eq!(
            DriverError::catalog(&sqlx::Error::RowNotFound),
            DriverError::Catalog("No rows returned".into())
        );

        let timed_out = DriverError::query("SELECT 1", &sqlx::Error::PoolTimedOut);
        assert!(
            matches!(timed_out, DriverError::Query { code: None, .. }),
            "there is no SQLSTATE for a failure the server never saw"
        );
    }

    /// An I/O failure's own message is the terse part worth keeping; the address
    /// it applies to comes from the layer that knew it.
    #[test]
    fn a_connect_failure_keeps_the_address_and_the_terse_reason() {
        let io = sqlx::Error::Io(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            "connection refused",
        ));
        assert_eq!(
            DriverError::connect("localhost:5432", &io),
            DriverError::Connect {
                address: "localhost:5432".into(),
                message: "connection refused".into(),
            }
        );
    }

    /// Anything else falls back to sqlx's own words rather than a placeholder,
    /// so an unfamiliar failure still says something.
    #[test]
    fn an_unfamiliar_failure_falls_back_to_sqlxs_own_words() {
        let protocol = sqlx::Error::Protocol("unexpected message from server".into());
        let described = DriverError::catalog(&protocol);
        assert_eq!(
            described,
            DriverError::Catalog(protocol.to_string()),
            "nothing is invented and nothing is dropped"
        );
    }
}
