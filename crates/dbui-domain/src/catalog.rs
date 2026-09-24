//! The tree in the sidebar: what a server contains.

use crate::connection::Driver;
use serde::{Deserialize, Serialize};

/// Everything the sidebar knows about one server, as of the last refresh.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub schemas: Vec<Schema>,
    /// Everything that is not a table or view: functions, triggers,
    /// sequences, types, extensions. Kept apart from `schemas` because only
    /// the tree lists them; everything that pages, sorts or edits rows walks
    /// tables and would only have to skip these.
    #[serde(default)]
    pub objects: Vec<DbObject>,
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

impl Catalog {
    /// The objects of one kind in one schema, in the order the adapter gave.
    pub fn objects_of<'a>(
        &'a self,
        schema: &'a str,
        kind: ObjectKind,
    ) -> impl Iterator<Item = &'a DbObject> + 'a {
        self.objects
            .iter()
            .filter(move |object| object.schema == schema && object.kind == kind)
    }
}

/// The non-table things a schema can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ObjectKind {
    Function,
    Procedure,
    Trigger,
    Sequence,
    /// Enums, domains, composite and range types.
    Type,
    Extension,
}

impl ObjectKind {
    /// Tree order.
    pub const ALL: [ObjectKind; 6] = [
        ObjectKind::Function,
        ObjectKind::Procedure,
        ObjectKind::Trigger,
        ObjectKind::Sequence,
        ObjectKind::Type,
        ObjectKind::Extension,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectKind::Function => "function",
            ObjectKind::Procedure => "procedure",
            ObjectKind::Trigger => "trigger",
            ObjectKind::Sequence => "sequence",
            ObjectKind::Type => "type",
            ObjectKind::Extension => "extension",
        }
    }

    /// The group heading in the tree.
    pub fn plural(self) -> &'static str {
        match self {
            ObjectKind::Function => "Functions",
            ObjectKind::Procedure => "Procedures",
            ObjectKind::Trigger => "Triggers",
            ObjectKind::Sequence => "Sequences",
            ObjectKind::Type => "Types",
            ObjectKind::Extension => "Extensions",
        }
    }
}

/// One function, trigger, sequence... as the tree lists it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DbObject {
    pub schema: String,
    pub name: String,
    pub kind: ObjectKind,
    /// A few words beside the name: a function's arguments, the table a
    /// trigger is on, what sort of type a type is, an extension's version.
    pub detail: Option<String>,
    /// What the adapter needs to find this object again to read its
    /// definition -- a Postgres OID, say. Opaque above the adapter.
    pub key: String,
}

/// What it takes to create a table or view again, as a dump writes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CreateStatements {
    /// Before the rows: the `CREATE TABLE` (or `VIEW`) and its indexes.
    pub create: Vec<String>,
    /// After every table's rows: the constraints that point between tables,
    /// which would otherwise need the tables loaded in dependency order.
    pub after_data: Vec<String>,
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

/// One column's reference to another table.
///
/// Only single-column keys are carried. A composite foreign key cannot be
/// followed from one cell -- the value in front of the user is one part of a
/// key, and jumping on it would land on rows that merely share that part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    /// The column on this table.
    pub column: String,
    pub references: TableRef,
    /// The column on the referenced table.
    pub references_column: String,
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
    /// Where this column points, when it is a single-column foreign key.
    #[serde(default)]
    pub references: Option<ForeignKey>,
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
