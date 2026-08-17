//! Introspection SQL for MySQL.
//!
//! MySQL has no schema layer under a database: `information_schema` calls a
//! database a `TABLE_SCHEMA`, which is exactly the level the domain's
//! [`Schema`] sits at. So a MySQL "schema" in this app is a database, and the
//! four system databases are hidden the way `pg_catalog` is on the other side.
//!
//! Vitess also exposes `_vt` (sidecar metadata) via `information_schema`, but
//! that keyspace is not in the VSchema — querying it returns VT05003 — so it
//! is filtered out here too.
//!
//! Every statement here is a `&'static str`, which is also what lets sqlx 0.9
//! accept it without an `AssertSqlSafe` wrapper: no catalog query is assembled
//! from anything the user typed.
//!
//! [`Schema`]: dbui_domain::Schema

/// The user's databases, excluding the server's own bookkeeping (and Vitess `_vt`).
pub const SCHEMAS: &str = "
    SELECT SCHEMA_NAME AS schema_name
      FROM information_schema.SCHEMATA
     WHERE SCHEMA_NAME NOT IN (
         'mysql', 'information_schema', 'performance_schema', 'sys', '_vt'
     )
     ORDER BY SCHEMA_NAME
";

pub const RELATIONS: &str = "
    SELECT TABLE_SCHEMA AS schema_name,
           TABLE_NAME   AS relation_name,
           TABLE_TYPE   AS relation_kind
      FROM information_schema.TABLES
     WHERE TABLE_SCHEMA NOT IN (
         'mysql', 'information_schema', 'performance_schema', 'sys', '_vt'
     )
     ORDER BY TABLE_SCHEMA, TABLE_NAME
";

/// `COLUMN_TYPE` rather than `DATA_TYPE`: the former is `varchar(255)` and
/// `int unsigned`, the latter just `varchar` and `int`.
pub const COLUMNS: &str = "
    SELECT COLUMN_NAME      AS column_name,
           COLUMN_TYPE      AS data_type,
           IS_NULLABLE      AS is_nullable,
           COLUMN_DEFAULT   AS column_default,
           COLUMN_KEY       AS column_key,
           ORDINAL_POSITION AS ordinal
      FROM information_schema.COLUMNS
     WHERE TABLE_SCHEMA = ?
       AND TABLE_NAME = ?
     ORDER BY ORDINAL_POSITION
";

/// Single-column foreign keys on one table.
///
/// The `HAVING count(*) = 1` is the composite-key filter: a key spanning two
/// columns cannot be followed from the one cell the user clicked.
pub const FOREIGN_KEYS: &str = "
    SELECT k.COLUMN_NAME            AS column_name,
           k.REFERENCED_TABLE_SCHEMA AS ref_schema,
           k.REFERENCED_TABLE_NAME   AS ref_table,
           k.REFERENCED_COLUMN_NAME  AS ref_column
      FROM information_schema.KEY_COLUMN_USAGE k
      JOIN (
             SELECT CONSTRAINT_SCHEMA, CONSTRAINT_NAME
               FROM information_schema.KEY_COLUMN_USAGE
              WHERE TABLE_SCHEMA = ?
                AND TABLE_NAME = ?
                AND REFERENCED_TABLE_NAME IS NOT NULL
              GROUP BY CONSTRAINT_SCHEMA, CONSTRAINT_NAME
             HAVING count(*) = 1
           ) single
        ON single.CONSTRAINT_SCHEMA = k.CONSTRAINT_SCHEMA
       AND single.CONSTRAINT_NAME = k.CONSTRAINT_NAME
     WHERE k.TABLE_SCHEMA = ?
       AND k.TABLE_NAME = ?
       AND k.REFERENCED_TABLE_NAME IS NOT NULL
";

pub const SERVER_VERSION: &str = "SELECT VERSION()";

/// `information_schema.TABLES.TABLE_TYPE` -> the domain's [`TableKind`].
///
/// MySQL has no materialised views, so there are only two cases to tell apart.
///
/// [`TableKind`]: dbui_domain::TableKind
pub fn table_kind(table_type: &str) -> dbui_domain::TableKind {
    use dbui_domain::TableKind;
    if table_type.eq_ignore_ascii_case("VIEW") {
        TableKind::View
    } else {
        TableKind::Table
    }
}

#[cfg(test)]
mod tests {
    use dbui_domain::TableKind;

    /// `TABLE_TYPE` is `BASE TABLE`, `VIEW` or a temporary/system variant, and
    /// MySQL does not promise a case for it -- everything that is not a view is
    /// something the grid can read from.
    #[test]
    fn only_a_view_is_a_view() {
        assert_eq!(super::table_kind("VIEW"), TableKind::View);
        assert_eq!(super::table_kind("view"), TableKind::View);
        assert_eq!(super::table_kind("BASE TABLE"), TableKind::Table);
        assert_eq!(super::table_kind("SYSTEM VIEW"), TableKind::Table);
        assert_eq!(super::table_kind(""), TableKind::Table);
    }
}
