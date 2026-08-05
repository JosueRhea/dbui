//! The domain model every other layer speaks in.
//!
//! Nothing here performs I/O or knows that Postgres and MySQL exist beyond the
//! [`Driver`] discriminant. A type belongs in this crate when both database
//! adapters and the UI need to agree on its shape -- a column, a cell value, a
//! result set -- and nowhere else.
//!
//! The rule that keeps this honest: this crate has no dependency other than
//! `serde`. If something added here needs a database client or an async
//! runtime, it belongs one layer out, in `dbui-driver`.

pub mod catalog;
pub mod connection;
pub mod query;
pub mod value;

pub use catalog::{Catalog, Column, Schema, Table, TableKind, TableRef};
pub use connection::{ConnectionConfig, ConnectionId, Driver, TlsMode};
pub use query::{ColumnInfo, Page, QueryOutcome, QueryResult, QueryStats, ResultSet, Row};
pub use value::{Value, ValueKind};
