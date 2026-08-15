//! The root view: all UI state, and the handlers that move it.
//!
//! GPUI is confined to this crate, and the mutable state of the window is
//! confined to this file. The components in `components/` are `impl DbUi`
//! blocks that only render -- they read state and attach listeners, they do not
//! define it. When a task lands, exactly one of the methods here folds it in.

use crate::components::context_menu::{ConfirmPrompt, ContextMenu};
use crate::components::palette::{Palette, PaletteKind};
use crate::components::{ConnectionForm, DetailInput, FormAction};
use crate::sql_complete::CompletionPopup;
use crate::tabs::{RowDraft, Tabs, WorkspaceTab};
use crate::theme::{metrics, Theme};
use dbui_app::commands;
use dbui_app::domain::{
    Catalog, Column, ConnectionId, Page, QueryOutcome, QueryResult, ResultSet, TableRef,
};
use dbui_app::{
    session, store, ConnectionStatus, DbRuntime, RowUpdate, SavedConnectionTab, Session, Workspace,
};
use gpui::{
    div, prelude::*, px, Context, FocusHandle, KeyDownEvent, MouseButton, MouseMoveEvent,
    MouseUpEvent, Pixels, SharedString, Window,
};
use std::collections::HashMap;

/// Which surface the keyboard is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    /// The table filter box above the tree.
    SidebarSearch,
    Editor,
    Grid,
    Detail,
    Filter,
    PageSize,
}

/// Focus target inside the filter strip (Tab cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterFocus {
    Where,
    Apply,
    Clear,
}

impl FilterFocus {
    const ORDER: [FilterFocus; 3] = [FilterFocus::Where, FilterFocus::Apply, FilterFocus::Clear];

    fn cycle(self, backward: bool) -> Self {
        let index = Self::ORDER
            .iter()
            .position(|item| *item == self)
            .unwrap_or(0);
        let next = if backward {
            (index + Self::ORDER.len() - 1) % Self::ORDER.len()
        } else {
            (index + 1) % Self::ORDER.len()
        };
        Self::ORDER[next]
    }
}

/// Keyboard / click cursor in the sidebar tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarItem {
    Schema {
        connection: ConnectionId,
        name: String,
    },
    Table {
        connection: ConnectionId,
        table: TableRef,
    },
}

/// Where the rows on screen came from.
///
/// Kept because the two cases behave differently afterwards: a table can be
/// paged and refreshed, a query can only be re-run.
#[derive(Clone)]
pub enum ResultSource {
    Table {
        table: TableRef,
        page: Page,
        total_rows: Option<i64>,
        where_clause: String,
    },
    Query {
        sql: String,
    },
}

/// A result set, plus everything derived from it that the grid would otherwise
/// recompute every frame.
pub struct ResultView {
    pub set: ResultSet,
    /// Per-column pixel widths, measured once when the rows arrive.
    pub widths: Vec<f32>,
    pub source: ResultSource,
    pub summary: String,
    /// Column metadata, when the rows came from a table rather than a query.
    pub structure: Vec<Column>,
}

impl ResultView {
    pub(crate) fn new(
        set: ResultSet,
        source: ResultSource,
        summary: String,
        structure: Vec<Column>,
    ) -> Self {
        let widths = column_widths(&set);
        Self {
            set,
            widths,
            source,
            summary,
            structure,
        }
    }
}

/// Estimate a starting width for each column.
///
/// Only the first rows are sampled: with 500 rows and 40 columns, measuring
/// everything is 20,000 string renders on the way to a number the user can
/// drag anyway. The header is always included so a wide name is never clipped
/// by a narrow column of values.
fn column_widths(set: &ResultSet) -> Vec<f32> {
    const SAMPLE: usize = 200;

    set.columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let mut widest = column.name.chars().count();
            for row in set.rows.iter().take(SAMPLE) {
                if let Some(value) = row.get(index) {
                    widest = widest.max(value.to_cell(64).chars().count());
                }
            }
            (widest as f32 * metrics::char_width() + metrics::cell_padding())
                .clamp(metrics::column_min_width(), metrics::column_max_width())
        })
        .collect()
}

/// A message for the status bar.
#[derive(Clone)]
pub enum Status {
    Idle,
    Busy(SharedString),
    Info(SharedString),
    Error(SharedString),
}

impl Status {
    pub fn info(text: impl Into<SharedString>) -> Self {
        Status::Info(text.into())
    }

    pub fn error(text: impl Into<SharedString>) -> Self {
        Status::Error(text.into())
    }

    pub fn busy(text: impl Into<SharedString>) -> Self {
        Status::Busy(text.into())
    }
}

pub struct DbUi {
    pub(crate) runtime: DbRuntime,
    pub(crate) workspace: Workspace,
    pub(crate) theme: Theme,
    pub(crate) focus_handle: FocusHandle,

    pub(crate) focus: Focus,
    /// The front connection tab's table/SQL tabs. Every other connection's
    /// live in `stashed_tabs`; switching swaps one for the other, which is
    /// what lets the rest of this file go on saying `self.tabs`.
    pub(crate) tabs: Tabs,
    /// Tab sets belonging to connection tabs that are not in front.
    pub(crate) stashed_tabs: HashMap<ConnectionId, Tabs>,
    pub(crate) detail_open: bool,
    /// Which draft field / search box is focused in the detail sidebar.
    pub(crate) detail_input: Option<DetailInput>,
    /// Field index whose special-value menu (NULL / EMPTY / DEFAULT) is open.
    pub(crate) detail_value_menu: Option<usize>,
    /// Which control in the filter strip owns the keyboard, if any.
    pub(crate) filter_focus: Option<FilterFocus>,
    pub(crate) page_size_focus: bool,
    pub(crate) status: Status,
    /// In-flight table/SQL loads. Lets status clear when background tabs finish
    /// without stomping a newer busy message on the active tab.
    pub(crate) loads_in_flight: u32,
    /// The cell the user last clicked, shown in full in the status bar --
    /// a grid cell is truncated, and the whole value has to be readable
    /// somewhere.
    pub(crate) selected_cell: Option<(usize, usize)>,

    pub(crate) modal: Option<ConnectionForm>,
    /// Titlebar connection switcher dropdown.
    pub(crate) connection_picker_open: bool,
    /// ⌘P / ⌘⇧P overlay; owns keys while open.
    pub(crate) palette: Option<Palette>,
    /// Arrow-key cursor in the sidebar list.
    pub(crate) sidebar_cursor: Option<SidebarItem>,
    /// Theme id to restore if the theme picker is dismissed.
    pub(crate) theme_prev: Option<String>,
    /// Where the self-update flow has got to; drawn as a status-bar chip.
    pub(crate) update: crate::update::UpdateState,
    /// Height of the expanded change bubble's diff area, dragged by its top
    /// edge. A one-key edit needs three lines and a rewritten blob needs
    /// thirty, and only the person looking at it knows which this is.
    pub(crate) change_bubble_height: Pixels,
    /// Live drag: `(pointer y, height)` as they were when the edge was grabbed.
    pub(crate) change_bubble_drag: Option<(Pixels, Pixels)>,
    /// SQL editor pane height (dragged by the strip under the editor).
    pub(crate) editor_height: Pixels,
    /// Live drag for the SQL editor resize: `(pointer y, height)`.
    pub(crate) editor_drag: Option<(Pixels, Pixels)>,
    /// Open SQL autocomplete popup, if any.
    pub(crate) completion: Option<CompletionPopup>,
    /// Cached `driver.columns` results keyed by `(schema, table)`.
    pub(crate) column_cache: HashMap<(String, String), Vec<Column>>,
    /// Substring filter over the schema tree. Empty means show everything.
    pub(crate) sidebar_filter: crate::text_input::TextInput,
    /// The row a drag has reached, set while the button is down. `Some` means
    /// a drag is in progress; the value is what stops a pointer wandering
    /// inside one row from rebuilding the range on every mouse-move.
    pub(crate) row_drag: Option<usize>,
    /// Right-click menu, if one is open.
    pub(crate) context_menu: Option<ContextMenu>,
    /// A destructive action waiting on the user typing the table's name.
    pub(crate) confirm: Option<ConfirmPrompt>,
}

/// Starting height of the diff area, and the range the drag is allowed.
pub(crate) const BUBBLE_HEIGHT_DEFAULT: f32 = 180.;
const BUBBLE_HEIGHT_MIN: f32 = 48.;
/// Past this the bubble is eating the grid it is describing.
const BUBBLE_HEIGHT_MAX_FRACTION: f32 = 0.7;

pub(crate) const EDITOR_HEIGHT_DEFAULT: f32 = 150.;
const EDITOR_HEIGHT_MIN: f32 = 80.;
const EDITOR_HEIGHT_MAX_FRACTION: f32 = 0.6;

/// Resolve a drag into a height. `rise` is how far the edge was pulled upward,
/// which is the direction that makes the panel bigger.
fn bubble_height_for(start_height: Pixels, rise: Pixels, viewport_height: Pixels) -> Pixels {
    let max = (f32::from(viewport_height) * BUBBLE_HEIGHT_MAX_FRACTION).max(BUBBLE_HEIGHT_MIN);
    px((f32::from(start_height) + f32::from(rise)).clamp(BUBBLE_HEIGHT_MIN, max))
}

fn editor_height_for(start_height: Pixels, delta: Pixels, viewport_height: Pixels) -> Pixels {
    let max = (f32::from(viewport_height) * EDITOR_HEIGHT_MAX_FRACTION).max(EDITOR_HEIGHT_MIN);
    px((f32::from(start_height) + f32::from(delta)).clamp(EDITOR_HEIGHT_MIN, max))
}

/// Which schemas to leave unfolded once a catalog arrives.
///
/// Whatever the session restored wins, minus any schema the server no longer
/// has — a folder for something that is gone is worse than a closed one.
/// Nothing restored falls back to opening the first, so a new connection is
/// not a wall of closed folders.
fn schemas_to_expand(restored: &[String], catalog: &Catalog) -> Vec<String> {
    let kept: Vec<String> = restored
        .iter()
        .filter(|name| catalog.schemas.iter().any(|schema| &schema.name == *name))
        .cloned()
        .collect();
    if !kept.is_empty() {
        return kept;
    }
    catalog
        .schemas
        .first()
        .map(|schema| vec![schema.name.clone()])
        .unwrap_or_default()
}

impl DbUi {
    pub fn new(runtime: DbRuntime, workspace: Workspace, focus_handle: FocusHandle) -> Self {
        Self {
            runtime,
            workspace,
            theme: Theme::default(),
            focus_handle,
            focus: Focus::Sidebar,
            tabs: Tabs::default(),
            stashed_tabs: HashMap::new(),
            detail_open: false,
            detail_input: None,
            detail_value_menu: None,
            filter_focus: None,
            page_size_focus: false,
            status: Status::Idle,
            loads_in_flight: 0,
            selected_cell: None,
            modal: None,
            connection_picker_open: false,
            palette: None,
            sidebar_cursor: None,
            theme_prev: None,
            update: crate::update::UpdateState::default(),
            change_bubble_height: px(BUBBLE_HEIGHT_DEFAULT),
            change_bubble_drag: None,
            editor_height: px(EDITOR_HEIGHT_DEFAULT),
            editor_drag: None,
            completion: None,
            column_cache: HashMap::new(),
            sidebar_filter: crate::text_input::TextInput::new(false),
            row_drag: None,
            context_menu: None,
            confirm: None,
        }
    }

    /// Grab the bubble's top edge at pointer position `y`.
    pub(crate) fn begin_change_bubble_drag(&mut self, y: Pixels, cx: &mut Context<Self>) {
        self.change_bubble_drag = Some((y, self.change_bubble_height));
        cx.notify();
    }

    /// Track the pointer. Dragging the top edge upwards makes the panel taller,
    /// so the delta is inverted.
    pub(crate) fn drag_change_bubble(
        &mut self,
        y: Pixels,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some((start_y, start_height)) = self.change_bubble_drag else {
            return;
        };
        self.change_bubble_height =
            bubble_height_for(start_height, start_y - y, window.viewport_size().height);
        cx.notify();
    }

    pub(crate) fn end_change_bubble_drag(&mut self, cx: &mut Context<Self>) {
        if self.change_bubble_drag.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn begin_editor_drag(&mut self, y: Pixels, cx: &mut Context<Self>) {
        self.editor_drag = Some((y, self.editor_height));
        cx.notify();
    }

    pub(crate) fn drag_editor(&mut self, y: Pixels, window: &Window, cx: &mut Context<Self>) {
        let Some((start_y, start_height)) = self.editor_drag else {
            return;
        };
        // Dragging the bottom edge downward grows the editor.
        self.editor_height =
            editor_height_for(start_height, y - start_y, window.viewport_size().height);
        cx.notify();
    }

    pub(crate) fn end_editor_drag(&mut self, cx: &mut Context<Self>) {
        if self.editor_drag.take().is_some() {
            self.persist_prefs(cx);
        }
    }

    pub fn apply_editor_height_px(&mut self, px_value: u32) {
        self.editor_height = px((px_value as f32).clamp(EDITOR_HEIGHT_MIN, 600.));
    }

    pub fn apply_theme_id(&mut self, id: &str) {
        self.theme = Theme::named(id);
    }

    pub fn apply_zoom_pct(&mut self, pct: u32) {
        metrics::set_zoom_pct(pct);
    }

    pub fn persist_prefs(&mut self, cx: &mut Context<Self>) {
        let prefs = store::Prefs {
            theme: self.theme.id.to_string(),
            zoom_pct: metrics::zoom_pct(),
            sql_editor_height_px: f32::from(self.editor_height).round() as u32,
        };
        match store::prefs_path().and_then(|path| store::save_prefs(&path, &prefs)) {
            Ok(()) => {}
            Err(error) => {
                self.status = Status::error(format!("Could not save prefs: {error}"));
            }
        }
        cx.notify();
    }

    pub fn persist_theme(&mut self, cx: &mut Context<Self>) {
        let prefs = store::Prefs {
            theme: self.theme.id.to_string(),
            zoom_pct: metrics::zoom_pct(),
            sql_editor_height_px: f32::from(self.editor_height).round() as u32,
        };
        match store::prefs_path().and_then(|path| store::save_prefs(&path, &prefs)) {
            Ok(()) => {
                self.status = Status::info(format!("Theme: {}", self.theme.label));
            }
            Err(error) => {
                self.status = Status::error(format!("Could not save theme: {error}"));
            }
        }
        cx.notify();
    }

    pub(crate) fn zoom_delta(&mut self, direction: i32, cx: &mut Context<Self>) {
        let pct = match direction {
            1 => metrics::zoom_in(),
            -1 => metrics::zoom_out(),
            _ => metrics::zoom_reset(),
        };
        self.persist_prefs(cx);
        self.status = Status::info(format!("Zoom: {pct}%"));
        cx.notify();
    }

    /// Surface a failure that happened before the window existed.
    pub fn report_startup_error(&mut self, message: impl Into<SharedString>) {
        self.status = Status::Error(message.into());
    }

    // -- tabs ---------------------------------------------------------------

    pub(crate) fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.items.len() || index == self.tabs.active {
            return;
        }
        self.stash_current_draft(cx);
        self.tabs.activate(index);
        self.selected_cell = None;
        self.detail_input = None;
        self.detail_value_menu = None;
        self.filter_focus = None;
        self.page_size_focus = false;
        if let Some(tab) = self.tabs.active() {
            if let Some(table) = tab.table_ref() {
                self.workspace.open_table = Some(table.clone());
            } else {
                self.workspace.open_table = None;
            }
        }
        self.persist_session();
        cx.notify();
    }

    pub(crate) fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.items.len() {
            return;
        }
        self.tabs.close(index);
        self.selected_cell = None;
        self.detail_input = None;
        self.detail_value_menu = None;
        self.filter_focus = None;
        self.page_size_focus = false;
        self.workspace.open_table = self.tabs.active().and_then(|tab| tab.table_ref().cloned());
        if self.tabs.items.is_empty() {
            self.workspace.open_table = None;
        }
        self.persist_session();
        cx.notify();
    }

    pub(crate) fn close_active_tab(&mut self, cx: &mut Context<Self>) {
        if self.tabs.items.is_empty() {
            return;
        }
        self.close_tab(self.tabs.active, cx);
    }

    pub(crate) fn next_tab(&mut self, cx: &mut Context<Self>) {
        let len = self.tabs.items.len();
        if len < 2 {
            return;
        }
        self.activate_tab((self.tabs.active + 1) % len, cx);
    }

    pub(crate) fn prev_tab(&mut self, cx: &mut Context<Self>) {
        let len = self.tabs.items.len();
        if len < 2 {
            return;
        }
        let prev = if self.tabs.active == 0 {
            len - 1
        } else {
            self.tabs.active - 1
        };
        self.activate_tab(prev, cx);
    }

    /// Select tab by 1-based number. `9` jumps to the last tab (browser-style).
    pub(crate) fn select_tab_number(&mut self, number: u8, cx: &mut Context<Self>) {
        let len = self.tabs.items.len();
        if len == 0 || !(1..=9).contains(&number) {
            return;
        }
        let index = if number == 9 {
            len - 1
        } else {
            let index = (number as usize) - 1;
            if index >= len {
                return;
            }
            index
        };
        self.activate_tab(index, cx);
    }

    pub(crate) fn toggle_detail(&mut self, cx: &mut Context<Self>) {
        self.detail_open = !self.detail_open;
        cx.notify();
    }

    /// Tab order in the detail sidebar: search, then visible non-PK fields.
    fn detail_tab_targets(&self) -> Vec<DetailInput> {
        let draft = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            })
            | Some(WorkspaceTab::Sql {
                draft: Some(draft), ..
            }) => draft,
            _ => return Vec::new(),
        };
        let search = draft.field_search.text().to_ascii_lowercase();
        let mut targets = vec![DetailInput::Search];
        for (index, (name, _, is_pk)) in draft.fields.iter().enumerate() {
            if *is_pk {
                continue;
            }
            if !search.is_empty() && !name.to_ascii_lowercase().contains(&search) {
                continue;
            }
            targets.push(DetailInput::Field(index));
        }
        targets
    }

    fn cycle_detail_focus(&mut self, backward: bool, cx: &mut Context<Self>) {
        let targets = self.detail_tab_targets();
        if targets.is_empty() {
            return;
        }
        let current = self.detail_input.unwrap_or(DetailInput::Search);
        let index = targets
            .iter()
            .position(|target| *target == current)
            .unwrap_or(0);
        let next = if backward {
            (index + targets.len() - 1) % targets.len()
        } else {
            (index + 1) % targets.len()
        };
        self.detail_input = Some(targets[next]);
        self.focus = Focus::Detail;
        cx.notify();
    }

    pub(crate) fn open_sql_tab(&mut self, cx: &mut Context<Self>) {
        self.tabs.open_sql();
        self.focus = Focus::Editor;
        self.persist_session();
        cx.notify();
    }

    // -- connections ------------------------------------------------------

    pub(crate) fn connect(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        let Some(entry) = self.workspace.get_mut(id) else {
            return;
        };
        if entry.status.is_connected() || entry.status.is_busy() {
            return;
        }

        let config = entry.config.clone();
        entry.status = ConnectionStatus::Connecting;
        self.status = Status::busy(format!("Connecting to {}…", config.summary()));

        let task = commands::connect(&self.runtime, config);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                let Some(outcome) = landed else { return };
                match outcome {
                    Ok((driver, catalog)) => {
                        let version = driver.server_version().to_string();
                        if let Some(entry) = this.workspace.get_mut(id) {
                            entry.status = ConnectionStatus::Connected(driver);
                            entry.expanded = schemas_to_expand(&entry.expanded, &catalog);
                            entry.catalog = Some(catalog);
                        }
                        // Only jump to it if it is still what the user is
                        // looking at -- a background tab finishing its connect
                        // must not yank them off the tab they switched to.
                        if this.workspace.active_id().is_none() {
                            this.workspace.activate(id);
                        }
                        this.status = Status::info(format!("Connected — {version}"));
                        // The driver only exists now, so a tab restored from
                        // disk (or left over from a disconnect) gets its rows
                        // here rather than at the moment it was put in front.
                        if this.workspace.active_id() == Some(id) {
                            this.load_active_table_if_empty(cx);
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if let Some(entry) = this.workspace.get_mut(id) {
                            entry.status = ConnectionStatus::Failed(message.clone());
                        }
                        this.status = Status::error(message);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn disconnect(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        let Some(entry) = self.workspace.get_mut(id) else {
            return;
        };
        if let Some(driver) = entry.status.driver().cloned() {
            commands::disconnect(&self.runtime, driver);
        }
        entry.disconnect();

        // The rows are stale, but the arrangement is not: what was open is
        // still what the user wants open when the connection comes back.
        if self.workspace.active_id() == Some(id) {
            self.tabs.clear_results();
            self.selected_cell = None;
            self.detail_input = None;
            self.detail_value_menu = None;
        } else if let Some(tabs) = self.stashed_tabs.get_mut(&id) {
            tabs.clear_results();
        }
        self.status = Status::info("Disconnected");
        cx.notify();
    }

    /// Bring a connection tab to the front, swapping in the tabs it owns.
    ///
    /// The front connection's tab set lives in `self.tabs` and every other
    /// one's in `stashed_tabs`; a switch is a swap between the two. Nothing is
    /// thrown away, which is the difference between a tab and a mode: coming
    /// back finds the same tables open on the same tab.
    pub(crate) fn select_connection(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        let previous = self.workspace.active_id();
        if previous == Some(id) {
            self.workspace.activate(id);
            self.focus = Focus::Sidebar;
            cx.notify();
            return;
        }

        // Fold any half-finished row edit into the outgoing tab set before it
        // is put away, or the change is lost on the way out.
        self.stash_current_draft(cx);
        if let Some(previous) = previous {
            self.stashed_tabs
                .insert(previous, std::mem::take(&mut self.tabs));
        }

        self.workspace.activate(id);
        self.tabs = self.stashed_tabs.remove(&id).unwrap_or_default();

        self.selected_cell = None;
        self.detail_input = None;
        self.detail_value_menu = None;
        self.filter_focus = None;
        self.page_size_focus = false;
        self.sidebar_cursor = None;
        self.completion = None;
        self.column_cache.clear();
        self.workspace.open_table = self.tabs.active().and_then(|tab| tab.table_ref().cloned());
        self.focus = Focus::Sidebar;

        self.persist_session();
        cx.notify();
        // A restored tab has no rows yet. Loading here rather than on restore
        // means a background connection never dials out on its own.
        self.load_active_table_if_empty(cx);
    }

    // -- connection tabs ----------------------------------------------------

    /// Open a connection as a tab and connect it if it is not already.
    pub(crate) fn open_connection_tab(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        self.select_connection(id, cx);
        let connected = self
            .workspace
            .get(id)
            .map(|entry| entry.status.is_connected() || entry.status.is_busy())
            .unwrap_or(false);
        if !connected {
            self.connect(id, cx);
        }
    }

    /// Close a connection tab: drop its tabs, close its socket, keep the
    /// connection itself saved so it can be reopened from the picker.
    pub(crate) fn close_connection_tab(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        if !self.workspace.is_open(id) {
            return;
        }
        let was_active = self.workspace.active_id() == Some(id);
        if was_active {
            self.stash_current_draft(cx);
        }

        if let Some(entry) = self.workspace.get(id) {
            if let Some(driver) = entry.status.driver().cloned() {
                commands::disconnect(&self.runtime, driver);
            }
        }
        if let Some(entry) = self.workspace.get_mut(id) {
            entry.disconnect();
        }

        self.stashed_tabs.remove(&id);
        let promoted = self.workspace.close_connection(id);

        if was_active {
            self.tabs = promoted
                .and_then(|next| self.stashed_tabs.remove(&next))
                .unwrap_or_default();
            self.selected_cell = None;
            self.detail_input = None;
            self.detail_value_menu = None;
            self.filter_focus = None;
            self.page_size_focus = false;
            self.sidebar_cursor = None;
            self.completion = None;
            self.column_cache.clear();
            self.workspace.open_table =
                self.tabs.active().and_then(|tab| tab.table_ref().cloned());
            self.focus = Focus::Sidebar;
        }

        self.persist_session();
        cx.notify();
        if was_active {
            self.load_active_table_if_empty(cx);
        }
    }

    pub(crate) fn close_active_connection_tab(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.workspace.active_id() {
            self.close_connection_tab(id, cx);
        }
    }

    /// Step through the connection tab bar. `forward` wraps at either end.
    pub(crate) fn cycle_connection_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        let count = self.workspace.open_count();
        if count < 2 {
            return;
        }
        let Some(index) = self.workspace.active_index() else {
            return;
        };
        let next = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        let Some(id) = self.workspace.open_ids().get(next).copied() else {
            return;
        };
        self.open_connection_tab(id, cx);
    }

    /// Load the front table tab's rows if it has none — a tab restored from
    /// disk, or one that was open when its connection dropped.
    fn load_active_table_if_empty(&mut self, cx: &mut Context<Self>) {
        if !self.tabs.active_needs_load() || self.workspace.active_driver().is_none() {
            return;
        }
        self.load_active_table(cx);
    }

    pub(crate) fn toggle_schema(&mut self, id: ConnectionId, schema: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.workspace.get_mut(id) {
            entry.toggle_schema(schema);
        }
        self.persist_session();
        cx.notify();
    }

    // -- the table filter ---------------------------------------------------

    pub(crate) fn focus_sidebar_search(&mut self, cx: &mut Context<Self>) {
        self.close_palette(cx);
        self.focus = Focus::SidebarSearch;
        self.filter_focus = None;
        self.page_size_focus = false;
        self.detail_input = None;
        self.sidebar_filter.select_all();
        cx.notify();
    }

    /// Escape out of the filter: empty it if it has text, otherwise step back
    /// to the tree. Two presses always get you out, and the first one never
    /// throws away a search the user is still reading the results of.
    pub(crate) fn dismiss_sidebar_search(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_filter.is_empty() {
            self.focus = Focus::Sidebar;
        } else {
            self.sidebar_filter.clear();
            self.sidebar_cursor = None;
        }
        cx.notify();
    }

    pub(crate) fn clear_sidebar_filter(&mut self, cx: &mut Context<Self>) {
        if !self.sidebar_filter.is_empty() {
            self.sidebar_filter.clear();
            self.sidebar_cursor = None;
            cx.notify();
        }
    }

    /// The filter as typed, lowercased. Empty means "show everything".
    pub(crate) fn sidebar_query(&self) -> String {
        self.sidebar_filter.text().trim().to_lowercase()
    }

    /// Whether a table survives the filter. Both the bare and the qualified
    /// name are matched, so `public.us` finds what `us` does.
    pub(crate) fn table_matches_filter(&self, table: &dbui_app::domain::Table, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        table.name.to_lowercase().contains(query)
            || format!("{}.{}", table.schema, table.name)
                .to_lowercase()
                .contains(query)
    }

    /// Move from the filter box into the tree, landing on the first match.
    pub(crate) fn enter_filtered_tree(&mut self, cx: &mut Context<Self>) {
        let first = self
            .sidebar_visible_items()
            .into_iter()
            .find(|item| matches!(item, SidebarItem::Table { .. }));
        if let Some(item) = first {
            self.sidebar_cursor = Some(item);
            self.focus = Focus::Sidebar;
            cx.notify();
        }
    }

    /// Enter in the filter box: open the first table it found.
    pub(crate) fn open_first_filtered_table(&mut self, cx: &mut Context<Self>) {
        let first = self.sidebar_visible_items().into_iter().find_map(|item| {
            match item {
                SidebarItem::Table { table, .. } => Some(table),
                SidebarItem::Schema { .. } => None,
            }
        });
        let Some(table) = first else {
            return;
        };
        self.focus = Focus::Sidebar;
        self.open_table_tab(table, cx);
    }

    pub(crate) fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(driver) = self.workspace.active_driver() else {
            return;
        };
        let Some(id) = self.workspace.active_id() else {
            return;
        };

        self.status = Status::busy("Refreshing…");
        let task = commands::refresh_catalog(&self.runtime, driver);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                match landed {
                    Some(Ok(catalog)) => {
                        if let Some(entry) = this.workspace.get_mut(id) {
                            entry.catalog = Some(catalog);
                        }
                        this.status = Status::info("Catalog refreshed");
                    }
                    Some(Err(error)) => this.status = Status::error(error.to_string()),
                    None => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- tables and queries -----------------------------------------------

    pub(crate) fn open_table_tab(&mut self, table: TableRef, cx: &mut Context<Self>) {
        self.tabs.open_table(table.clone());
        self.workspace.open_table = Some(table);
        self.selected_cell = None;
        self.persist_session();
        self.load_active_table(cx);
    }

    fn load_active_table(&mut self, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active_id() else {
            return;
        };
        self.load_table(tab_id, cx);
    }

    fn load_table(&mut self, tab_id: crate::tabs::TabId, cx: &mut Context<Self>) {
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let (table, page, where_clause, sort) = match self.tabs.get_mut(tab_id) {
            Some(WorkspaceTab::Table {
                table,
                page,
                where_clause,
                sort,
                ..
            }) => (table.clone(), *page, where_clause.clone(), sort.clone()),
            _ => return,
        };

        let load_seq = match self.tabs.get_mut(tab_id) {
            Some(tab) => tab.bump_load_seq(),
            None => return,
        };

        self.loads_in_flight = self.loads_in_flight.saturating_add(1);
        if self.tabs.active_id() == Some(tab_id) {
            self.status = Status::busy(format!("Loading {}…", table.qualified()));
        }

        let task = commands::open_table(
            &self.runtime,
            driver,
            table.clone(),
            page,
            where_clause.clone(),
            sort,
        );
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.finish_tab_load(
                    tab_id,
                    load_seq,
                    |this, is_current, is_active| match landed {
                        Some(Ok(contents)) if is_current => {
                            let summary = table_summary(&contents);
                            if let Some(WorkspaceTab::Table {
                                result,
                                page,
                                page_size_draft,
                                where_clause,
                                selected_row,
                                selection,
                                draft,
                                ..
                            }) = this.tabs.get_mut(tab_id)
                            {
                                *page = contents.page;
                                *page_size_draft = crate::text_input::TextInput::with_text(
                                    contents.page.limit.to_string(),
                                    false,
                                );
                                *where_clause = contents.where_clause.clone();
                                *selected_row = None;
                                // A fresh page is fresh rows: the old indices
                                // point at whatever now occupies them.
                                selection.clear();
                                *draft = None;
                                *result = Some(ResultView::new(
                                    contents.rows,
                                    ResultSource::Table {
                                        table: contents.table,
                                        page: contents.page,
                                        total_rows: contents.total_rows,
                                        where_clause: contents.where_clause,
                                    },
                                    summary,
                                    contents.columns,
                                ));
                            }
                            if is_active {
                                this.status = Status::Idle;
                                this.focus = Focus::Grid;
                            }
                        }
                        Some(Err(error)) if is_current && is_active => {
                            this.status = Status::error(error.to_string());
                        }
                        _ => {}
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Book-keep an async tab load: drop the in-flight count and clear a stale
    /// busy status when nothing else is loading.
    fn finish_tab_load(
        &mut self,
        tab_id: crate::tabs::TabId,
        load_seq: u64,
        apply: impl FnOnce(&mut Self, bool, bool),
    ) {
        let is_current = self.tabs.load_is_current(tab_id, load_seq);
        let is_active = self.tabs.active_id() == Some(tab_id);
        apply(self, is_current, is_active);
        self.loads_in_flight = self.loads_in_flight.saturating_sub(1);
        if self.loads_in_flight == 0 && matches!(self.status, Status::Busy(_)) {
            self.status = Status::Idle;
        }
    }

    /// Click a header: sort ascending, then descending, then back to the
    /// table's own key order.
    ///
    /// Sorting reads a fresh page rather than reordering what is on screen —
    /// the rows here are one window onto the table, and sorting only that
    /// window would order five hundred rows out of five million and call it
    /// sorted.
    pub(crate) fn toggle_sort(&mut self, column: &str, cx: &mut Context<Self>) {
        // Whatever is staged refers to rows by key, not by position, so it
        // survives the reload -- but a half-typed draft has to be folded in
        // before the rows underneath it move.
        self.stash_current_draft(cx);

        let Some(WorkspaceTab::Table { sort, page, .. }) = self.tabs.active_mut() else {
            return;
        };
        let next = dbui_app::domain::SortKey::cycled(sort.as_ref(), column);
        *sort = next.clone();
        // A new order makes the old offset meaningless.
        page.offset = 0;

        self.status = match &next {
            Some(key) if key.ascending => Status::info(format!("Sorted by {column} ↑")),
            Some(_) => Status::info(format!("Sorted by {column} ↓")),
            None => Status::info("Sort cleared"),
        };
        self.load_active_table(cx);
    }

    /// Drop the sort and go back to the table's own key order.
    pub(crate) fn clear_sort(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table { sort, page, .. }) = self.tabs.active_mut() else {
            return;
        };
        if sort.take().is_none() {
            return;
        }
        page.offset = 0;
        self.status = Status::info("Sort cleared");
        self.load_active_table(cx);
    }

    /// The sort the active tab is showing, for the header arrow.
    pub(crate) fn active_sort(&self) -> Option<&dbui_app::domain::SortKey> {
        match self.tabs.active() {
            Some(WorkspaceTab::Table { sort, .. }) => sort.as_ref(),
            _ => None,
        }
    }

    /// Move the active table tab's window by a page.
    pub(crate) fn page(&mut self, forward: bool, cx: &mut Context<Self>) {
        let next = match self.tabs.active() {
            Some(WorkspaceTab::Table { result, page, .. }) => {
                let current = result
                    .as_ref()
                    .and_then(|view| match &view.source {
                        ResultSource::Table { page, .. } => Some(*page),
                        _ => None,
                    })
                    .unwrap_or(*page);
                if forward {
                    current.next()
                } else {
                    current.previous()
                }
            }
            _ => return,
        };

        let Some(WorkspaceTab::Table { page, result, .. }) = self.tabs.active_mut() else {
            return;
        };
        let current = result
            .as_ref()
            .and_then(|view| match &view.source {
                ResultSource::Table { page, .. } => Some(*page),
                _ => None,
            })
            .unwrap_or(*page);
        if next == current {
            return;
        }
        *page = next;
        self.load_active_table(cx);
    }

    pub(crate) fn run_query(&mut self, cx: &mut Context<Self>) {
        let Some(sql) = self.resolve_run_sql() else {
            return;
        };
        self.dispatch_statements(vec![sql], cx);
    }

    pub(crate) fn run_all_queries(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.resolve_run_all_sql() else {
            return;
        };
        self.dispatch_statements(statements, cx);
    }

    /// Open or refresh the SQL autocomplete popup at the caret.
    pub(crate) fn trigger_completion(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return;
        };
        let sql = editor.text().to_string();
        let caret = editor.cursor();
        let request = crate::sql_complete::request_at(&sql, caret);
        let catalog = self
            .workspace
            .active()
            .and_then(|entry| entry.catalog.as_ref());

        if let Some(table) = crate::sql_complete::pending_column_fetch(
            &request,
            catalog,
            &self.column_cache,
            &sql,
            caret,
        ) {
            self.fetch_columns_for_completion(table, cx);
        }

        let catalog = self
            .workspace
            .active()
            .and_then(|entry| entry.catalog.as_ref());
        self.completion =
            crate::sql_complete::build_popup(&request, catalog, &self.column_cache, &sql, caret);
        cx.notify();
    }

    fn fetch_columns_for_completion(&mut self, table: TableRef, cx: &mut Context<Self>) {
        let Some(driver) = self.workspace.active_driver() else {
            return;
        };
        let task = commands::fetch_columns(&self.runtime, driver, table);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if let Some(Ok((table, columns))) = landed {
                    this.column_cache
                        .insert((table.schema.clone(), table.name.clone()), columns);
                    // Rebuild the popup now that columns are available.
                    if this.focus == Focus::Editor {
                        this.trigger_completion(cx);
                    } else {
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn accept_completion(&mut self, cx: &mut Context<Self>) {
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.current().cloned() else {
            return;
        };
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() else {
            return;
        };
        editor.replace_range(popup.replace_range, &item.label);
        cx.notify();
    }

    pub(crate) fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        if self.completion.take().is_some() {
            cx.notify();
        }
    }

    /// ⌘↵ target: selection if present, else the statement under the caret.
    pub(crate) fn resolve_run_sql(&self) -> Option<String> {
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return None;
        };
        if let Some(selected) = editor.selected_text() {
            let trimmed = selected.trim();
            if trimmed.is_empty() {
                return None;
            }
            return Some(trimmed.to_string());
        }
        let text = editor.text();
        let range = dbui_app::domain::statement_at(text, editor.cursor())?;
        let stmt = text[range].trim();
        if stmt.is_empty() {
            None
        } else {
            Some(stmt.to_string())
        }
    }

    /// ⌘⇧↵ target: every statement in the selection, or the whole buffer.
    pub(crate) fn resolve_run_all_sql(&self) -> Option<Vec<String>> {
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return None;
        };
        let scope = if let Some(selected) = editor.selected_text() {
            selected
        } else {
            editor.text()
        };
        let statements: Vec<String> = dbui_app::domain::split_statements(scope)
            .into_iter()
            .map(|range| scope[range].trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if statements.is_empty() {
            None
        } else {
            Some(statements)
        }
    }

    fn dispatch_statements(&mut self, statements: Vec<String>, cx: &mut Context<Self>) {
        if statements.is_empty() {
            return;
        }

        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let Some((tab_id, load_seq)) = self.tabs.begin_active_load() else {
            return;
        };

        self.workspace.open_table = None;
        self.selected_cell = None;
        self.loads_in_flight = self.loads_in_flight.saturating_add(1);
        self.status = Status::busy("Running…");

        let task = commands::run_queries(&self.runtime, driver, statements);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.finish_tab_load(
                    tab_id,
                    load_seq,
                    |this, is_current, is_active| match landed {
                        Some(Ok(batch)) if is_current => {
                            this.absorb_batch_result(tab_id, batch, is_active);
                        }
                        Some(Err(error)) if is_current && is_active => {
                            this.status = Status::error(error.to_string());
                        }
                        _ => {}
                    },
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn absorb_batch_result(
        &mut self,
        tab_id: crate::tabs::TabId,
        batch: commands::BatchQueryResult,
        is_active: bool,
    ) {
        let summary = batch.summary();
        if let Some(result) = batch.last_rows {
            self.absorb_query_result(tab_id, result, is_active);
            if is_active && batch.results.len() > 1 {
                self.status = Status::info(summary);
            }
            return;
        }

        // No row-producing statement: keep any existing grid, surface the
        // batch / last affected summary on the status bar.
        if is_active {
            self.status = Status::info(summary);
        }
    }

    fn absorb_query_result(
        &mut self,
        tab_id: crate::tabs::TabId,
        result: QueryResult,
        is_active: bool,
    ) {
        let sql = result.statement.clone();
        let summary = result.summary();
        match result.outcome {
            QueryOutcome::Rows(set) => {
                if let Some(WorkspaceTab::Sql {
                    result: tab_result,
                    selected_row,
                    selection,
                    draft,
                    ..
                }) = self.tabs.get_mut(tab_id)
                {
                    *selected_row = None;
                    selection.clear();
                    *draft = None;
                    *tab_result = Some(ResultView::new(
                        set,
                        ResultSource::Query { sql },
                        summary.clone(),
                        Vec::new(),
                    ));
                }
                if is_active {
                    self.focus = Focus::Grid;
                    self.status = Status::Idle;
                }
            }
            QueryOutcome::Affected(_) => {
                if is_active {
                    self.status = Status::info(summary);
                }
            }
        }
    }

    pub(crate) fn refresh_result(&mut self, cx: &mut Context<Self>) {
        match self.tabs.active() {
            Some(WorkspaceTab::Table { .. }) => self.load_active_table(cx),
            Some(WorkspaceTab::Sql { .. }) => self.run_query(cx),
            None => self.refresh_catalog(cx),
        }
    }

    pub(crate) fn set_table_pane(&mut self, pane: crate::tabs::TablePane, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table { pane: tab_pane, .. }) = self.tabs.active_mut() {
            *tab_pane = pane;
        }
        cx.notify();
    }

    pub(crate) fn toggle_filters_open(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table {
            filters_open,
            where_draft,
            where_clause,
            ..
        }) = self.tabs.active_mut()
        {
            *filters_open = !*filters_open;
            if *filters_open && where_draft.text().is_empty() && !where_clause.is_empty() {
                *where_draft = crate::text_input::TextInput::with_text(where_clause.clone(), false);
            }
            if *filters_open {
                self.filter_focus = Some(FilterFocus::Where);
                self.focus = Focus::Filter;
            } else {
                self.filter_focus = None;
            }
        }
        cx.notify();
    }

    pub(crate) fn toggle_columns_open(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table { columns_open, .. }) = self.tabs.active_mut() {
            *columns_open = !*columns_open;
        }
        cx.notify();
    }

    pub(crate) fn toggle_column_hidden(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table { hidden_columns, .. }) = self.tabs.active_mut() {
            if hidden_columns.contains(name) {
                hidden_columns.remove(name);
            } else {
                hidden_columns.insert(name.to_string());
            }
        }
        cx.notify();
    }

    pub(crate) fn apply_filters(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table {
            where_clause,
            where_draft,
            page,
            ..
        }) = self.tabs.active_mut()
        {
            *where_clause = where_draft.text().trim().to_string();
            page.offset = 0;
        }
        self.filter_focus = None;
        self.load_active_table(cx);
    }

    pub(crate) fn clear_filters(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table {
            where_clause,
            where_draft,
            page,
            ..
        }) = self.tabs.active_mut()
        {
            where_clause.clear();
            where_draft.clear();
            *page = Page {
                limit: page.limit,
                offset: 0,
            };
        }
        self.filter_focus = None;
        self.load_active_table(cx);
    }

    pub(crate) fn apply_page_size(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            page,
            page_size_draft,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };

        let raw = page_size_draft.text().trim().to_string();
        let Ok(parsed) = raw.parse::<u32>() else {
            *page_size_draft =
                crate::text_input::TextInput::with_text(page.limit.to_string(), false);
            self.status = Status::error("Page size must be a number");
            self.page_size_focus = false;
            cx.notify();
            return;
        };

        let limit = parsed.clamp(1, 5_000);
        if limit != parsed {
            *page_size_draft = crate::text_input::TextInput::with_text(limit.to_string(), false);
        }
        page.limit = limit;
        page.offset = 0;
        self.page_size_focus = false;
        self.focus = Focus::Grid;
        self.load_active_table(cx);
    }

    pub(crate) fn select_row(&mut self, row: usize, cx: &mut Context<Self>) {
        // Stash the current dirty draft into the batch before switching.
        self.stash_current_draft(cx);

        // Picking one row is also collapsing the selection to it: arrowing
        // away from a range the user built has to leave them somewhere they
        // can see, not with fifty rows still lit behind the caret.
        if let Some(tab) = self.tabs.active_mut() {
            tab.selection_mut().set_single(row);
        }

        self.rebuild_draft(Some(row), cx);
        if self.tabs.active().is_some_and(|tab| tab.result().is_some()) {
            self.focus = Focus::Detail;
        }
        self.selected_cell = None;
        cx.notify();
    }

    /// Rebuild the detail draft over whatever is selected now.
    ///
    /// Every route into the selection ends here, which is what makes the
    /// sidebar describe the selection rather than the last row that happened
    /// to be clicked. `lead` is the row the detail is *about* -- the one the
    /// pointer landed on -- and only matters for the grid's own highlight.
    pub(crate) fn rebuild_draft(&mut self, lead: Option<usize>, cx: &mut Context<Self>) {
        let opened = {
            let Some(tab) = self.tabs.active_mut() else {
                return;
            };
            let rows = tab.selection().ordered();
            let lead = lead
                .filter(|row| rows.contains(row))
                .or_else(|| rows.first().copied());

            match tab {
                WorkspaceTab::Table {
                    result,
                    selected_row,
                    draft,
                    pending_edits,
                    ..
                } => {
                    *selected_row = lead;
                    *draft = match result.as_ref() {
                        Some(view) if !rows.is_empty() => {
                            Some(RowDraft::from_rows(&rows, view, pending_edits))
                        }
                        _ => None,
                    };
                }
                // A query result has nothing staged against it: there is no
                // table to write the edit back to.
                WorkspaceTab::Sql {
                    result,
                    selected_row,
                    draft,
                    ..
                } => {
                    *selected_row = lead;
                    *draft = match result.as_ref() {
                        Some(view) if !rows.is_empty() => {
                            Some(RowDraft::from_rows(&rows, view, &[]))
                        }
                        _ => None,
                    };
                }
            }
            draft_is_open(tab)
        };

        if opened {
            self.detail_open = true;
        }
        self.detail_input = None;
        self.detail_value_menu = None;
        cx.notify();
    }

    /// Fold the open draft away and rebuild it over the new selection.
    ///
    /// The order matters: what was typed has to reach `pending_edits` before
    /// the editors holding it are replaced.
    fn restage_draft(&mut self, cx: &mut Context<Self>) {
        self.stash_current_draft(cx);
        self.rebuild_draft(None, cx);
    }

    /// Fold the open draft into `pending_edits`.
    ///
    /// The draft's rows are cleared out and restaged rather than merged into:
    /// `to_pending_batch` returns the whole of what each row should end up
    /// with — including the columns it deliberately left alone — so anything
    /// left over from before would be counted twice. It is also what lets a
    /// field typed back to its stored value un-stage itself.
    fn stash_current_draft(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            draft,
            result,
            pending_edits,
            pending_deletes,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };
        let Some(draft_ref) = draft.as_ref() else {
            return;
        };
        let Some(view) = result.as_ref() else {
            return;
        };

        let keys = draft_ref.row_keys(view);
        let outcome = draft_ref.to_pending_batch(view, pending_edits);

        match outcome {
            Ok(mut edits) => {
                // A row on its way out has nothing left to update.
                edits.retain(|edit| {
                    !pending_deletes
                        .iter()
                        .any(|staged| staged.matches_pk(&edit.pk))
                });
                pending_edits.retain(|edit| !keys.iter().any(|pk| edit.matches_pk(pk)));
                pending_edits.extend(edits);
            }
            Err(message) => {
                if let Some(draft) = draft.as_mut() {
                    draft.message = Some((false, message));
                }
                cx.notify();
            }
        }
    }

    // -- grid selection -----------------------------------------------------

    /// Whether the grid's shortcuts apply right now.
    ///
    /// Clicking a row hands the keyboard to the detail sidebar so the fields
    /// are typeable, and ⌘A there still means "the rows I clicked" until an
    /// actual field takes focus — otherwise selecting rows and pressing ⌘A
    /// would do nothing, which reads as the shortcut being broken.
    pub(crate) fn grid_owns_keys(&self) -> bool {
        self.focus == Focus::Grid
            || (self.focus == Focus::Detail && self.detail_input.is_none())
    }

    /// Whether a text editor currently owns ⌘Z for its own undo stack.
    ///
    /// Everywhere else ⌘Z means "discard the staged batch". The batch belongs
    /// to the tab rather than to any one surface, so it has to work from the
    /// tree as well as the grid — the change bubble is on screen either way,
    /// and a shortcut that does nothing while the thing it undoes is visible
    /// reads as broken.
    pub(crate) fn text_undo_has_focus(&self) -> bool {
        match self.focus {
            Focus::Editor | Focus::Filter | Focus::PageSize | Focus::SidebarSearch => true,
            Focus::Detail => self.detail_input.is_some(),
            Focus::Sidebar | Focus::Grid => false,
        }
    }

    fn result_row_count(&self) -> usize {
        self.tabs
            .active()
            .and_then(|tab| tab.result())
            .map(|view| view.set.rows.len())
            .unwrap_or(0)
    }

    pub(crate) fn select_all_rows(&mut self, cx: &mut Context<Self>) {
        let count = self.result_row_count();
        if count == 0 {
            return;
        }
        self.stash_current_draft(cx);
        if let Some(tab) = self.tabs.active_mut() {
            tab.selection_mut().select_all(count);
        }
        self.rebuild_draft(None, cx);
        self.focus = Focus::Grid;
        self.status = Status::info(format!("{count} row(s) selected"));
        cx.notify();
    }

    pub(crate) fn clear_row_selection(&mut self, cx: &mut Context<Self>) {
        self.stash_current_draft(cx);
        if let Some(tab) = self.tabs.active_mut() {
            tab.selection_mut().clear();
        }
        self.rebuild_draft(None, cx);
        cx.notify();
    }

    /// A press on a grid row. `column` is `Some` when the press landed on a
    /// cell, which additionally makes that cell the one shown in full below.
    pub(crate) fn grid_pointer_down(
        &mut self,
        row: usize,
        column: Option<usize>,
        modifiers: gpui::Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.close_context_menu(cx);

        // Shift and ⌘ are choosing a set of rows, not changing which row the
        // detail sidebar is describing — so neither disturbs the open draft.
        if modifiers.shift {
            self.stash_current_draft(cx);
            if let Some(tab) = self.tabs.active_mut() {
                tab.selection_mut().extend_to(row);
            }
            self.rebuild_draft(Some(row), cx);
            self.row_drag = Some(row);
            self.focus = Focus::Grid;
            cx.notify();
            return;
        }
        if modifiers.platform {
            self.stash_current_draft(cx);
            if let Some(tab) = self.tabs.active_mut() {
                tab.selection_mut().toggle(row);
            }
            self.rebuild_draft(Some(row), cx);
            self.focus = Focus::Grid;
            cx.notify();
            return;
        }

        self.select_row(row, cx);
        match column {
            Some(column) => {
                self.selected_cell = Some((row, column));
                self.focus = Focus::Grid;
            }
            None => self.selected_cell = None,
        }
        self.row_drag = Some(row);
        cx.notify();
    }

    /// The pointer crossed a row with the button still down.
    pub(crate) fn grid_drag_over(&mut self, row: usize, cx: &mut Context<Self>) {
        // Comparing against the last row the drag reached is what keeps a
        // pointer wandering inside one row from rebuilding the range on every
        // mouse-move event.
        if self.row_drag.is_none() || self.row_drag == Some(row) {
            return;
        }
        self.row_drag = Some(row);
        if let Some(tab) = self.tabs.active_mut() {
            tab.selection_mut().extend_to(row);
        }
        self.focus = Focus::Grid;
        cx.notify();
    }

    pub(crate) fn end_row_drag(&mut self, cx: &mut Context<Self>) {
        if self.row_drag.take().is_none() {
            return;
        }
        // The draft is rebuilt on release rather than on every row the pointer
        // crosses: a drag over a few hundred rows would otherwise rebuild the
        // whole sidebar that many times on the way there.
        self.restage_draft(cx);
        cx.notify();
    }

    /// ⇧↑ / ⇧↓ — grow the selection a row at a time.
    pub(crate) fn extend_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.result_row_count();
        if count == 0 {
            return;
        }
        self.stash_current_draft(cx);
        let Some(tab) = self.tabs.active_mut() else {
            return;
        };
        let selection = tab.selection_mut();
        let from = selection
            .ordered()
            .last()
            .copied()
            .or_else(|| selection.anchor())
            .unwrap_or(0);
        let next = if delta < 0 {
            from.saturating_sub(delta.unsigned_abs())
        } else {
            (from + delta as usize).min(count - 1)
        };
        selection.extend_to(next);
        self.rebuild_draft(Some(next), cx);
        self.focus = Focus::Grid;
        cx.notify();
    }

    // -- staged deletes -----------------------------------------------------

    /// Stage every selected row for deletion. Nothing reaches the server until
    /// the batch is committed.
    pub(crate) fn delete_selected_rows(&mut self, cx: &mut Context<Self>) {
        // A new row was never on the server, so there is nothing to delete --
        // ⌘⌫ simply takes it back off the staging list.
        if let Some(index) = self.tabs.active().and_then(|tab| tab.editing_insert()) {
            self.remove_insert(index, cx);
            return;
        }

        // Fold a half-typed edit in first, so what the bubble lists is the
        // whole of what ⌘S will write.
        self.stash_current_draft(cx);

        match self.stage_selected_deletes() {
            Ok(0) => {}
            Ok(count) => {
                let plural = if count == 1 { "row" } else { "rows" };
                self.status =
                    Status::info(format!("{count} {plural} staged for deletion — ⌘S to commit"));
            }
            Err(message) => self.status = Status::error(message),
        }
        cx.notify();
    }

    fn stage_selected_deletes(&mut self) -> Result<usize, String> {
        // The keys are read off the result view first because staging them
        // needs a mutable borrow of the same tab.
        let keys = {
            let Some(WorkspaceTab::Table {
                result: Some(view),
                selection,
                ..
            }) = self.tabs.active()
            else {
                return Err("Deleting rows needs a table tab".into());
            };
            let rows = selection.ordered();
            if rows.is_empty() {
                return Err("Select a row first — ⌘A selects them all".into());
            }
            let mut keys = Vec::with_capacity(rows.len());
            for row in rows {
                let Some(values) = view.set.rows.get(row) else {
                    continue;
                };
                let pk = crate::tabs::row_pk(&view.set.columns, &values.0, &view.structure)
                    .map_err(|message| format!("Cannot delete: {message}"))?;
                let label = crate::tabs::pk_label(&pk);
                keys.push((pk, label));
            }
            keys
        };

        let Some(WorkspaceTab::Table {
            pending_edits,
            pending_deletes,
            change_bubble_expanded,
            ..
        }) = self.tabs.active_mut()
        else {
            return Ok(0);
        };

        let mut staged = 0usize;
        for (pk, label) in keys {
            // A row on its way out has nothing left to update. Dropping the
            // edit keeps the bubble from listing a change that will never be
            // written, and the UPDATE from running against a doomed row.
            pending_edits.retain(|edit| !edit.matches_pk(&pk));
            if pending_deletes.iter().any(|row| row.matches_pk(&pk)) {
                continue;
            }
            pending_deletes.push(crate::tabs::PendingRowDelete { pk, label });
            staged += 1;
        }
        if staged > 0 {
            *change_bubble_expanded = true;
        }
        Ok(staged)
    }

    // -- staged inserts -----------------------------------------------------

    /// Stage a blank new row and open it for editing.
    ///
    /// The row is drawn under the real ones rather than hidden in a dialog, so
    /// it is filled in with the same columns, in the same order, in the same
    /// sidebar as every other row on screen.
    pub(crate) fn add_row(&mut self, cx: &mut Context<Self>) {
        self.stash_current_draft(cx);

        let (columns, structure) = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                result: Some(view), ..
            }) => (view.set.columns.clone(), view.structure.clone()),
            Some(WorkspaceTab::Table { result: None, .. }) => {
                self.status = Status::info("Load the table first");
                cx.notify();
                return;
            }
            _ => {
                self.status = Status::info("New rows need a table tab");
                cx.notify();
                return;
            }
        };

        let Some(WorkspaceTab::Table {
            pending_inserts,
            editing_insert,
            selection,
            draft,
            selected_row,
            change_bubble_expanded,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };

        pending_inserts.push(crate::tabs::PendingRowInsert::blank(&columns, &structure));
        *editing_insert = Some(pending_inserts.len() - 1);
        *change_bubble_expanded = true;
        // A new row is not one of the stored ones, so nothing in the grid is
        // selected while it is being filled in.
        selection.clear();
        *selected_row = None;
        *draft = None;

        self.detail_open = true;
        self.detail_input = None;
        self.detail_value_menu = None;
        self.focus = Focus::Detail;
        self.status = Status::info("New row staged — ⌘S to commit");
        cx.notify();
    }

    /// Open one of the staged inserts in the detail sidebar.
    pub(crate) fn edit_insert(&mut self, index: usize, cx: &mut Context<Self>) {
        self.stash_current_draft(cx);
        let Some(WorkspaceTab::Table {
            pending_inserts,
            editing_insert,
            selection,
            draft,
            selected_row,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };
        if index >= pending_inserts.len() {
            return;
        }
        *editing_insert = Some(index);
        selection.clear();
        *selected_row = None;
        *draft = None;

        self.detail_open = true;
        self.detail_input = None;
        self.focus = Focus::Detail;
        cx.notify();
    }

    /// Drop a staged insert. This is what ⌘⌫ means on a new row: it was never
    /// on the server, so there is nothing to delete — it is simply unstaged.
    pub(crate) fn remove_insert(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            pending_inserts,
            editing_insert,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };
        if index >= pending_inserts.len() {
            return;
        }
        pending_inserts.remove(index);
        *editing_insert = match *editing_insert {
            Some(open) if open == index => None,
            // Everything after the removed row shifted down by one.
            Some(open) if open > index => Some(open - 1),
            other => other,
        };
        self.status = Status::info("New row discarded");
        cx.notify();
    }

    /// How many new rows are staged, whether or not they parse yet.
    pub(crate) fn staged_insert_count(&self) -> usize {
        self.tabs
            .active()
            .map(|tab| tab.pending_inserts().len())
            .unwrap_or(0)
    }

    /// The staged inserts as values ready to bind, or the first parse failure.
    pub(crate) fn collect_batch_inserts(&self) -> Result<Vec<dbui_app::RowInsert>, String> {
        let Some(WorkspaceTab::Table {
            pending_inserts, ..
        }) = self.tabs.active()
        else {
            return Ok(Vec::new());
        };
        pending_inserts
            .iter()
            .map(|row| {
                row.to_values()
                    .map(|values| dbui_app::RowInsert { values })
            })
            .collect()
    }

    /// Copy the selected rows to the clipboard.
    ///
    /// Falls back to the whole page when nothing is selected: "copy" with no
    /// selection meaning "copy nothing" is a shortcut that looks broken.
    pub(crate) fn copy_selected_rows(
        &mut self,
        format: crate::row_export::RowFormat,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let Some(view) = tab.result() else {
            self.status = Status::info("Nothing to copy");
            cx.notify();
            return;
        };

        let selected = tab.selection().ordered();
        let rows: Vec<Vec<dbui_app::domain::Value>> = if selected.is_empty() {
            view.set.rows.iter().map(|row| row.0.clone()).collect()
        } else {
            selected
                .iter()
                .filter_map(|index| view.set.rows.get(*index))
                .map(|row| row.0.clone())
                .collect()
        };

        if rows.is_empty() {
            self.status = Status::info("Nothing to copy");
            cx.notify();
            return;
        }

        let columns = view.set.columns.clone();
        let table = tab.table_ref().cloned();
        let driver = self
            .active_driver_kind()
            .unwrap_or(dbui_app::domain::Driver::Postgres);
        let text = crate::row_export::render(format, &columns, &rows, driver, table.as_ref());

        let count = rows.len();
        let plural = if count == 1 { "row" } else { "rows" };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.status = Status::info(format!("Copied {count} {plural}"));
        cx.notify();
    }

    /// Rows staged for deletion on the active tab.
    pub(crate) fn collect_batch_deletes(&self) -> Vec<crate::tabs::PendingRowDelete> {
        self.tabs
            .active()
            .map(|tab| tab.pending_deletes().to_vec())
            .unwrap_or_default()
    }

    pub(crate) fn toggle_change_bubble(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Table {
            change_bubble_expanded,
            ..
        }) = self.tabs.active_mut()
        {
            *change_bubble_expanded = !*change_bubble_expanded;
        }
        cx.notify();
    }

    /// Effective batch: pending edits plus the current dirty draft (if any).
    pub(crate) fn collect_batch_edits(&self) -> Vec<crate::tabs::PendingRowEdit> {
        let Some(WorkspaceTab::Table {
            draft,
            result,
            pending_edits,
            pending_deletes,
            ..
        }) = self.tabs.active()
        else {
            return Vec::new();
        };

        let mut batch = pending_edits.clone();
        if let (Some(draft), Some(view)) = (draft.as_ref(), result.as_ref()) {
            // Only on success: a draft mid-edit that will not parse must not
            // take the rest of the staged batch off the screen with it.
            if let Ok(edits) = draft.to_pending_batch(view, pending_edits) {
                let keys = draft.row_keys(view);
                batch.retain(|edit| !keys.iter().any(|pk| edit.matches_pk(pk)));
                batch.extend(edits);
            }
        }
        batch.retain(|edit| {
            !pending_deletes
                .iter()
                .any(|staged| staged.matches_pk(&edit.pk))
        });
        batch
    }

    /// Throw away everything staged on the active tab.
    ///
    /// This is what ⌘Z means once the grid has the keyboard: there is no
    /// step-by-step undo of a staged batch, and the batch is the thing the
    /// user is looking at. Inside a text field ⌘Z still undoes typing —
    /// see [`grid_owns_keys`](DbUi::grid_owns_keys).
    pub(crate) fn discard_pending_edits(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Table { saving: true, .. })
        ) {
            return;
        }

        // Counted before anything is cleared, and including the open draft:
        // what is being thrown away is everything ⌘S would have written.
        let discarded = self.collect_batch_edits().len()
            + self.collect_batch_deletes().len()
            + self.staged_insert_count();
        if discarded == 0 {
            // Saying "changes discarded" with nothing staged would be
            // reporting an undo that never happened.
            return;
        }

        if let Some(WorkspaceTab::Table {
            draft,
            result,
            pending_edits,
            pending_deletes,
            pending_inserts,
            editing_insert,
            change_bubble_expanded,
            ..
        }) = self.tabs.active_mut()
        {
            pending_edits.clear();
            pending_deletes.clear();
            pending_inserts.clear();
            *editing_insert = None;
            *change_bubble_expanded = false;
            if let (Some(draft), Some(view)) = (draft.as_mut(), result.as_ref()) {
                draft.reset(view);
            }
        }

        let plural = if discarded == 1 { "change" } else { "changes" };
        self.status = Status::info(format!("Discarded {discarded} {plural}"));
        cx.notify();
    }

    /// Commit everything staged on the active table tab in one transaction.
    ///
    /// Edits and deletions go together because that is what the user staged:
    /// splitting them into two round trips would let one succeed while the
    /// other rolls back, which is exactly the state a batch editor exists to
    /// prevent.
    pub(crate) fn save_pending_edits(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Table { saving: true, .. })
        ) {
            return;
        }

        // Fold the open draft in first so Save catches in-progress edits.
        self.stash_current_draft(cx);

        // The staged inserts are turned into values here rather than later:
        // a row that will not parse has to stop the commit before anything is
        // sent, not halfway through the transaction.
        let inserts = match self.collect_batch_inserts() {
            Ok(inserts) => inserts,
            Err(message) => {
                self.status = Status::error(message);
                cx.notify();
                return;
            }
        };

        let (table, edits, deletes, tab_id) = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                id,
                table,
                pending_edits,
                pending_deletes,
                ..
            }) => (
                table.clone(),
                pending_edits.clone(),
                pending_deletes.clone(),
                *id,
            ),
            // ⌘S is a global shortcut, so it lands on tabs with nothing to
            // commit. Saying so beats a silent no-op.
            Some(WorkspaceTab::Sql { .. }) => {
                self.status = Status::info("Nothing to commit on a query tab");
                cx.notify();
                return;
            }
            None => return,
        };

        if edits.is_empty() && deletes.is_empty() && inserts.is_empty() {
            self.status = Status::info("No changes to commit");
            cx.notify();
            return;
        }

        // Checked after the batch so an unsaved-but-disconnected tab says the
        // useful thing rather than "no changes".
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let count = edits.len() + deletes.len() + inserts.len();
        if let Some(WorkspaceTab::Table { saving, .. }) = self.tabs.get_mut(tab_id) {
            *saving = true;
        }
        self.status = Status::busy(format!("Committing {count} change(s)…"));
        cx.notify();

        let runtime = self.runtime.clone();
        let batch = dbui_app::RowBatch {
            inserts,
            updates: edits
                .iter()
                .map(|edit| RowUpdate {
                    pk: edit.pk.clone(),
                    changes: edit
                        .changes
                        .iter()
                        .map(|c| (c.column.clone(), c.new_value.clone()))
                        .collect(),
                })
                .collect(),
            deletes: deletes
                .iter()
                .map(|row| dbui_app::RowDelete { pk: row.pk.clone() })
                .collect(),
        };

        cx.spawn(async move |this, cx| {
            let landed = commands::apply_changes(&runtime, driver, table, batch).await;

            this.update(cx, |this, cx| {
                if let Some(WorkspaceTab::Table { saving, .. }) = this.tabs.get_mut(tab_id) {
                    *saving = false;
                }
                let is_active = this.tabs.active_id() == Some(tab_id);
                match landed {
                    Some(Ok(saved)) => {
                        if let Some(WorkspaceTab::Table {
                            pending_edits,
                            pending_deletes,
                            pending_inserts,
                            editing_insert,
                            change_bubble_expanded,
                            selection,
                            draft,
                            ..
                        }) = this.tabs.get_mut(tab_id)
                        {
                            pending_edits.clear();
                            pending_deletes.clear();
                            pending_inserts.clear();
                            *editing_insert = None;
                            *change_bubble_expanded = false;
                            // Row indices mean nothing once the rows below a
                            // deleted one have moved up.
                            selection.clear();
                            if let Some(draft) = draft.as_mut() {
                                draft.message = Some((true, "Saved".into()));
                            }
                        }
                        if is_active {
                            this.status = Status::info(format!("Committed {saved} change(s)"));
                        }
                        this.load_table(tab_id, cx);
                    }
                    Some(Err(error)) => {
                        // Transaction rolled back — leave everything staged.
                        if is_active {
                            this.status = Status::error(error.to_string());
                        }
                        cx.notify();
                    }
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    /// Move the selected result row by `delta` (−1 up, +1 down).
    pub(crate) fn move_selected_row(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.tabs.active().and_then(|tab| tab.result()) else {
            return;
        };
        let row_count = view.set.rows.len();
        if row_count == 0 {
            return;
        }

        let current = self
            .tabs
            .active()
            .and_then(|tab| tab.selected_row())
            .or_else(|| self.selected_cell.map(|(row, _)| row));

        let next = match current {
            None if delta > 0 => 0,
            None => return,
            Some(row) if delta < 0 => row.saturating_sub(delta.unsigned_abs()),
            Some(row) => (row + delta as usize).min(row_count - 1),
        };

        if current == Some(next) {
            return;
        }

        let stay_on_grid = self.focus == Focus::Grid;
        let column = self.selected_cell.map(|(_, column)| column);

        self.select_row(next, cx);
        if stay_on_grid {
            if let Some(column) = column {
                self.selected_cell = Some((next, column));
            }
            self.focus = Focus::Grid;
            cx.notify();
        }
    }

    // -- the connection form ----------------------------------------------

    pub(crate) fn open_new_connection(&mut self, cx: &mut Context<Self>) {
        self.connection_picker_open = false;
        self.modal = Some(ConnectionForm::new());
        self.focus = Focus::Sidebar;
        cx.notify();
    }

    /// Pull PostgreSQL / MySQL connections out of TablePlus's plist + keychain.
    pub(crate) fn import_tableplus_connections(&mut self, cx: &mut Context<Self>) {
        self.connection_picker_open = false;
        let existing = self.workspace.configs();
        match dbui_app::import_from_tableplus(&existing) {
            Ok(report) => {
                let summary = report.summary();
                let added = report.imported.len();
                for config in report.imported {
                    self.workspace.add(config);
                }
                if added > 0 {
                    self.persist_connections();
                }
                self.status = Status::info(summary);
            }
            Err(error) => {
                self.status = Status::error(error.to_string());
            }
        }
        cx.notify();
    }

    pub(crate) fn edit_connection(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        self.connection_picker_open = false;
        if let Some(entry) = self.workspace.get(id) {
            self.modal = Some(ConnectionForm::editing(entry.config.clone()));
            cx.notify();
        }
    }

    pub(crate) fn toggle_connection_picker(&mut self, cx: &mut Context<Self>) {
        self.connection_picker_open = !self.connection_picker_open;
        cx.notify();
    }

    pub(crate) fn close_connection_picker(&mut self, cx: &mut Context<Self>) {
        if self.connection_picker_open {
            self.connection_picker_open = false;
            cx.notify();
        }
    }

    pub(crate) fn pick_connection(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        self.connection_picker_open = false;
        self.open_connection_tab(id, cx);
    }

    pub(crate) fn close_modal(&mut self, cx: &mut Context<Self>) {
        self.modal = None;
        cx.notify();
    }

    pub(crate) fn save_connection(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.modal.as_mut() else {
            return;
        };
        let config = form.to_config();

        let problems = config.validate();
        if !problems.is_empty() {
            form.set_message(false, problems.join(", "));
            cx.notify();
            return;
        }

        let id = config.id;
        let existing = self.workspace.get(id).is_some();
        if existing {
            if let Some(entry) = self.workspace.get_mut(id) {
                entry.config = config;
                if entry.status.is_connected() {
                    entry.disconnect();
                }
            }
        } else {
            self.workspace.add(config);
        }

        self.modal = None;
        self.persist_connections();
        self.connect(id, cx);
        cx.notify();
    }

    pub(crate) fn test_connection(&mut self, cx: &mut Context<Self>) {
        let Some(form) = self.modal.as_mut() else {
            return;
        };
        let config = form.to_config();

        let problems = config.validate();
        if !problems.is_empty() {
            form.set_message(false, problems.join(", "));
            cx.notify();
            return;
        }

        form.testing = true;
        form.set_message(true, "Testing…");

        let task = commands::test_connection(&self.runtime, config);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if let Some(form) = this.modal.as_mut() {
                    form.testing = false;
                    match landed {
                        Some(Ok(version)) => {
                            form.set_message(true, format!("Connected — {version}"))
                        }
                        Some(Err(error)) => form.set_message(false, error.to_string()),
                        None => form.set_message(false, "Test cancelled"),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn remove_connection(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        if let Some(entry) = self.workspace.get(id) {
            if let Some(driver) = entry.status.driver().cloned() {
                commands::disconnect(&self.runtime, driver);
            }
        }
        let was_active = self.workspace.active_id() == Some(id);
        self.workspace.remove(id);
        store::delete_password(id);
        self.stashed_tabs.remove(&id);

        // Deleting the connection that was in front leaves its tabs pointing
        // at a server that no longer exists; the promoted tab brings its own.
        if was_active {
            self.tabs = self
                .workspace
                .active_id()
                .and_then(|next| self.stashed_tabs.remove(&next))
                .unwrap_or_default();
            self.workspace.open_table =
                self.tabs.active().and_then(|tab| tab.table_ref().cloned());
            self.selected_cell = None;
        }

        self.persist_connections();
        self.persist_session();
        cx.notify();
        if was_active {
            self.load_active_table_if_empty(cx);
        }
    }

    fn persist_connections(&mut self) {
        let configs = self.workspace.configs();
        let result = store::connections_path().and_then(|path| store::save(&path, &configs));
        if let Err(error) = result {
            self.status = Status::error(format!("Could not save connections: {error}"));
        }
    }

    // -- the session --------------------------------------------------------

    /// What is open right now, in the shape that survives a restart.
    pub(crate) fn session_snapshot(&self) -> Session {
        let active_id = self.workspace.active_id();
        let tabs = self
            .workspace
            .open_ids()
            .iter()
            .map(|id| {
                let (tabs, active_tab) = if active_id == Some(*id) {
                    self.tabs.to_saved()
                } else {
                    self.stashed_tabs
                        .get(id)
                        .map(Tabs::to_saved)
                        .unwrap_or_default()
                };
                SavedConnectionTab {
                    connection: *id,
                    tabs,
                    active_tab,
                    expanded: self
                        .workspace
                        .get(*id)
                        .map(|entry| entry.expanded.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();

        Session {
            tabs,
            active: self.workspace.active_index().unwrap_or(0),
        }
    }

    /// Write the session out.
    ///
    /// Called on every structural change — a tab opened, closed or switched —
    /// and once more on quit, which is what catches SQL text typed since. A
    /// failure here is deliberately silent: the session is a convenience, and
    /// an unwritable one is not worth taking over the status bar that is
    /// describing the user's actual query.
    pub(crate) fn persist_session(&self) {
        let session = self.session_snapshot();
        let _ = session::session_path().and_then(|path| session::save(&path, &session));
    }

    /// Apply a session read from disk: rebuild the tab bar, restore each
    /// connection's tabs, and hand back the connection to connect.
    ///
    /// Only the front tab's connection is returned. Restoring is not a reason
    /// to dial every server the user has ever saved — including the production
    /// one they left open last week.
    pub fn restore_session(&mut self, session: &Session) -> Option<ConnectionId> {
        if session.is_empty() {
            return self.workspace.active_id();
        }

        self.workspace
            .restore_open(session.tabs.iter().map(|tab| tab.connection), session.active);

        let active = self.workspace.active_id();
        for saved in &session.tabs {
            if !self.workspace.is_open(saved.connection) {
                continue;
            }
            let tabs = Tabs::from_saved(&saved.tabs, saved.active_tab);
            if Some(saved.connection) == active {
                self.tabs = tabs;
            } else {
                self.stashed_tabs.insert(saved.connection, tabs);
            }
            // Held until the connection opens: the tree cannot draw a folder
            // before the catalog naming it has arrived, and `connect` keeps
            // whatever is here rather than defaulting to the first schema.
            if let Some(entry) = self.workspace.get_mut(saved.connection) {
                entry.expanded.clone_from(&saved.expanded);
            }
        }

        self.workspace.open_table = self.tabs.active().and_then(|tab| tab.table_ref().cloned());
        active
    }

    // -- keyboard ----------------------------------------------------------

    pub(crate) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let keystroke = &event.keystroke;
        let key = keystroke.key.as_str();
        let command = keystroke.modifiers.platform;
        let shift = keystroke.modifiers.shift;
        let alt = keystroke.modifiers.alt;

        if self.palette.is_some() {
            self.handle_palette_key(keystroke, cx);
            return;
        }

        // A typed confirmation owns the keyboard: the whole point is that
        // nothing else can be triggered by accident while it is up.
        if self.confirm.is_some() {
            self.handle_confirm_key(keystroke, cx);
            return;
        }

        if self.context_menu.is_some() {
            match key {
                "escape" => {
                    self.close_context_menu(cx);
                    return;
                }
                "up" | "down" | "enter" => {
                    self.handle_context_menu_key(key, cx);
                    return;
                }
                _ => self.close_context_menu(cx),
            }
        }

        if self.modal.is_some() {
            match key {
                "escape" => self.close_modal(cx),
                "enter" if !command => {
                    let action = self
                        .modal
                        .as_ref()
                        .map(|form| form.focused_action())
                        .unwrap_or(FormAction::Save);
                    match action {
                        FormAction::Cancel => self.close_modal(cx),
                        FormAction::Test => self.test_connection(cx),
                        FormAction::Field | FormAction::Save => self.save_connection(cx),
                    }
                }
                _ => {
                    if let Some(form) = self.modal.as_mut() {
                        form.handle_key(keystroke, cx);
                    }
                    cx.notify();
                }
            }
            return;
        }

        if self.connection_picker_open {
            if key == "escape" {
                self.close_connection_picker(cx);
            }
            return;
        }

        // ⌃⇥ / ⌃⇧⇥ — claim before any surface that might treat Tab as indent.
        if key == "tab" && keystroke.modifiers.control {
            if shift {
                self.prev_tab(cx);
            } else {
                self.next_tab(cx);
            }
            return;
        }

        // Global ⌘ shortcuts (also registered as GPUI actions for menus).
        if command {
            match key {
                "p" if shift => {
                    self.open_palette(PaletteKind::Actions, cx);
                    return;
                }
                "t" if shift => {
                    self.open_palette(PaletteKind::Themes, cx);
                    return;
                }
                "p" => {
                    self.open_palette(PaletteKind::GoToTable, cx);
                    return;
                }
                // ⌘⇧F searches the tree for a table, ⌘F filters the rows of
                // the one already open. The shifted arm has to come first.
                "f" if shift => {
                    self.focus_sidebar_search(cx);
                    return;
                }
                "f" => {
                    self.cmd_find(cx);
                    return;
                }
                "s" => {
                    self.save_pending_edits(cx);
                    return;
                }
                // Grid shortcuts, claimed only when the grid has the keyboard
                // so ⌘A in the SQL editor still selects its text.
                "a" if self.grid_owns_keys() => {
                    self.select_all_rows(cx);
                    return;
                }
                "backspace" | "delete" if self.grid_owns_keys() => {
                    self.delete_selected_rows(cx);
                    return;
                }
                // ⌘C over the grid copies rows; inside an editor it is still
                // "copy the selected text".
                "c" if !self.text_undo_has_focus() => {
                    self.copy_selected_rows(crate::row_export::RowFormat::Tsv, cx);
                    return;
                }
                // Unshifted only: ⌘⇧Z is redo, and a staged batch has nothing
                // to redo — so it falls through rather than discarding twice.
                "z" if !shift && !self.text_undo_has_focus() => {
                    self.discard_pending_edits(cx);
                    return;
                }
                "enter" if shift => {
                    self.run_all_queries(cx);
                    return;
                }
                "enter" => {
                    self.run_query(cx);
                    return;
                }
                "n" => {
                    self.open_new_connection(cx);
                    return;
                }
                "r" => {
                    self.refresh_result(cx);
                    return;
                }
                "e" => {
                    self.open_sql_tab(cx);
                    return;
                }
                // ⌘⇧W closes the whole connection, ⌘W one table tab. The
                // shifted arm has to come first or ⌘W swallows both.
                "w" if shift => {
                    self.close_active_connection_tab(cx);
                    return;
                }
                "w" => {
                    self.close_active_tab(cx);
                    return;
                }
                "k" => {
                    if let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() {
                        editor.clear();
                    }
                    cx.notify();
                    return;
                }
                // ⌥ lifts the bracket keys from paging to the connection bar.
                "[" if alt => {
                    self.cycle_connection_tab(false, cx);
                    return;
                }
                "]" if alt => {
                    self.cycle_connection_tab(true, cx);
                    return;
                }
                "[" => {
                    self.page(false, cx);
                    return;
                }
                "]" => {
                    self.page(true, cx);
                    return;
                }
                _ => {}
            }
        }

        if self.focus == Focus::Detail {
            if key == "tab" && !command {
                self.cycle_detail_focus(shift, cx);
                return;
            }
            // Row chrome (no field focused): ↑/↓ walk the grid selection.
            if !command && self.detail_input.is_none() {
                match key {
                    "up" if shift => {
                        self.extend_selection(-1, cx);
                        return;
                    }
                    "down" if shift => {
                        self.extend_selection(1, cx);
                        return;
                    }
                    "up" => {
                        self.move_selected_row(-1, cx);
                        return;
                    }
                    "down" => {
                        self.move_selected_row(1, cx);
                        return;
                    }
                    _ => {}
                }
            }
            // A staged insert owns the sidebar while it is open.
            if let Some(DetailInput::Field(index)) = self.detail_input {
                let typed = match self.tabs.active_mut() {
                    Some(WorkspaceTab::Table {
                        pending_inserts,
                        editing_insert: Some(open),
                        ..
                    }) => pending_inserts
                        .get_mut(*open)
                        .and_then(|row| row.fields.get_mut(index))
                        .map(|(_, input, _)| input.handle_key(keystroke, cx))
                        .unwrap_or(false),
                    _ => false,
                };
                if typed {
                    cx.notify();
                    return;
                }
            }

            let handled = match self.tabs.active_mut() {
                Some(WorkspaceTab::Table {
                    draft: Some(draft), ..
                })
                | Some(WorkspaceTab::Sql {
                    draft: Some(draft), ..
                }) => match self.detail_input {
                    Some(DetailInput::Search) => draft.field_search.handle_key(keystroke, cx),
                    Some(DetailInput::Field(index)) => draft
                        .fields
                        .get_mut(index)
                        .map(|(_, input, _)| input.handle_key(keystroke, cx))
                        .unwrap_or(false),
                    None => false,
                },
                _ => false,
            };
            if handled {
                cx.notify();
                return;
            }
        }

        if self.focus == Focus::Filter {
            if key == "tab" && !command {
                let current = self.filter_focus.unwrap_or(FilterFocus::Where);
                self.filter_focus = Some(current.cycle(shift));
                cx.notify();
                return;
            }
            match self.filter_focus {
                Some(FilterFocus::Apply) => {
                    if key == "enter" && !command {
                        self.apply_filters(cx);
                    }
                    return;
                }
                Some(FilterFocus::Clear) => {
                    if key == "enter" && !command {
                        self.clear_filters(cx);
                    }
                    return;
                }
                Some(FilterFocus::Where) | None => {
                    if key == "enter" && !command {
                        self.apply_filters(cx);
                        return;
                    }
                    if let Some(WorkspaceTab::Table { where_draft, .. }) = self.tabs.active_mut() {
                        if where_draft.handle_key(keystroke, cx) {
                            cx.notify();
                            return;
                        }
                    }
                }
            }
        }

        if self.focus == Focus::PageSize && self.page_size_focus {
            if key == "enter" && !command {
                self.apply_page_size(cx);
                return;
            }
            if let Some(WorkspaceTab::Table {
                page_size_draft, ..
            }) = self.tabs.active_mut()
            {
                if page_size_draft.handle_key(keystroke, cx) {
                    cx.notify();
                    return;
                }
            }
        }

        if self.focus == Focus::Editor {
            // Completion popup owns navigation while open.
            if self.completion.is_some() {
                match key {
                    "escape" => {
                        self.dismiss_completion(cx);
                        return;
                    }
                    "up" if !command => {
                        if let Some(popup) = self.completion.as_mut() {
                            popup.select_delta(-1);
                        }
                        cx.notify();
                        return;
                    }
                    "down" if !command => {
                        if let Some(popup) = self.completion.as_mut() {
                            popup.select_delta(1);
                        }
                        cx.notify();
                        return;
                    }
                    "enter" | "tab" if !command => {
                        self.accept_completion(cx);
                        return;
                    }
                    _ => {}
                }
            }

            if key == " " && keystroke.modifiers.control {
                self.trigger_completion(cx);
                return;
            }

            // Tab accepts the selected completion when the popup is open;
            // otherwise it falls through to the editor (indent).
            if key == "tab" && !command && !shift && self.completion.is_some() {
                self.accept_completion(cx);
                return;
            }

            if let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() {
                if editor.handle_key(keystroke, cx) {
                    let should_refresh = self.completion.is_some()
                        && !command
                        && (key.len() == 1 || key == "backspace" || key == "delete");
                    if should_refresh {
                        self.trigger_completion(cx);
                    } else if self.completion.is_some()
                        && (key.len() == 1 || key == "backspace" || key == "delete")
                    {
                        // Unreachable when should_refresh is true; kept for clarity.
                        self.dismiss_completion(cx);
                    } else {
                        cx.notify();
                    }
                    return;
                }
            }
        }

        if self.focus == Focus::SidebarSearch {
            match key {
                "escape" => {
                    self.dismiss_sidebar_search(cx);
                    return;
                }
                "down" if !command => {
                    self.enter_filtered_tree(cx);
                    return;
                }
                "enter" if !command => {
                    self.open_first_filtered_table(cx);
                    return;
                }
                _ => {
                    if self.sidebar_filter.handle_key(keystroke, cx) {
                        // The cursor may be sitting on a row the filter just
                        // hid; a cursor on nothing draws nothing.
                        let visible = self.sidebar_visible_items();
                        if self
                            .sidebar_cursor
                            .as_ref()
                            .is_some_and(|item| !visible.contains(item))
                        {
                            self.sidebar_cursor = None;
                        }
                        cx.notify();
                        return;
                    }
                }
            }
        }

        if self.focus == Focus::Sidebar && !command {
            match key {
                "up" => {
                    self.sidebar_move(-1, cx);
                    return;
                }
                "down" => {
                    self.sidebar_move(1, cx);
                    return;
                }
                "left" => {
                    self.sidebar_expand(false, cx);
                    return;
                }
                "right" => {
                    self.sidebar_expand(true, cx);
                    return;
                }
                "enter" => {
                    self.sidebar_activate(cx);
                    return;
                }
                _ => {}
            }
        }

        if self.focus == Focus::Grid && !command {
            match key {
                // Shift grows the range from the anchor; a bare arrow moves
                // the one selected row, which is also what collapses a range.
                "up" if shift => {
                    self.extend_selection(-1, cx);
                    return;
                }
                "down" if shift => {
                    self.extend_selection(1, cx);
                    return;
                }
                "up" => {
                    self.move_selected_row(-1, cx);
                    return;
                }
                "down" => {
                    self.move_selected_row(1, cx);
                    return;
                }
                _ => {}
            }
        }

        if key == "escape" {
            if self.detail_value_menu.is_some() {
                self.detail_value_menu = None;
                cx.notify();
                return;
            }
            // A multi-row selection is a mode of sorts, and Escape is how
            // every other mode in this window is left.
            if self
                .tabs
                .active()
                .is_some_and(|tab| tab.selection().len() > 1)
            {
                self.clear_row_selection(cx);
                return;
            }
            self.focus = Focus::Sidebar;
            self.detail_input = None;
            self.detail_value_menu = None;
            self.filter_focus = None;
            self.page_size_focus = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_detail_value_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        self.detail_value_menu = if self.detail_value_menu == Some(index) {
            None
        } else {
            Some(index)
        };
        self.detail_input = Some(DetailInput::Field(index));
        self.focus = Focus::Detail;
        cx.notify();
    }

    pub(crate) fn close_detail_value_menu(&mut self, cx: &mut Context<Self>) {
        if self.detail_value_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Apply a special write token (`NULL` / `EMPTY` / `DEFAULT`) to a detail field.
    pub(crate) fn set_detail_special_value(
        &mut self,
        index: usize,
        token: &'static str,
        cx: &mut Context<Self>,
    ) {
        let applied = match self.tabs.active_mut() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            })
            | Some(WorkspaceTab::Sql {
                draft: Some(draft), ..
            }) => {
                if let Some((_, input, is_pk)) = draft.fields.get_mut(index) {
                    if *is_pk {
                        false
                    } else {
                        input.set_text(token);
                        true
                    }
                } else {
                    false
                }
            }
            _ => false,
        };
        self.detail_value_menu = None;
        if applied {
            self.detail_input = Some(DetailInput::Field(index));
            self.focus = Focus::Detail;
        }
        cx.notify();
    }
}

fn draft_is_open(tab: &WorkspaceTab) -> bool {
    matches!(
        tab,
        WorkspaceTab::Table { draft: Some(_), .. } | WorkspaceTab::Sql { draft: Some(_), .. }
    )
}

fn table_summary(contents: &dbui_app::TableContents) -> String {
    let shown = contents.rows.rows.len();
    let base = match contents.total_rows {
        Some(total) => format!(
            "Rows {}–{} of {}",
            contents.page.offset + 1,
            contents.page.offset + shown as u64,
            total
        ),
        None => format!("{shown} rows"),
    };
    if contents.where_clause.trim().is_empty() {
        base
    } else {
        format!("{base} · filtered")
    }
}

impl Render for DbUi {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if window.focused(cx).is_none() {
            window.focus(&self.focus_handle);
        }
        window.set_rem_size(metrics::rem_size());

        let modal = self.modal.is_some().then(|| self.render_modal(cx));
        let palette = self.render_palette(cx);
        let change_bubble = self.render_change_bubble(cx);
        let context_menu = self.render_context_menu(window, cx);
        let confirm = self.render_confirm(cx);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(self.theme.background)
            .text_color(self.theme.text)
            .font_family(metrics::UI_FONT)
            .text_size(metrics::text_size())
            .key_context("DbUi")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            // The pointer leaves the 5px grab strip on the first frame of a
            // drag, so the tracking lives on the root instead.
            .when(
                self.change_bubble_drag.is_some()
                    || self.editor_drag.is_some()
                    || self.row_drag.is_some(),
                |root| {
                    root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        if this.change_bubble_drag.is_some() {
                            this.drag_change_bubble(event.position.y, window, cx);
                        }
                        if this.editor_drag.is_some() {
                            this.drag_editor(event.position.y, window, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_change_bubble_drag(cx);
                            this.end_editor_drag(cx);
                            this.end_row_drag(cx);
                        }),
                    )
                    // Releasing outside the window has to end the drag too, or
                    // the next pointer move over the grid extends a selection
                    // nobody is still holding.
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_change_bubble_drag(cx);
                            this.end_editor_drag(cx);
                            this.end_row_drag(cx);
                        }),
                    )
                },
            )
            .on_action(cx.listener(|this, _: &crate::NewConnection, _window, cx| {
                this.open_new_connection(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::GoToTable, _window, cx| {
                this.open_palette(PaletteKind::GoToTable, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::CommandPalette, _window, cx| {
                this.open_palette(PaletteKind::Actions, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::ChooseTheme, _window, cx| {
                this.open_palette(PaletteKind::Themes, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::Find, _window, cx| this.cmd_find(cx)))
            .on_action(cx.listener(|this, _: &crate::SearchTables, _window, cx| {
                this.focus_sidebar_search(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::CommitChanges, _window, cx| {
                this.save_pending_edits(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::SelectAllRows, _window, cx| {
                this.select_all_rows(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::DeleteRows, _window, cx| {
                this.delete_selected_rows(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::DiscardChanges, _window, cx| {
                this.discard_pending_edits(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::OpenSql, _window, cx| this.open_sql_tab(cx)))
            .on_action(cx.listener(|this, _: &crate::Refresh, _window, cx| this.refresh_result(cx)))
            .on_action(cx.listener(|this, _: &crate::RunQuery, _window, cx| this.run_query(cx)))
            .on_action(
                cx.listener(|this, _: &crate::RunAllQueries, _window, cx| this.run_all_queries(cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::CloseTab, _window, cx| this.close_active_tab(cx)),
            )
            .on_action(cx.listener(|this, _: &crate::NextTab, _window, cx| this.next_tab(cx)))
            .on_action(cx.listener(|this, _: &crate::PrevTab, _window, cx| this.prev_tab(cx)))
            .on_action(cx.listener(|this, _: &crate::CloseConnection, _window, cx| {
                this.close_active_connection_tab(cx)
            }))
            .on_action(cx.listener(|this, _: &crate::NextConnection, _window, cx| {
                this.cycle_connection_tab(true, cx)
            }))
            .on_action(cx.listener(|this, _: &crate::PrevConnection, _window, cx| {
                this.cycle_connection_tab(false, cx)
            }))
            .on_action(
                cx.listener(|this, _: &crate::SelectTab1, _window, cx| {
                    this.select_tab_number(1, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab2, _window, cx| {
                    this.select_tab_number(2, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab3, _window, cx| {
                    this.select_tab_number(3, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab4, _window, cx| {
                    this.select_tab_number(4, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab5, _window, cx| {
                    this.select_tab_number(5, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab6, _window, cx| {
                    this.select_tab_number(6, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab7, _window, cx| {
                    this.select_tab_number(7, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab8, _window, cx| {
                    this.select_tab_number(8, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::SelectTab9, _window, cx| {
                    this.select_tab_number(9, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &crate::ZoomIn, _window, cx| this.zoom_delta(1, cx)))
            .on_action(cx.listener(|this, _: &crate::ZoomOut, _window, cx| this.zoom_delta(-1, cx)))
            .on_action(
                cx.listener(|this, _: &crate::ZoomReset, _window, cx| this.zoom_delta(0, cx)),
            )
            .child(self.render_titlebar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(self.render_sidebar(window, cx))
                    .child(self.render_main(window, cx))
                    .child(self.render_detail_sidebar(cx)),
            )
            .children(change_bubble)
            .child(self.render_status_bar(cx))
            .children(modal)
            .children(palette)
            .children(context_menu)
            .children(confirm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dragging_the_bubble_edge_upward_makes_it_taller() {
        let viewport = px(800.);
        let start = px(BUBBLE_HEIGHT_DEFAULT);
        assert_eq!(bubble_height_for(start, px(60.), viewport), px(240.));
        assert_eq!(bubble_height_for(start, px(-60.), viewport), px(120.));
    }

    #[test]
    fn the_bubble_cannot_be_dragged_past_either_stop() {
        let viewport = px(800.);
        let start = px(BUBBLE_HEIGHT_DEFAULT);
        assert_eq!(
            bubble_height_for(start, px(-9000.), viewport),
            px(BUBBLE_HEIGHT_MIN),
            "it must never collapse to nothing"
        );
        assert_eq!(
            bubble_height_for(start, px(9000.), viewport),
            px(800. * BUBBLE_HEIGHT_MAX_FRACTION),
            "and never swallow the grid it describes"
        );
    }

    /// A very short window must not produce a max below the min, which would
    /// make `clamp` panic.
    #[test]
    fn a_tiny_window_still_yields_a_valid_range() {
        assert_eq!(
            bubble_height_for(px(BUBBLE_HEIGHT_DEFAULT), px(9000.), px(10.)),
            px(BUBBLE_HEIGHT_MIN)
        );
    }

    // -- restoring the tree ---------------------------------------------------

    fn catalog_of(names: &[&str]) -> Catalog {
        Catalog {
            schemas: names
                .iter()
                .map(|name| dbui_app::domain::Schema {
                    name: (*name).to_string(),
                    tables: Vec::new(),
                })
                .collect(),
        }
    }

    /// The bug this fixes: connecting used to overwrite the restored expansion
    /// with the first schema, so every folder the user had open came back shut.
    #[test]
    fn a_restored_expansion_survives_the_catalog_arriving() {
        let catalog = catalog_of(&["drizzle", "pscale_extensions", "public"]);
        assert_eq!(
            schemas_to_expand(&["public".into(), "drizzle".into()], &catalog),
            vec!["public".to_string(), "drizzle".to_string()]
        );
    }

    /// A folder for a schema that is gone is worse than a closed one.
    #[test]
    fn a_schema_the_server_dropped_is_not_expanded() {
        let catalog = catalog_of(&["public"]);
        assert_eq!(
            schemas_to_expand(&["public".into(), "retired".into()], &catalog),
            vec!["public".to_string()]
        );
    }

    /// Nothing restored -- or nothing left after filtering -- still opens one
    /// folder, so a fresh connection is not a wall of closed ones.
    #[test]
    fn with_nothing_restored_the_first_schema_opens() {
        let catalog = catalog_of(&["drizzle", "public"]);
        assert_eq!(
            schemas_to_expand(&[], &catalog),
            vec!["drizzle".to_string()]
        );
        assert_eq!(
            schemas_to_expand(&["all-gone".into()], &catalog),
            vec!["drizzle".to_string()]
        );
    }

    #[test]
    fn a_server_with_no_schemas_expands_nothing() {
        assert!(schemas_to_expand(&["public".into()], &catalog_of(&[])).is_empty());
    }
}
