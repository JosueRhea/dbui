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
pub mod ddl;
pub mod query;
pub mod sql_split;
pub mod statement;
pub mod value;

pub use catalog::{Catalog, Column, ForeignKey, Schema, Table, TableKind, TableRef};
pub use connection::{ConnectionConfig, ConnectionId, Driver, Environment, SshConfig, TlsMode};
pub use ddl::{ColumnSpec, Index};
pub use query::{
    order_for, ColumnInfo, Page, QueryOutcome, QueryResult, QueryStats, ResultSet, Row, SortKey,
    TransactionState,
};
pub use sql_split::{split_statements, split_statements_for, statement_at, statement_at_for};
pub use statement::{may_change_transaction, writes, StatementInfo};
pub use value::{compare as compare_values, Value, ValueKind};
