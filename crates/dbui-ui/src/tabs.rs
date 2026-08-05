//! Workspace tabs: each open table or SQL query is one tab.

use crate::json_format;
use crate::root::ResultView;
use crate::text_input::TextInput;
use dbui_app::domain::{Column, ColumnInfo, Page, TableRef, Value};
use std::collections::HashSet;

/// Stable identity for a tab across activates/closes. Async loads key off this
/// so a slow response never lands on whatever tab happens to be active.
pub type TabId = u64;

/// Which surface of a table tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TablePane {
    #[default]
    Data,
    Structure,
}

/// One column change inside a pending row edit.
#[derive(Clone)]
pub struct FieldChange {
    pub column: String,
    pub old_text: String,
    pub new_value: Value,
    pub new_text: String,
}

/// A dirty row waiting to be saved with the rest of the batch.
#[derive(Clone)]
pub struct PendingRowEdit {
    pub pk: Vec<(String, Value)>,
    pub label: String,
    pub changes: Vec<FieldChange>,
}

impl PendingRowEdit {
    pub fn matches_pk(&self, pk: &[(String, Value)]) -> bool {
        self.pk.len() == pk.len()
            && self
                .pk
                .iter()
                .zip(pk.iter())
                .all(|(a, b)| a.0 == b.0 && values_equal(&a.1, &b.1))
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Default, Value::Default) => true,
        (Value::Default, _) | (_, Value::Default) => false,
        (Value::Null, Value::Null) => true,
        (Value::Null, _) | (_, Value::Null) => false,
        _ => {
            let left = a.to_text();
            let right = b.to_text();
            // Compact DB JSON and the pretty-printed editor form must not
            // register as a change — that was why Discard looked broken.
            left == right || json_format::texts_equivalent(&left, &right)
        }
    }
}

/// How a value appears in the change bubble / draft editors.
fn display_change_text(value: &Value) -> String {
    value_editor_text(value)
}

/// Editable draft of one selected row in the detail sidebar.
pub struct RowDraft {
    pub row_index: usize,
    /// `(column name, editor, is_primary_key)`
    pub fields: Vec<(String, TextInput, bool)>,
    pub message: Option<(bool, String)>,
    pub field_search: TextInput,
}

impl RowDraft {
    /// Build editors aligned with the result-set columns (same order as `values`).
    /// Primary-key flags are looked up by name from table `structure` when present.
    pub fn from_row(
        row_index: usize,
        result_columns: &[ColumnInfo],
        values: &[Value],
        structure: &[Column],
    ) -> Self {
        let fields = result_columns
            .iter()
            .enumerate()
            .map(|(i, column)| {
                let is_pk = structure
                    .iter()
                    .find(|c| c.name == column.name)
                    .map(|c| c.is_primary_key)
                    .unwrap_or(false);
                let text = values.get(i).map(value_editor_text).unwrap_or_default();
                (
                    column.name.clone(),
                    // Non-PK cells are always multiline editors so JSON / long
                    // text stay editable with real newlines.
                    TextInput::with_text(text, !is_pk),
                    is_pk,
                )
            })
            .collect();
        Self {
            row_index,
            fields,
            message: None,
            field_search: TextInput::new(false),
        }
    }

    /// SQL / ad-hoc results: every field is an editable multiline editor.
    pub fn from_sql_row(row_index: usize, columns: &[ColumnInfo], values: &[Value]) -> Self {
        Self::from_row(row_index, columns, values, &[])
    }

    pub fn is_dirty(&self, originals: &[Value]) -> bool {
        self.fields.iter().enumerate().any(|(i, (_, input, is_pk))| {
            if *is_pk {
                return false;
            }
            let Some(original) = originals.get(i) else {
                return false;
            };
            match parse_draft_value(input.text(), original) {
                Ok(parsed) => !values_equal(&parsed, original),
                Err(_) => {
                    let original_text = value_editor_text(original);
                    !json_format::texts_equivalent(input.text(), &original_text)
                }
            }
        })
    }

    /// Build a pending edit from this draft, or `None` if nothing changed / parse failed.
    pub fn to_pending(
        &self,
        originals: &[Value],
    ) -> Result<Option<PendingRowEdit>, String> {
        if !self.is_dirty(originals) {
            return Ok(None);
        }

        let mut pk = Vec::new();
        let mut changes = Vec::new();
        let mut label_parts = Vec::new();

        for (index, (name, input, is_pk)) in self.fields.iter().enumerate() {
            let Some(original) = originals.get(index) else {
                continue;
            };
            let parsed = parse_draft_value(input.text(), original)?;
            if *is_pk {
                label_parts.push(format!("{name}={}", display_change_text(&parsed)));
                pk.push((name.clone(), parsed));
            } else if !values_equal(&parsed, original) {
                changes.push(FieldChange {
                    column: name.clone(),
                    old_text: display_change_text(original),
                    new_text: display_change_text(&parsed),
                    new_value: parsed,
                });
            }
        }

        if changes.is_empty() {
            return Ok(None);
        }
        if pk.is_empty() {
            return Err("table has no primary key".into());
        }

        Ok(Some(PendingRowEdit {
            pk,
            label: label_parts.join(", "),
            changes,
        }))
    }

    /// Apply a pending edit's new values onto this draft's editors.
    pub fn apply_pending(&mut self, pending: &PendingRowEdit) {
        for change in &pending.changes {
            if let Some((_, input, _)) = self
                .fields
                .iter_mut()
                .find(|(name, _, _)| name == &change.column)
            {
                *input = TextInput::with_text(change.new_text.clone(), true);
            }
        }
    }

    pub fn pk_values(&self, originals: &[Value]) -> Result<Vec<(String, Value)>, String> {
        let mut pk = Vec::new();
        for (index, (name, input, is_pk)) in self.fields.iter().enumerate() {
            if !*is_pk {
                continue;
            }
            let Some(original) = originals.get(index) else {
                continue;
            };
            pk.push((name.clone(), parse_draft_value(input.text(), original)?));
        }
        Ok(pk)
    }

    pub fn reset_to(&mut self, originals: &[Value]) {
        for (index, (_, input, _)) in self.fields.iter_mut().enumerate() {
            let text = originals
                .get(index)
                .map(value_editor_text)
                .unwrap_or_default();
            let multiline = input.is_multiline();
            *input = TextInput::with_text(text, multiline);
        }
        self.message = None;
    }
}

/// Text shown / edited in a detail field (pretty JSON when applicable).
///
/// Special write tokens are visible as themselves: SQL `NULL`, empty string as
/// `EMPTY`, and `DEFAULT`. A cell whose real content is one of those words is
/// shown quoted so it round-trips instead of becoming the token.
fn value_editor_text(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_string(),
        Value::Default => "DEFAULT".to_string(),
        Value::Text(s) if s.is_empty() => "EMPTY".to_string(),
        Value::Text(s) if is_special_token(s) => quote_draft_literal(s),
        other => {
            let text = other.to_text();
            if is_special_token(&text) {
                quote_draft_literal(&text)
            } else {
                json_format::display_text(&text)
            }
        }
    }
}

fn is_special_token(text: &str) -> bool {
    matches!(
        text.trim().to_ascii_uppercase().as_str(),
        "NULL" | "EMPTY" | "DEFAULT"
    )
}

fn quote_draft_literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Strip a single layer of `'…'` or `"…"` quotes, honouring backslash escapes.
fn unquote_draft_literal(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.len() < 2 {
        return None;
    }
    let quote = bytes[0];
    if (quote != b'\'' && quote != b'"') || bytes[bytes.len() - 1] != quote {
        return None;
    }
    let mut out = String::with_capacity(bytes.len() - 2);
    let mut chars = text[1..text.len() - 1].chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(escaped) => out.push(escaped),
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    Some(out)
}

pub fn upsert_pending(pending: &mut Vec<PendingRowEdit>, edit: PendingRowEdit) {
    if let Some(existing) = pending.iter_mut().find(|row| row.matches_pk(&edit.pk)) {
        *existing = edit;
    } else {
        pending.push(edit);
    }
}

pub enum WorkspaceTab {
    Table {
        id: TabId,
        /// Incremented on every load request; stale responses must not apply.
        load_seq: u64,
        table: TableRef,
        page: Page,
        /// Applied WHERE body (empty = no filter).
        where_clause: String,
        /// Draft text while the filter strip is open.
        where_draft: TextInput,
        /// Editable page size shown in the bottom bar.
        page_size_draft: TextInput,
        hidden_columns: HashSet<String>,
        result: Option<ResultView>,
        selected_row: Option<usize>,
        draft: Option<RowDraft>,
        pending_edits: Vec<PendingRowEdit>,
        change_bubble_expanded: bool,
        /// True while a transactional save for this tab is in flight.
        saving: bool,
        pane: TablePane,
        filters_open: bool,
        columns_open: bool,
    },
    Sql {
        id: TabId,
        load_seq: u64,
        editor: TextInput,
        result: Option<ResultView>,
        selected_row: Option<usize>,
        /// Editable detail editors for the selected result row.
        draft: Option<RowDraft>,
    },
}

impl WorkspaceTab {
    fn table(id: TabId, table: TableRef) -> Self {
        Self::Table {
            id,
            load_seq: 0,
            table,
            page: Page::first(),
            where_clause: String::new(),
            where_draft: TextInput::new(false),
            page_size_draft: TextInput::with_text(Page::DEFAULT_LIMIT.to_string(), false),
            hidden_columns: HashSet::new(),
            result: None,
            selected_row: None,
            draft: None,
            pending_edits: Vec::new(),
            change_bubble_expanded: false,
            saving: false,
            pane: TablePane::Data,
            filters_open: false,
            columns_open: false,
        }
    }

    fn sql(id: TabId) -> Self {
        Self::Sql {
            id,
            load_seq: 0,
            editor: TextInput::new(true),
            result: None,
            selected_row: None,
            draft: None,
        }
    }

    pub fn id(&self) -> TabId {
        match self {
            Self::Table { id, .. } | Self::Sql { id, .. } => *id,
        }
    }

    pub fn load_seq(&self) -> u64 {
        match self {
            Self::Table { load_seq, .. } | Self::Sql { load_seq, .. } => *load_seq,
        }
    }

    pub(crate) fn bump_load_seq(&mut self) -> u64 {
        match self {
            Self::Table { load_seq, .. } | Self::Sql { load_seq, .. } => {
                *load_seq = load_seq.wrapping_add(1);
                *load_seq
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::Table { table, .. } => table.name.clone(),
            Self::Sql { .. } => "SQL Query".into(),
        }
    }

    pub fn is_sql(&self) -> bool {
        matches!(self, Self::Sql { .. })
    }

    pub fn table_ref(&self) -> Option<&TableRef> {
        match self {
            Self::Table { table, .. } => Some(table),
            Self::Sql { .. } => None,
        }
    }

    pub fn result(&self) -> Option<&ResultView> {
        match self {
            Self::Table { result, .. } | Self::Sql { result, .. } => result.as_ref(),
        }
    }

    pub fn selected_row(&self) -> Option<usize> {
        match self {
            Self::Table { selected_row, .. } | Self::Sql { selected_row, .. } => *selected_row,
        }
    }
}

#[derive(Default)]
pub struct Tabs {
    pub items: Vec<WorkspaceTab>,
    pub active: usize,
    next_id: TabId,
}

impl Tabs {
    pub fn active(&self) -> Option<&WorkspaceTab> {
        self.items.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut WorkspaceTab> {
        self.items.get_mut(self.active)
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active().map(WorkspaceTab::id)
    }

    pub fn get_mut(&mut self, id: TabId) -> Option<&mut WorkspaceTab> {
        self.items.iter_mut().find(|tab| tab.id() == id)
    }

    /// Stamp a new load on the active tab. Returns `(tab_id, seq)` for the request.
    pub fn begin_active_load(&mut self) -> Option<(TabId, u64)> {
        let tab = self.active_mut()?;
        let id = tab.id();
        let seq = tab.bump_load_seq();
        Some((id, seq))
    }

    /// Whether `seq` is still the latest load for `id` (tab may have closed).
    pub fn load_is_current(&self, id: TabId, seq: u64) -> bool {
        self.items
            .iter()
            .any(|tab| tab.id() == id && tab.load_seq() == seq)
    }

    fn alloc_id(&mut self) -> TabId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id
    }

    pub fn activate(&mut self, index: usize) {
        if index < self.items.len() {
            self.active = index;
        }
    }

    /// Focus an existing tab for `table`, or push a new one. Returns the index.
    pub fn open_table(&mut self, table: TableRef) -> usize {
        if let Some(index) = self
            .items
            .iter()
            .position(|tab| tab.table_ref() == Some(&table))
        {
            self.active = index;
            return index;
        }
        let id = self.alloc_id();
        self.items.push(WorkspaceTab::table(id, table));
        self.active = self.items.len() - 1;
        self.active
    }

    /// Focus the SQL tab, creating one if needed.
    pub fn open_sql(&mut self) -> usize {
        if let Some(index) = self.items.iter().position(|tab| tab.is_sql()) {
            self.active = index;
            return index;
        }
        let id = self.alloc_id();
        self.items.push(WorkspaceTab::sql(id));
        self.active = self.items.len() - 1;
        self.active
    }

    pub fn close(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        self.items.remove(index);
        if self.items.is_empty() {
            self.active = 0;
        } else if self.active >= self.items.len() {
            self.active = self.items.len() - 1;
        } else if index < self.active {
            self.active -= 1;
        }
    }
}

/// Parse a sidebar draft string back into a [`Value`], using the original
/// cell as a type hint.
///
/// Non-normal values are typed as tokens (case-insensitive):
/// - `NULL` → SQL NULL
/// - `EMPTY` → empty string
/// - `DEFAULT` → column default (`SET col = DEFAULT`)
///
/// A value that is literally one of those words is entered quoted
/// (`"NULL"`, `'EMPTY'`).
pub fn parse_draft_value(text: &str, original: &Value) -> Result<Value, String> {
    let trimmed = text.trim();

    if let Some(inner) = unquote_draft_literal(trimmed) {
        return parse_typed_literal(&inner, original);
    }

    if trimmed.eq_ignore_ascii_case("NULL") {
        return Ok(Value::Null);
    }
    if trimmed.eq_ignore_ascii_case("DEFAULT") {
        return Ok(Value::Default);
    }
    if trimmed.eq_ignore_ascii_case("EMPTY") {
        return empty_for(original);
    }
    if trimmed.is_empty() {
        // Cleared buffer: keep NULL as NULL, otherwise treat like EMPTY for
        // text-shaped columns and ask for an explicit token elsewhere.
        return match original {
            Value::Null => Ok(Value::Null),
            Value::Text(_)
            | Value::Json(_)
            | Value::Uuid(_)
            | Value::Temporal(_)
            | Value::Decimal(_) => empty_for(original),
            _ => Err(
                "use NULL, EMPTY, or DEFAULT — or type a value (quote specials like \"NULL\")"
                    .into(),
            ),
        };
    }

    parse_typed_literal(trimmed, original)
}

fn empty_for(original: &Value) -> Result<Value, String> {
    match original {
        Value::Json(_) => Ok(Value::Json(String::new())),
        Value::Uuid(_) => Ok(Value::Uuid(String::new())),
        Value::Temporal(_) => Ok(Value::Temporal(String::new())),
        Value::Decimal(_) => Ok(Value::Decimal(String::new())),
        Value::Text(_) | Value::Null | Value::Default => Ok(Value::Text(String::new())),
        Value::Bool(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Bytes(_)
        | Value::Array(_)
        | Value::Unsupported(_) => Err("EMPTY is only valid for text-like columns".into()),
    }
}

fn parse_typed_literal(trimmed: &str, original: &Value) -> Result<Value, String> {
    match original {
        Value::Bool(_) => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" => Ok(Value::Bool(true)),
            "false" | "f" | "0" => Ok(Value::Bool(false)),
            _ => Err(format!("expected a boolean, got {trimmed:?}")),
        },
        Value::Int(_) => trimmed
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| format!("expected an integer, got {trimmed:?}")),
        Value::Float(_) => trimmed
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| format!("expected a number, got {trimmed:?}")),
        Value::Decimal(_) => Ok(Value::Decimal(trimmed.to_string())),
        Value::Null | Value::Default => {
            if let Ok(i) = trimmed.parse::<i64>() {
                Ok(Value::Int(i))
            } else {
                Ok(Value::Text(trimmed.to_string()))
            }
        }
        Value::Text(_) | Value::Uuid(_) | Value::Temporal(_) => {
            Ok(Value::Text(trimmed.to_string()))
        }
        Value::Json(_) => Ok(Value::Json(trimmed.to_string())),
        Value::Bytes(_) | Value::Array(_) | Value::Unsupported(_) => Err(
            "this column type cannot be edited from the sidebar yet".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_table_assigns_stable_ids() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "a"));
        tabs.open_table(TableRef::new("public", "b"));
        assert_eq!(tabs.items[0].id(), 0);
        assert_eq!(tabs.items[1].id(), 1);
        assert_eq!(tabs.active_id(), Some(1));
    }

    #[test]
    fn begin_load_invalidates_prior_seq() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "a"));
        let (id, seq1) = tabs.begin_active_load().unwrap();
        assert!(tabs.load_is_current(id, seq1));
        let (id2, seq2) = tabs.begin_active_load().unwrap();
        assert_eq!(id, id2);
        assert!(!tabs.load_is_current(id, seq1));
        assert!(tabs.load_is_current(id, seq2));
    }

    #[test]
    fn get_mut_finds_inactive_tab() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "a"));
        let a = tabs.active_id().unwrap();
        tabs.open_table(TableRef::new("public", "b"));
        assert_ne!(tabs.active_id(), Some(a));
        assert!(matches!(
            tabs.get_mut(a),
            Some(WorkspaceTab::Table { .. })
        ));
    }

    #[test]
    fn draft_tokens_cover_null_empty_and_default() {
        assert_eq!(
            parse_draft_value("NULL", &Value::Text("x".into())).unwrap(),
            Value::Null
        );
        assert_eq!(
            parse_draft_value("empty", &Value::Text("x".into())).unwrap(),
            Value::Text(String::new())
        );
        assert_eq!(
            parse_draft_value("DEFAULT", &Value::Int(1)).unwrap(),
            Value::Default
        );
        assert_eq!(
            parse_draft_value("\"NULL\"", &Value::Text("x".into())).unwrap(),
            Value::Text("NULL".into())
        );
        assert!(parse_draft_value("EMPTY", &Value::Int(1)).is_err());
    }

    #[test]
    fn editor_text_makes_specials_visible_and_round_trips() {
        assert_eq!(value_editor_text(&Value::Null), "NULL");
        assert_eq!(value_editor_text(&Value::Text(String::new())), "EMPTY");
        assert_eq!(value_editor_text(&Value::Text("NULL".into())), "\"NULL\"");
        assert_eq!(
            parse_draft_value(
                &value_editor_text(&Value::Text("EMPTY".into())),
                &Value::Text("x".into())
            )
            .unwrap(),
            Value::Text("EMPTY".into())
        );
    }

    #[test]
    fn empty_buffer_is_not_dirty_when_cell_was_already_empty() {
        let draft = RowDraft {
            row_index: 0,
            fields: vec![(
                "name".into(),
                TextInput::with_text("", true),
                false,
            )],
            message: None,
            field_search: TextInput::new(false),
        };
        assert!(!draft.is_dirty(&[Value::Text(String::new())]));
        assert!(!draft.is_dirty(&[Value::Null]));
    }

    #[test]
    fn pretty_json_is_not_a_pending_change() {
        let compact = Value::Json(r#"{"Hello":"World"}"#.into());
        let pretty = value_editor_text(&compact);
        assert!(pretty.contains('\n'), "editor should pretty-print JSON");
        let draft = RowDraft {
            row_index: 0,
            fields: vec![("feature_flags".into(), TextInput::with_text(pretty, true), false)],
            message: None,
            field_search: TextInput::new(false),
        };
        assert!(!draft.is_dirty(&[compact.clone()]));
        assert!(draft.to_pending(&[compact]).unwrap().is_none());
    }

    #[test]
    fn discard_style_reset_leaves_json_clean() {
        let compact = Value::Json(r#"{"Hello":"World"}"#.into());
        let mut draft = RowDraft {
            row_index: 0,
            fields: vec![(
                "feature_flags".into(),
                TextInput::with_text(r#"{"Hello":"Changed"}"#, true),
                false,
            )],
            message: None,
            field_search: TextInput::new(false),
        };
        assert!(draft.is_dirty(&[compact.clone()]));
        draft.reset_to(&[compact.clone()]);
        assert!(!draft.is_dirty(&[compact]));
    }
}
