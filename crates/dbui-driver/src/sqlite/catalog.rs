//! SQLite introspection.
//!
//! SQLite has no `information_schema` and no `pg_catalog`. `sqlite_master` is
//! the closest thing, and the per-table details come from `pragma_*` table
//! functions -- which is why these queries look nothing like the other two
//! adapters'.

/// SQLite has exactly one schema worth showing.
///
/// `ATTACH`ed databases appear in `pragma_database_list`, but a connection
/// this app opens attaches nothing, so `main` is the whole story.
pub const SCHEMA_NAME: &str = "main";

pub const RELATIONS: &str = "
    SELECT name AS relation_name,
           type AS relation_kind
      FROM sqlite_master
     WHERE type IN ('table', 'view')
       AND name NOT LIKE 'sqlite_%'
     ORDER BY name
";

/// Columns of one table.
///
/// `pragma_table_info` reports the declared type, nullability and which
/// columns are in the primary key (`pk` is the 1-based position, 0 for
/// columns outside it).
pub const COLUMNS: &str = "
    SELECT name         AS column_name,
           type         AS data_type,
           \"notnull\"    AS not_null,
           dflt_value   AS column_default,
           pk           AS pk_position,
           cid          AS ordinal
      FROM pragma_table_info(?)
     ORDER BY cid
";

/// Single-column foreign keys of one table.
///
/// `pragma_foreign_key_list` gives one row per column of each key, numbered by
/// `id`; a key with more than one row is composite and cannot be followed from
/// a single cell.
pub const FOREIGN_KEYS: &str = "
    SELECT f.\"from\"  AS column_name,
           f.\"table\" AS ref_table,
           f.\"to\"    AS ref_column
      FROM pragma_foreign_key_list(?) f
      JOIN (
             SELECT id
               FROM pragma_foreign_key_list(?)
              GROUP BY id
             HAVING count(*) = 1
           ) single ON single.id = f.id
";

pub const SERVER_VERSION: &str = "SELECT sqlite_version()";

/// `sqlite_master.type` -> the domain's [`TableKind`].
///
/// SQLite has no materialised views, so there are only two cases.
///
/// [`TableKind`]: dbui_domain::TableKind
pub fn table_kind(kind: &str) -> dbui_domain::TableKind {
    match kind {
        "view" => dbui_domain::TableKind::View,
        _ => dbui_domain::TableKind::Table,
    }
}
