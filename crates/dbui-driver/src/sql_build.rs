//! Build SELECT / UPDATE SQL fragments shared by the adapters.
//!
//! Identifiers are quoted per engine. Freeform WHERE text from the filter
//! strip is appended as typed (same trust model as the SQL editor). UPDATE
//! values are bound, never interpolated.

use dbui_domain::{Driver, SortKey, TableKind, TableRef, Value};

/// A SQL fragment plus the values to bind, in order.
pub struct BoundSql {
    pub sql: String,
    pub binds: Vec<Value>,
}

/// Normalize freeform filter text into a ` WHERE …` fragment (or empty).
///
/// Strips a leading `WHERE` so the UI label and typed text do not double up.
pub fn where_clause(raw: &str) -> BoundSql {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return BoundSql {
            sql: String::new(),
            binds: Vec::new(),
        };
    }

    let body = trimmed
        .strip_prefix("WHERE")
        .or_else(|| trimmed.strip_prefix("where"))
        .or_else(|| trimmed.strip_prefix("Where"))
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .unwrap_or(trimmed);

    BoundSql {
        sql: format!(" WHERE {body}"),
        binds: Vec::new(),
    }
}

/// ` ORDER BY "a" ASC, "b" DESC`, or empty when there is nothing to order by.
///
/// Column names are quoted rather than bound: an identifier cannot be a
/// parameter, and these come from the result set's own headers.
pub fn order_by(driver: Driver, order: &[SortKey]) -> String {
    if order.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = order
        .iter()
        .map(|key| {
            format!(
                "{} {}",
                driver.quote_identifier(&key.column),
                if key.ascending { "ASC" } else { "DESC" }
            )
        })
        .collect();
    format!(" ORDER BY {}", parts.join(", "))
}

pub fn select_page_sql(
    driver: Driver,
    table: &TableRef,
    where_raw: &str,
    order: &[SortKey],
) -> BoundSql {
    let where_part = where_clause(where_raw);
    let (limit_ph, offset_ph) = match driver {
        Driver::Postgres => (
            placeholder(Driver::Postgres, 1),
            placeholder(Driver::Postgres, 2),
        ),
        Driver::MySql => ("?".to_string(), "?".to_string()),
    };
    BoundSql {
        sql: format!(
            "SELECT * FROM {}{}{} LIMIT {limit_ph} OFFSET {offset_ph}",
            table.quoted(driver),
            where_part.sql,
            order_by(driver, order)
        ),
        binds: where_part.binds,
    }
}

pub fn count_sql(driver: Driver, table: &TableRef, where_raw: &str) -> BoundSql {
    let where_part = where_clause(where_raw);
    BoundSql {
        sql: format!(
            "SELECT count(*) FROM {}{}",
            table.quoted(driver),
            where_part.sql
        ),
        binds: where_part.binds,
    }
}

/// `UPDATE table SET … WHERE pk…` with all binds as strings (engine coerces).
pub fn update_sql(
    driver: Driver,
    table: &TableRef,
    changes: &[(String, Value)],
    pk: &[(String, Value)],
) -> Result<BoundSql, String> {
    if changes.is_empty() {
        return Err("nothing to update".into());
    }
    if pk.is_empty() {
        return Err("table has no primary key".into());
    }

    let mut binds = Vec::new();
    let mut param = 1usize;
    let mut sets = Vec::new();
    for (name, value) in changes {
        let col = driver.quote_identifier(name);
        match value {
            Value::Null => sets.push(format!("{col} = NULL")),
            Value::Default => sets.push(format!("{col} = DEFAULT")),
            _ => {
                let ph = typed_placeholder(driver, param, value);
                param += 1;
                sets.push(format!("{col} = {ph}"));
                binds.push(value_to_bind(value));
            }
        }
    }

    let mut wheres = Vec::new();
    for (name, value) in pk {
        let col = driver.quote_identifier(name);
        if value.is_null() {
            wheres.push(format!("{col} IS NULL"));
        } else {
            let ph = typed_placeholder(driver, param, value);
            param += 1;
            wheres.push(format!("{col} = {ph}"));
            binds.push(value_to_bind(value));
        }
    }

    Ok(BoundSql {
        sql: format!(
            "UPDATE {} SET {} WHERE {}",
            table.quoted(driver),
            sets.join(", "),
            wheres.join(" AND ")
        ),
        binds,
    })
}

/// `DELETE FROM table WHERE pk…` — one row, identified by its primary key.
///
/// The same `WHERE` shape as [`update_sql`], and for the same reason: a row
/// the UI cannot name exactly is a row it must not delete. A table with no
/// primary key is refused rather than matched on its values, because a
/// predicate over non-key columns can take rows the user never selected.
pub fn delete_sql(
    driver: Driver,
    table: &TableRef,
    pk: &[(String, Value)],
) -> Result<BoundSql, String> {
    if pk.is_empty() {
        return Err("table has no primary key".into());
    }

    let mut binds = Vec::new();
    let mut param = 1usize;
    let mut wheres = Vec::new();
    for (name, value) in pk {
        let col = driver.quote_identifier(name);
        if value.is_null() {
            wheres.push(format!("{col} IS NULL"));
        } else {
            let ph = typed_placeholder(driver, param, value);
            param += 1;
            wheres.push(format!("{col} = {ph}"));
            binds.push(value_to_bind(value));
        }
    }

    Ok(BoundSql {
        sql: format!(
            "DELETE FROM {} WHERE {}",
            table.quoted(driver),
            wheres.join(" AND ")
        ),
        binds,
    })
}

/// `INSERT INTO table (…) VALUES (…)` for one new row.
///
/// Columns the caller left out are simply absent from the statement, which is
/// how the server's own defaults and generated keys still fire. A row naming
/// no columns at all is the engine's "all defaults" spelling, which differs
/// between the two.
pub fn insert_sql(
    driver: Driver,
    table: &TableRef,
    values: &[(String, Value)],
) -> Result<BoundSql, String> {
    if values.is_empty() {
        return Ok(BoundSql {
            sql: match driver {
                Driver::Postgres => format!("INSERT INTO {} DEFAULT VALUES", table.quoted(driver)),
                Driver::MySql => format!("INSERT INTO {} () VALUES ()", table.quoted(driver)),
            },
            binds: Vec::new(),
        });
    }

    let mut binds = Vec::new();
    let mut param = 1usize;
    let mut columns = Vec::new();
    let mut slots = Vec::new();

    for (name, value) in values {
        columns.push(driver.quote_identifier(name));
        match value {
            Value::Null => slots.push("NULL".to_string()),
            Value::Default => slots.push("DEFAULT".to_string()),
            _ => {
                slots.push(typed_placeholder(driver, param, value));
                param += 1;
                binds.push(value_to_bind(value));
            }
        }
    }

    Ok(BoundSql {
        sql: format!(
            "INSERT INTO {} ({}) VALUES ({})",
            table.quoted(driver),
            columns.join(", "),
            slots.join(", ")
        ),
        binds,
    })
}

/// `TRUNCATE` for the engine. MySQL has no `TRUNCATE TABLE … CASCADE`, and
/// Postgres needs `RESTART IDENTITY` spelled out to reset sequences.
pub fn truncate_sql(driver: Driver, table: &TableRef) -> String {
    match driver {
        Driver::Postgres => format!("TRUNCATE TABLE {} RESTART IDENTITY", table.quoted(driver)),
        Driver::MySql => format!("TRUNCATE TABLE {}", table.quoted(driver)),
    }
}

/// `DROP …` for whichever kind of relation this is.
///
/// A view is not a table: `DROP TABLE` on one is an error on both engines, and
/// the menu offering "Drop" has to mean the statement that works.
pub fn drop_sql(driver: Driver, table: &TableRef, kind: TableKind) -> String {
    let what = match kind {
        TableKind::Table => "TABLE",
        TableKind::View => "VIEW",
        TableKind::MaterializedView => "MATERIALIZED VIEW",
    };
    format!("DROP {what} {}", table.quoted(driver))
}

fn placeholder(driver: Driver, index: usize) -> String {
    match driver {
        Driver::Postgres => format!("${index}"),
        Driver::MySql => "?".to_string(),
    }
}

/// A placeholder, cast where the value's own variant names a type the engine
/// will not infer.
///
/// Postgres types every parameter from the wire, so a value sent as text
/// against a `numeric` column plans as `numeric = text` and is rejected -- an
/// operator that does not exist. The variants below are the ones this crate
/// carries as strings but the server stores as something else, so the cast is
/// what puts them back. MySQL coerces on its own and needs none of this.
fn typed_placeholder(driver: Driver, index: usize, value: &Value) -> String {
    let placeholder = placeholder(driver, index);
    if driver != Driver::Postgres {
        return placeholder;
    }
    match value {
        Value::Decimal(_) => format!("{placeholder}::numeric"),
        Value::Uuid(_) => format!("{placeholder}::uuid"),
        // `jsonb` rather than `json`: it is the type with the operators, and
        // assigning it to a `json` column is a cast Postgres already allows.
        Value::Json(_) => format!("{placeholder}::jsonb"),
        Value::Temporal(text) => format!("{placeholder}::{}", temporal_type(text)),
        _ => placeholder,
    }
}

/// Guess which temporal type a formatted value is, from its own shape.
///
/// The adapter has already flattened date, time and timestamp into one string
/// variant, so this reads it back. Postgres parses all three spellings, and a
/// wrong guess is a rejected statement carrying the server's own explanation,
/// not a silently wrong write.
fn temporal_type(text: &str) -> &'static str {
    let trimmed = text.trim();
    let has_date = trimmed
        .split(['T', ' '])
        .next()
        .is_some_and(|head| head.matches('-').count() >= 2);
    if !has_date {
        return if trimmed.contains(':') {
            "time"
        } else {
            // Not a date and not a clock: an interval is the remaining shape
            // this variant carries.
            "interval"
        };
    }
    if trimmed.contains(':') {
        "timestamptz"
    } else {
        "date"
    }
}

/// The value to bind for a placeholder.
///
/// `NULL` and `DEFAULT` are written into the statement rather than bound, so
/// reaching here with one would be a bug in the caller; an untyped NULL keeps
/// the two lists the same length rather than shifting every later parameter by
/// one.
fn value_to_bind(value: &Value) -> Value {
    match value {
        Value::Null | Value::Default => Value::Null,
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_where_is_omitted() {
        let bound = where_clause("  ");
        assert!(bound.sql.is_empty());
        assert!(bound.binds.is_empty());
    }

    #[test]
    fn where_prefix_is_stripped() {
        let bound = where_clause("WHERE id = 1");
        assert_eq!(bound.sql, " WHERE id = 1");
        let again = where_clause("id = 1 AND name = 'Ada'");
        assert_eq!(again.sql, " WHERE id = 1 AND name = 'Ada'");
    }

    #[test]
    fn select_page_embeds_the_predicate() {
        let table = TableRef::new("public", "people");
        let bound = select_page_sql(Driver::Postgres, &table, "name = 'Ada'", &[]);
        assert!(bound.sql.contains(" WHERE name = 'Ada'"));
        assert!(bound.sql.contains("LIMIT $1 OFFSET $2"));
        assert!(bound.binds.is_empty());
    }

    /// The order has to sit between the predicate and the window, or the
    /// engine rejects the statement outright.
    #[test]
    fn the_order_goes_after_the_where_and_before_the_limit() {
        let table = TableRef::new("public", "people");
        let bound = select_page_sql(
            Driver::Postgres,
            &table,
            "active",
            &[SortKey::desc("name"), SortKey::asc("id")],
        );
        assert_eq!(
            bound.sql,
            "SELECT * FROM \"public\".\"people\" WHERE active \
             ORDER BY \"name\" DESC, \"id\" ASC LIMIT $1 OFFSET $2"
        );
    }

    /// Nothing to order by leaves the clause out rather than emitting an
    /// empty `ORDER BY`.
    #[test]
    fn an_empty_order_is_omitted() {
        assert!(order_by(Driver::MySql, &[]).is_empty());
        let bound = select_page_sql(Driver::MySql, &TableRef::new("s", "t"), "", &[]);
        assert!(!bound.sql.contains("ORDER BY"), "got: {}", bound.sql);
    }

    /// A column name is pasted, not bound -- so it is quoted like every other
    /// identifier this crate emits.
    #[test]
    fn a_hostile_column_name_cannot_escape_the_order() {
        let order = order_by(Driver::Postgres, &[SortKey::asc("x\"; DROP TABLE t; --")]);
        assert_eq!(order, " ORDER BY \"x\"\"; DROP TABLE t; --\" ASC");
    }

    #[test]
    fn update_requires_pk_and_changes() {
        let table = TableRef::new("public", "t");
        assert!(update_sql(Driver::Postgres, &table, &[], &[]).is_err());
        let ok = update_sql(
            Driver::Postgres,
            &table,
            &[("name".into(), Value::Text("x".into()))],
            &[("id".into(), Value::Int(1))],
        )
        .unwrap();
        assert!(ok.sql.starts_with("UPDATE "));
        assert_eq!(ok.binds.len(), 2);
    }

    #[test]
    fn delete_needs_a_primary_key() {
        let table = TableRef::new("public", "t");
        assert!(delete_sql(Driver::Postgres, &table, &[]).is_err());
    }

    #[test]
    fn delete_matches_on_every_key_column() {
        let table = TableRef::new("public", "t");
        let bound = delete_sql(
            Driver::Postgres,
            &table,
            &[
                ("tenant".into(), Value::Int(4)),
                ("id".into(), Value::Int(7)),
            ],
        )
        .unwrap();
        assert_eq!(
            bound.sql,
            "DELETE FROM \"public\".\"t\" WHERE \"tenant\" = $1 AND \"id\" = $2"
        );
        assert_eq!(
            bound.binds,
            vec![Value::Int(4), Value::Int(7)],
            "keys keep their own type, so the server is not asked to compare \
             a bigint with text"
        );
    }

    /// A NULL key part cannot be bound with `=`, and a `= NULL` predicate
    /// matches nothing -- which would silently delete no rows and report
    /// success.
    #[test]
    fn a_null_key_part_becomes_is_null() {
        let table = TableRef::new("public", "t");
        let bound = delete_sql(Driver::MySql, &table, &[("id".into(), Value::Null)]).unwrap();
        assert_eq!(bound.sql, "DELETE FROM `public`.`t` WHERE `id` IS NULL");
        assert!(bound.binds.is_empty());
    }

    /// The bug this fixes: every value went over as text, so `WHERE "id" = $1`
    /// against a bigint column planned as `bigint = text` and Postgres refused
    /// it -- every row edit and every delete, for the commonest kind of key.
    #[test]
    fn scalar_keys_are_bound_without_a_cast() {
        let table = TableRef::new("public", "t");
        let bound = delete_sql(Driver::Postgres, &table, &[("id".into(), Value::Int(7))]).unwrap();
        assert!(
            bound.sql.ends_with("\"id\" = $1"),
            "an int needs no cast, only its own type on the wire: {}",
            bound.sql
        );
        assert_eq!(bound.binds, vec![Value::Int(7)]);
    }

    /// The variants this crate carries as strings are the ones Postgres has to
    /// be told about, because text is not what the column holds.
    #[test]
    fn string_shaped_values_are_cast_back_to_their_type() {
        let table = TableRef::new("public", "t");
        let cast_for = |value: Value| {
            update_sql(
                Driver::Postgres,
                &table,
                &[("c".into(), value)],
                &[("id".into(), Value::Int(1))],
            )
            .unwrap()
            .sql
        };

        assert!(cast_for(Value::Decimal("1.50".into())).contains("\"c\" = $1::numeric"));
        assert!(cast_for(Value::Uuid("0-0".into())).contains("\"c\" = $1::uuid"));
        assert!(cast_for(Value::Json("{}".into())).contains("\"c\" = $1::jsonb"));
        assert!(cast_for(Value::Text("x".into())).contains("\"c\" = $1"));
        assert!(
            !cast_for(Value::Text("x".into())).contains("::"),
            "text needs no cast, and an unnecessary one would defeat an index"
        );
    }

    /// MySQL coerces on its own, and `::` is not its cast syntax anyway.
    #[test]
    fn mysql_placeholders_are_never_cast() {
        let table = TableRef::new("s", "t");
        let bound = update_sql(
            Driver::MySql,
            &table,
            &[("c".into(), Value::Decimal("1.50".into()))],
            &[("id".into(), Value::Int(1))],
        )
        .unwrap();
        assert!(!bound.sql.contains("::"), "got: {}", bound.sql);
        assert_eq!(bound.sql.matches('?').count(), 2);
    }

    #[test]
    fn a_temporal_value_is_cast_by_the_shape_of_its_own_text() {
        assert_eq!(temporal_type("2024-01-01 00:00:00+00"), "timestamptz");
        assert_eq!(temporal_type("2024-01-01T00:00:00Z"), "timestamptz");
        assert_eq!(temporal_type("2024-01-01"), "date");
        assert_eq!(temporal_type("12:30:00"), "time");
        assert_eq!(temporal_type("1 day"), "interval");
    }

    /// Columns the user never filled in are absent from the statement, which
    /// is what lets a sequence or a `DEFAULT` still fire.
    #[test]
    fn insert_names_only_the_columns_it_was_given() {
        let table = TableRef::new("public", "people");
        let bound = insert_sql(
            Driver::Postgres,
            &table,
            &[
                ("name".into(), Value::Text("Ada".into())),
                ("score".into(), Value::Decimal("1.50".into())),
            ],
        )
        .unwrap();
        assert_eq!(
            bound.sql,
            "INSERT INTO \"public\".\"people\" (\"name\", \"score\") \
             VALUES ($1, $2::numeric)"
        );
        assert!(!bound.sql.contains("\"id\""), "an untouched key is left out");
    }

    /// NULL and DEFAULT are written into the statement, not bound -- DEFAULT
    /// is not a value any parameter could carry.
    #[test]
    fn insert_writes_null_and_default_inline() {
        let bound = insert_sql(
            Driver::Postgres,
            &TableRef::new("s", "t"),
            &[
                ("a".into(), Value::Null),
                ("b".into(), Value::Default),
                ("c".into(), Value::Int(1)),
            ],
        )
        .unwrap();
        assert!(bound.sql.contains("VALUES (NULL, DEFAULT, $1)"), "got: {}", bound.sql);
        assert_eq!(bound.binds, vec![Value::Int(1)]);
    }

    /// A row with nothing filled in is still a legal statement -- and the two
    /// engines spell "all defaults" differently.
    #[test]
    fn an_all_defaults_insert_uses_each_engines_spelling() {
        let table = TableRef::new("s", "t");
        assert_eq!(
            insert_sql(Driver::Postgres, &table, &[]).unwrap().sql,
            "INSERT INTO \"s\".\"t\" DEFAULT VALUES"
        );
        assert_eq!(
            insert_sql(Driver::MySql, &table, &[]).unwrap().sql,
            "INSERT INTO `s`.`t` () VALUES ()"
        );
    }

    /// A hostile table name must not break out of the identifier.
    #[test]
    fn generated_ddl_quotes_the_table() {
        let table = TableRef::new("public", "users\"; DROP DATABASE x; --");
        let sql = truncate_sql(Driver::Postgres, &table);
        assert_eq!(
            sql,
            "TRUNCATE TABLE \"public\".\"users\"\"; DROP DATABASE x; --\" RESTART IDENTITY"
        );
        assert!(
            drop_sql(Driver::MySql, &TableRef::new("s", "t"), TableKind::Table)
                .ends_with("`s`.`t`")
        );
    }

    /// `DROP TABLE` on a view is an error on both engines.
    #[test]
    fn dropping_names_the_kind_of_relation() {
        let table = TableRef::new("public", "v");
        assert!(drop_sql(Driver::Postgres, &table, TableKind::View).starts_with("DROP VIEW "));
        assert!(drop_sql(Driver::Postgres, &table, TableKind::MaterializedView)
            .starts_with("DROP MATERIALIZED VIEW "));
        assert!(drop_sql(Driver::Postgres, &table, TableKind::Table).starts_with("DROP TABLE "));
    }

    #[test]
    fn update_emits_null_empty_and_default_without_binds() {
        let table = TableRef::new("public", "t");
        let ok = update_sql(
            Driver::Postgres,
            &table,
            &[
                ("a".into(), Value::Null),
                ("b".into(), Value::Text(String::new())),
                ("c".into(), Value::Default),
            ],
            &[("id".into(), Value::Int(1))],
        )
        .unwrap();
        assert!(ok.sql.contains("\"a\" = NULL"));
        assert!(ok.sql.contains("\"b\" = $1"));
        assert!(ok.sql.contains("\"c\" = DEFAULT"));
        assert_eq!(ok.binds, vec![Value::Text(String::new()), Value::Int(1)]);
    }
}
