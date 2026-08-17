//! The engine-independent half of an adapter.
//!
//! Only what actually differs per engine belongs in an adapter module:
//! connection options, introspection SQL, decoding a wire value. Everything
//! else -- shaping rows into a [`ResultSet`], backfilling a header row, running
//! a batch in one transaction, timing a statement -- was the same code three
//! times over, so it lives here once, generic over the `sqlx` database.
//!
//! [`Engine`] is the seam. `sqlx` states what it needs as bounds on the value
//! types crossing the wire (`i64: Encode<DB>`, `String: Decode<DB>`, a pool
//! that is an `Executor`), which no generic caller can name without repeating
//! the whole list. So the list is written once, as the bounds of a blanket
//! `impl`, and every helper below asks only for `DB: Engine`.

use crate::error::{DriverError, Result};
use crate::port::RowBatch;
use crate::sql_build::{self, BoundSql};
use async_trait::async_trait;
use dbui_domain::{
    query, Catalog, Column, ColumnInfo, Driver, ForeignKey, Page, QueryOutcome, QueryResult,
    QueryStats, ResultSet, Row as DomainRow, Schema, SortKey, Table, TableKind, TableRef, Value,
};
use sqlx::{
    AssertSqlSafe, ColumnIndex, Database, Decode, Encode, Executor, FromRow, IntoArguments, Pool,
    Row as _, SqlSafeStr as _, Statement as _, Type,
};
use std::time::{Duration, Instant};

/// A GUI issues one query at a time per window, plus the occasional catalog
/// refresh behind it. A larger pool would just hold idle server connections
/// open.
pub const MAX_CONNECTIONS: u32 = 4;

/// Fail fast enough that a wrong host is a message, not a hang.
pub const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// One query with its binds, the shape [`sqlx::query`] hands back.
type Query<'q, DB> = sqlx::query::Query<'q, DB, <DB as Database>::Arguments>;

/// How many rows a statement touched.
///
/// `sqlx` gives every engine's query result this method but no trait carrying
/// it, so generic code cannot ask for it. This is that trait, and nothing more.
pub trait Affected {
    fn rows_affected(&self) -> u64;
}

macro_rules! affected {
    ($result:ty) => {
        impl Affected for $result {
            fn rows_affected(&self) -> u64 {
                <$result>::rows_affected(self)
            }
        }
    };
}

affected!(sqlx::postgres::PgQueryResult);
affected!(sqlx::mysql::MySqlQueryResult);
affected!(sqlx::sqlite::SqliteQueryResult);

/// The few things a `sqlx` database has to do for the helpers in this module.
///
/// Deliberately small and mechanical: send a statement, read a text column,
/// prepare without executing, run a list of statements in one transaction.
/// Every engine gets these from the blanket `impl` below, so no adapter
/// implements this trait itself.
///
/// Errors are mapped here rather than by callers, because the statement that
/// failed is in scope and the message reaches a status bar.
#[async_trait]
pub trait Engine: Database + Sized {
    /// Read a text column by name, or `None` if it is absent or not text.
    ///
    /// Catalog queries alias their columns to fixed names, so reading them by
    /// name is what lets one fold serve three dialects' SQL.
    fn text_at(row: &Self::Row, column: &str) -> Option<String>;

    /// Run a statement and collect its rows.
    async fn fetch(pool: &Pool<Self>, sql: &str, binds: &[Value]) -> Result<Vec<Self::Row>>;

    /// Run a statement for its effect, and report how many rows it touched.
    async fn run(pool: &Pool<Self>, sql: &str, binds: &[Value]) -> Result<u64>;

    /// Run a statement that returns a single number, such as a `count(*)`.
    async fn count(pool: &Pool<Self>, sql: &str) -> Result<i64>;

    /// The shape of a statement, without running it. `None` if the server
    /// would not prepare it.
    async fn statement_columns(pool: &Pool<Self>, sql: &str) -> Option<Vec<ColumnInfo>>;

    /// Run every statement in one transaction, or none of them.
    async fn run_all(pool: &Pool<Self>, statements: &[BoundSql]) -> Result<u64>;
}

#[async_trait]
impl<DB> Engine for DB
where
    DB: Database,
    for<'c> &'c Pool<DB>: Executor<'c, Database = DB>,
    for<'c> &'c mut <DB as Database>::Connection: Executor<'c, Database = DB>,
    <DB as Database>::Arguments: IntoArguments<DB>,
    <DB as Database>::QueryResult: Affected,
    for<'q> Option<String>: Encode<'q, DB> + Type<DB>,
    for<'q> String: Encode<'q, DB> + Type<DB>,
    for<'q> bool: Encode<'q, DB> + Type<DB>,
    for<'q> i64: Encode<'q, DB> + Type<DB>,
    for<'q> f64: Encode<'q, DB> + Type<DB>,
    for<'q> Vec<u8>: Encode<'q, DB> + Type<DB>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
    for<'r> (i64,): FromRow<'r, DB::Row>,
    for<'i> &'i str: ColumnIndex<DB::Row>,
{
    fn text_at(row: &DB::Row, column: &str) -> Option<String> {
        row.try_get::<String, _>(column).ok()
    }

    async fn fetch(pool: &Pool<DB>, sql: &str, binds: &[Value]) -> Result<Vec<DB::Row>> {
        bound_query::<DB>(sql, binds)
            .fetch_all(pool)
            .await
            .map_err(|error| DriverError::query(sql, &error))
    }

    async fn run(pool: &Pool<DB>, sql: &str, binds: &[Value]) -> Result<u64> {
        bound_query::<DB>(sql, binds)
            .execute(pool)
            .await
            .map(|done| done.rows_affected())
            .map_err(|error| DriverError::query(sql, &error))
    }

    async fn count(pool: &Pool<DB>, sql: &str) -> Result<i64> {
        sqlx::query_scalar(AssertSqlSafe(sql.to_string()))
            .fetch_one(pool)
            .await
            .map_err(|error| DriverError::query(sql, &error))
    }

    async fn statement_columns(pool: &Pool<DB>, sql: &str) -> Option<Vec<ColumnInfo>> {
        let prepared = Executor::prepare(pool, AssertSqlSafe(sql.to_string()).into_sql_str());
        prepared
            .await
            .ok()
            .map(|statement| column_info::<DB>(statement.columns()))
    }

    async fn run_all(pool: &Pool<DB>, statements: &[BoundSql]) -> Result<u64> {
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| DriverError::query("BEGIN", &error))?;

        let mut total = 0u64;
        for bound in statements {
            let done = bound_query::<DB>(&bound.sql, &bound.binds)
                .execute(&mut *tx)
                .await
                .map_err(|error| DriverError::query(&bound.sql, &error))?;
            total += done.rows_affected();
        }

        tx.commit()
            .await
            .map_err(|error| DriverError::query("COMMIT", &error))?;
        Ok(total)
    }
}

/// A statement with its values bound in order.
///
/// Each value goes over as the type it actually is. Every parameter used to be
/// sent as text, which Postgres refuses outright: `WHERE "id" = $1` against a
/// `bigint` column plans as `bigint = text`, and there is no such operator.
/// MySQL and SQLite coerce a string on their own, but binding the real type
/// skips that guesswork -- an integer key then compares as an integer, and all
/// three engines say the same thing about the same batch.
///
/// Decimal, Uuid, Json and Temporal ride over as text and are cast back by
/// `sql_build`'s placeholders where the engine needs it; Text needs no cast.
fn bound_query<'q, DB>(sql: &str, binds: &[Value]) -> Query<'q, DB>
where
    DB: Database,
    for<'e> Option<String>: Encode<'e, DB> + Type<DB>,
    for<'e> String: Encode<'e, DB> + Type<DB>,
    for<'e> bool: Encode<'e, DB> + Type<DB>,
    for<'e> i64: Encode<'e, DB> + Type<DB>,
    for<'e> f64: Encode<'e, DB> + Type<DB>,
    for<'e> Vec<u8>: Encode<'e, DB> + Type<DB>,
{
    let mut query = sqlx::query(AssertSqlSafe(sql.to_string()));
    for value in binds {
        query = match value {
            Value::Null | Value::Default => query.bind(Option::<String>::None),
            Value::Bool(flag) => query.bind(*flag),
            Value::Int(number) => query.bind(*number),
            Value::Float(number) => query.bind(*number),
            Value::Bytes(bytes) => query.bind(bytes.clone()),
            other => query.bind(other.to_text()),
        };
    }
    query
}

/// One engine's row decoder -- the only part of reading a result set that is
/// not shared.
pub type DecodeRow<DB> = fn(&<DB as Database>::Row) -> Vec<Value>;

/// Column names and type names, for a result set's header row.
fn column_info<DB: Database>(columns: &[DB::Column]) -> Vec<ColumnInfo> {
    use sqlx::{Column as _, TypeInfo as _};

    columns
        .iter()
        .map(|column| ColumnInfo {
            name: column.name().to_string(),
            type_name: column.type_info().name().to_string(),
        })
        .collect()
}

/// Turn sqlx rows into a [`ResultSet`], keeping at most `keep` of them.
///
/// The caller asked for `keep + 1`; if that many arrived there is more behind
/// them, which is exactly what `truncated` means.
fn result_set<DB: Database>(rows: Vec<DB::Row>, keep: usize, decode: DecodeRow<DB>) -> ResultSet {
    ResultSet {
        columns: rows
            .first()
            .map(|row| column_info::<DB>(row.columns()))
            .unwrap_or_default(),
        truncated: rows.len() > keep,
        rows: rows
            .iter()
            .take(keep)
            .map(|row| DomainRow(decode(row)))
            .collect(),
    }
}

/// Decode every column of one row, handing `cell` the type name the engine
/// reported for it.
pub fn decode_row<DB: Database>(
    row: &DB::Row,
    cell: fn(&DB::Row, usize, &str) -> Value,
) -> Vec<Value> {
    use sqlx::{Column as _, TypeInfo as _};

    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| cell(row, index, column.type_info().name()))
        .collect()
}

/// Ask the server for a statement's shape when the rows did not reveal it.
///
/// Column metadata rides along with the rows, so a query that matched nothing
/// arrives with no columns either -- and a grid with no headers looks broken
/// rather than empty. Preparing the statement gets its shape without running
/// it, which is the cheap way to get the header row back. Best-effort: if the
/// prepare fails, an empty grid is still correct.
async fn backfill_columns<DB: Engine>(pool: &Pool<DB>, set: &mut ResultSet, sql: &str) {
    if !set.columns.is_empty() || !set.rows.is_empty() {
        return;
    }
    if let Some(columns) = DB::statement_columns(pool, sql).await {
        set.columns = columns;
    }
}

/// Round-trip the connection to prove it is still there.
pub async fn ping<DB: Engine>(pool: &Pool<DB>) -> Result<()> {
    DB::run(pool, "SELECT 1", &[]).await.map(|_| ())
}

/// One page of a table's rows.
pub async fn table_page<DB: Engine>(
    pool: &Pool<DB>,
    driver: Driver,
    table: &TableRef,
    page: Page,
    where_clause: &str,
    order: &[SortKey],
    decode: DecodeRow<DB>,
) -> Result<ResultSet> {
    let bound = sql_build::select_page_sql(driver, table, where_clause, order);

    // The window's own two parameters are the last in the statement, so they
    // go on the end of the filter's binds.
    let mut binds = bound.binds;
    binds.push(Value::Int(page.probe_limit()));
    binds.push(Value::Int(page.offset as i64));

    let rows = DB::fetch(pool, &bound.sql, &binds).await?;
    let mut set = result_set::<DB>(rows, page.limit as usize, decode);
    backfill_columns(pool, &mut set, &bound.sql).await;
    Ok(set)
}

/// Total rows matching the same WHERE as [`table_page`].
pub async fn row_count<DB: Engine>(
    pool: &Pool<DB>,
    driver: Driver,
    table: &TableRef,
    where_clause: &str,
) -> Result<i64> {
    let bound = sql_build::count_sql(driver, table, where_clause);
    // The filter is freeform text spliced into the statement, so a count has
    // no parameters of its own -- the same trust model as the editor.
    debug_assert!(bound.binds.is_empty(), "count_sql binds nothing");
    DB::count(pool, &bound.sql).await
}

/// Apply a whole batch of inserts, edits and deletions in one transaction.
///
/// All of it or none of it: an editor that stages changes and commits them
/// together cannot honour that if the statements travel separately.
pub async fn apply_changes<DB: Engine>(
    pool: &Pool<DB>,
    driver: Driver,
    table: &TableRef,
    batch: &RowBatch,
) -> Result<u64> {
    if batch.is_empty() {
        return Ok(0);
    }
    let statements = sql_build::batch_sql(driver, table, batch)?;
    DB::run_all(pool, &statements).await
}

/// Run one statement as typed by the user, and time it.
pub async fn execute<DB: Engine>(
    pool: &Pool<DB>,
    sql: &str,
    decode: DecodeRow<DB>,
) -> Result<QueryResult> {
    let started = Instant::now();

    let outcome = if query::returns_rows(sql) {
        let rows = DB::fetch(pool, sql, &[]).await?;
        // A hand-written query is shown as-is: the user asked for these rows,
        // so no probe row is added and nothing is marked truncated.
        let mut set = result_set::<DB>(rows, usize::MAX, decode);
        backfill_columns(pool, &mut set, sql).await;
        QueryOutcome::Rows(set)
    } else {
        QueryOutcome::Affected(DB::run(pool, sql, &[]).await?)
    };

    Ok(QueryResult {
        statement: sql.to_string(),
        outcome,
        stats: QueryStats {
            elapsed: started.elapsed(),
        },
    })
}

/// Fold the schema and relation rows of a catalog query into a [`Catalog`].
///
/// Both are read by column name, so an adapter's SQL only has to alias them to
/// `schema_name`, `relation_name` and `relation_kind`.
pub fn catalog<DB: Engine>(
    schema_rows: &[DB::Row],
    relation_rows: &[DB::Row],
    table_kind: fn(&str) -> TableKind,
) -> Catalog {
    let mut schemas: Vec<Schema> = schema_rows
        .iter()
        .filter_map(|row| DB::text_at(row, "schema_name"))
        .map(|name| Schema {
            name,
            tables: Vec::new(),
        })
        .collect();

    for row in relation_rows {
        let (Some(schema_name), Some(name), Some(kind)) = (
            DB::text_at(row, "schema_name"),
            DB::text_at(row, "relation_name"),
            DB::text_at(row, "relation_kind"),
        ) else {
            continue;
        };

        let table = Table {
            schema: schema_name.clone(),
            name,
            kind: table_kind(&kind),
        };

        // The two queries can disagree if a schema is created between them;
        // trust the relation and add the schema rather than dropping the table
        // on the floor.
        match schemas.iter_mut().find(|s| s.name == schema_name) {
            Some(schema) => schema.tables.push(table),
            None => schemas.push(Schema {
                name: schema_name,
                tables: vec![table],
            }),
        }
    }

    Catalog { schemas }
}

/// Single-column foreign keys, from rows aliased `column_name`, `ref_table`
/// and `ref_column`.
///
/// `fixed_schema` is for an engine whose keys cannot cross a schema: SQLite has
/// only one, so its query has no `ref_schema` column to read.
pub fn foreign_keys<DB: Engine>(rows: &[DB::Row], fixed_schema: Option<&str>) -> Vec<ForeignKey> {
    rows.iter()
        .filter_map(|row| {
            let schema = match fixed_schema {
                Some(name) => name.to_string(),
                None => DB::text_at(row, "ref_schema")?,
            };
            Some(ForeignKey {
                column: DB::text_at(row, "column_name")?,
                references: TableRef::new(schema, DB::text_at(row, "ref_table")?),
                references_column: DB::text_at(row, "ref_column")?,
            })
        })
        .collect()
}

/// Point each column at the foreign key it is the source of, for the grid's
/// jump arrows.
pub fn attach_references(columns: &mut [Column], keys: &[ForeignKey]) {
    for column in columns {
        column.references = keys.iter().find(|key| key.column == column.name).cloned();
    }
}
