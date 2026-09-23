//! Statements that change a table's shape, spelled for each engine.
//!
//! Every one of these is shown to the user before it runs, so they are built
//! to read the way a person would write them. Names are quoted, always. Types
//! and defaults are the user's own SQL -- a type is `numeric(10,2)`, a
//! default is `now()` -- and go in as typed, the same trust the SQL editor
//! extends; the one thing refused is a `;`, which would turn one statement
//! into two behind the preview's back.
//!
//! The engines disagree more here than anywhere else. Postgres alters a
//! column one property at a time; MySQL restates the whole definition with
//! `MODIFY`; SQLite can rename and drop a column but cannot change one in
//! place at all. What an engine cannot do comes back as an `Err` saying so,
//! rather than as a rebuild-the-table dance the user did not ask for.

use crate::{Column, Driver, TableRef};

/// A column as the user describes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ColumnSpec {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    /// A SQL expression, as typed. `None` for no default.
    pub default: Option<String>,
}

impl ColumnSpec {
    /// The spec a column already has, to edit from.
    pub fn of(column: &Column) -> Self {
        Self {
            name: column.name.clone(),
            data_type: column.data_type.clone(),
            nullable: column.nullable,
            default: column.default.clone(),
        }
    }

    fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("A column needs a name".into());
        }
        if self.data_type.trim().is_empty() {
            return Err(format!("Give “{}” a type", self.name.trim()));
        }
        refuse_semicolon("type", &self.data_type)?;
        if let Some(default) = &self.default {
            refuse_semicolon("default", default)?;
        }
        Ok(())
    }

    /// `"name" type NOT NULL DEFAULT expr`, the way every engine reads it.
    fn definition(&self, driver: Driver) -> String {
        let mut out = format!(
            "{} {}",
            driver.quote_identifier(self.name.trim()),
            self.data_type.trim()
        );
        if !self.nullable {
            out.push_str(" NOT NULL");
        }
        if let Some(default) = self
            .default
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            out.push_str(" DEFAULT ");
            out.push_str(default);
        }
        out
    }
}

fn refuse_semicolon(what: &str, text: &str) -> Result<(), String> {
    if text.contains(';') {
        Err(format!("A {what} cannot contain “;”"))
    } else {
        Ok(())
    }
}

fn check_name(what: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err(format!("The {what} needs a name"))
    } else {
        Ok(())
    }
}

/// `ALTER TABLE … ADD COLUMN …`.
pub fn add_column(
    driver: Driver,
    table: &TableRef,
    column: &ColumnSpec,
) -> Result<Vec<String>, String> {
    column.check()?;
    Ok(vec![format!(
        "ALTER TABLE {} ADD COLUMN {}",
        table.quoted(driver),
        column.definition(driver)
    )])
}

/// Take a column from `before` to `after`: the statements that do it, in
/// order, or why this engine cannot. Nothing changed is no statements.
pub fn alter_column(
    driver: Driver,
    table: &TableRef,
    before: &Column,
    after: &ColumnSpec,
) -> Result<Vec<String>, String> {
    after.check()?;
    let target = table.quoted(driver);
    let old = ColumnSpec::of(before);
    let renamed = old.name != after.name.trim();
    let retyped = old.data_type.trim() != after.data_type.trim();
    let renulled = old.nullable != after.nullable;
    let redefaulted = normalize_default(&old.default) != normalize_default(&after.default);

    let mut statements = Vec::new();
    if renamed {
        statements.push(format!(
            "ALTER TABLE {target} RENAME COLUMN {} TO {}",
            driver.quote_identifier(&old.name),
            driver.quote_identifier(after.name.trim())
        ));
    }
    if !(retyped || renulled || redefaulted) {
        return Ok(statements);
    }

    let column = driver.quote_identifier(after.name.trim());
    match driver {
        Driver::Postgres => {
            if retyped {
                statements.push(format!(
                    "ALTER TABLE {target} ALTER COLUMN {column} TYPE {}",
                    after.data_type.trim()
                ));
            }
            if renulled {
                statements.push(format!(
                    "ALTER TABLE {target} ALTER COLUMN {column} {} NOT NULL",
                    if after.nullable { "DROP" } else { "SET" }
                ));
            }
            if redefaulted {
                statements.push(match normalize_default(&after.default) {
                    Some(default) => {
                        format!("ALTER TABLE {target} ALTER COLUMN {column} SET DEFAULT {default}")
                    }
                    None => format!("ALTER TABLE {target} ALTER COLUMN {column} DROP DEFAULT"),
                });
            }
        }
        // MySQL has no "change just the nullability": MODIFY restates the
        // whole column, and anything left out of it is lost.
        Driver::MySql => statements.push(format!(
            "ALTER TABLE {target} MODIFY COLUMN {}",
            after.definition(driver)
        )),
        Driver::Sqlite => {
            return Err(
                "SQLite cannot change a column's type, nullability or default in place — \
                 only rename or drop it. Changing it means rebuilding the table."
                    .into(),
            )
        }
    }
    Ok(statements)
}

fn normalize_default(default: &Option<String>) -> Option<String> {
    default
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// `ALTER TABLE … DROP COLUMN …`.
pub fn drop_column(driver: Driver, table: &TableRef, column: &str) -> Vec<String> {
    vec![format!(
        "ALTER TABLE {} DROP COLUMN {}",
        table.quoted(driver),
        driver.quote_identifier(column)
    )]
}

/// An index over `columns`, in the order given.
pub fn create_index(
    driver: Driver,
    table: &TableRef,
    name: &str,
    columns: &[String],
    unique: bool,
) -> Result<Vec<String>, String> {
    check_name("index", name)?;
    if columns.is_empty() {
        return Err("Pick at least one column to index".into());
    }
    let listed = columns
        .iter()
        .map(|column| driver.quote_identifier(column))
        .collect::<Vec<_>>()
        .join(", ");
    let unique = if unique { "UNIQUE " } else { "" };
    let name = name.trim();
    // Where the index's own name is qualified differs: SQLite puts the schema
    // on the index and leaves the table bare; the others name the index bare
    // and it lands in the table's schema.
    Ok(vec![match driver {
        Driver::Sqlite => format!(
            "CREATE {unique}INDEX {} ON {} ({listed})",
            TableRef::new(table.schema.clone(), name).quoted(driver),
            driver.quote_identifier(&table.name)
        ),
        _ => format!(
            "CREATE {unique}INDEX {} ON {} ({listed})",
            driver.quote_identifier(name),
            table.quoted(driver)
        ),
    }])
}

/// Drop one of `table`'s indexes.
pub fn drop_index(driver: Driver, table: &TableRef, name: &str) -> Vec<String> {
    vec![match driver {
        // MySQL's indexes belong to their table, and are dropped from it.
        Driver::MySql => format!(
            "DROP INDEX {} ON {}",
            driver.quote_identifier(name),
            table.quoted(driver)
        ),
        _ => format!(
            "DROP INDEX {}",
            TableRef::new(table.schema.clone(), name).quoted(driver)
        ),
    }]
}

/// A new table: its columns, and the ones that make up its primary key.
pub fn create_table(
    driver: Driver,
    table: &TableRef,
    columns: &[ColumnSpec],
    primary_key: &[String],
) -> Result<Vec<String>, String> {
    check_name("table", &table.name)?;
    if columns.is_empty() {
        return Err("A table needs at least one column".into());
    }
    let mut lines = Vec::with_capacity(columns.len() + 1);
    for column in columns {
        column.check()?;
        lines.push(format!("  {}", column.definition(driver)));
    }
    if !primary_key.is_empty() {
        lines.push(format!(
            "  PRIMARY KEY ({})",
            primary_key
                .iter()
                .map(|column| driver.quote_identifier(column))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(vec![format!(
        "CREATE TABLE {} (\n{}\n)",
        table.quoted(driver),
        lines.join(",\n")
    )])
}

/// One of a table's indexes, as read back from the engine.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Index {
    pub name: String,
    /// In index order.
    pub columns: Vec<String>,
    pub unique: bool,
    /// The primary key's own index -- shown, but not dropped from here.
    pub primary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users() -> TableRef {
        TableRef::new("public", "users")
    }

    fn spec(name: &str, data_type: &str, nullable: bool, default: Option<&str>) -> ColumnSpec {
        ColumnSpec {
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: default.map(str::to_string),
        }
    }

    fn column(name: &str, data_type: &str, nullable: bool, default: Option<&str>) -> Column {
        Column {
            name: name.into(),
            data_type: data_type.into(),
            nullable,
            default: default.map(str::to_string),
            is_primary_key: false,
            ordinal: 1,
            references: None,
        }
    }

    #[test]
    fn adding_a_column_quotes_the_name_and_keeps_the_type_as_typed() {
        let sql = add_column(
            Driver::Postgres,
            &users(),
            &spec("signed up", "timestamptz", false, Some("now()")),
        )
        .unwrap();
        assert_eq!(
            sql,
            [
                r#"ALTER TABLE "public"."users" ADD COLUMN "signed up" timestamptz NOT NULL DEFAULT now()"#
            ]
        );
    }

    #[test]
    fn a_hostile_name_stays_one_identifier() {
        let sql = add_column(
            Driver::MySql,
            &TableRef::new("shop", "t"),
            &spec("x` INT; DROP TABLE t; --", "int", true, None),
        )
        .unwrap();
        assert_eq!(
            sql,
            ["ALTER TABLE `shop`.`t` ADD COLUMN `x`` INT; DROP TABLE t; --` int"]
        );
    }

    #[test]
    fn a_semicolon_in_a_type_or_default_is_refused() {
        assert!(add_column(
            Driver::Postgres,
            &users(),
            &spec("a", "int; DROP", true, None)
        )
        .is_err());
        assert!(add_column(
            Driver::Postgres,
            &users(),
            &spec("a", "int", true, Some("1; DROP"))
        )
        .is_err());
    }

    #[test]
    fn postgres_alters_one_property_at_a_time() {
        let before = column("age", "integer", true, None);
        let sql = alter_column(
            Driver::Postgres,
            &users(),
            &before,
            &spec("years", "bigint", false, Some("0")),
        )
        .unwrap();
        assert_eq!(
            sql,
            [
                r#"ALTER TABLE "public"."users" RENAME COLUMN "age" TO "years""#,
                r#"ALTER TABLE "public"."users" ALTER COLUMN "years" TYPE bigint"#,
                r#"ALTER TABLE "public"."users" ALTER COLUMN "years" SET NOT NULL"#,
                r#"ALTER TABLE "public"."users" ALTER COLUMN "years" SET DEFAULT 0"#,
            ]
        );
    }

    #[test]
    fn postgres_drops_a_default_and_not_null() {
        let before = column("age", "integer", false, Some("0"));
        let sql = alter_column(
            Driver::Postgres,
            &users(),
            &before,
            &spec("age", "integer", true, None),
        )
        .unwrap();
        assert_eq!(
            sql,
            [
                r#"ALTER TABLE "public"."users" ALTER COLUMN "age" DROP NOT NULL"#,
                r#"ALTER TABLE "public"."users" ALTER COLUMN "age" DROP DEFAULT"#,
            ]
        );
    }

    #[test]
    fn mysql_restates_the_whole_column() {
        let before = column("age", "int", true, None);
        let sql = alter_column(
            Driver::MySql,
            &TableRef::new("shop", "users"),
            &before,
            &spec("age", "int", false, Some("0")),
        )
        .unwrap();
        assert_eq!(
            sql,
            ["ALTER TABLE `shop`.`users` MODIFY COLUMN `age` int NOT NULL DEFAULT 0"]
        );
    }

    #[test]
    fn sqlite_renames_but_will_not_retype() {
        let table = TableRef::new("main", "users");
        let before = column("age", "INTEGER", true, None);
        assert_eq!(
            alter_column(
                Driver::Sqlite,
                &table,
                &before,
                &spec("years", "INTEGER", true, None)
            )
            .unwrap(),
            [r#"ALTER TABLE "main"."users" RENAME COLUMN "age" TO "years""#]
        );
        let refused = alter_column(
            Driver::Sqlite,
            &table,
            &before,
            &spec("age", "TEXT", true, None),
        );
        assert!(refused.unwrap_err().contains("SQLite cannot"));
    }

    #[test]
    fn an_unchanged_column_is_no_statements() {
        let before = column("age", "integer", true, Some("0"));
        assert!(alter_column(
            Driver::Postgres,
            &users(),
            &before,
            &spec("age", "integer", true, Some(" 0 "))
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn indexes_are_named_where_each_engine_wants_them() {
        let cols = vec!["email".to_string(), "org id".to_string()];
        assert_eq!(
            create_index(Driver::Postgres, &users(), "users_email", &cols, true).unwrap(),
            [r#"CREATE UNIQUE INDEX "users_email" ON "public"."users" ("email", "org id")"#]
        );
        assert_eq!(
            create_index(
                Driver::Sqlite,
                &TableRef::new("main", "users"),
                "by_email",
                &cols[..1],
                false
            )
            .unwrap(),
            [r#"CREATE INDEX "main"."by_email" ON "users" ("email")"#]
        );
        assert_eq!(
            drop_index(Driver::MySql, &TableRef::new("shop", "users"), "by_email"),
            ["DROP INDEX `by_email` ON `shop`.`users`"]
        );
        assert_eq!(
            drop_index(Driver::Postgres, &users(), "users_email"),
            [r#"DROP INDEX "public"."users_email""#]
        );
        assert!(create_index(Driver::Postgres, &users(), "x", &[], false).is_err());
        assert!(create_index(Driver::Postgres, &users(), "  ", &cols, false).is_err());
    }

    #[test]
    fn a_new_table_lists_its_columns_then_its_key() {
        let sql = create_table(
            Driver::Postgres,
            &TableRef::new("public", "notes"),
            &[
                spec("id", "bigserial", false, None),
                spec("body", "text", true, None),
            ],
            &["id".to_string()],
        )
        .unwrap();
        assert_eq!(
            sql,
            [
                "CREATE TABLE \"public\".\"notes\" (\n  \"id\" bigserial NOT NULL,\n  \"body\" text,\n  PRIMARY KEY (\"id\")\n)"
            ]
        );
        assert!(create_table(Driver::Postgres, &TableRef::new("public", "x"), &[], &[]).is_err());
        assert!(create_table(
            Driver::Postgres,
            &TableRef::new("public", "x"),
            &[spec("", "int", true, None)],
            &[]
        )
        .is_err());
    }
}
