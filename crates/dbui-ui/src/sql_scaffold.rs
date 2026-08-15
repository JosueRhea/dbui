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
