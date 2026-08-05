//! Build SELECT / UPDATE SQL fragments shared by the adapters.
//!
//! Identifiers are quoted per engine. Freeform WHERE text from the filter
//! strip is appended as typed (same trust model as the SQL editor). UPDATE
//! values are bound, never interpolated.

use dbui_domain::{Driver, TableRef, Value};

/// A SQL fragment plus the string values to bind, in order.
pub struct BoundSql {
    pub sql: String,
    pub binds: Vec<String>,
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

pub fn select_page_sql(driver: Driver, table: &TableRef, where_raw: &str) -> BoundSql {
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
            "SELECT * FROM {}{} LIMIT {limit_ph} OFFSET {offset_ph}",
            table.quoted(driver),
            where_part.sql
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
                let ph = placeholder(driver, param);
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
            let ph = placeholder(driver, param);
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

fn placeholder(driver: Driver, index: usize) -> String {
    match driver {
        Driver::Postgres => format!("${index}"),
        Driver::MySql => "?".to_string(),
    }
}

fn value_to_bind(value: &Value) -> String {
    match value {
        Value::Null | Value::Default => String::new(),
        other => other.to_text(),
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
        let bound = select_page_sql(Driver::Postgres, &table, "name = 'Ada'");
        assert!(bound.sql.contains(" WHERE name = 'Ada'"));
        assert!(bound.sql.contains("LIMIT $1 OFFSET $2"));
        assert!(bound.binds.is_empty());
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
        assert_eq!(ok.binds, vec![String::new(), "1".into()]);
    }
}
