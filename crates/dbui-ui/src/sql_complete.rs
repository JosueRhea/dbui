//! Autocomplete candidates for the SQL editor.
//!
//! Prefix matching only — keywords, catalog schemas/tables, and cached columns.
//! Resolving `alias.` uses a light scan of the current statement's FROM/JOIN
//! clauses, not a full SQL parse.

use std::collections::HashMap;
use std::ops::Range;

use dbui_app::domain::{statement_at, Catalog, Column, TableRef};

use crate::sql_format;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    pub kind: CompletionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Keyword,
    Schema,
    Table,
    Column,
}

impl CompletionKind {
    pub fn label(self) -> &'static str {
        match self {
            CompletionKind::Keyword => "keyword",
            CompletionKind::Schema => "schema",
            CompletionKind::Table => "table",
            CompletionKind::Column => "column",
        }
    }
}

#[derive(Debug, Clone)]
pub struct CompletionPopup {
    pub items: Vec<CompletionItem>,
    pub selected: usize,
    pub replace_range: Range<usize>,
}

impl CompletionPopup {
    pub fn select_delta(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let next = (self.selected as isize + delta).rem_euclid(len) as usize;
        self.selected = next;
    }

    pub fn current(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }
}

/// What the caret is completing: a bare prefix, or `qualifier.prefix`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    pub prefix: String,
    pub replace_range: Range<usize>,
    /// Identifier before the `.`, when completing after a dot.
    pub qualifier: Option<String>,
}

/// Find the completion request at `caret` in `sql`.
pub fn request_at(sql: &str, caret: usize) -> CompletionRequest {
    let caret = caret.min(sql.len());
    let before = &sql[..caret];

    // Walk back over the identifier under the caret.
    let mut start = caret;
    for (idx, ch) in before.char_indices().rev() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' {
            start = idx;
        } else {
            break;
        }
    }
    let prefix = before[start..].to_string();

    // Optional qualifier: `foo.` immediately before the prefix.
    let mut qualifier = None;
    if start > 0 && before.as_bytes()[start - 1] == b'.' {
        let q_end = start - 1;
        let mut q_start = q_end;
        for (idx, ch) in before[..q_end].char_indices().rev() {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '$' || ch == '"' || ch == '`' {
                q_start = idx;
            } else {
                break;
            }
        }
        if q_start < q_end {
            let raw = before[q_start..q_end].trim_matches(|c| c == '"' || c == '`');
            if !raw.is_empty() {
                qualifier = Some(raw.to_string());
            }
        }
    }

    CompletionRequest {
        prefix,
        replace_range: start..caret,
        qualifier,
    }
}

/// Build a popup for `request` from the catalog and column cache.
pub fn build_popup(
    request: &CompletionRequest,
    catalog: Option<&Catalog>,
    column_cache: &HashMap<(String, String), Vec<Column>>,
    sql: &str,
    caret: usize,
) -> Option<CompletionPopup> {
    let mut items = Vec::new();
    let prefix = request.prefix.to_ascii_lowercase();
    let matches =
        |label: &str| prefix.is_empty() || label.to_ascii_lowercase().starts_with(&prefix);

    if let Some(qualifier) = &request.qualifier {
        // schema. → tables in that schema
        if let Some(catalog) = catalog {
            if let Some(schema) = catalog
                .schemas
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(qualifier))
            {
                for table in &schema.tables {
                    if matches(&table.name) {
                        items.push(CompletionItem {
                            label: table.name.clone(),
                            kind: CompletionKind::Table,
                        });
                    }
                }
            }
        }

        // table. or alias. → columns
        let table_ref = resolve_qualifier(qualifier, catalog, sql, caret);
        if let Some(table) = table_ref {
            let key = (table.schema.clone(), table.name.clone());
            if let Some(columns) = column_cache.get(&key) {
                for column in columns {
                    if matches(&column.name) {
                        items.push(CompletionItem {
                            label: column.name.clone(),
                            kind: CompletionKind::Column,
                        });
                    }
                }
            }
        }
    } else {
        for keyword in sql_format::completion_keywords() {
            if matches(keyword) {
                items.push(CompletionItem {
                    label: (*keyword).to_string(),
                    kind: CompletionKind::Keyword,
                });
            }
        }
        if let Some(catalog) = catalog {
            for schema in &catalog.schemas {
                if matches(&schema.name) {
                    items.push(CompletionItem {
                        label: schema.name.clone(),
                        kind: CompletionKind::Schema,
                    });
                }
                for table in &schema.tables {
                    if matches(&table.name) {
                        items.push(CompletionItem {
                            label: table.name.clone(),
                            kind: CompletionKind::Table,
                        });
                    }
                }
            }
        }
    }

    // Stable order: kind then label. Cap the list so the popup stays usable.
    items.sort_by(|a, b| {
        (a.kind as u8, a.label.to_ascii_lowercase())
            .cmp(&(b.kind as u8, b.label.to_ascii_lowercase()))
    });
    items.dedup_by(|a, b| a.label.eq_ignore_ascii_case(&b.label));
    items.truncate(40);

    if items.is_empty() {
        return None;
    }

    Some(CompletionPopup {
        items,
        selected: 0,
        replace_range: request.replace_range.clone(),
    })
}

/// Whether we should kick off a `columns()` fetch for this request.
pub fn pending_column_fetch(
    request: &CompletionRequest,
    catalog: Option<&Catalog>,
    column_cache: &HashMap<(String, String), Vec<Column>>,
    sql: &str,
    caret: usize,
) -> Option<TableRef> {
    let qualifier = request.qualifier.as_ref()?;
    let table = resolve_qualifier(qualifier, catalog, sql, caret)?;
    let key = (table.schema.clone(), table.name.clone());
    if column_cache.contains_key(&key) {
        return None;
    }
    Some(table)
}

fn resolve_qualifier(
    qualifier: &str,
    catalog: Option<&Catalog>,
    sql: &str,
    caret: usize,
) -> Option<TableRef> {
    // Direct table name in the catalog.
    if let Some(catalog) = catalog {
        for schema in &catalog.schemas {
            if let Some(table) = schema
                .tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(qualifier))
            {
                return Some(TableRef::new(table.schema.clone(), table.name.clone()));
            }
        }
    }

    // Alias from FROM / JOIN in the current statement.
    let stmt = statement_at(sql, caret).map(|r| &sql[r]).unwrap_or(sql);
    for (alias, table) in scan_from_aliases(stmt) {
        if alias.eq_ignore_ascii_case(qualifier) {
            if let Some(catalog) = catalog {
                for schema in &catalog.schemas {
                    if let Some(t) = schema
                        .tables
                        .iter()
                        .find(|t| t.name.eq_ignore_ascii_case(&table))
                    {
                        return Some(TableRef::new(t.schema.clone(), t.name.clone()));
                    }
                }
            }
            return Some(TableRef::new("", table));
        }
    }

    None
}

/// Rough `(alias_or_name, table_name)` pairs from FROM/JOIN clauses.
fn scan_from_aliases(sql: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let upper = sql.to_ascii_uppercase();
    let bytes = sql.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        // Find FROM or JOIN as whole words.
        let rest = &upper[i..];
        let at_from = rest.starts_with("FROM")
            && rest
                .as_bytes()
                .get(4)
                .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');
        let at_join = rest.starts_with("JOIN")
            && rest
                .as_bytes()
                .get(4)
                .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_');

        if !(at_from || at_join) {
            i += 1;
            continue;
        }
        i += 4;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let Some((table, next)) = read_ident(sql, i) else {
            continue;
        };
        i = next;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // Optional AS
        if upper[i..].starts_with("AS")
            && upper
                .as_bytes()
                .get(i + 2)
                .is_none_or(|c| !c.is_ascii_alphanumeric() && *c != b'_')
        {
            i += 2;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
        }
        let alias = if let Some((alias, next)) = read_ident(sql, i) {
            // Don't treat a following keyword as an alias.
            if sql_format::completion_keywords()
                .iter()
                .any(|k| alias.eq_ignore_ascii_case(k))
            {
                table.clone()
            } else {
                i = next;
                alias
            }
        } else {
            table.clone()
        };
        out.push((alias, table));
    }

    out
}

fn read_ident(sql: &str, start: usize) -> Option<(String, usize)> {
    let bytes = sql.as_bytes();
    if start >= bytes.len() {
        return None;
    }
    let quote = match bytes[start] {
        b'"' | b'`' => Some(bytes[start]),
        _ => None,
    };
    if let Some(q) = quote {
        let mut i = start + 1;
        while i < bytes.len() && bytes[i] != q {
            i += 1;
        }
        if i < bytes.len() {
            let name = sql[start + 1..i].to_string();
            return Some((name, i + 1));
        }
        return None;
    }
    if !bytes[start].is_ascii_alphabetic() && bytes[start] != b'_' {
        return None;
    }
    let mut i = start + 1;
    while i < bytes.len()
        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
    {
        // schema.table — take the last segment as the table name for catalog lookup.
        i += 1;
    }
    let raw = &sql[start..i];
    let name = raw.rsplit('.').next().unwrap_or(raw).to_string();
    Some((name, i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_app::domain::{Schema, Table, TableKind};

    fn catalog() -> Catalog {
        Catalog {
            schemas: vec![Schema {
                name: "public".into(),
                tables: vec![Table {
                    schema: "public".into(),
                    name: "users".into(),
                    kind: TableKind::Table,
                }],
            }],
        }
    }

    #[test]
    fn request_finds_prefix_and_qualifier() {
        let req = request_at("SELECT u.", 9);
        assert_eq!(req.prefix, "");
        assert_eq!(req.qualifier.as_deref(), Some("u"));

        let req = request_at("SELECT us", 9);
        assert_eq!(req.prefix, "us");
        assert!(req.qualifier.is_none());
    }

    #[test]
    fn suggests_tables_and_keywords() {
        let req = request_at("SELECT * FROM us", 16);
        let popup = build_popup(
            &req,
            Some(&catalog()),
            &HashMap::new(),
            "SELECT * FROM us",
            16,
        )
        .expect("popup");
        assert!(popup.items.iter().any(|i| i.label == "users"));
        assert!(popup
            .items
            .iter()
            .any(|i| i.label == "USING" || i.kind == CompletionKind::Keyword));
    }

    #[test]
    fn resolves_alias_to_table() {
        let sql = "SELECT u. FROM users u";
        let caret = 9; // after `u.`
        let req = request_at(sql, caret);
        assert_eq!(req.qualifier.as_deref(), Some("u"));
        let table = resolve_qualifier("u", Some(&catalog()), sql, caret).unwrap();
        assert_eq!(table.name, "users");
    }
}
