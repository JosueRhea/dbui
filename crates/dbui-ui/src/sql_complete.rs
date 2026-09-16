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

/// Declaration order is the ranking: `build_popup` sorts on `kind as u8`, so
/// catalog objects must come before `Keyword`. Every keyword matches an empty
/// prefix, and with keywords first the 40-item cap filled up before a single
/// schema or table got in — a loaded catalog was invisible, and `users` ranked
/// below `USING` for prefix `u`.
///
/// Among the catalog kinds the order runs narrowest first, which decides one
/// real case: a qualifier that names both a schema and a table makes the
/// qualifier branch emit that schema's tables *and* that table's columns, and
/// after `x.` the caret is inside `x`, so its columns are the closer answer and
/// lead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionKind {
    Column,
    Table,
    Schema,
    Keyword,
}

impl CompletionKind {
    pub fn label(self) -> &'static str {
        match self {
            CompletionKind::Column => "column",
            CompletionKind::Table => "table",
            CompletionKind::Schema => "schema",
            CompletionKind::Keyword => "keyword",
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

    // Stable order: kind then label, so the names in the user's database lead
    // and keywords fill whatever is left.
    items.sort_by(|a, b| {
        (a.kind as u8, a.label.to_ascii_lowercase())
            .cmp(&(b.kind as u8, b.label.to_ascii_lowercase()))
    });
    // Only neighbours are compared, which suffices because the sort has already
    // made equal labels adjacent. A schema and a table sharing a name collapse
    // to the table; accepting either inserts the same text, so only the kind
    // chip differs.
    items.dedup_by(|a, b| a.label.eq_ignore_ascii_case(&b.label));

    // Cap the list so the popup stays usable, but hold a few slots back for
    // keywords: on a database with more than `MAX - KEYWORD_SLOTS` matching
    // objects the catalog would otherwise fill every slot, and ⌃Space on an
    // empty line would offer no SQL at all. Keywords sort last, so the overflow
    // to drop is the tail of the catalog run.
    const MAX: usize = 40;
    const KEYWORD_SLOTS: usize = 8;
    if items.len() > MAX {
        let keywords_at = items.partition_point(|i| i.kind != CompletionKind::Keyword);
        let reserved = (items.len() - keywords_at).min(KEYWORD_SLOTS);
        let keep = (MAX - reserved).min(keywords_at);
        items.drain(keep..keywords_at);
        // `partition_point` finds the keyword run only while `Keyword` is the
        // last variant. Reorder the enum so it is not, and this would silently
        // drop catalog rows and keep keyword overflow instead — no panic, and no
        // failing test, since every fixture has keywords last either way.
        debug_assert!(
            items[keep..]
                .iter()
                .all(|i| i.kind == CompletionKind::Keyword),
            "CompletionKind::Keyword must sort last for the reserve to work"
        );
    }
    items.truncate(MAX);

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

/// Whether `upper` holds `word` at byte `i` with a non-identifier byte on each
/// side. The scan walks one byte at a time, so it can land inside a multi-byte
/// character; comparing bytes rather than slicing a `&str` keeps that from
/// panicking. The leading byte matters too, or `valid_from` reads as `FROM`.
fn keyword_at(upper: &[u8], i: usize, word: &[u8]) -> bool {
    let ident = |c: &u8| c.is_ascii_alphanumeric() || *c == b'_';
    upper[i..].starts_with(word)
        && upper[..i].last().is_none_or(|c| !ident(c))
        && upper[i + word.len()..].first().is_none_or(|c| !ident(c))
}

/// Rough `(alias_or_name, table_name)` pairs from FROM/JOIN clauses.
fn scan_from_aliases(sql: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    // ASCII uppercasing never changes a byte's width, so offsets into `upper`
    // still line up with `sql`.
    let upper = sql.to_ascii_uppercase();
    let upper_bytes = upper.as_bytes();
    let bytes = sql.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        // Find FROM or JOIN as whole words.
        let at_from = keyword_at(upper_bytes, i, b"FROM");
        let at_join = keyword_at(upper_bytes, i, b"JOIN");

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
        if keyword_at(upper_bytes, i, b"AS") {
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

    /// A catalog wide enough that its tables alone overflow the 40-item cap.
    fn wide_catalog() -> Catalog {
        Catalog {
            schemas: vec![Schema {
                name: "public".into(),
                tables: (0..61)
                    .map(|i| Table {
                        schema: "public".into(),
                        name: format!("t_{i:02}"),
                        kind: TableKind::Table,
                    })
                    .collect(),
            }],
        }
    }

    fn column(name: &str) -> Column {
        Column {
            name: name.to_string(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_primary_key: false,
            ordinal: 0,
            references: None,
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
    fn empty_prefix_still_shows_the_catalog() {
        // Every keyword matches an empty prefix, so a kind order that put
        // keywords first spent the whole cap on them and hid the loaded
        // catalog entirely.
        let sql = "SELECT * FROM ";
        let req = request_at(sql, sql.len());
        let popup =
            build_popup(&req, Some(&catalog()), &HashMap::new(), sql, sql.len()).expect("popup");
        assert!(popup.items.iter().any(|i| i.label == "users"));
        assert!(popup.items.iter().any(|i| i.label == "public"));
        assert!(popup
            .items
            .iter()
            .any(|i| i.kind == CompletionKind::Keyword));
        assert!(popup.items.len() <= 40);
    }

    #[test]
    fn table_outranks_keywords_for_the_same_prefix() {
        let sql = "SELECT * FROM u";
        let req = request_at(sql, sql.len());
        let popup =
            build_popup(&req, Some(&catalog()), &HashMap::new(), sql, sql.len()).expect("popup");
        let users = popup
            .items
            .iter()
            .position(|i| i.label == "users")
            .expect("users");
        let keyword = popup
            .items
            .iter()
            .position(|i| i.kind == CompletionKind::Keyword)
            .expect("a keyword");
        assert!(
            users < keyword,
            "expected users ahead of keywords, got {:?}",
            popup.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn qualifier_completes_columns_only() {
        let sql = "SELECT u. FROM users u";
        let caret = 9;
        let mut columns = HashMap::new();
        columns.insert(
            ("public".to_string(), "users".to_string()),
            vec![column("id"), column("name"), column("email")],
        );
        let req = request_at(sql, caret);
        let popup = build_popup(&req, Some(&catalog()), &columns, sql, caret).expect("popup");
        assert!(popup.items.iter().all(|i| i.kind == CompletionKind::Column));
        assert_eq!(
            popup
                .items
                .iter()
                .map(|i| i.label.as_str())
                .collect::<Vec<_>>(),
            vec!["email", "id", "name"]
        );
    }

    #[test]
    fn a_wide_catalog_cannot_evict_every_keyword() {
        // 61 tables all match an empty prefix, so without reserved slots the
        // catalog fills the cap and the popup offers no SQL at all.
        let sql = "SELECT * FROM ";
        let req = request_at(sql, sql.len());
        let popup = build_popup(&req, Some(&wide_catalog()), &HashMap::new(), sql, sql.len())
            .expect("popup");
        assert_eq!(popup.items.len(), 40);
        assert_eq!(popup.items[0].label, "t_00");
        assert_eq!(
            popup
                .items
                .iter()
                .filter(|i| i.kind == CompletionKind::Keyword)
                .count(),
            8
        );

        // Prefix `t` matches every table and only four keywords, so all four
        // fit in the reserved slots rather than being pushed out.
        let sql = "SELECT * FROM t";
        let req = request_at(sql, sql.len());
        let popup = build_popup(&req, Some(&wide_catalog()), &HashMap::new(), sql, sql.len())
            .expect("popup");
        assert!(popup.items.len() <= 40);
        for keyword in ["TABLE", "THEN", "TRANSACTION", "TRUE"] {
            assert!(
                popup.items.iter().any(|i| i.label == keyword),
                "{keyword} was evicted"
            );
        }
    }

    #[test]
    fn a_qualifier_that_names_both_a_schema_and_a_table_leads_with_columns() {
        let catalog = Catalog {
            schemas: vec![
                Schema {
                    name: "audit".into(),
                    tables: vec![Table {
                        schema: "audit".into(),
                        name: "events".into(),
                        kind: TableKind::Table,
                    }],
                },
                Schema {
                    name: "public".into(),
                    tables: vec![Table {
                        schema: "public".into(),
                        name: "audit".into(),
                        kind: TableKind::Table,
                    }],
                },
            ],
        };
        let mut columns = HashMap::new();
        columns.insert(
            ("public".to_string(), "audit".to_string()),
            vec![column("id"), column("actor")],
        );
        let sql = "SELECT audit. FROM audit";
        let caret = 13;
        let req = request_at(sql, caret);
        assert_eq!(req.qualifier.as_deref(), Some("audit"));
        let popup = build_popup(&req, Some(&catalog), &columns, sql, caret).expect("popup");
        assert_eq!(
            popup
                .items
                .iter()
                .map(|i| (i.label.as_str(), i.kind))
                .collect::<Vec<_>>(),
            vec![
                ("actor", CompletionKind::Column),
                ("id", CompletionKind::Column),
                ("events", CompletionKind::Table),
            ]
        );
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

    #[test]
    fn scans_aliases_past_non_ascii_literals() {
        let sql = "SELECT * FROM users u WHERE u.name = 'café' AND u.";
        assert_eq!(
            scan_from_aliases(sql),
            vec![("u".to_string(), "users".to_string())]
        );
    }

    #[test]
    fn ignores_identifier_ending_in_from() {
        let sql = "SELECT valid_from FROM users u WHERE u.";
        assert_eq!(
            scan_from_aliases(sql),
            vec![("u".to_string(), "users".to_string())]
        );
    }
}
