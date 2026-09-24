//! A whole database -- or some of its schemas -- written out as one SQL file
//! that recreates it.
//!
//! Built from what the adapters already read rather than by running
//! `pg_dump` or `mysqldump`: those have to be installed, have to match the
//! server's version, and cannot reach a database that is only reachable
//! through the app's SSH tunnel. This needs nothing but the connection.
//!
//! The file is ordered so it runs top to bottom on an empty database: types,
//! extensions and sequences first, then each table and its rows, then what
//! points between tables (foreign keys), then the code (functions, views,
//! triggers). It is plain SQL, split the way the app's editor splits it, so
//! [`crate::commands::run_script`] -- or psql, mysql, sqlite3 -- restores it.
//!
//! What it is not: a byte-for-byte replacement for `pg_dump`. Roles, grants,
//! comments, table storage options, partitioning and publications are not
//! written; neither are the few values of a type this build cannot decode,
//! which are counted in the report and written as NULL.

use dbui_domain::{
    order_for, Catalog, Driver, ObjectKind, Page, SortKey, Table, TableKind, TableRef, Value,
};
use dbui_driver::DatabaseDriver;
use std::io::Write;

/// Rows per `INSERT`: few enough that one statement stays readable and well
/// under every engine's packet limit, many enough that a restore is not one
/// round trip per row.
const ROWS_PER_INSERT: usize = 100;

/// Rows read per page from the server.
const PAGE: u32 = 5_000;

/// What a dump wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DumpReport {
    pub tables: usize,
    pub rows: u64,
    /// Views, functions, types... everything scripted besides tables.
    pub objects: usize,
    /// Cells of a type this build cannot decode, written as NULL.
    pub unreadable_values: u64,
    /// Objects that could not be scripted, with why.
    pub skipped: Vec<String>,
}

impl DumpReport {
    pub fn summary(&self) -> String {
        let mut out = format!(
            "{} table{}, {} row{}, {} other object{}",
            self.tables,
            plural(self.tables as u64),
            self.rows,
            plural(self.rows),
            self.objects,
            plural(self.objects as u64)
        );
        if self.unreadable_values > 0 {
            out.push_str(&format!(
                " · {} unreadable value{} written as NULL",
                self.unreadable_values,
                plural(self.unreadable_values)
            ));
        }
        if !self.skipped.is_empty() {
            out.push_str(&format!(" · {} skipped", self.skipped.len()));
        }
        out
    }
}

fn plural(n: u64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// The schemas a dump of this connection covers when nothing narrower is
/// asked for: MySQL lists every database on the server, so there it is the
/// one the connection names (or all, when it names none); elsewhere it is
/// every schema in the database.
pub fn default_schemas(driver: Driver, catalog: &Catalog, database: &str) -> Vec<String> {
    let all = catalog.schemas.iter().map(|schema| schema.name.clone());
    match driver {
        Driver::MySql if !database.is_empty() => all.filter(|name| name == database).collect(),
        _ => all.collect(),
    }
}

/// Write `schemas` of the database behind `driver` to `out`.
///
/// `progress` hears the name of each table as its rows start, for a status
/// line.
pub async fn dump(
    driver: &dyn DatabaseDriver,
    schemas: &[String],
    out: &mut impl Write,
    mut progress: impl FnMut(&str),
) -> Result<DumpReport, String> {
    let engine = driver.driver();
    let catalog = driver.catalog().await.map_err(|error| error.to_string())?;
    let mut report = DumpReport::default();
    let mut w = Writer { out };

    w.line(&format!("-- dbui dump · {}", driver.server_version()))?;
    w.line(&format!("-- Schemas: {}", schemas.join(", ")))?;
    w.line("-- Runs top to bottom on an empty database: types and sequences, tables and")?;
    w.line("-- their rows, foreign keys, then functions, views and triggers.")?;
    w.line("")?;

    // Foreign keys are enforced as each row lands; turned off for the load,
    // the tables can come in any order. (Postgres adds its keys after the
    // data instead, so it needs no switch.)
    match engine {
        Driver::MySql => w.statement("SET FOREIGN_KEY_CHECKS = 0")?,
        Driver::Sqlite => w.statement("PRAGMA foreign_keys = OFF")?,
        Driver::Postgres => {}
    }

    let mut after_data: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();

    for schema_name in schemas {
        let Some(schema) = catalog.schemas.iter().find(|s| &s.name == schema_name) else {
            report
                .skipped
                .push(format!("schema {schema_name}: not found"));
            continue;
        };
        w.line(&format!("\n-- ==== {schema_name} ===="))?;
        match engine {
            Driver::Postgres => w.statement(&format!(
                "CREATE SCHEMA IF NOT EXISTS {}",
                engine.quote_identifier(schema_name)
            ))?,
            // A MySQL schema is a database, and its tables are written
            // unqualified by the server's own CREATE TABLE -- so they land in
            // whichever database is in use.
            Driver::MySql => {
                w.statement(&format!(
                    "CREATE DATABASE IF NOT EXISTS {}",
                    engine.quote_identifier(schema_name)
                ))?;
                w.statement(&format!("USE {}", engine.quote_identifier(schema_name)))?;
            }
            Driver::Sqlite => {}
        }

        // Things tables are built from.
        for kind in [
            ObjectKind::Extension,
            ObjectKind::Type,
            ObjectKind::Sequence,
        ] {
            for object in catalog.objects_of(schema_name, kind) {
                match driver.definition(object).await {
                    Ok(sql) => {
                        let sql = if kind == ObjectKind::Extension {
                            sql.replacen("CREATE EXTENSION ", "CREATE EXTENSION IF NOT EXISTS ", 1)
                        } else {
                            sql
                        };
                        w.statement(&sql)?;
                        report.objects += 1;
                    }
                    Err(error) => {
                        report
                            .skipped
                            .push(format!("{} {}: {error}", kind.label(), object.name))
                    }
                }
            }
        }

        // Tables, each followed by its rows.
        for table in schema.tables.iter().filter(|t| t.kind == TableKind::Table) {
            let statements = match driver.create_statements(table).await {
                Ok(statements) => statements,
                Err(error) => {
                    report
                        .skipped
                        .push(format!("table {}: {error}", table.name));
                    continue;
                }
            };
            w.line(&format!("\n-- {}", table.name))?;
            for sql in &statements.create {
                w.statement(&strip_definer(sql))?;
            }
            progress(&table.name);
            let (rows, unreadable) = write_rows(driver, engine, table, &mut w).await?;
            report.rows += rows;
            report.unreadable_values += unreadable;
            report.tables += 1;
            after_data.extend(statements.after_data);
        }

        // Where sequences had got to, so the next id is not one just loaded.
        for sequence in catalog.objects_of(schema_name, ObjectKind::Sequence) {
            if let Ok(Some(sql)) = driver.sequence_position(sequence).await {
                after_data.push(sql);
            }
        }

        // Code last: a function's body, a view's query and a trigger's table
        // can all name tables, and some engines check that on creation.
        for kind in [ObjectKind::Function, ObjectKind::Procedure] {
            for object in catalog.objects_of(schema_name, kind) {
                match driver.definition(object).await {
                    Ok(sql) => {
                        code.push(strip_definer(&sql));
                        report.objects += 1;
                    }
                    Err(error) => {
                        report
                            .skipped
                            .push(format!("{} {}: {error}", kind.label(), object.name))
                    }
                }
            }
        }
        for view in schema.tables.iter().filter(|t| t.kind.is_view()) {
            match driver.create_statements(view).await {
                Ok(statements) => {
                    code.extend(statements.create.iter().map(|sql| strip_definer(sql)));
                    report.objects += 1;
                }
                Err(error) => report.skipped.push(format!("view {}: {error}", view.name)),
            }
        }
        for trigger in catalog.objects_of(schema_name, ObjectKind::Trigger) {
            match driver.definition(trigger).await {
                Ok(sql) => {
                    code.push(strip_definer(&sql));
                    report.objects += 1;
                }
                Err(error) => report
                    .skipped
                    .push(format!("trigger {}: {error}", trigger.name)),
            }
        }
        // MySQL's code goes in the database it came from; `USE` again, in
        // case the next schema's section moved on.
        if engine == Driver::MySql && !code.is_empty() {
            code.insert(0, format!("USE {}", engine.quote_identifier(schema_name)));
        }
        if !code.is_empty() {
            w.line("\n-- functions, views and triggers")?;
            for sql in code.drain(..) {
                w.statement(&sql)?;
            }
        }
    }

    if !after_data.is_empty() {
        w.line("\n-- foreign keys and sequence positions")?;
        for sql in &after_data {
            w.statement(sql)?;
        }
    }

    match engine {
        Driver::MySql => w.statement("SET FOREIGN_KEY_CHECKS = 1")?,
        Driver::Sqlite => w.statement("PRAGMA foreign_keys = ON")?,
        Driver::Postgres => {}
    }
    for skipped in &report.skipped {
        w.line(&format!("-- skipped: {skipped}"))?;
    }
    w.out.flush().map_err(|error| error.to_string())?;
    Ok(report)
}

/// Every row of `table`, as multi-row `INSERT`s, read a page at a time in
/// key order. Returns the rows written and the cells that could not be read.
async fn write_rows(
    driver: &dyn DatabaseDriver,
    engine: Driver,
    table: &Table,
    w: &mut Writer<'_, impl Write>,
) -> Result<(u64, u64), String> {
    let reference = TableRef::new(&table.schema, &table.name);
    let columns = driver
        .columns(&reference)
        .await
        .map_err(|error| error.to_string())?;
    let mut keys: Vec<_> = columns.iter().filter(|c| c.is_primary_key).collect();
    keys.sort_by_key(|column| column.ordinal);
    let keys: Vec<String> = keys.into_iter().map(|c| c.name.clone()).collect();
    let order: Vec<SortKey> = order_for(None, &keys);
    // Without a key there is no order that holds still between pages, so
    // the table is read in one go.
    let limit = if keys.is_empty() { u32::MAX - 1 } else { PAGE };

    // MySQL and SQLite write their tables unqualified -- the dump's `USE`
    // or the file itself says where -- and so do the rows.
    let target = match engine {
        Driver::Postgres => reference.quoted(engine),
        _ => engine.quote_identifier(&table.name),
    };
    // A GENERATED ALWAYS identity refuses an explicit id without this;
    // everywhere else Postgres ignores it.
    let overriding = if engine == Driver::Postgres {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };

    let mut offset = 0u64;
    let mut written = 0u64;
    let mut unreadable = 0u64;
    loop {
        let set = driver
            .table_rows(&reference, Page { limit, offset }, "", &order)
            .await
            .map_err(|error| format!("reading {}: {error}", table.name))?;
        if written == 0 && set.rows.is_empty() {
            break;
        }
        let names = set
            .columns
            .iter()
            .map(|column| engine.quote_identifier(&column.name))
            .collect::<Vec<_>>()
            .join(", ");
        for chunk in set.rows.chunks(ROWS_PER_INSERT) {
            let values: Vec<String> = chunk
                .iter()
                .map(|row| {
                    unreadable += row
                        .0
                        .iter()
                        .filter(|value| matches!(value, Value::Unsupported(_)))
                        .count() as u64;
                    let literals: Vec<String> = row
                        .0
                        .iter()
                        .map(|value| value.sql_literal(engine))
                        .collect();
                    format!("  ({})", literals.join(", "))
                })
                .collect();
            w.statement(&format!(
                "INSERT INTO {target} ({names}){overriding} VALUES\n{}",
                values.join(",\n")
            ))?;
        }
        let count = set.rows.len() as u64;
        written += count;
        offset += count;
        if !set.truncated || count == 0 {
            break;
        }
    }
    Ok((written, unreadable))
}

/// `CREATE DEFINER=`root`@`%` VIEW ...` -> `CREATE VIEW ...`.
///
/// MySQL writes the creating user into views, routines and triggers, and
/// restoring one as anybody else then needs a privilege most users lack. A
/// restored object belongs to whoever restores it, as `mysqldump
/// --skip-definer` would have it.
fn strip_definer(sql: &str) -> String {
    let Some(start) = sql.find("DEFINER=") else {
        return sql.to_string();
    };
    // Only in the statement's head, before the body starts.
    if sql[..start].contains('(') || start > 120 {
        return sql.to_string();
    }
    let rest = &sql[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    format!("{}{}", &sql[..start], rest[end..].trim_start())
}

struct Writer<'a, W: Write> {
    out: &'a mut W,
}

impl<W: Write> Writer<'_, W> {
    fn line(&mut self, text: &str) -> Result<(), String> {
        writeln!(self.out, "{text}").map_err(|error| error.to_string())
    }

    /// One statement, terminated. A body that ends in its own `;` -- a
    /// function's `$$ ... $$;` never does, a trigger's `END;` does -- is not
    /// given a second.
    fn statement(&mut self, sql: &str) -> Result<(), String> {
        let sql = sql.trim_end();
        if sql.is_empty() {
            return Ok(());
        }
        if sql.ends_with(';') {
            self.line(sql)
        } else {
            self.line(&format!("{sql};"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_definer_is_dropped_from_the_head_only() {
        assert_eq!(
            strip_definer("CREATE ALGORITHM=UNDEFINED DEFINER=`root`@`%` SQL SECURITY DEFINER VIEW `v` AS select 1"),
            "CREATE ALGORITHM=UNDEFINED SQL SECURITY DEFINER VIEW `v` AS select 1"
        );
        assert_eq!(
            strip_definer("CREATE DEFINER=`app`@`localhost` TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1"),
            "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1"
        );
        let body = "CREATE FUNCTION f() RETURNS text RETURN 'DEFINER=x'";
        assert_eq!(strip_definer(body), body, "not inside the body");
    }

    #[test]
    fn statements_get_exactly_one_terminator() {
        let mut out = Vec::new();
        let mut w = Writer { out: &mut out };
        w.statement("SELECT 1").unwrap();
        w.statement("CREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END;")
            .unwrap();
        w.statement("   ").unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "SELECT 1;\nCREATE TRIGGER t AFTER INSERT ON x BEGIN SELECT 1; END;\n"
        );
    }

    #[test]
    fn a_mysql_dump_covers_the_database_the_connection_names() {
        let catalog = Catalog {
            schemas: ["app", "other"]
                .iter()
                .map(|name| dbui_domain::Schema {
                    name: name.to_string(),
                    tables: Vec::new(),
                })
                .collect(),
            objects: Vec::new(),
        };
        assert_eq!(default_schemas(Driver::MySql, &catalog, "app"), vec!["app"]);
        assert_eq!(default_schemas(Driver::MySql, &catalog, "").len(), 2);
        assert_eq!(default_schemas(Driver::Postgres, &catalog, "app").len(), 2);
    }
}
