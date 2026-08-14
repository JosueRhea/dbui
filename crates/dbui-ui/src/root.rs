//! The root view: all UI state, and the handlers that move it.
//!
//! GPUI is confined to this crate, and the mutable state of the window is
//! confined to this file. The components in `components/` are `impl DbUi`
//! blocks that only render -- they read state and attach listeners, they do not
//! define it. When a task lands, exactly one of the methods here folds it in.

use crate::components::palette::{Palette, PaletteKind};
use crate::components::{ConnectionForm, DetailInput, FormAction};
use crate::sql_complete::CompletionPopup;
use crate::tabs::{upsert_pending, RowDraft, Tabs, WorkspaceTab};
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
    fn new(set: ResultSet, source: ResultSource, summary: String, structure: Vec<Column>) -> Self {
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

        let (table, page, where_clause) = match self.tabs.get_mut(tab_id) {
            Some(WorkspaceTab::Table {
                table,
                page,
                where_clause,
                ..
            }) => (table.clone(), *page, where_clause.clone()),
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
                    draft,
                    ..
                }) = self.tabs.get_mut(tab_id)
                {
                    *selected_row = None;
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

        match self.tabs.active_mut() {
            Some(WorkspaceTab::Table {
                selected_row,
                draft,
                result,
                pending_edits,
                ..
            }) => {
                *selected_row = Some(row);
                let mut next = result.as_ref().and_then(|view| {
                    view.set.rows.get(row).map(|values| {
                        RowDraft::from_row(row, &view.set.columns, &values.0, &view.structure)
                    })
                });

                let restore = next.as_ref().and_then(|next_draft| {
                    let view = result.as_ref()?;
                    let values = view.set.rows.get(row)?;
                    let pk = next_draft.pk_values(&values.0).ok()?;
                    pending_edits
                        .iter()
                        .find(|edit| edit.matches_pk(&pk))
                        .cloned()
                });
                if let (Some(next_draft), Some(pending)) = (next.as_mut(), restore) {
                    next_draft.apply_pending(&pending);
                }

                *draft = next;
                self.detail_open = true;
                self.detail_input = None;
                self.detail_value_menu = None;
                self.focus = Focus::Detail;
            }
            Some(WorkspaceTab::Sql {
                selected_row,
                result,
                draft,
                ..
            }) => {
                *selected_row = Some(row);
                *draft = result.as_ref().and_then(|view| {
                    view.set
                        .rows
                        .get(row)
                        .map(|values| RowDraft::from_sql_row(row, &view.set.columns, &values.0))
                });
                self.detail_open = true;
                self.detail_input = None;
                self.detail_value_menu = None;
                self.focus = Focus::Detail;
            }
            None => {}
        }
        self.selected_cell = None;
        cx.notify();
    }

    /// Fold the open draft into `pending_edits` when it has unsaved field changes.
    fn stash_current_draft(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            draft,
            result,
            pending_edits,
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
        let Some(values) = view.set.rows.get(draft_ref.row_index) else {
            return;
        };

        match draft_ref.to_pending(&values.0) {
            Ok(Some(edit)) => upsert_pending(pending_edits, edit),
            Ok(None) => {}
            Err(message) => {
                if let Some(draft) = draft.as_mut() {
                    draft.message = Some((false, message));
                }
                cx.notify();
            }
        }
    }

    pub(crate) fn select_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        self.select_row(row, cx);
        self.selected_cell = Some((row, column));
        self.focus = Focus::Grid;
        cx.notify();
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
            ..
        }) = self.tabs.active()
        else {
            return Vec::new();
        };

        let mut batch = pending_edits.clone();
        if let (Some(draft), Some(view)) = (draft.as_ref(), result.as_ref()) {
            if let Some(values) = view.set.rows.get(draft.row_index) {
                if let Ok(Some(edit)) = draft.to_pending(&values.0) {
                    upsert_pending(&mut batch, edit);
                }
            }
        }
        batch
    }

    pub(crate) fn discard_pending_edits(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Table { saving: true, .. })
        ) {
            return;
        }
        if let Some(WorkspaceTab::Table {
            draft,
            result,
            pending_edits,
            change_bubble_expanded,
            ..
        }) = self.tabs.active_mut()
        {
            pending_edits.clear();
            *change_bubble_expanded = false;
            if let (Some(draft), Some(view)) = (draft.as_mut(), result.as_ref()) {
                if let Some(values) = view.set.rows.get(draft.row_index) {
                    draft.reset_to(&values.0);
                }
            }
        }
        self.status = Status::info("Changes discarded");
        cx.notify();
    }

    pub(crate) fn save_pending_edits(&mut self, cx: &mut Context<Self>) {
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        if matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Table { saving: true, .. })
        ) {
            return;
        }

        // Fold the open draft in first so Save catches in-progress edits.
        self.stash_current_draft(cx);

        let (table, batch, tab_id) = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                id,
                table,
                pending_edits,
                ..
            }) => (table.clone(), pending_edits.clone(), *id),
            _ => return,
        };

        if batch.is_empty() {
            return;
        }

        if let Some(WorkspaceTab::Table { saving, .. }) = self.tabs.get_mut(tab_id) {
            *saving = true;
        }
        self.status = Status::busy(format!("Saving {} change(s)…", batch.len()));
        cx.notify();

        let runtime = self.runtime.clone();
        let rows: Vec<RowUpdate> = batch
            .iter()
            .map(|edit| RowUpdate {
                pk: edit.pk.clone(),
                changes: edit
                    .changes
                    .iter()
                    .map(|c| (c.column.clone(), c.new_value.clone()))
                    .collect(),
            })
            .collect();

        cx.spawn(async move |this, cx| {
            let landed = commands::update_rows(&runtime, driver, table, rows).await;

            this.update(cx, |this, cx| {
                if let Some(WorkspaceTab::Table { saving, .. }) = this.tabs.get_mut(tab_id) {
                    *saving = false;
                }
                let is_active = this.tabs.active_id() == Some(tab_id);
                match landed {
                    Some(Ok(saved)) => {
                        if let Some(WorkspaceTab::Table {
                            pending_edits,
                            change_bubble_expanded,
                            draft,
                            ..
                        }) = this.tabs.get_mut(tab_id)
                        {
                            pending_edits.clear();
                            *change_bubble_expanded = false;
                            if let Some(draft) = draft.as_mut() {
                                draft.message = Some((true, "Saved".into()));
                            }
                        }
                        if is_active {
                            this.status = Status::info(format!("Saved {saved} change(s)"));
                        }
                        this.load_table(tab_id, cx);
                    }
                    Some(Err(error)) => {
                        // Transaction rolled back — leave every pending edit in place.
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
                "f" => {
                    self.cmd_find(cx);
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
                self.change_bubble_drag.is_some() || self.editor_drag.is_some(),
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
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_change_bubble_drag(cx);
                            this.end_editor_drag(cx);
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
