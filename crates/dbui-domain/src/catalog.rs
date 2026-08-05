//! The tree in the sidebar: what a server contains.

use crate::connection::Driver;
use serde::{Deserialize, Serialize};

/// Everything the sidebar knows about one server, as of the last refresh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub schemas: Vec<Schema>,
}

impl Catalog {
    pub fn table_count(&self) -> usize {
        self.schemas.iter().map(|s| s.tables.len()).sum()
    }

    pub fn find(&self, reference: &TableRef) -> Option<&Table> {
        self.schemas
            .iter()
            .filter(|schema| schema.name == reference.schema)
            .flat_map(|schema| &schema.tables)
            .find(|table| table.name == reference.name)
    }
}

/// A namespace of tables.
///
/// MySQL has no schema layer -- a database *is* the namespace -- so its adapter
/// reports each database as a `Schema`. That is the difference the rest of the
/// app does not have to know about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
    pub tables: Vec<Table>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    Table,
    View,
    MaterializedView,
}

impl TableKind {
    pub fn label(self) -> &'static str {
        match self {
            TableKind::Table => "table",
            TableKind::View => "view",
            TableKind::MaterializedView => "materialized view",
        }
    }

    /// Views have no rows of their own to edit; the UI greys their actions out.
    pub fn is_view(self) -> bool {
        !matches!(self, TableKind::Table)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub schema: String,
    pub name: String,
    pub kind: TableKind,
}

impl Table {
    pub fn reference(&self) -> TableRef {
        TableRef {
            schema: self.schema.clone(),
            name: self.name.clone(),
        }
    }
}

/// A table addressed by name, which is how every query for its contents is
/// built.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}

impl TableRef {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    /// `schema.name`, for display only.
    pub fn qualified(&self) -> String {
        if self.schema.is_empty() {
            self.name.clone()
        } else {
            format!("{}.{}", self.schema, self.name)
        }
    }

    /// `"schema"."name"`, quoted for the engine and safe to interpolate.
    ///
    /// Identifiers cannot be bound as parameters in either engine, so
    /// generated SQL has to paste them in. Everything that does goes through
    /// here, where [`Driver::quote_identifier`] escapes them.
    pub fn quoted(&self, driver: Driver) -> String {
        if self.schema.is_empty() {
            driver.quote_identifier(&self.name)
        } else {
            format!(
                "{}.{}",
                driver.quote_identifier(&self.schema),
                driver.quote_identifier(&self.name)
            )
        }
    }
}

/// One column of a table, as the structure pane lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    /// The engine's own spelling: `character varying(255)`, `bigint unsigned`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub is_primary_key: bool,
    pub ordinal: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_quote_per_engine() {
        let table = TableRef::new("public", "users");
        assert_eq!(table.quoted(Driver::Postgres), "\"public\".\"users\"");
        assert_eq!(table.quoted(Driver::MySql), "`public`.`users`");
        assert_eq!(table.qualified(), "public.users");
    }

    #[test]
    fn an_unqualified_reference_omits_the_dot() {
        let table = TableRef::new("", "users");
        assert_eq!(table.qualified(), "users");
        assert_eq!(table.quoted(Driver::MySql), "`users`");
    }

    #[test]
    fn a_hostile_table_name_cannot_escape_its_quotes() {
        let table = TableRef::new("public", "users\"; DROP TABLE users; --");
        let sql = table.quoted(Driver::Postgres);
        assert_eq!(sql, "\"public\".\"users\"\"; DROP TABLE users; --\"");
        // One opening quote, one closing quote, and the doubled pair between
        // them -- an even count means nothing broke out.
        assert_eq!(sql.matches('"').count() % 2, 0);
    }
}
