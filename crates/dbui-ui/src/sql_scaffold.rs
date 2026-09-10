//! Statements the context menu hands the user to edit.
//!
//! These are scaffolds, not statements this app runs: they land in the SQL
//! editor or on the clipboard, and a person reads them before anything
//! executes. That is why `create_table` is allowed to be an approximation --
//! it is built from the columns the catalog reports, which is enough to
//! restate a table's shape and not enough to reproduce its indexes,
//! constraints or storage options.
//!
//! Identifiers still go through [`Driver::quote_identifier`]. A generated
//! statement that cannot survive a table named `"; DROP …` is a generated
//! statement nobody should paste.

use dbui_app::domain::{Column, Driver, TableRef};

/// `SELECT * FROM "schema"."table" LIMIT 100;`
pub fn select_statement(driver: Driver, table: &TableRef) -> String {
    format!("SELECT *\nFROM {}\nLIMIT 100;\n", table.quoted(driver))
}

/// An `INSERT` with one placeholder per column, ready to be filled in.
///
/// Generated columns and defaults are left in rather than guessed at: a column
/// the user does not want to write is one keystroke to delete, and one the
/// scaffold silently dropped is a bug they find at runtime.
pub fn insert_template(driver: Driver, table: &TableRef, columns: &[Column]) -> String {
    if columns.is_empty() {
        return format!("INSERT INTO {} VALUES ();\n", table.quoted(driver));
    }

    let names: Vec<String> = columns
        .iter()
        .map(|column| driver.quote_identifier(&column.name))
        .collect();
    let values: Vec<String> = columns
        .iter()
        .map(|column| {
            // The declared type is the useful hint here, and a comment is the
            // only place to put it that does not break the statement.
            format!("  NULL /* {} {} */", column.name, column.data_type)
        })
        .collect();

    format!(
        "INSERT INTO {} (\n  {}\n) VALUES (\n{}\n);\n",
        table.quoted(driver),
        names.join(",\n  "),
        values.join(",\n")
    )
}

/// A `CREATE TABLE` restating the columns the catalog reports.
pub fn create_table(driver: Driver, table: &TableRef, columns: &[Column]) -> String {
    let mut body: Vec<String> = columns
        .iter()
        .map(|column| {
            let mut line = format!(
                "  {} {}",
                driver.quote_identifier(&column.name),
                column.data_type
            );
            if !column.nullable {
                line.push_str(" NOT NULL");
            }
            if let Some(default) = column.default.as_ref().filter(|d| !d.trim().is_empty()) {
                line.push_str(&format!(" DEFAULT {default}"));
            }
            line
        })
        .collect();

    let keys: Vec<String> = columns
        .iter()
        .filter(|column| column.is_primary_key)
        .map(|column| driver.quote_identifier(&column.name))
        .collect();
    if !keys.is_empty() {
        body.push(format!("  PRIMARY KEY ({})", keys.join(", ")));
    }

    format!(
        "-- Columns only: indexes, foreign keys and constraints are not \
         reproduced.\nCREATE TABLE {} (\n{}\n);\n",
        table.quoted(driver),
        body.join(",\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, data_type: &str) -> Column {
        Column {
            name: name.to_string(),
            data_type: data_type.to_string(),
            nullable: true,
            default: None,
            is_primary_key: false,
            ordinal: 0,
            references: None,
        }
    }

    #[test]
    fn select_quotes_per_engine() {
        let table = TableRef::new("public", "users");
        assert!(select_statement(Driver::Postgres, &table).contains("\"public\".\"users\""));
        assert!(select_statement(Driver::MySql, &table).contains("`public`.`users`"));
    }

    #[test]
    fn insert_lists_every_column() {
        let table = TableRef::new("public", "users");
        let sql = insert_template(
            Driver::Postgres,
            &table,
            &[column("id", "bigint"), column("email", "text")],
        );
        assert!(sql.contains("\"id\""));
        assert!(sql.contains("\"email\""));
        assert_eq!(sql.matches("NULL").count(), 2);
    }

    /// A table with no readable columns still produces something legal rather
    /// than a statement with an empty column list.
    #[test]
    fn insert_with_no_columns_is_still_a_statement() {
        let sql = insert_template(Driver::MySql, &TableRef::new("s", "t"), &[]);
        assert_eq!(sql, "INSERT INTO `s`.`t` VALUES ();\n");
    }

    #[test]
    fn create_table_carries_nullability_defaults_and_the_key() {
        let mut id = column("id", "bigint");
        id.nullable = false;
        id.is_primary_key = true;
        let mut created = column("created_at", "timestamptz");
        created.default = Some("now()".into());

        let sql = create_table(
            Driver::Postgres,
            &TableRef::new("public", "users"),
            &[id, created],
        );
        assert!(sql.contains("\"id\" bigint NOT NULL"));
        assert!(sql.contains("\"created_at\" timestamptz DEFAULT now()"));
        assert!(sql.contains("PRIMARY KEY (\"id\")"));
        assert!(
            sql.starts_with("-- Columns only"),
            "the approximation has to say so"
        );
    }

    /// A composite key is one constraint naming both columns, not two.
    #[test]
    fn a_composite_key_is_one_clause() {
        let mut a = column("tenant", "int");
        a.is_primary_key = true;
        let mut b = column("id", "int");
        b.is_primary_key = true;
        let sql = create_table(Driver::MySql, &TableRef::new("s", "t"), &[a, b]);
        assert_eq!(sql.matches("PRIMARY KEY").count(), 1);
        assert!(sql.contains("PRIMARY KEY (`tenant`, `id`)"));
    }

    /// The table name is pasted, not bound -- so it has to be quoted.
    #[test]
    fn a_hostile_name_cannot_break_out() {
        let table = TableRef::new("public", "t\"; DROP DATABASE x; --");
        let sql = select_statement(Driver::Postgres, &table);
        assert!(sql.contains("\"t\"\"; DROP DATABASE x; --\""));
    }
}

/// One ready-made statement offered from the templates palette.
pub struct Template {
    /// What the palette lists it as.
    pub name: &'static str,
    /// One line saying what it is for, under the name.
    pub about: &'static str,
    /// The statement itself, ready to be typed over.
    pub body: String,
}

/// The statements worth having a starting point for.
///
/// These are shapes, not answers: every one of them names a `table` and a
/// `column` the user has to replace, because the point is the clause structure
/// -- which side of a `LEFT JOIN` keeps its rows, where `HAVING` goes, what an
/// upsert is called on this engine -- and not the identifiers.
///
/// When a table is open its name is filled in, quoted for the engine. It is
/// the table the user was just looking at, so it is the one they are most
/// likely to be writing about, and a wrong guess is one selection to fix.
///
/// `UPDATE` and `DELETE` carry their `WHERE` already written. A template that
/// hands someone `DELETE FROM t;` and trusts them to add the clause before
/// they press ⌘↵ is a template that eventually empties a table.
pub fn templates(driver: Driver, table: Option<&TableRef>) -> Vec<Template> {
    let name = table
        .map(|table| table.quoted(driver))
        .unwrap_or_else(|| driver.quote_identifier("table_name"));
    let other = driver.quote_identifier("other_table");
    let column = driver.quote_identifier("column_name");

    // Postgres and SQLite spell an upsert the same way; MySQL does not.
    let upsert = match driver {
        Driver::MySql => format!(
            "INSERT INTO {name} (id, {column})\nVALUES (1, 'value')\n\
             ON DUPLICATE KEY UPDATE\n  {column} = VALUES({column});\n"
        ),
        _ => format!(
            "INSERT INTO {name} (id, {column})\nVALUES (1, 'value')\n\
             ON CONFLICT (id) DO UPDATE SET\n  {column} = EXCLUDED.{column};\n"
        ),
    };

    vec![
        Template {
            name: "SELECT with WHERE",
            about: "Filtered rows, newest first",
            body: format!(
                "SELECT *\nFROM {name}\nWHERE {column} = 'value'\n\
                 ORDER BY {column} DESC\nLIMIT 100;\n"
            ),
        },
        Template {
            name: "SELECT a page",
            about: "LIMIT and OFFSET, ordered so the page is stable",
            body: format!("SELECT *\nFROM {name}\nORDER BY id\nLIMIT 50 OFFSET 0;\n"),
        },
        Template {
            name: "COUNT rows",
            about: "How many, with the same filter you would select by",
            body: format!("SELECT count(*) AS total\nFROM {name}\nWHERE {column} = 'value';\n"),
        },
        Template {
            name: "GROUP BY with HAVING",
            about: "Counts per value, keeping only the groups worth seeing",
            body: format!(
                "SELECT {column}, count(*) AS total\nFROM {name}\n\
                 GROUP BY {column}\nHAVING count(*) > 1\nORDER BY total DESC;\n"
            ),
        },
        Template {
            name: "DISTINCT values",
            about: "What is actually in a column",
            body: format!("SELECT DISTINCT {column}\nFROM {name}\nORDER BY {column};\n"),
        },
        Template {
            name: "INNER JOIN",
            about: "Rows that match on both sides",
            body: format!(
                "SELECT a.*, b.*\nFROM {name} AS a\n\
                 JOIN {other} AS b ON b.id = a.{column}\nLIMIT 100;\n"
            ),
        },
        Template {
            name: "LEFT JOIN",
            about: "Every row on the left, matched or not",
            body: format!(
                "SELECT a.*, b.*\nFROM {name} AS a\n\
                 LEFT JOIN {other} AS b ON b.id = a.{column}\n\
                 WHERE b.id IS NULL\nLIMIT 100;\n"
            ),
        },
        Template {
            name: "INSERT",
            about: "One row, columns named",
            body: format!("INSERT INTO {name} ({column})\nVALUES ('value');\n"),
        },
        Template {
            name: "INSERT or update",
            about: if matches!(driver, Driver::MySql) {
                "ON DUPLICATE KEY UPDATE"
            } else {
                "ON CONFLICT DO UPDATE"
            },
            body: upsert,
        },
        Template {
            name: "UPDATE with WHERE",
            about: "The WHERE is already there, on purpose",
            body: format!("UPDATE {name}\nSET {column} = 'value'\nWHERE id = 1;\n"),
        },
        Template {
            name: "DELETE with WHERE",
            about: "The WHERE is already there, on purpose",
            body: format!("DELETE FROM {name}\nWHERE id = 1;\n"),
        },
        Template {
            name: "WITH (common table expression)",
            about: "Name a subquery, then select from it",
            body: format!(
                "WITH recent AS (\n  SELECT *\n  FROM {name}\n  \
                 ORDER BY id DESC\n  LIMIT 100\n)\nSELECT *\nFROM recent;\n"
            ),
        },
    ]
}
