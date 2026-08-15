//! Workspace tabs: each open table or SQL query is one tab.

use crate::json_format;
use crate::root::ResultView;
use crate::text_input::TextInput;
use dbui_app::domain::{Column, ColumnInfo, Page, SortKey, TableRef, Value};
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
        pk_equal(&self.pk, pk)
    }
}

/// A row staged for removal, waiting for the same commit as the edits.
///
/// Deleting is staged rather than immediate for the same reason editing is:
/// the person doing it gets to see the whole list, and to change their mind,
/// before anything reaches the server.
#[derive(Clone)]
pub struct PendingRowDelete {
    pub pk: Vec<(String, Value)>,
    pub label: String,
}

impl PendingRowDelete {
    pub fn matches_pk(&self, pk: &[(String, Value)]) -> bool {
        pk_equal(&self.pk, pk)
    }
}

fn pk_equal(left: &[(String, Value)], right: &[(String, Value)]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.iter())
            .all(|(a, b)| a.0 == b.0 && values_equal(&a.1, &b.1))
}

/// The primary key of one result row, as `(column, value)` pairs.
///
/// `Err` when the rows came from somewhere with no key to name them by -- a
/// join, a view, a table declared without one. A row this cannot identify is a
/// row the app must refuse to delete rather than match on its other columns.
pub fn row_pk(
    columns: &[ColumnInfo],
    values: &[Value],
    structure: &[Column],
) -> Result<Vec<(String, Value)>, String> {
    let mut pk = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        let is_key = structure
            .iter()
            .any(|meta| meta.name == column.name && meta.is_primary_key);
        if !is_key {
            continue;
        }
        if let Some(value) = values.get(index) {
            pk.push((column.name.clone(), value.clone()));
        }
    }
    if pk.is_empty() {
        return Err("table has no primary key".into());
    }
    Ok(pk)
}

/// `id=7, tenant=4` -- how a staged row is named in the change bubble.
pub fn pk_label(pk: &[(String, Value)]) -> String {
    pk.iter()
        .map(|(name, value)| format!("{name}={}", display_change_text(value)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A new row staged for insertion.
///
/// It keeps live editors rather than finished values because, unlike an edit,
/// there is no stored row underneath to fall back on: the buffer *is* the row
/// until it is committed. A field left untouched is written as [`Value::Default`]
/// -- absent from the statement, so the column's own default or sequence fires.
pub struct PendingRowInsert {
    /// `(column name, editor, an empty value of the column's own type)`.
    ///
    /// The prototype is what a typed literal is parsed against. An edit can
    /// read the type off the value already in the cell; a new row has no such
    /// value, and sending everything as text is what makes Postgres refuse an
    /// INSERT against a `bigint` column.
    pub fields: Vec<(String, TextInput, Value)>,
}

impl PendingRowInsert {
    /// A blank row shaped like the result set, every field reading `DEFAULT`.
    pub fn blank(columns: &[ColumnInfo], structure: &[Column]) -> Self {
        Self {
            fields: columns
                .iter()
                .map(|column| {
                    let declared = structure
                        .iter()
                        .find(|meta| meta.name == column.name)
                        .map(|meta| meta.data_type.as_str())
                        // A query result carries the wire type rather than the
                        // declared one, which is close enough to widen by.
                        .unwrap_or(column.type_name.as_str());
                    (
                        column.name.clone(),
                        TextInput::with_text(DEFAULT_TOKEN, true),
                        Value::prototype_for(declared),
                    )
                })
                .collect(),
        }
    }

    /// The columns to actually write.
    ///
    /// Fields still reading `DEFAULT` are dropped rather than sent: naming a
    /// column at all overrides its default, so an untouched `id` would insert
    /// a literal DEFAULT where the sequence should have run.
    pub fn to_values(&self) -> Result<Vec<(String, Value)>, String> {
        let mut values = Vec::new();
        for (name, input, prototype) in &self.fields {
            let text = input.text().trim();
            if text.eq_ignore_ascii_case(DEFAULT_TOKEN) {
                continue;
            }
            let parsed = parse_draft_value(input.text(), prototype)?;
            values.push((name.clone(), parsed));
        }
        Ok(values)
    }

    /// How the change bubble names it: the first few filled-in columns.
    pub fn label(&self) -> String {
        let filled: Vec<String> = self
            .fields
            .iter()
            .filter(|(_, input, _)| !input.text().trim().eq_ignore_ascii_case(DEFAULT_TOKEN))
            .take(3)
            .map(|(name, input, _)| format!("{name}={}", one_line_value(input.text())))
            .collect();
        if filled.is_empty() {
            "all defaults".to_string()
        } else {
            filled.join(", ")
        }
    }
}

pub const DEFAULT_TOKEN: &str = "DEFAULT";

fn one_line_value(text: &str) -> String {
    let flat: String = text.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    if flat.chars().count() > 24 {
        format!("{}…", flat.chars().take(24).collect::<String>())
    } else {
        flat
    }
}

/// Which result rows are selected, and where a range grows from.
///
/// The anchor is the last row picked deliberately, which is not the same as
/// the last row a range happened to touch: dragging back and forth has to keep
/// measuring from where the drag started.
#[derive(Default, Clone)]
pub struct RowSelection {
    rows: HashSet<usize>,
    anchor: Option<usize>,
}

impl RowSelection {
    pub fn contains(&self, row: usize) -> bool {
        self.rows.contains(&row)
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Selected rows, low to high -- the order they are on screen in.
    pub fn ordered(&self) -> Vec<usize> {
        let mut rows: Vec<usize> = self.rows.iter().copied().collect();
        rows.sort_unstable();
        rows
    }

    /// A plain click: this row and nothing else.
    pub fn set_single(&mut self, row: usize) {
        self.rows.clear();
        self.rows.insert(row);
        self.anchor = Some(row);
    }

    /// ⌘-click: add or remove one row without disturbing the rest.
    pub fn toggle(&mut self, row: usize) {
        if !self.rows.remove(&row) {
            self.rows.insert(row);
        }
        self.anchor = Some(row);
    }

    /// Shift-click or drag: replace the selection with anchor..=row.
    ///
    /// The anchor stays put so dragging back the other way shrinks the range
    /// instead of dragging the far end along with the pointer.
    pub fn extend_to(&mut self, row: usize) {
        let anchor = self.anchor.unwrap_or(row);
        self.rows.clear();
        let (low, high) = if anchor <= row {
            (anchor, row)
        } else {
            (row, anchor)
        };
        self.rows.extend(low..=high);
        self.anchor = Some(anchor);
    }

    pub fn select_all(&mut self, row_count: usize) {
        self.rows = (0..row_count).collect();
        if self.anchor.is_none() {
            self.anchor = Some(0);
        }
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.anchor = None;
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

/// The token a field shows when the selected rows do not agree on its value.
///
/// It belongs to the same vocabulary as `NULL`, `EMPTY` and `DEFAULT`: the box
/// says exactly what will be written. Leaving `MIXED` alone leaves every row's
/// own value alone; replacing it writes the replacement to all of them. A cell
/// whose real content is the word is shown quoted, like the other tokens.
pub const MIXED: &str = "MIXED";

/// Editable draft of the selected row -- or of every selected row at once.
///
/// Bulk editing is this same object with more than one index in `rows`. A
/// column the rows agree on shows that value; one they disagree on shows
/// [`MIXED`]. Keeping it one type is what stops "edit a row" and "edit a
/// selection" from becoming two code paths that stage changes differently --
/// the second of which would be the one nobody tested.
pub struct RowDraft {
    /// Rows this draft edits, in grid order.
    pub rows: Vec<usize>,
    /// `(column name, editor, is_primary_key)`
    pub fields: Vec<(String, TextInput, bool)>,
    pub message: Option<(bool, String)>,
    pub field_search: TextInput,
}

/// One selected row, paired with the edit already staged for it.
///
/// Resolved once per row rather than once per cell: finding the pending edit
/// means a primary-key comparison, and doing that for every cell on screen is
/// the same answer computed a few thousand times.
type StagedRow<'a> = (&'a [Value], Option<&'a PendingRowEdit>);

impl RowDraft {
    /// Build editors over `rows`, showing what those rows currently say --
    /// including edits already staged for them, so leaving a row and coming
    /// back does not look like the edit was thrown away.
    pub fn from_rows(rows: &[usize], view: &ResultView, staged: &[PendingRowEdit]) -> Self {
        let rows: Vec<usize> = rows
            .iter()
            .copied()
            .filter(|row| *row < view.set.rows.len())
            .collect();
        let resolved = resolve_staged(&rows, view, staged);

        let fields = view
            .set
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let is_pk = view
                    .structure
                    .iter()
                    .any(|meta| meta.name == column.name && meta.is_primary_key);
                let text = agreed_text(&resolved, index, &column.name);
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
            rows,
            fields,
            message: None,
            field_search: TextInput::new(false),
        }
    }

    /// Whether this draft is speaking for more than one row.
    pub fn is_bulk(&self) -> bool {
        self.rows.len() > 1
    }

    /// The primary key of every row this draft covers.
    ///
    /// What the caller uses to clear out this draft's rows before restaging
    /// them: [`to_pending_batch`] returns the whole intent for each row, so
    /// merging into what is already there would double-count.
    ///
    /// [`to_pending_batch`]: RowDraft::to_pending_batch
    pub fn row_keys(&self, view: &ResultView) -> Vec<Vec<(String, Value)>> {
        self.rows
            .iter()
            .filter_map(|&row| view.set.rows.get(row))
            .filter_map(|values| row_pk(&view.set.columns, &values.0, &view.structure).ok())
            .collect()
    }

    /// One pending edit per covered row that would actually change.
    ///
    /// Two rules do the work of bulk editing. A row whose stored value already
    /// matches what was typed produces nothing, so setting a column across a
    /// selection writes only the rows that differ. And a column left [`MIXED`]
    /// keeps whatever was staged for it before -- the draft never showed those
    /// values, so it has no opinion to overwrite them with.
    pub fn to_pending_batch(
        &self,
        view: &ResultView,
        staged: &[PendingRowEdit],
    ) -> Result<Vec<PendingRowEdit>, String> {
        let mut edits = Vec::new();

        for &row in &self.rows {
            let Some(values) = view.set.rows.get(row) else {
                continue;
            };
            let pk = row_pk(&view.set.columns, &values.0, &view.structure).ok();
            let existing = pk
                .as_ref()
                .and_then(|pk| staged.iter().find(|edit| edit.matches_pk(pk)));

            let mut changes = Vec::new();
            for (index, (name, input, is_pk)) in self.fields.iter().enumerate() {
                if *is_pk {
                    continue;
                }
                if is_mixed(input.text()) {
                    if let Some(change) = existing
                        .and_then(|edit| edit.changes.iter().find(|change| &change.column == name))
                    {
                        changes.push(change.clone());
                    }
                    continue;
                }
                let Some(original) = values.0.get(index) else {
                    continue;
                };
                // Untouched fields are skipped before they are parsed, not
                // after. A column this build has no decoder for round-trips as
                // text but does not parse back, and reporting that as an error
                // every time a row is selected would be a message about
                // nothing the user did.
                if input.text() == value_editor_text(original) {
                    continue;
                }
                let parsed = parse_draft_value(input.text(), original)?;
                if values_equal(&parsed, original) {
                    continue;
                }
                changes.push(FieldChange {
                    column: name.clone(),
                    old_text: display_change_text(original),
                    new_text: display_change_text(&parsed),
                    edited_text: input.text().to_string(),
                    new_value: parsed,
                });
            }

            if changes.is_empty() {
                continue;
            }
            // Checked here rather than up front: a selection over a result with
            // no key is only a problem once there is something to write.
            let Some(pk) = pk else {
                return Err("table has no primary key".into());
            };
            edits.push(PendingRowEdit {
                label: pk_label(&pk),
                pk,
                changes,
            });
        }

        Ok(edits)
    }

    /// Put every field back to what the rows actually hold, dropping both what
    /// was typed and anything staged.
    pub fn reset(&mut self, view: &ResultView) {
        let resolved = resolve_staged(&self.rows, view, &[]);
        for (index, (name, input, _)) in self.fields.iter_mut().enumerate() {
            let multiline = input.is_multiline();
            *input = TextInput::with_text(agreed_text(&resolved, index, name), multiline);
        }
        self.message = None;
    }
}

/// Whether a field is saying "the rows differ" rather than naming a value.
fn is_mixed(text: &str) -> bool {
    text.trim() == MIXED
}

fn resolve_staged<'a>(
    rows: &[usize],
    view: &'a ResultView,
    staged: &'a [PendingRowEdit],
) -> Vec<StagedRow<'a>> {
    rows.iter()
        .filter_map(|&row| view.set.rows.get(row))
        .map(|values| {
            let edit = if staged.is_empty() {
                None
            } else {
                row_pk(&view.set.columns, &values.0, &view.structure)
                    .ok()
                    .and_then(|pk| staged.iter().find(|edit| edit.matches_pk(&pk)))
            };
            (values.0.as_slice(), edit)
        })
        .collect()
}

/// What one column reads across the selection: the text every row shares, or
/// [`MIXED`] as soon as two of them disagree.
///
/// Returns on the first disagreement, which is what keeps selecting a few
/// hundred rows from rendering every cell of every one of them.
fn agreed_text(staged: &[StagedRow<'_>], index: usize, name: &str) -> String {
    let mut shared: Option<String> = None;
    for (values, edit) in staged {
        let text = match edit
            .and_then(|edit| edit.changes.iter().find(|change| change.column == name))
        {
            // The buffer verbatim, not the value re-rendered: coming back to a
            // row must not hand back a reformatted copy of what was typed.
            Some(change) => change.edited_text.clone(),
            None => values.get(index).map(value_editor_text).unwrap_or_default(),
        };
        match &shared {
            None => shared = Some(text),
            Some(seen) if *seen == text => {}
            Some(_) => return MIXED.to_string(),
        }
    }
    shared.unwrap_or_default()
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
        "NULL" | "EMPTY" | "DEFAULT" | MIXED
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


pub enum WorkspaceTab {
    Table {
        id: TabId,
        /// Incremented on every load request; stale responses must not apply.
        load_seq: u64,
        table: TableRef,
        page: Page,
        /// Applied WHERE body (empty = no filter).
        where_clause: String,
        /// The column the user sorted by, if any. The primary key trails it
        /// so paging stays stable -- see `dbui_domain::order_for`.
        sort: Option<SortKey>,
        /// Draft text while the filter strip is open.
        where_draft: TextInput,
        /// Editable page size shown in the bottom bar.
        page_size_draft: TextInput,
        hidden_columns: HashSet<String>,
        result: Option<ResultView>,
        selected_row: Option<usize>,
        /// Every selected row. `selected_row` is the one whose detail is open;
        /// this is what ⌘A, shift-click and drag build, and what ⌘⌫ acts on.
        selection: RowSelection,
        draft: Option<RowDraft>,
        pending_edits: Vec<PendingRowEdit>,
        pending_deletes: Vec<PendingRowDelete>,
        /// New rows staged for insertion, drawn under the real ones.
        pending_inserts: Vec<PendingRowInsert>,
        /// Which staged insert the detail sidebar is editing, if any.
        editing_insert: Option<usize>,
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
        selection: RowSelection,
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
            sort: None,
            where_draft: TextInput::new(false),
            page_size_draft: TextInput::with_text(Page::DEFAULT_LIMIT.to_string(), false),
            hidden_columns: HashSet::new(),
            result: None,
            selected_row: None,
            selection: RowSelection::default(),
            draft: None,
            pending_edits: Vec::new(),
            pending_deletes: Vec::new(),
            pending_inserts: Vec::new(),
            editing_insert: None,
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
            selection: RowSelection::default(),
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

    pub fn selection(&self) -> &RowSelection {
        match self {
            Self::Table { selection, .. } | Self::Sql { selection, .. } => selection,
        }
    }

    pub fn selection_mut(&mut self) -> &mut RowSelection {
        match self {
            Self::Table { selection, .. } | Self::Sql { selection, .. } => selection,
        }
    }

    /// New rows staged for insertion.
    pub fn pending_inserts(&self) -> &[PendingRowInsert] {
        match self {
            Self::Table {
                pending_inserts, ..
            } => pending_inserts,
            Self::Sql { .. } => &[],
        }
    }

    /// Which staged insert the sidebar is editing.
    pub fn editing_insert(&self) -> Option<usize> {
        match self {
            Self::Table {
                editing_insert, ..
            } => *editing_insert,
            Self::Sql { .. } => None,
        }
    }

    /// Rows staged for deletion. Only a table tab can have any: a query result
    /// has no table to delete from.
    pub fn pending_deletes(&self) -> &[PendingRowDelete] {
        match self {
            Self::Table {
                pending_deletes, ..
            } => pending_deletes,
            Self::Sql { .. } => &[],
        }
    }

    /// Whether the row at `index` is struck through as staged for deletion.
    pub fn row_is_staged_for_delete(&self, index: usize) -> bool {
        let Self::Table {
            pending_deletes,
            result: Some(view),
            ..
        } = self
        else {
            return false;
        };
        if pending_deletes.is_empty() {
            return false;
        }
        let Some(values) = view.set.rows.get(index) else {
            return false;
        };
        match row_pk(&view.set.columns, &values.0, &view.structure) {
            Ok(pk) => pending_deletes.iter().any(|row| row.matches_pk(&pk)),
            Err(_) => false,
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
                sort,
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
                    sort: sort.clone(),
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
                sort,
            } => {
                let mut tab = Self::table(id, TableRef::new(schema, name));
                if let Self::Table {
                    where_clause: clause,
                    where_draft,
                    hidden_columns: hidden,
                    sort: tab_sort,
                    ..
                } = &mut tab
                {
                    clause.clone_from(where_clause);
                    tab_sort.clone_from(sort);
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
                    selection,
                    draft,
                    ..
                }
                | WorkspaceTab::Sql {
                    result,
                    selected_row,
                    selection,
                    draft,
                    ..
                } => {
                    *result = None;
                    *selected_row = None;
                    selection.clear();
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

    // Callers skip a field still reading MIXED, so reaching here with one is a
    // bug -- and writing the literal word to every selected row is the worst
    // possible way for it to show up.
    if trimmed.eq_ignore_ascii_case(MIXED) {
        return Err("MIXED means the selected rows differ — type a value to set them all".into());
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
                    sort: None,
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

    /// A result with an `id` primary key and one editable column.
    fn view_with(column: &str, rows: &[(i64, Value)]) -> ResultView {
        use crate::root::ResultSource;
        use dbui_app::domain::{ResultSet, Row};

        ResultView::new(
            ResultSet {
                columns: vec![
                    ColumnInfo {
                        name: "id".into(),
                        type_name: "int8".into(),
                    },
                    ColumnInfo {
                        name: column.to_string(),
                        type_name: "text".into(),
                    },
                ],
                rows: rows
                    .iter()
                    .map(|(id, value)| Row(vec![Value::Int(*id), value.clone()]))
                    .collect(),
                truncated: false,
            },
            ResultSource::Query {
                sql: String::new(),
            },
            String::new(),
            vec![Column {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                default: None,
                is_primary_key: true,
                ordinal: 1,
            }],
        )
    }

    fn draft_over(view: &ResultView, rows: &[usize]) -> RowDraft {
        RowDraft::from_rows(rows, view, &[])
    }

    /// One row, with `buffer` typed over its editable column.
    fn one_row_draft(column: &str, stored: Value, buffer: &str) -> (ResultView, RowDraft) {
        let view = view_with(column, &[(1, stored)]);
        let mut draft = draft_over(&view, &[0]);
        draft.fields[1].1 = TextInput::with_text(buffer, true);
        (view, draft)
    }

    /// The single change a one-row, one-column draft should produce.
    fn sole_change(view: &ResultView, draft: &RowDraft) -> FieldChange {
        let edits = draft.to_pending_batch(view, &[]).unwrap();
        assert_eq!(edits.len(), 1, "one row should change");
        assert_eq!(edits[0].changes.len(), 1, "one column should change");
        edits[0].changes[0].clone()
    }

    /// What each field of a draft reads, for comparing against MIXED.
    fn texts(draft: &RowDraft) -> Vec<String> {
        draft
            .fields
            .iter()
            .map(|(_, input, _)| input.text().to_string())
            .collect()
    }

    #[test]
    fn an_empty_buffer_over_an_empty_cell_writes_nothing() {
        for stored in [Value::Text(String::new()), Value::Null] {
            let (view, draft) = one_row_draft("name", stored, "");
            assert!(draft.to_pending_batch(&view, &[]).unwrap().is_empty());
        }
    }

    #[test]
    fn pretty_json_is_not_a_pending_change() {
        let compact = Value::Json(r#"{"Hello":"World"}"#.into());
        let pretty = value_editor_text(&compact);
        assert!(pretty.contains('\n'), "editor should pretty-print JSON");
        let (view, draft) = one_row_draft("feature_flags", compact, &pretty);
        assert!(draft.to_pending_batch(&view, &[]).unwrap().is_empty());
    }

    #[test]
    fn editing_one_key_of_a_compact_json_column_writes_compact_json() {
        let stored = Value::Json(r#"{"beta":2,"alpha":1}"#.into());
        let buffer = value_editor_text(&stored).replace("\"beta\": 2", "\"beta\": 3");
        let (view, draft) = one_row_draft("flags", stored, &buffer);
        let change = sole_change(&view, &draft);

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
        let (view, draft) = one_row_draft("settings", stored, &buffer);
        assert_eq!(
            sole_change(&view, &draft).new_value,
            Value::Text(r#"{"on":true}"#.into())
        );
    }

    #[test]
    fn plain_text_is_not_touched_by_the_json_layout_rules() {
        let (view, draft) = one_row_draft("note", Value::Text("hello".into()), "hello there");
        assert_eq!(
            sole_change(&view, &draft).new_value,
            Value::Text("hello there".into())
        );
    }

    /// Leaving a row and coming back must return the buffer as typed. It used
    /// to restore a re-serialized copy, silently reformatting the user's JSON.
    #[test]
    fn revisiting_a_row_restores_the_text_that_was_typed() {
        let typed = r#"{"a":2}"#;
        let (view, draft) = one_row_draft("blob", Value::Json(r#"{"a":1}"#.into()), typed);
        let staged = draft.to_pending_batch(&view, &[]).unwrap();

        let reopened = RowDraft::from_rows(&[0], &view, &staged);
        assert_eq!(reopened.fields[1].1.text(), typed);
    }

    // -- editing a selection --------------------------------------------------

    /// Three rows: two share a status, all three have different names.
    fn three_rows() -> ResultView {
        use crate::root::ResultSource;
        use dbui_app::domain::{ResultSet, Row};

        ResultView::new(
            ResultSet {
                columns: ["id", "name", "status"]
                    .iter()
                    .map(|name| ColumnInfo {
                        name: (*name).to_string(),
                        type_name: "text".into(),
                    })
                    .collect(),
                rows: vec![
                    Row(vec![
                        Value::Int(1),
                        Value::Text("Ada".into()),
                        Value::Text("active".into()),
                    ]),
                    Row(vec![
                        Value::Int(2),
                        Value::Text("Grace".into()),
                        Value::Text("active".into()),
                    ]),
                    Row(vec![
                        Value::Int(3),
                        Value::Text("Alan".into()),
                        Value::Text("paused".into()),
                    ]),
                ],
                truncated: false,
            },
            ResultSource::Query {
                sql: String::new(),
            },
            String::new(),
            vec![Column {
                name: "id".into(),
                data_type: "bigint".into(),
                nullable: false,
                default: None,
                is_primary_key: true,
                ordinal: 1,
            }],
        )
    }

    #[test]
    fn a_selection_shows_what_its_rows_agree_on_and_marks_the_rest_mixed() {
        let view = three_rows();

        // Rows 0 and 1 share a status but not a name.
        let draft = draft_over(&view, &[0, 1]);
        assert_eq!(texts(&draft), vec![MIXED, MIXED, "active"]);
        assert!(draft.is_bulk());

        // One row on its own is not a bulk edit and shows its own values.
        let single = draft_over(&view, &[2]);
        assert_eq!(texts(&single), vec!["3", "Alan", "paused"]);
        assert!(!single.is_bulk());
    }

    /// The rule the whole feature rests on: a field nobody touched is left
    /// alone, however many rows are selected.
    #[test]
    fn a_field_left_mixed_writes_nothing() {
        let view = three_rows();
        let draft = draft_over(&view, &[0, 1, 2]);
        assert_eq!(texts(&draft), vec![MIXED, MIXED, MIXED]);
        assert!(draft.to_pending_batch(&view, &[]).unwrap().is_empty());
    }

    #[test]
    fn editing_a_field_writes_it_to_every_row_that_differs() {
        let view = three_rows();
        let mut draft = draft_over(&view, &[0, 1, 2]);
        draft.fields[2].1 = TextInput::with_text("archived", true);

        let edits = draft.to_pending_batch(&view, &[]).unwrap();
        assert_eq!(edits.len(), 3, "every selected row changes");
        for edit in &edits {
            assert_eq!(edit.changes.len(), 1, "only the touched column");
            assert_eq!(edit.changes[0].column, "status");
            assert_eq!(edit.changes[0].new_value, Value::Text("archived".into()));
        }
        assert_eq!(edits[0].label, "id=1");
    }

    /// Rows that already hold the typed value are not rewritten: setting a
    /// column across a selection should touch the rows that differ and no more.
    #[test]
    fn rows_that_already_match_are_left_out_of_the_batch() {
        let view = three_rows();
        let mut draft = draft_over(&view, &[0, 1, 2]);
        draft.fields[2].1 = TextInput::with_text("active", true);

        let edits = draft.to_pending_batch(&view, &[]).unwrap();
        assert_eq!(edits.len(), 1, "only the paused row differs");
        assert_eq!(edits[0].label, "id=3");
    }

    /// The bug this guards: rows staged with *different* values make their
    /// column read MIXED, and a draft with no opinion on it must not be taken
    /// as an instruction to drop what was already staged.
    #[test]
    fn a_mixed_field_keeps_the_edits_already_staged_for_those_rows() {
        let view = three_rows();

        // Stage a different name on each of two rows.
        let mut first = draft_over(&view, &[0]);
        first.fields[1].1 = TextInput::with_text("Ada L", true);
        let mut staged = first.to_pending_batch(&view, &[]).unwrap();

        let mut second = draft_over(&view, &[1]);
        second.fields[1].1 = TextInput::with_text("Grace H", true);
        staged.extend(second.to_pending_batch(&view, &staged).unwrap());
        assert_eq!(staged.len(), 2);

        // Selecting both shows the names as MIXED -- they disagree.
        let bulk = RowDraft::from_rows(&[0, 1], &view, &staged);
        assert_eq!(texts(&bulk)[1], MIXED);

        // Editing only the status must leave both staged names in place.
        let mut bulk = bulk;
        bulk.fields[2].1 = TextInput::with_text("archived", true);
        let edits = bulk.to_pending_batch(&view, &staged).unwrap();

        assert_eq!(edits.len(), 2);
        for edit in &edits {
            let columns: Vec<&str> = edit
                .changes
                .iter()
                .map(|change| change.column.as_str())
                .collect();
            assert!(
                columns.contains(&"name") && columns.contains(&"status"),
                "the staged name survives alongside the new status: {columns:?}"
            );
        }
    }

    /// Typing a value back to what the server holds un-stages it, rather than
    /// leaving an edit behind that would be written anyway.
    #[test]
    fn typing_a_value_back_drops_its_staged_edit() {
        let view = three_rows();
        let mut draft = draft_over(&view, &[0]);
        draft.fields[1].1 = TextInput::with_text("Ada L", true);
        let staged = draft.to_pending_batch(&view, &[]).unwrap();
        assert_eq!(staged.len(), 1);

        let reopened = RowDraft::from_rows(&[0], &view, &staged);
        assert_eq!(texts(&reopened)[1], "Ada L");

        let mut reverted = reopened;
        reverted.fields[1].1 = TextInput::with_text("Ada", true);
        assert!(reverted.to_pending_batch(&view, &staged).unwrap().is_empty());
    }

    /// `MIXED` is a write token like the others, so a cell whose real content
    /// is the word has to survive being shown and typed back.
    #[test]
    fn a_cell_holding_the_word_mixed_round_trips_quoted() {
        let stored = Value::Text(MIXED.into());
        assert_eq!(value_editor_text(&stored), "\"MIXED\"");

        let view = view_with("note", &[(1, stored.clone())]);
        let draft = draft_over(&view, &[0]);
        assert_eq!(
            draft.fields[1].1.text(),
            "\"MIXED\"",
            "shown quoted, so it is not mistaken for the token"
        );
        assert!(
            draft.to_pending_batch(&view, &[]).unwrap().is_empty(),
            "and reading it back is not a change"
        );
    }

    /// Reaching the parser with the token means a caller forgot to skip the
    /// field -- writing the literal word to every selected row would be the
    /// worst possible outcome, so it is refused.
    #[test]
    fn the_mixed_token_is_never_parsed_into_a_value() {
        assert!(parse_draft_value(MIXED, &Value::Text("x".into())).is_err());
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

        let (view, draft) = one_row_draft("settings", stored.clone(), &buffer);
        let change = &sole_change(&view, &draft);

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
        let (view, mut draft) =
            one_row_draft("feature_flags", compact, r#"{"Hello":"Changed"}"#);
        assert_eq!(draft.to_pending_batch(&view, &[]).unwrap().len(), 1);

        draft.reset(&view);
        assert!(draft.to_pending_batch(&view, &[]).unwrap().is_empty());
    }
}
