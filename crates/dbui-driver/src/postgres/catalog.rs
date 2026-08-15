//! Introspection SQL for Postgres.
//!
//! Kept in one place so the queries can be read as queries. `information_schema`
//! is used where it suffices and `pg_catalog` where it does not -- materialised
//! views, notably, are invisible to the standard views.

/// Schemas and their relations, in one pass.
///
/// `pg_class.relkind` covers ordinary tables (`r`), partitioned tables (`p`),
/// views (`v`), materialised views (`m`) and foreign tables (`f`) -- the last
/// of which `information_schema.tables` also reports, but without telling you
/// it is foreign.
///
/// The `pg_catalog`/`information_schema` schemas are excluded: they are the
/// server's own bookkeeping and would bury the user's tables under hundreds of
/// rows on every connection.
pub const RELATIONS: &str = "
    SELECT n.nspname       AS schema_name,
           c.relname       AS relation_name,
           c.relkind::text AS relation_kind
      FROM pg_catalog.pg_class c
      JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace
     WHERE c.relkind IN ('r', 'p', 'v', 'm', 'f')
       AND n.nspname NOT IN ('pg_catalog', 'information_schema')
       AND n.nspname NOT LIKE 'pg_toast%'
       AND n.nspname NOT LIKE 'pg_temp%'
     ORDER BY n.nspname, c.relname
";

/// Every schema, including the empty ones.
///
/// A schema with no tables yet still belongs in the tree -- otherwise creating
/// the first table in it looks like it did nothing.
pub const SCHEMAS: &str = "
    SELECT nspname AS schema_name
      FROM pg_catalog.pg_namespace
     WHERE nspname NOT IN ('pg_catalog', 'information_schema')
       AND nspname NOT LIKE 'pg_toast%'
       AND nspname NOT LIKE 'pg_temp%'
     ORDER BY nspname
";

/// Columns of one table, with `format_type` doing the work of rendering
/// `character varying(255)` rather than leaving us to reassemble it from
/// `information_schema`'s separate length and precision columns.
///
/// `attnum > 0` skips the system columns (`ctid`, `xmin`); `NOT attisdropped`
/// skips the tombstones a dropped column leaves behind.
pub const COLUMNS: &str = "
    SELECT a.attname                                             AS column_name,
           pg_catalog.format_type(a.atttypid, a.atttypmod)       AS data_type,
           NOT a.attnotnull                                      AS is_nullable,
           pg_catalog.pg_get_expr(d.adbin, d.adrelid)            AS column_default,
           COALESCE(pk.is_primary, false)                        AS is_primary_key,
           a.attnum                                              AS ordinal
      FROM pg_catalog.pg_attribute a
      JOIN pg_catalog.pg_class c      ON c.oid = a.attrelid
      JOIN pg_catalog.pg_namespace n  ON n.oid = c.relnamespace
      LEFT JOIN pg_catalog.pg_attrdef d
             ON d.adrelid = a.attrelid AND d.adnum = a.attnum
      LEFT JOIN LATERAL (
               SELECT true AS is_primary
                 FROM pg_catalog.pg_index i
                WHERE i.indrelid = a.attrelid
                  AND i.indisprimary
                  AND a.attnum = ANY (i.indkey)
           ) pk ON true
     WHERE n.nspname = $1
       AND c.relname = $2
       AND a.attnum > 0
       AND NOT a.attisdropped
     ORDER BY a.attnum
";

/// Single-column foreign keys on one table.
///
/// `array_length(conkey, 1) = 1` is the filter that matters: a composite key
/// cannot be followed from one cell, because the value on screen is only part
/// of it. Following it anyway would land on rows that merely share that part.
pub const FOREIGN_KEYS: &str = "
    SELECT src.attname   AS column_name,
           tn.nspname    AS ref_schema,
           tc.relname    AS ref_table,
           tgt.attname   AS ref_column
      FROM pg_catalog.pg_constraint con
      JOIN pg_catalog.pg_class c      ON c.oid = con.conrelid
      JOIN pg_catalog.pg_namespace n  ON n.oid = c.relnamespace
      JOIN pg_catalog.pg_class tc     ON tc.oid = con.confrelid
      JOIN pg_catalog.pg_namespace tn ON tn.oid = tc.relnamespace
      JOIN pg_catalog.pg_attribute src
             ON src.attrelid = con.conrelid AND src.attnum = con.conkey[1]
      JOIN pg_catalog.pg_attribute tgt
             ON tgt.attrelid = con.confrelid AND tgt.attnum = con.confkey[1]
     WHERE con.contype = 'f'
       AND n.nspname = $1
       AND c.relname = $2
       AND array_length(con.conkey, 1) = 1
";

pub const SERVER_VERSION: &str = "SELECT version()";

/// `relkind` -> the domain's [`TableKind`].
///
/// Partitioned (`p`) and foreign (`f`) tables are ordinary tables as far as the
/// UI is concerned: you select from them the same way.
///
/// [`TableKind`]: dbui_domain::TableKind
pub fn table_kind(relkind: &str) -> dbui_domain::TableKind {
    use dbui_domain::TableKind;
    match relkind {
        "v" => TableKind::View,
        "m" => TableKind::MaterializedView,
        _ => TableKind::Table,
    }
}
