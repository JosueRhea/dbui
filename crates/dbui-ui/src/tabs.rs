//! Workspace tabs: each open table or SQL query is one tab.

use crate::json_format;
use crate::root::ResultView;
use crate::text_input::TextInput;
use dbui_app::domain::{Column, ColumnInfo, Page, TableRef, Value};
use dbui_app::SavedTab;
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
    /// The stored value, normalized the same way as `new_text` so that the
    /// diff shows the change and not the difference in layout between them.
    pub old_text: String,
    /// The pending value, normalized alongside `old_text`.
    pub new_text: String,
    /// The editor buffer verbatim. Reopening the row restores this, so leaving
    /// a row and coming back does not hand the user back a reformatted copy of
    /// what they typed.
    pub edited_text: String,
    /// Exactly what the UPDATE will bind.
    pub new_value: Value,
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

/// How a value appears in the change bubble.
///
/// Both sides of a diff go through this, so a compact stored blob and the
/// compact value replacing it are compared expanded — the diff then reports
/// the key that moved rather than "the whole line changed".
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
                    edited_text: input.text().to_string(),
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
                *input = TextInput::with_text(change.edited_text.clone(), true);
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
                json_format::editor_text(&text)
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

    /// The part of this tab worth writing to disk.
    ///
    /// Rows are left out on purpose: they are what the server held at the time,
    /// and restoring them would show stale data under a live heading. What is
    /// kept is the question — which table, which filter, which columns were
    /// hidden, what SQL was typed — and asking it again is a page load.
    pub fn to_saved(&self) -> SavedTab {
        match self {
            Self::Table {
                table,
                where_clause,
                hidden_columns,
                ..
            } => {
                // Sorted so an unordered set does not rewrite the file with
                // the same tabs in a different order on every save.
                let mut hidden: Vec<String> = hidden_columns.iter().cloned().collect();
                hidden.sort();
                SavedTab::Table {
                    schema: table.schema.clone(),
                    name: table.name.clone(),
                    where_clause: where_clause.clone(),
                    hidden_columns: hidden,
                }
            }
            Self::Sql { editor, .. } => SavedTab::Sql {
                text: editor.text().to_string(),
            },
        }
    }

    fn from_saved(id: TabId, saved: &SavedTab) -> Self {
        match saved {
            SavedTab::Table {
                schema,
                name,
                where_clause,
                hidden_columns,
            } => {
                let mut tab = Self::table(id, TableRef::new(schema, name));
                if let Self::Table {
                    where_clause: clause,
                    where_draft,
                    hidden_columns: hidden,
                    ..
                } = &mut tab
                {
                    clause.clone_from(where_clause);
                    // The strip opens showing the filter that is applied, not
                    // an empty box over filtered rows.
                    *where_draft = TextInput::with_text(where_clause.clone(), false);
                    *hidden = hidden_columns.iter().cloned().collect();
                }
                tab
            }
            SavedTab::Sql { text } => {
                let mut tab = Self::sql(id);
                if let Self::Sql { editor, .. } = &mut tab {
                    *editor = TextInput::with_text(text.clone(), true);
                }
                tab
            }
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

    /// This tab set as it survives a restart.
    pub fn to_saved(&self) -> (Vec<SavedTab>, usize) {
        (
            self.items.iter().map(WorkspaceTab::to_saved).collect(),
            self.active,
        )
    }

    /// Rebuild a tab set from disk. Every tab starts with no rows: the active
    /// one loads when the connection it belongs to is in front and connected.
    pub fn from_saved(saved: &[SavedTab], active: usize) -> Self {
        let mut tabs = Self::default();
        for entry in saved {
            let id = tabs.alloc_id();
            tabs.items.push(WorkspaceTab::from_saved(id, entry));
        }
        tabs.active = active.min(tabs.items.len().saturating_sub(1));
        tabs
    }

    /// Drop every result, keeping the tabs themselves.
    ///
    /// Disconnecting invalidates the rows but not the arrangement: what was
    /// open is still what the user wants open when the connection comes back.
    pub fn clear_results(&mut self) {
        for tab in &mut self.items {
            match tab {
                WorkspaceTab::Table {
                    result,
                    selected_row,
                    draft,
                    ..
                }
                | WorkspaceTab::Sql {
                    result,
                    selected_row,
                    draft,
                    ..
                } => {
                    *result = None;
                    *selected_row = None;
                    *draft = None;
                }
            }
        }
    }

    /// Whether the tab in front is showing rows already.
    ///
    /// Restored tabs have none, which is what tells the UI to load on the way
    /// in rather than reloading a tab the user merely stepped away from.
    pub fn active_needs_load(&self) -> bool {
        matches!(self.active(), Some(tab) if tab.result().is_none() && !tab.is_sql())
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
        // A `text` column is included on purpose: one holding JSON is expanded
        // for editing like any other, so it has to be put back the same way.
        Value::Text(_) => Ok(Value::Text(json_format::write_text(
            &original.to_text(),
            trimmed,
        ))),
        Value::Uuid(_) | Value::Temporal(_) => Ok(Value::Text(trimmed.to_string())),
        Value::Json(_) => Ok(Value::Json(json_format::write_text(
            &original.to_text(),
            trimmed,
        ))),
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

    // -- surviving a restart -------------------------------------------------

    /// What the user arranged comes back: the tables, the SQL text, which tab
    /// was in front. What the server said does not -- that is reloaded.
    #[test]
    fn a_tab_set_survives_a_save_and_reload() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "users"));
        tabs.open_table(TableRef::new("shop", "orders"));
        tabs.open_sql();
        if let Some(WorkspaceTab::Sql { editor, .. }) = tabs.active_mut() {
            *editor = TextInput::with_text("select 1", true);
        }
        tabs.activate(1);

        let (saved, active) = tabs.to_saved();
        let restored = Tabs::from_saved(&saved, active);

        assert_eq!(restored.items.len(), 3);
        assert_eq!(restored.active, 1);
        assert_eq!(
            restored.items[1].table_ref(),
            Some(&TableRef::new("shop", "orders"))
        );
        assert!(matches!(
            &restored.items[2],
            WorkspaceTab::Sql { editor, .. } if editor.text() == "select 1"
        ));
    }

    /// A filter is part of the question the tab is asking, so it has to come
    /// back applied -- and visible in the strip, not an empty box over
    /// filtered rows.
    #[test]
    fn a_filtered_table_tab_comes_back_filtered() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "users"));
        if let Some(WorkspaceTab::Table {
            where_clause,
            hidden_columns,
            ..
        }) = tabs.active_mut()
        {
            *where_clause = "id > 10".into();
            hidden_columns.insert("secret".into());
        }

        let (saved, active) = tabs.to_saved();
        let restored = Tabs::from_saved(&saved, active);

        let WorkspaceTab::Table {
            where_clause,
            where_draft,
            hidden_columns,
            ..
        } = &restored.items[0]
        else {
            panic!("expected a table tab");
        };
        assert_eq!(where_clause, "id > 10");
        assert_eq!(where_draft.text(), "id > 10");
        assert!(hidden_columns.contains("secret"));
    }

    /// Ids are minted fresh on restore, so an in-flight load keyed off an id
    /// from the previous run cannot land on a restored tab.
    #[test]
    fn restored_tabs_get_fresh_ids_that_keep_allocating() {
        let restored = Tabs::from_saved(
            &[
                SavedTab::Sql {
                    text: String::new(),
                },
                SavedTab::Table {
                    schema: "public".into(),
                    name: "users".into(),
                    where_clause: String::new(),
                    hidden_columns: Vec::new(),
                },
            ],
            0,
        );
        assert_eq!(restored.items[0].id(), 0);
        assert_eq!(restored.items[1].id(), 1);

        let mut restored = restored;
        restored.open_table(TableRef::new("public", "orders"));
        assert_eq!(restored.items[2].id(), 2, "ids must not collide");
    }

    #[test]
    fn an_empty_save_restores_to_an_empty_tab_set() {
        let restored = Tabs::from_saved(&[], 4);
        assert!(restored.items.is_empty());
        assert_eq!(restored.active, 0);
        assert!(!restored.active_needs_load());
    }

    /// Disconnecting invalidates the rows but not the arrangement.
    #[test]
    fn clearing_results_keeps_the_tabs() {
        let mut tabs = Tabs::default();
        tabs.open_table(TableRef::new("public", "users"));
        assert!(tabs.active_needs_load(), "a tab with no rows wants a load");

        if let Some(WorkspaceTab::Table { selected_row, .. }) = tabs.active_mut() {
            *selected_row = Some(3);
        }
        tabs.clear_results();

        assert_eq!(tabs.items.len(), 1);
        assert_eq!(tabs.items[0].selected_row(), None);
        assert!(tabs.active_needs_load());
    }

    /// A SQL tab has nothing to reload -- it holds a query the user has not
    /// necessarily run, and running it on restore would be the app deciding to
    /// execute something on its own.
    #[test]
    fn a_restored_sql_tab_is_never_loaded_on_the_way_in() {
        let tabs = Tabs::from_saved(
            &[SavedTab::Sql {
                text: "delete from users".into(),
            }],
            0,
        );
        assert!(!tabs.active_needs_load());
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

    /// Build a draft over an `id` primary key plus one editable column.
    fn draft_with(column: &str, buffer: &str) -> RowDraft {
        RowDraft {
            row_index: 0,
            fields: vec![
                ("id".into(), TextInput::with_text("1", false), true),
                (column.into(), TextInput::with_text(buffer, true), false),
            ],
            message: None,
            field_search: TextInput::new(false),
        }
    }

    #[test]
    fn editing_one_key_of_a_compact_json_column_writes_compact_json() {
        let stored = Value::Json(r#"{"beta":2,"alpha":1}"#.into());
        let buffer = value_editor_text(&stored).replace("\"beta\": 2", "\"beta\": 3");
        let draft = draft_with("flags", &buffer);

        let pending = draft
            .to_pending(&[Value::Int(1), stored])
            .unwrap()
            .expect("one change");
        let change = &pending.changes[0];

        assert_eq!(
            change.new_value,
            Value::Json(r#"{"beta":3,"alpha":1}"#.into()),
            "the write must keep the stored layout and key order"
        );
        assert!(
            change.old_text.contains('\n') && change.new_text.contains('\n'),
            "both diff sides are expanded, so the diff shows one key and not one line"
        );
    }

    /// A `text` column holding JSON is expanded for editing like a `json` one,
    /// so it has to be put back compact too -- `text` keeps whitespace verbatim.
    #[test]
    fn a_text_column_holding_json_also_keeps_its_layout() {
        let stored = Value::Text(r#"{"on":false}"#.into());
        let buffer = value_editor_text(&stored).replace("false", "true");
        let draft = draft_with("settings", &buffer);

        let pending = draft
            .to_pending(&[Value::Int(1), stored])
            .unwrap()
            .expect("one change");
        assert_eq!(
            pending.changes[0].new_value,
            Value::Text(r#"{"on":true}"#.into())
        );
    }

    #[test]
    fn plain_text_is_not_touched_by_the_json_layout_rules() {
        let stored = Value::Text("hello".into());
        let draft = draft_with("note", "hello there");

        let pending = draft
            .to_pending(&[Value::Int(1), stored])
            .unwrap()
            .expect("one change");
        assert_eq!(
            pending.changes[0].new_value,
            Value::Text("hello there".into())
        );
    }

    /// Leaving a row and coming back must return the buffer as typed. It used
    /// to restore a re-serialized copy, silently reformatting the user's JSON.
    #[test]
    fn revisiting_a_row_restores_the_text_that_was_typed() {
        let stored = Value::Json(r#"{"a":1}"#.into());
        let typed = r#"{"a":2}"#;
        let pending = draft_with("blob", typed)
            .to_pending(&[Value::Int(1), stored.clone()])
            .unwrap()
            .expect("one change");

        let mut reopened = draft_with("blob", &value_editor_text(&stored));
        reopened.apply_pending(&pending);
        assert_eq!(reopened.fields[1].1.text(), typed);
    }

    /// A settings blob of the shape these columns really hold: mixed key
    /// styles, a nested object, and an array of objects.
    const SETTINGS: &str = r#"{
  "currency": "GTQ",
  "ghost_mode": false,
  "clean_chat_after": 180,
  "ordering-flow-id": "1558815002402718",
  "pickup_auto_report": {
    "wa": true,
    "app": true,
    "web": true,
    "call": true
  },
  "catalog_meal_schedules": {
    "desayuno": [
      {
        "fin": "10:50",
        "Sunday": true,
        "inicio": "07:00"
      },
      {
        "fin": "10:50",
        "inicio": "07:45",
        "Thursday": true
      }
    ]
  },
  "minimum_purchase_on_delivery": 70,
  "teta-for-scheduled-orders-pickup": 0
}"#;

    /// The whole point of the layout work, measured end to end on a document
    /// big enough for a whole-blob rewrite to be obvious.
    ///
    /// A `jsonb` column arrives compact (`JsonValue::to_string`), is expanded
    /// to edit, and must go back changed only where the user typed -- both in
    /// the bytes bound to the UPDATE and in what the change bubble draws.
    fn assert_one_key_edit(find: &str, replace: &str, expect_diff_rows: usize) {
        let parsed: serde_json::Value = serde_json::from_str(SETTINGS).unwrap();
        let stored = Value::Json(serde_json::to_string(&parsed).unwrap());

        let seed = value_editor_text(&stored);
        assert!(seed.contains('\n'), "a compact column expands to edit");
        assert!(seed.contains(find), "`{find}` should be in the buffer");
        let buffer = seed.replacen(find, replace, 1);

        let draft = draft_with("settings", &buffer);
        let pending = draft
            .to_pending(&[Value::Int(1), stored.clone()])
            .unwrap()
            .expect("one change");
        let change = &pending.changes[0];

        // The bubble diffs the two normalized sides, and must report the edit
        // rather than all ~28 lines of the document.
        let rows = crate::text_diff::line_diff(&change.old_text, &change.new_text)
            .expect("small enough to diff");
        assert_eq!(
            rows.len(),
            expect_diff_rows,
            "expected a {expect_diff_rows}-row diff, got {rows:#?}"
        );

        // The write stays compact, and differs from the stored bytes only in
        // the region the edit touched.
        let written = change.new_value.to_text();
        assert!(!written.contains('\n'), "a compact column is written compact");

        let old = stored.to_text();
        let prefix = old
            .as_bytes()
            .iter()
            .zip(written.as_bytes())
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = old.as_bytes()[prefix..]
            .iter()
            .rev()
            .zip(written.as_bytes()[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let rewritten = old.len() - prefix - suffix;
        assert!(
            rewritten < 64,
            "only the edited span may be rewritten, but {rewritten} bytes of \
             {} changed: {:?}",
            old.len(),
            &old[prefix..old.len() - suffix]
        );
    }

    #[test]
    fn editing_a_number_deep_in_a_settings_blob_rewrites_only_that_number() {
        assert_one_key_edit("\"clean_chat_after\": 180", "\"clean_chat_after\": 240", 2);
    }

    #[test]
    fn editing_a_value_inside_a_nested_array_rewrites_only_that_value() {
        assert_one_key_edit(
            "\"fin\": \"10:50\",\n        \"inicio\": \"07:45\"",
            "\"fin\": \"11:50\",\n        \"inicio\": \"07:45\"",
            2,
        );
    }

    /// A run of changed lines groups removals above additions, the way a
    /// unified diff does -- not one interleaved pair per line.
    #[test]
    fn flipping_four_flags_in_a_nested_object_shows_four_of_each() {
        assert_one_key_edit(
            "\"wa\": true,\n    \"app\": true,\n    \"web\": true,\n    \"call\": true",
            "\"wa\": false,\n    \"app\": false,\n    \"web\": false,\n    \"call\": false",
            8,
        );
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
