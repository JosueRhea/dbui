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

/// Everything the tree lists besides relations, in one pass.
///
/// Objects that belong to an extension (`pg_depend.deptype = 'e'`) are left
/// out: installing `pgcrypto` adds forty functions nobody wrote, and listing
/// them would bury the three that somebody did. The extension itself is
/// listed instead. `key` is the OID, which is what the definition queries
/// below look an object up by.
pub const OBJECTS: &str = "
    WITH user_schemas AS (
        SELECT oid, nspname
          FROM pg_catalog.pg_namespace
         WHERE nspname NOT IN ('pg_catalog', 'information_schema')
           AND nspname NOT LIKE 'pg_toast%'
           AND nspname NOT LIKE 'pg_temp%'
    ),
    extension_owned AS (
        SELECT classid, objid FROM pg_catalog.pg_depend WHERE deptype = 'e'
    )
    SELECT n.nspname AS schema_name, p.proname AS object_name,
           CASE p.prokind WHEN 'p' THEN 'procedure' ELSE 'function' END AS object_kind,
           pg_catalog.pg_get_function_identity_arguments(p.oid) AS detail,
           p.oid::text AS object_key
      FROM pg_catalog.pg_proc p
      JOIN user_schemas n ON n.oid = p.pronamespace
     WHERE p.prokind IN ('f', 'p')
       AND (p.tableoid, p.oid) NOT IN (SELECT classid, objid FROM extension_owned)
    UNION ALL
    SELECT n.nspname, t.tgname, 'trigger', c.relname, t.oid::text
      FROM pg_catalog.pg_trigger t
      JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid
      JOIN user_schemas n ON n.oid = c.relnamespace
     WHERE NOT t.tgisinternal
    UNION ALL
    SELECT n.nspname, c.relname, 'sequence', NULL, c.oid::text
      FROM pg_catalog.pg_class c
      JOIN user_schemas n ON n.oid = c.relnamespace
     WHERE c.relkind = 'S'
       AND (c.tableoid, c.oid) NOT IN (SELECT classid, objid FROM extension_owned)
    UNION ALL
    SELECT n.nspname, t.typname, 'type',
           CASE t.typtype WHEN 'e' THEN 'enum' WHEN 'd' THEN 'domain'
                          WHEN 'r' THEN 'range' ELSE 'composite' END,
           t.oid::text
      FROM pg_catalog.pg_type t
      JOIN user_schemas n ON n.oid = t.typnamespace
     WHERE (t.typtype IN ('e', 'd', 'r')
            OR (t.typtype = 'c'
                AND (SELECT relkind FROM pg_catalog.pg_class WHERE oid = t.typrelid) = 'c'))
       AND (t.tableoid, t.oid) NOT IN (SELECT classid, objid FROM extension_owned)
    UNION ALL
    SELECT n.nspname, e.extname, 'extension', e.extversion, e.oid::text
      FROM pg_catalog.pg_extension e
      JOIN user_schemas n ON n.oid = e.extnamespace
     ORDER BY 1, 3, 2
";

/// `CREATE OR REPLACE FUNCTION ...` / `PROCEDURE`, exactly as Postgres
/// would write it back.
pub const FUNCTION_DEFINITION: &str = "SELECT pg_catalog.pg_get_functiondef($1::text::oid)";

pub const TRIGGER_DEFINITION: &str =
    "SELECT pg_catalog.pg_get_triggerdef($1::text::oid, true) || ';'";

/// Postgres has no `pg_get_sequencedef`; this spells one out from
/// `pg_sequences`, with the current value as a comment -- it is usually the
/// thing someone opened a sequence to find out.
pub const SEQUENCE_DEFINITION: &str = "
    SELECT format(
               E'CREATE SEQUENCE %I.%I\n    AS %s\n    INCREMENT BY %s\n    MINVALUE %s\n    MAXVALUE %s\n    START WITH %s\n    CACHE %s%s;\n\n-- Last value: %s',
               s.schemaname, s.sequencename, s.data_type::text, s.increment_by,
               s.min_value, s.max_value, s.start_value, s.cache_size,
               CASE WHEN s.cycle THEN E'\n    CYCLE' ELSE '' END,
               COALESCE(s.last_value::text, 'never used'))
      FROM pg_catalog.pg_sequences s
      JOIN pg_catalog.pg_namespace n ON n.nspname = s.schemaname
      JOIN pg_catalog.pg_class c ON c.relnamespace = n.oid AND c.relname = s.sequencename
     WHERE c.oid = $1::text::oid
";

pub const ENUM_DEFINITION: &str = "
    SELECT format(E'CREATE TYPE %s AS ENUM (\n    %s\n);',
                  $1::text::oid::regtype,
                  string_agg(quote_literal(enumlabel), E',\n    ' ORDER BY enumsortorder))
      FROM pg_catalog.pg_enum
     WHERE enumtypid = $1::text::oid
";

pub const DOMAIN_DEFINITION: &str = "
    SELECT format('CREATE DOMAIN %s AS %s%s%s', t.oid::regtype,
                  pg_catalog.format_type(t.typbasetype, t.typtypmod),
                  CASE WHEN t.typnotnull THEN ' NOT NULL' ELSE '' END,
                  COALESCE(' DEFAULT ' || t.typdefault, ''))
           || COALESCE((SELECT string_agg(E'\n    CONSTRAINT ' || quote_ident(conname) || ' '
                                          || pg_catalog.pg_get_constraintdef(oid), '')
                          FROM pg_catalog.pg_constraint WHERE contypid = t.oid), '')
           || ';'
      FROM pg_catalog.pg_type t
     WHERE t.oid = $1::text::oid
";

pub const COMPOSITE_DEFINITION: &str = "
    SELECT format(E'CREATE TYPE %s AS (\n    %s\n);', t.oid::regtype,
                  string_agg(quote_ident(a.attname) || ' '
                             || pg_catalog.format_type(a.atttypid, a.atttypmod),
                             E',\n    ' ORDER BY a.attnum))
      FROM pg_catalog.pg_type t
      JOIN pg_catalog.pg_attribute a ON a.attrelid = t.typrelid
     WHERE t.oid = $1::text::oid AND a.attnum > 0 AND NOT a.attisdropped
     GROUP BY t.oid
";

pub const RANGE_DEFINITION: &str = "
    SELECT format('CREATE TYPE %s AS RANGE (SUBTYPE = %s);', r.rngtypid::regtype,
                  r.rngsubtype::regtype)
      FROM pg_catalog.pg_range r
     WHERE r.rngtypid = $1::text::oid
";

pub const EXTENSION_DEFINITION: &str = "
    SELECT format('CREATE EXTENSION %I WITH SCHEMA %I VERSION %L;',
                  e.extname, n.nspname, e.extversion)
      FROM pg_catalog.pg_extension e
      JOIN pg_catalog.pg_namespace n ON n.oid = e.extnamespace
     WHERE e.oid = $1::text::oid
";

/// Client connections, busiest first. Background workers (autovacuum, the
/// WAL writer) are the server's own and cannot be told anything useful.
pub const SESSIONS: &str = "
    SELECT pid::bigint AS id,
           COALESCE(usename, '') AS user_name,
           COALESCE(datname, '') AS database_name,
           COALESCE(host(client_addr), 'local')
             || CASE WHEN application_name <> '' THEN ' · ' || application_name ELSE '' END
             AS client,
           COALESCE(state, '') AS state,
           CASE WHEN state = 'active' AND wait_event IS NOT NULL
                THEN wait_event_type || ': ' || wait_event END AS waiting_on,
           COALESCE(query, '') AS query,
           EXTRACT(EPOCH FROM (clock_timestamp() - query_start))::float8 AS running_for,
           pid = pg_backend_pid() AS is_self
      FROM pg_catalog.pg_stat_activity
     WHERE backend_type = 'client backend'
     ORDER BY state = 'idle', query_start
";

pub const CANCEL_SESSION: &str = "SELECT pg_catalog.pg_cancel_backend($1::int)";
pub const TERMINATE_SESSION: &str = "SELECT pg_catalog.pg_terminate_backend($1::int)";

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

/// One table's indexes, one row per indexed column, in index order.
///
/// `indkey` lists the table's attribute numbers; `WITH ORDINALITY` keeps
/// their order, and a zero -- an expression, not a column -- drops out of
/// the join rather than naming nothing.
pub const INDEXES: &str = "
    SELECT i.relname::text     AS index_name,
           ix.indisunique      AS is_unique,
           ix.indisprimary     AS is_primary,
           a.attname::text     AS column_name
      FROM pg_catalog.pg_index ix
      JOIN pg_catalog.pg_class i     ON i.oid = ix.indexrelid
      JOIN pg_catalog.pg_class t     ON t.oid = ix.indrelid
      JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace
      CROSS JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS k(attnum, position)
      LEFT JOIN pg_catalog.pg_attribute a
             ON a.attrelid = t.oid AND a.attnum = k.attnum
     WHERE n.nspname = $1
       AND t.relname = $2
     ORDER BY i.relname, k.position
";
