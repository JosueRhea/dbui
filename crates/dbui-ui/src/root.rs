//! The root view: all UI state, and the handlers that move it.
//!
//! GPUI is confined to this crate, and the mutable state of the window is
//! confined to this file. The components in `components/` are `impl DbUi`
//! blocks that only render -- they read state and attach listeners, they do not
//! define it. When a task lands, exactly one of the methods here folds it in.

use crate::components::close_guard::{CloseGuard, CloseTarget, TabScope};
use crate::components::context_menu::{ConfirmPrompt, ContextMenu};
use crate::components::palette::{Palette, PaletteKind};
use crate::components::production_guard::{GuardedWrite, ProductionGuard};
use crate::components::{ConnectionForm, DetailInput, FormAction};
use crate::sql_complete::CompletionPopup;
use crate::tabs::{RowDraft, TabId, Tabs, WorkspaceTab};
use crate::theme::{metrics, Theme};
use dbui_app::commands;
use dbui_app::domain::{
    Catalog, Column, ColumnInfo, ConnectionId, Page, QueryOutcome, ResultSet, TableRef, Value,
};
use dbui_app::{
    session, store, ConnectionStatus, DbRuntime, RowUpdate, SavedConnectionTab, Session, Workspace,
};
use gpui::{
    div, point, prelude::*, px, AnyElement, Context, FocusHandle, KeyDownEvent, MouseButton,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollHandle, ScrollStrategy, SharedString,
    UniformListScrollHandle, Window, WindowBackgroundAppearance,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Which surface the keyboard is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    /// The table filter box above the tree.
    SidebarSearch,
    Editor,
    /// The find / replace bar over the SQL editor.
    Find,
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
    /// "Functions", "Triggers"... under a schema, folded until opened.
    Group {
        connection: ConnectionId,
        schema: String,
        kind: dbui_app::domain::ObjectKind,
    },
    Object {
        connection: ConnectionId,
        object: dbui_app::domain::DbObject,
    },
}

impl SidebarItem {
    /// The fold key for an object group. Kept in the same list as schema
    /// names -- and so in the session -- with a separator no schema name
    /// can contain.
    pub fn group_key(schema: &str, kind: dbui_app::domain::ObjectKind) -> String {
        format!("{schema}\u{1f}{}", kind.label())
    }
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
    /// Where each row sat when the server handed it over.
    ///
    /// Empty until the grid sorts a query result itself, which it does by
    /// reordering the page rather than re-running the SQL. Keeping the
    /// original positions -- one `usize` a row, not a second copy of the rows
    /// -- is what lets a third click on a header put the server's own order
    /// back.
    pub origin: Vec<usize>,
    /// The order the columns are drawn in, as indices into `set.columns`.
    ///
    /// A permutation, always -- the grid walks this instead of the result's
    /// own order, so dragging a header sideways moves a column without
    /// touching the rows, which still store their values in the order the
    /// server sent them.
    pub order: Vec<usize>,
    /// Which arrival this is, unique for the life of the process. The grid
    /// keys its fade-in on it, so a new page or a re-run fades in and a
    /// sort or a column drag -- the same rows, rearranged -- does not.
    pub arrival: u64,
}

impl ResultView {
    pub(crate) fn new(
        set: ResultSet,
        source: ResultSource,
        summary: String,
        structure: Vec<Column>,
    ) -> Self {
        let widths = column_widths(&set);
        let column_count = set.columns.len();
        Self {
            set,
            widths,
            source,
            summary,
            structure,
            origin: Vec::new(),
            order: (0..column_count).collect(),
            arrival: {
                static NEXT: AtomicU64 = AtomicU64::new(0);
                NEXT.fetch_add(1, Ordering::Relaxed)
            },
        }
    }

    /// The columns as they are drawn, each with its index into `set.columns`.
    ///
    /// Rows are indexed by the *result's* order, so the index has to travel
    /// with the column -- everything downstream reads cells by it.
    pub(crate) fn ordered_columns(&self) -> impl Iterator<Item = (usize, &ColumnInfo)> {
        self.order
            .iter()
            .filter_map(|index| self.set.columns.get(*index).map(|info| (*index, info)))
    }

    /// Carry the column at `column` to where `target` currently sits.
    ///
    /// Both are indices into `set.columns`, not positions in the order: the
    /// header hands over the column it is drawing, and where that column
    /// happens to be drawn right now is this function's business.
    pub(crate) fn move_column(&mut self, column: usize, target: usize) -> bool {
        let (Some(from), Some(to)) = (
            self.order.iter().position(|index| *index == column),
            self.order.iter().position(|index| *index == target),
        ) else {
            return false;
        };
        if from == to {
            return false;
        }
        let moved = self.order.remove(from);
        self.order.insert(to, moved);
        true
    }

    /// Put a saved order back on, naming columns rather than counting them.
    ///
    /// A reload that added, dropped or renamed a column leaves the rest where
    /// the user put them: named columns come first in the saved order, and
    /// anything unheard of keeps its natural place at the end.
    pub(crate) fn apply_order(&mut self, names: &[String]) {
        if names.is_empty() {
            return;
        }
        let mut order: Vec<usize> = Vec::with_capacity(self.set.columns.len());
        for name in names {
            if let Some(index) = self.set.columns.iter().position(|info| &info.name == name) {
                if !order.contains(&index) {
                    order.push(index);
                }
            }
        }
        for index in 0..self.set.columns.len() {
            if !order.contains(&index) {
                order.push(index);
            }
        }
        self.order = order;
    }

    /// The drawn order by name, which is how it is remembered across a
    /// reload and a restart.
    pub(crate) fn order_names(&self) -> Vec<String> {
        self.ordered_columns()
            .map(|(_, info)| info.name.clone())
            .collect()
    }

    /// Reorder the page in hand by one column.
    ///
    /// Stable, so rows the column cannot tell apart stay in the order they
    /// arrived in -- which is the order a second sort key would have given
    /// them, and the only one available without re-running the query.
    pub(crate) fn sort_rows(&mut self, column: usize, ascending: bool) {
        if column >= self.set.columns.len() {
            return;
        }
        if self.origin.is_empty() {
            self.origin = (0..self.set.rows.len()).collect();
        }

        let mut paired: Vec<(dbui_app::domain::Row, usize)> = std::mem::take(&mut self.set.rows)
            .into_iter()
            .zip(self.origin.iter().copied())
            .collect();
        paired.sort_by(|(left, _), (right, _)| {
            let order = match (left.get(column), right.get(column)) {
                (Some(left), Some(right)) => dbui_app::domain::compare_values(left, right),
                (None, None) => std::cmp::Ordering::Equal,
                (None, _) => std::cmp::Ordering::Greater,
                (_, None) => std::cmp::Ordering::Less,
            };
            if ascending {
                order
            } else {
                order.reverse()
            }
        });
        let (rows, origin): (Vec<_>, Vec<_>) = paired.into_iter().unzip();
        self.set.rows = rows;
        self.origin = origin;
    }

    /// Put the server's own order back.
    pub(crate) fn restore_server_order(&mut self) {
        if self.origin.is_empty() {
            return;
        }
        let mut paired: Vec<(dbui_app::domain::Row, usize)> = std::mem::take(&mut self.set.rows)
            .into_iter()
            .zip(std::mem::take(&mut self.origin))
            .collect();
        paired.sort_by_key(|(_, origin)| *origin);
        self.set.rows = paired.into_iter().map(|(row, _)| row).collect();
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
///
/// Compared as well as shown: a commit that lands late writes over the line it
/// put up itself, and over nothing else.
#[derive(Clone, PartialEq)]
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

/// Rows the grid lights up for a moment -- see
/// [`motion::flash`](crate::components::motion::flash).
pub(crate) struct RowFlash {
    pub tab: TabId,
    pub rows: FlashRows,
    pub started: Instant,
}

pub(crate) enum FlashRows {
    /// Copied: named by position, since the page they sit on stays put.
    Copied(Vec<usize>),
    /// Committed: named by key, since the reload that follows a commit moves
    /// rows around -- and the flash is meant to be seen on the reloaded page.
    Committed(Vec<Vec<(String, Value)>>),
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
    /// Why the reload now in flight could not keep the open draft.
    ///
    /// `stash_current_draft` can only complain into the sidebar, and the load
    /// it was folded for takes the sidebar away with the draft -- so on a
    /// table with no primary key the typed value went, and the only word about
    /// it went too. Held until the load lands, which is when there is a status
    /// bar free to say it.
    pub(crate) refused_draft: Option<String>,
    /// The cell the user last clicked, shown in full in the status bar --
    /// a grid cell is truncated, and the whole value has to be readable
    /// somewhere.
    pub(crate) selected_cell: Option<(usize, usize)>,
    /// The grid cell whose value was last copied.
    ///
    /// Same reason as [`Self::copied_field`]: the status bar is at the far
    /// bottom of the window, and a copy answered only there reads as nothing
    /// having happened.
    pub(crate) copied_cell: Option<(usize, usize)>,
    /// When [`Self::copied_cell`] was copied: the tint flares and settles
    /// rather than simply switching on.
    pub(crate) copied_at: Option<Instant>,
    /// A grid cell just staged by editing it in place, lit for a moment so
    /// the eye finds what changed: `(tab, row, column, when)`.
    pub(crate) cell_flash: Option<(TabId, usize, usize, Instant)>,
    /// Rows lit for a moment after something happened to them.
    pub(crate) row_flash: Option<RowFlash>,
    /// Presses of the two reload buttons. Each spins its icon once, keyed on
    /// the count so a press mid-spin starts a fresh turn.
    pub(crate) catalog_refreshes: usize,
    pub(crate) result_refreshes: usize,
    /// The detail field whose copy button was last pressed.
    ///
    /// The status bar is at the far bottom of the window, and a sidebar
    /// scrolled to its fortieth field is nowhere near it -- so the
    /// confirmation is drawn on the button that was pressed, where the user
    /// is already looking.
    pub(crate) copied_field: Option<usize>,

    pub(crate) modal: Option<ConnectionForm>,
    /// Titlebar connection switcher dropdown.
    pub(crate) connection_picker_open: bool,
    /// Titlebar gear dropdown: the app's own settings.
    pub(crate) settings_menu_open: bool,
    /// The detail panel's overflow (`⋮`) menu.
    pub(crate) detail_menu_open: bool,
    /// The rows-per-page preset dropdown in the toolbar.
    pub(crate) page_size_menu_open: bool,
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
    /// Schema tree width, in unzoomed pixels -- so a rail the user has
    /// resized still scales with ⌘+ like everything else.
    pub(crate) sidebar_width: f32,
    /// A press is on the titlebar's drag strip and the window is following the
    /// pointer.
    ///
    /// State here rather than a cell owned by the titlebar's own element:
    /// `render_titlebar` runs every frame and would hand each one a fresh cell,
    /// so a flag set on mouse-down was gone by the next repaint. That went
    /// unnoticed while the move handler only had to fire once -- it used to
    /// hand off to AppKit, which then ran the drag itself.
    pub(crate) titlebar_drag: bool,
    /// Live drag for the left rail: `(pointer x, width)` at the grab.
    pub(crate) sidebar_drag: Option<(Pixels, f32)>,
    /// Row detail panel width, in unzoomed pixels.
    pub(crate) detail_width: f32,
    /// The user's "Translucent Window" preference.
    pub(crate) translucent: bool,
    /// Whether this frame is drawn translucent: the preference, except in
    /// full screen, where there is no desktop behind the window to blur.
    /// Also what the window's background appearance was last set to.
    pub(crate) glass: bool,
    /// What the window's material was last set to: `None` for no material,
    /// otherwise whether it was the light one. The theme picks light or dark,
    /// so a theme change while translucent has to re-tint it.
    vibrancy: Option<bool>,
    /// How strongly the theme tints the glass, 0–100.
    pub(crate) glass_opacity_pct: u32,
    /// How far the glass blurs what is behind the window, in points.
    pub(crate) glass_blur: u32,
    /// Live drag for the detail panel: `(pointer x, width)` at the grab.
    pub(crate) detail_drag: Option<(Pixels, f32)>,
    /// Detail fields the user has folded back down to a scrolling box, by
    /// column name. A field draws at its full content height unless it is in
    /// here: the value is the point of the panel, and a box that hides two
    /// thirds of it makes you scroll inside a thing you are already scrolling.
    ///
    /// Keyed by name rather than index so a folded column stays folded as the
    /// selection moves down the table, and a different table -- with different
    /// column names -- starts fresh.
    pub(crate) detail_collapsed: HashSet<String>,
    /// SQL editor pane height (dragged by the strip under the editor).
    pub(crate) editor_height: Pixels,
    /// Live drag for the SQL editor resize: `(pointer y, height)`.
    pub(crate) editor_drag: Option<(Pixels, Pixels)>,
    /// Open SQL autocomplete popup, if any.
    pub(crate) completion: Option<CompletionPopup>,
    /// Whether the popup opened by itself as the user typed, rather than on
    /// ⌃Space. An automatic one keeps out of the way: it closes when nothing
    /// is left worth offering, where one asked for shows what there is.
    pub(crate) completion_auto: bool,
    /// Cached `driver.columns` results keyed by `(schema, table)`.
    pub(crate) column_cache: HashMap<(String, String), Vec<Column>>,
    /// Substring filter over the schema tree. Empty means show everything.
    pub(crate) sidebar_filter: crate::text_input::TextInput,
    /// The row a drag has reached, set while the button is down. `Some` means
    /// a drag is in progress; the value is what stops a pointer wandering
    /// inside one row from rebuilding the range on every mouse-move.
    pub(crate) row_drag: Option<usize>,
    /// The cell being edited in place, and the editor holding it.
    ///
    /// A single-line buffer of its own rather than the detail sidebar's field:
    /// that one is multiline so JSON stays editable, and a multiline box does
    /// not fit in a grid row. Committing writes back into the draft, so the
    /// change is staged by exactly the same path as a sidebar edit.
    pub(crate) editing_cell: Option<(usize, usize)>,
    pub(crate) cell_editor: crate::text_input::TextInput,
    /// Live column resize: `(column index, pointer x, width)` as they were
    /// when the header edge was grabbed.
    pub(crate) column_drag: Option<(usize, Pixels, f32)>,
    /// A header being carried to another position, if one is.
    pub(crate) column_move: Option<ColumnMove>,
    /// A tab being dragged along the strip, if one is.
    pub(crate) tab_drag: Option<TabDrag>,
    /// Runs in flight that Stop can end, by the connection and tab that
    /// started them -- tab ids restart per connection -- each with the id of
    /// the run it belongs to.
    pub(crate) running: HashMap<(Option<ConnectionId>, TabId), (u64, commands::StopHandle)>,
    /// Table page loads in flight, keyed like `running`. Kept apart from it
    /// because Stop is about the statement the user ran; these are only ever
    /// stopped by the tab they were loading into closing.
    pub(crate) table_loads: HashMap<(Option<ConnectionId>, TabId), (u64, commands::StopHandle)>,
    /// Loads and runs stopped because their tab closed, by run id. Already
    /// let go of in `loads_in_flight` when the tab closed -- the footer has
    /// no business saying "Loading" over a tab that is gone while the server
    /// winds down -- so their landing must not let go of them again.
    abandoned: HashSet<u64>,
    /// The sheet for changing a table's shape, while it is open.
    pub(crate) schema_sheet: Option<crate::components::schema_sheet::SchemaSheet>,
    /// The find / replace bar over the SQL editor, while it is open.
    pub(crate) editor_find: Option<crate::components::editor_find::EditorFind>,
    /// The id the next run gets. Only the run holding a handle may clear it:
    /// one superseded on the same tab lands late and must leave the live
    /// run's Stop alone.
    pub(crate) next_run: u64,
    /// Where the pointer is while a tab or a column is in hand, for the copy
    /// of it drawn under the pointer. Window coordinates.
    pub(crate) drag_pointer: Option<gpui::Point<Pixels>>,
    /// The underline under the front tab, where it is sliding to, and how
    /// far down the strip it sits.
    pub(crate) tab_indicator: Option<(crate::components::motion::Slide, Pixels)>,
    /// Right-click menu, if one is open.
    pub(crate) context_menu: Option<ContextMenu>,
    /// A destructive action waiting on the user typing the table's name.
    pub(crate) confirm: Option<ConfirmPrompt>,
    /// A close waiting on the user deciding what to do about staged changes.
    pub(crate) close_guard: Option<CloseGuard>,
    /// "Write to production?" -- standing in front of a commit or a writing
    /// statement on a connection tagged production.
    pub(crate) production_guard: Option<ProductionGuard>,
    /// Set for the one call the guard's answer makes, so that call is not
    /// stopped by the same question again.
    pub(crate) production_confirmed: bool,
    /// The server activity panel, while it is open.
    pub(crate) activity: Option<crate::components::activity::ActivityPanel>,
    /// Counts openings of the panel, so a refresh loop can tell it has been
    /// replaced by a newer one.
    pub(crate) activity_generation: u64,
    /// Stops the SQL file being run, while one is.
    pub(crate) script_stop: Option<dbui_app::commands::StopHandle>,
    /// Bumped each time a commit puts its "Committing…" line up, so the one
    /// that lands can tell whether the line on screen is still its own.
    pub(crate) commit_stamp: u64,
    /// Every statement run, newest first. Loaded once at launch.
    pub(crate) history: dbui_app::History,
    /// Queries kept under a name.
    pub(crate) saved_queries: dbui_app::SavedQueries,
    /// Why the saved-queries file could not be read, when it could not. Saving
    /// is refused while this is set: the file on disk is someone's kept work,
    /// and writing the empty list loaded in its place would erase it.
    pub(crate) saved_queries_unreadable: Option<String>,

    /// Vertical scroll of the result grid, and horizontal scroll of the pane
    /// holding it.
    ///
    /// Held here rather than left to the elements because the keyboard moves
    /// the cell cursor and the pointer does not: a cursor the arrow keys can
    /// walk off the bottom of the viewport is a cursor that has vanished.
    pub(crate) grid_scroll: UniformListScrollHandle,
    pub(crate) grid_h_scroll: ScrollHandle,
    /// Same, for the arrow-key cursor in the schema tree.
    pub(crate) sidebar_scroll: ScrollHandle,
    pub(crate) detail_scroll: ScrollHandle,
    /// The rest of the scrollable panes. These have no keyboard cursor to
    /// keep on screen; they are held here because a scrollbar can only read
    /// how far a pane scrolls off a handle the pane is tracking, and an
    /// element that tracks nothing keeps that privately.
    pub(crate) columns_scroll: ScrollHandle,
    pub(crate) error_scroll: ScrollHandle,
    pub(crate) structure_scroll: ScrollHandle,
    pub(crate) completion_scroll: ScrollHandle,
    pub(crate) picker_scroll: ScrollHandle,
    /// The workspace tab strip. Tracked so the strip's painted bounds can be
    /// read back -- a tab is dragged with the pointer, and where the strip
    /// ended up is the only way to say where its tabs are.
    pub(crate) tab_strip_scroll: ScrollHandle,
    pub(crate) change_bubble_scroll: ScrollHandle,
    /// Test-only: see `persist_session`.
    #[cfg(test)]
    pub(crate) session_writes: std::cell::Cell<usize>,
}

/// A tab being dragged along the strip.
///
/// The tab is named by [`TabId`] because the drag is what moves it: an index
/// grabbed on the press is stale the moment the strip reorders under it.
pub struct TabDrag {
    pub id: crate::tabs::TabId,
    /// Where the press landed. A press that never leaves this point is a
    /// click, and clicking a tab must not shuffle the strip.
    pub start_x: Pixels,
    /// The pointer x at the last reorder, or at the press before the first.
    ///
    /// A swap is only allowed in the direction the pointer has since moved.
    /// Without that, a tab dropped into a wider neighbour's slot leaves the
    /// neighbour still under the pointer, and the two trade places on every
    /// mouse-move for as long as the button is held.
    pub last_x: Pixels,
    /// Whether the pointer has left the slop radius around the press.
    pub moved: bool,
}

/// How far the pointer travels before a press counts as a drag.
const TAB_DRAG_SLOP: f32 = 4.;

/// A column header being carried to another position.
///
/// The same shape as [`TabDrag`], for the same reasons: a press that never
/// travels is a click, and a swap is only allowed in the direction the
/// pointer has since moved -- otherwise a narrow column dropped into a wide
/// one's slot stays under the pointer and the two trade places on every
/// mouse-move.
#[derive(Debug, Clone, Copy)]
pub struct ColumnMove {
    /// Index into the result's columns, not a position in the drawn order.
    pub column: usize,
    pub start_x: Pixels,
    pub last_x: Pixels,
    pub moved: bool,
}

/// Same slop as the tab strip: a header is a click target first.
const HEADER_DRAG_SLOP: f32 = 4.;

/// Starting height of the diff area, and the range the drag is allowed.
pub(crate) const BUBBLE_HEIGHT_DEFAULT: f32 = 180.;
const BUBBLE_HEIGHT_MIN: f32 = 48.;
/// Past this the bubble is eating the grid it is describing.
const BUBBLE_HEIGHT_MAX_FRACTION: f32 = 0.7;

/// Starting widths of the two rails, and how far a drag may take them.
///
/// The minimums are "still a panel"; the maximums are "still a window with a
/// grid in it".
const SIDEBAR_WIDTH_DEFAULT: f32 = 258.;
const SIDEBAR_WIDTH_MIN: f32 = 150.;
const SIDEBAR_WIDTH_MAX: f32 = 560.;

const DETAIL_WIDTH_DEFAULT: f32 = 280.;
const DETAIL_WIDTH_MIN: f32 = 180.;
const DETAIL_WIDTH_MAX: f32 = 720.;

pub(crate) const EDITOR_HEIGHT_DEFAULT: f32 = 150.;
const EDITOR_HEIGHT_MIN: f32 = 80.;
const EDITOR_HEIGHT_MAX_FRACTION: f32 = 0.6;

/// Resolve a drag into a height. `rise` is how far the edge was pulled upward,
/// which is the direction that makes the panel bigger.
fn bubble_height_for(start_height: Pixels, rise: Pixels, viewport_height: Pixels) -> Pixels {
    let max = (f32::from(viewport_height) * BUBBLE_HEIGHT_MAX_FRACTION).max(BUBBLE_HEIGHT_MIN);
    px((f32::from(start_height) + f32::from(rise)).clamp(BUBBLE_HEIGHT_MIN, max))
}

/// Resolve a rail drag into a width.
///
/// `delta` is the pointer's travel in real pixels; widths are kept unzoomed,
/// so the travel is divided by the zoom before it is added -- otherwise the
/// rail runs away from the pointer at any zoom but 100%.
fn panel_width_for(start_width: f32, delta: Pixels, zoom: f32, min: f32, max: f32) -> f32 {
    (start_width + f32::from(delta) / zoom.max(0.1)).clamp(min, max)
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
            refused_draft: None,
            selected_cell: None,
            copied_cell: None,
            copied_at: None,
            cell_flash: None,
            row_flash: None,
            catalog_refreshes: 0,
            result_refreshes: 0,
            copied_field: None,
            modal: None,
            connection_picker_open: false,
            settings_menu_open: false,
            detail_menu_open: false,
            page_size_menu_open: false,
            palette: None,
            sidebar_cursor: None,
            theme_prev: None,
            update: crate::update::UpdateState::default(),
            sidebar_width: SIDEBAR_WIDTH_DEFAULT,
            titlebar_drag: false,
            sidebar_drag: None,
            detail_width: DETAIL_WIDTH_DEFAULT,
            translucent: store::default_translucent(),
            glass: false,
            vibrancy: None,
            glass_opacity_pct: store::default_glass_opacity_pct(),
            glass_blur: store::default_glass_blur(),
            detail_drag: None,
            detail_collapsed: HashSet::new(),
            change_bubble_height: px(BUBBLE_HEIGHT_DEFAULT),
            change_bubble_drag: None,
            editor_height: px(EDITOR_HEIGHT_DEFAULT),
            editor_drag: None,
            completion: None,
            completion_auto: false,
            column_cache: HashMap::new(),
            sidebar_filter: crate::text_input::TextInput::new(false),
            row_drag: None,
            editing_cell: None,
            cell_editor: crate::text_input::TextInput::new(false),
            column_drag: None,
            column_move: None,
            tab_drag: None,
            running: HashMap::new(),
            table_loads: HashMap::new(),
            abandoned: HashSet::new(),
            editor_find: None,
            schema_sheet: None,
            next_run: 0,
            drag_pointer: None,
            tab_indicator: None,
            context_menu: None,
            confirm: None,
            close_guard: None,
            production_guard: None,
            production_confirmed: false,
            activity: None,
            activity_generation: 0,
            script_stop: None,
            commit_stamp: 0,
            grid_scroll: UniformListScrollHandle::new(),
            grid_h_scroll: ScrollHandle::new(),
            sidebar_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            columns_scroll: ScrollHandle::new(),
            error_scroll: ScrollHandle::new(),
            structure_scroll: ScrollHandle::new(),
            completion_scroll: ScrollHandle::new(),
            picker_scroll: ScrollHandle::new(),
            tab_strip_scroll: ScrollHandle::new(),
            change_bubble_scroll: ScrollHandle::new(),
            #[cfg(test)]
            session_writes: std::cell::Cell::new(0),
            history: dbui_app::history::history_path()
                .map(|path| dbui_app::history::load(&path))
                .unwrap_or_default(),
            // Read in `load_saved_queries`, at launch, not here: a view built
            // by a test must not pick up the user's own saved queries.
            saved_queries: Default::default(),
            saved_queries_unreadable: None,
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

    pub fn apply_sidebar_width_px(&mut self, px_value: u32) {
        self.sidebar_width = (px_value as f32).clamp(SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX);
    }

    pub fn apply_detail_width_px(&mut self, px_value: u32) {
        self.detail_width = (px_value as f32).clamp(DETAIL_WIDTH_MIN, DETAIL_WIDTH_MAX);
    }

    /// Grab the left rail's right edge at pointer position `x`.
    pub(crate) fn begin_sidebar_drag(&mut self, x: Pixels, cx: &mut Context<Self>) {
        self.sidebar_drag = Some((x, self.sidebar_width));
        cx.notify();
    }

    pub(crate) fn drag_sidebar(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.sidebar_drag else {
            return;
        };
        // The handle is on the rail's right edge: rightward is wider.
        self.sidebar_width = panel_width_for(
            start_width,
            x - start_x,
            metrics::zoom(),
            SIDEBAR_WIDTH_MIN,
            SIDEBAR_WIDTH_MAX,
        );
        cx.notify();
    }

    pub(crate) fn end_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_drag.take().is_some() {
            self.persist_prefs(cx);
        }
    }

    /// Grab the detail panel's left edge at pointer position `x`.
    pub(crate) fn begin_detail_drag(&mut self, x: Pixels, cx: &mut Context<Self>) {
        self.detail_drag = Some((x, self.detail_width));
        cx.notify();
    }

    pub(crate) fn drag_detail(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((start_x, start_width)) = self.detail_drag else {
            return;
        };
        // This handle is on the panel's *left* edge, so leftward is wider.
        self.detail_width = panel_width_for(
            start_width,
            start_x - x,
            metrics::zoom(),
            DETAIL_WIDTH_MIN,
            DETAIL_WIDTH_MAX,
        );
        cx.notify();
    }

    pub(crate) fn end_detail_drag(&mut self, cx: &mut Context<Self>) {
        if self.detail_drag.take().is_some() {
            self.persist_prefs(cx);
        }
    }

    /// Flip one detail field between its whole content and the eight-line cap.
    pub(crate) fn toggle_detail_collapsed(&mut self, field: &str, cx: &mut Context<Self>) {
        if !self.detail_collapsed.remove(field) {
            self.detail_collapsed.insert(field.to_string());
        }
        cx.notify();
    }

    pub fn apply_theme_id(&mut self, id: &str) {
        self.theme = Theme::named(id);
    }

    pub fn apply_translucent(&mut self, on: bool, opacity_pct: u32, blur: u32) {
        self.translucent = on;
        self.glass_opacity_pct = opacity_pct.min(100);
        self.glass_blur = blur.min(GLASS_BLUR_MAX);
    }

    /// Step the glass tint by `direction` notches of 5%.
    pub(crate) fn step_glass_opacity(&mut self, direction: i32, cx: &mut Context<Self>) {
        self.glass_opacity_pct = step_setting(self.glass_opacity_pct, direction, 5, 100);
        self.persist_prefs(cx);
        // Said in the footer too: from the palette, which closes behind the
        // step, the footer is the only place the new value shows.
        self.status = Status::info(format!("Glass tint: {}%", self.glass_opacity_pct));
    }

    /// Step the glass blur by `direction` notches of 5pt.
    pub(crate) fn step_glass_blur(&mut self, direction: i32, cx: &mut Context<Self>) {
        self.glass_blur = step_setting(self.glass_blur, direction, 5, GLASS_BLUR_MAX);
        self.persist_prefs(cx);
        self.status = Status::info(format!("Glass blur: {}", self.glass_blur));
    }

    pub(crate) fn toggle_translucent(&mut self, cx: &mut Context<Self>) {
        self.translucent = !self.translucent;
        self.persist_prefs(cx);
        self.status = Status::info(if self.translucent {
            "Translucent window: on"
        } else {
            "Translucent window: off"
        });
        cx.notify();
    }

    /// The tint over the window's material. The theme's panel colour, so
    /// each theme still colours its own chrome, but thin: AppKit's material
    /// already tints the blur, and this is only the theme's accent on it.
    pub(crate) fn glass_tint(&self) -> gpui::Rgba {
        // The same setting has to mean the same amount of glass in either
        // kind of theme, and it does not by itself: a near-white tint over
        // the light material is almost indistinguishable from a solid panel
        // long before a dark one over the dark material is. So a light theme
        // lays it on at well under half strength.
        let strength = if self.theme.is_light {
            LIGHT_TINT_STRENGTH
        } else {
            1.
        };
        gpui::Rgba {
            a: self.glass_opacity_pct as f32 / 100. * strength,
            ..self.theme.panel
        }
    }

    /// The theme the chrome on the glass draws with: its raised surfaces --
    /// the search fields, the front tab and chip -- become a wash of the text
    /// colour rather than a solid block, so the glass shows through them too.
    /// Menus that drop from the chrome keep `self.theme`: they sit over the
    /// content, not the glass, and need to be solid to be read.
    pub(crate) fn chrome_theme(&self) -> Theme {
        let mut theme = self.theme.clone();
        if self.glass {
            let wash = gpui::Rgba {
                a: 0.08,
                ..theme.text
            };
            theme.background = wash;
            theme.elevated = wash;
            theme.border = gpui::Rgba {
                a: 0.12,
                ..theme.text
            };
            // The dim text tones were picked against a solid panel. Over a
            // blurred desktop that may be lighter or busier than the panel
            // ever is, they fade out: placeholders first, then the tree.
            // Pulled toward the full text colour, they keep their order --
            // faint under muted under text -- but read again.
            theme.text_muted = mix(theme.text_muted, theme.text, 0.55);
            theme.text_faint = mix(theme.text_faint, theme.text, 0.45);
        }
        theme
    }

    pub fn apply_zoom_pct(&mut self, pct: u32) {
        metrics::set_zoom_pct(pct);
    }

    /// Everything the app remembers about how the window is laid out.
    fn current_prefs(&self) -> store::Prefs {
        store::Prefs {
            theme: self.theme.id.to_string(),
            zoom_pct: metrics::zoom_pct(),
            sql_editor_height_px: f32::from(self.editor_height).round() as u32,
            sidebar_width_px: self.sidebar_width.round() as u32,
            detail_width_px: self.detail_width.round() as u32,
            translucent: self.translucent,
            glass_opacity_pct: self.glass_opacity_pct,
            glass_blur: self.glass_blur,
        }
    }

    pub fn persist_prefs(&mut self, cx: &mut Context<Self>) {
        let prefs = self.current_prefs();
        match store::prefs_path().and_then(|path| store::save_prefs(&path, &prefs)) {
            Ok(()) => {}
            Err(error) => {
                self.status = Status::error(format!("Could not save prefs: {error}"));
            }
        }
        cx.notify();
    }

    pub fn persist_theme(&mut self, cx: &mut Context<Self>) {
        let prefs = self.current_prefs();
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

    /// Fold what is still being typed into the tab it was typed into, before
    /// anything else becomes the one in front.
    ///
    /// The order is the whole of it: the cell editor writes into the draft and
    /// the stash folds the draft into `pending_edits`, so stashing first
    /// stages the row as it stood before the last keystroke and drops the
    /// cell. `editing_cell` hangs off the window rather than off the tab, so
    /// an editor left open across a switch is an edit lost with nothing said
    /// -- or, when the row indices happen to line up, one committed into the
    /// table that was switched to.
    ///
    /// Both halves are idempotent, which is what lets `close_tab` call this
    /// and then hand off to `close_tab_now`, which calls it again: the second
    /// `finish_cell_edit` has no editor left to commit, and a second stash
    /// restages the same draft against the same keys.
    fn leave_front_tab(&mut self, cx: &mut Context<Self>) {
        self.finish_cell_edit(cx);
        self.stash_current_draft(cx);
    }

    pub(crate) fn activate_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        // Before the switch, and only for a switch that is really happening:
        // folding the draft away for a tab that is already in front would
        // stage a value the user is still typing.
        if index >= self.tabs.items.len() || index == self.tabs.active {
            return;
        }
        self.leave_front_tab(cx);
        self.tabs.activate(index);
        // The bar searched the editor that was in front; the next tab's is a
        // different text, and may not be an editor at all.
        if self.editor_find.take().is_some() && self.focus == Focus::Find {
            self.focus = Focus::Editor;
        }
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
        // Launch loads only the front tab; the rest of a restored strip has
        // no rows until it is brought forward -- which is now.
        self.load_active_table_if_empty(cx);
    }

    /// Close a tab, asking first if it is holding staged changes.
    pub(crate) fn close_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.items.len() {
            return;
        }
        // What is still being typed -- the sidebar draft, and the box open
        // over a cell -- has to reach `pending_edits` before anything counts
        // them, or a value typed and not yet committed is work the prompt does
        // not know about, and ⌘W closes the tab over it without asking.
        if index == self.tabs.active {
            self.leave_front_tab(cx);
        }
        let Some(tab) = self.tabs.items.get(index) else {
            return;
        };
        let changes = tab.pending_change_count();
        if changes > 0 {
            self.close_guard = Some(CloseGuard {
                // Named rather than numbered: the list this index points into
                // can be swapped out from under the question -- ⌘⌥] carries
                // another connection's tabs in -- and a stale index closes, or
                // commits, whatever slid into the slot.
                target: CloseTarget::Tab {
                    connection: self.workspace.active_id(),
                    id: tab.id(),
                },
                label: SharedString::from(tab.label()),
                changes,
            });
            cx.notify();
            return;
        }
        self.close_tab_now(index, cx);
    }

    /// Close a tab and take the staged batch with it, no questions asked.
    ///
    /// For the callers where the question makes no sense: the guard has just
    /// been answered, or the table the tab was showing has been dropped and
    /// there is nothing left to commit the batch against.
    pub(crate) fn close_tab_now(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.tabs.items.len() {
            return;
        }
        // The routes that skip the question still have to close the editor
        // over the tab they are removing: one left open outlives the tab it
        // was opened on, and commits into whatever the close brings forward.
        if index == self.tabs.active {
            self.leave_front_tab(cx);
        }
        // Whatever the tab was waiting on has nowhere left to land, so it is
        // stopped rather than left to hold a connection -- and a lock, and the
        // server's time -- until it finishes on its own.
        if let Some(tab) = self.tabs.items.get(index) {
            let key = (self.workspace.active_id(), tab.id());
            for (run, handle) in [self.running.remove(&key), self.table_loads.remove(&key)]
                .into_iter()
                .flatten()
            {
                handle.stop();
                self.abandoned.insert(run);
                self.release_load();
            }
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
        // The tab now in front may be a restored one that has never loaded.
        self.load_active_table_if_empty(cx);
    }

    /// The tabs a bulk close is aimed at, left to right.
    ///
    /// An anchor that is no longer open answers with nothing: the menu was
    /// opened on a tab that has since gone, and closing "the others" of a tab
    /// that does not exist would be closing everything.
    pub(crate) fn tabs_in_scope(&self, scope: TabScope) -> Vec<crate::tabs::TabId> {
        let ids = |keep: &dyn Fn(usize) -> bool| -> Vec<crate::tabs::TabId> {
            self.tabs
                .items
                .iter()
                .enumerate()
                .filter(|(index, _)| keep(*index))
                .map(|(_, tab)| tab.id())
                .collect()
        };
        let anchor = |id: crate::tabs::TabId| self.tabs.items.iter().position(|tab| tab.id() == id);
        match scope {
            TabScope::All => ids(&|_| true),
            TabScope::Others(id) => match anchor(id) {
                Some(keep) => ids(&|index| index != keep),
                None => Vec::new(),
            },
            TabScope::ToRight(id) => match anchor(id) {
                Some(from) => ids(&|index| index > from),
                None => Vec::new(),
            },
        }
    }

    /// Close a run of tabs, asking once if any of them is holding staged work.
    ///
    /// One question for the batch, not one per tab: the user picked a single
    /// menu entry, and answering the same prompt four times is not consent,
    /// it is attrition.
    pub(crate) fn close_tab_scope(&mut self, scope: TabScope, cx: &mut Context<Self>) {
        // Only when the active tab is going: folding what is being typed into
        // a tab that stays would stage a value the user has not finished. When
        // it is going -- "Close All Tabs" from the palette, which does not
        // close the cell editor on the way in -- the box over the cell counts
        // too, or every tab closes without the question its value deserved.
        if self
            .tabs
            .active_id()
            .is_some_and(|id| self.tabs_in_scope(scope).contains(&id))
        {
            self.leave_front_tab(cx);
        }
        let doomed = self.tabs_in_scope(scope);
        if doomed.is_empty() {
            return;
        }
        let changes: usize = self
            .tabs
            .items
            .iter()
            .filter(|tab| doomed.contains(&tab.id()))
            .map(|tab| tab.pending_change_count())
            .sum();
        if changes > 0 {
            let count = doomed.len();
            let noun = if count == 1 { "tab" } else { "tabs" };
            self.close_guard = Some(CloseGuard {
                target: CloseTarget::TabGroup(scope),
                label: SharedString::from(format!("{count} {noun}")),
                changes,
            });
            cx.notify();
            return;
        }
        self.close_tab_scope_now(scope, cx);
    }

    /// Close the whole run, staged work and all.
    pub(crate) fn close_tab_scope_now(&mut self, scope: TabScope, cx: &mut Context<Self>) {
        for id in self.tabs_in_scope(scope) {
            if let Some(index) = self.tabs.items.iter().position(|tab| tab.id() == id) {
                self.close_tab_now(index, cx);
            }
        }
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
        // Tabbing past the bottom of the panel has to bring the next field
        // with it, or the caret walks off-screen.
        if let Some(DetailInput::Field(field)) = self.detail_input {
            self.reveal_detail_field(field);
        }
        cx.notify();
    }

    pub(crate) fn open_sql_tab(&mut self, cx: &mut Context<Self>) {
        // Opening a tab moves the front of the deck the same way clicking one
        // does, so what is being typed has to be folded away first -- see the
        // note on `leave_front_tab`. Without it the tab left behind holds a
        // typed value that nothing counts, and closing it later throws the
        // value away without asking.
        self.leave_front_tab(cx);
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

        self.workspace.activate(id);
        self.swap_front_tabs(previous, cx);

        self.persist_session();
        cx.notify();
        // A restored tab has no rows yet. Loading here rather than on restore
        // means a background connection never dials out on its own.
        self.load_active_table_if_empty(cx);
    }

    /// Put the front tab list away under the connection that owned it, and
    /// take up the list belonging to whichever connection is in front now.
    ///
    /// Called once `workspace` has been told to move the front; `previous`
    /// names who held it before. Both ways the front changes come through
    /// here -- a connection brought forward, and a connection just made --
    /// because everything below is a pointer into the list being put away,
    /// and the two paths had already drifted into clearing different halves
    /// of it. A third copy of this list is a third set of omissions.
    ///
    /// `column_cache` is the one with teeth: it is keyed by schema and table
    /// with no connection in the key, so a `users` left in it from the old
    /// connection is handed to autocomplete as the new connection's `users`
    /// until a real fetch lands. Same-named tables across two connections is
    /// the ordinary case -- staging and production -- not an exotic one.
    fn swap_front_tabs(&mut self, previous: Option<ConnectionId>, cx: &mut Context<Self>) {
        // Fold any half-finished row edit -- the open cell included -- into
        // the outgoing tab set before it is put away, or the change is lost on
        // the way out.
        self.leave_front_tab(cx);
        if let Some(previous) = previous {
            self.stashed_tabs
                .insert(previous, std::mem::take(&mut self.tabs));
        }
        // A connection nobody has opened tabs on yet -- one made a moment ago
        // above all -- has no list waiting, and starts with an empty one.
        self.tabs = self
            .workspace
            .active_id()
            .and_then(|id| self.stashed_tabs.remove(&id))
            .unwrap_or_default();

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
        // ⌘⇧W and the × on a connection chip both arrive with the cell editor
        // still open, and neither closes it on the way. Uncounted, the whole
        // tab set is dropped without the question a staged change would have
        // raised.
        if self.workspace.active_id() == Some(id) {
            self.leave_front_tab(cx);
        }

        // Every tab this connection owns goes with it, so the count is over
        // all of them -- the ones behind the front tab included.
        let changes: usize = self
            .connection_tabs(id)
            .map(|tabs| {
                tabs.items
                    .iter()
                    .map(|tab| tab.pending_change_count())
                    .sum()
            })
            .unwrap_or(0);
        if changes > 0 {
            let label = self
                .workspace
                .get(id)
                .map(|entry| entry.config.name.clone())
                .unwrap_or_else(|| "This connection".to_string());
            self.close_guard = Some(CloseGuard {
                target: CloseTarget::Connection(id),
                label: SharedString::from(label),
                changes,
            });
            cx.notify();
            return;
        }
        self.close_connection_tab_now(id, cx);
    }

    /// Go through with the close the guard is holding.
    pub(crate) fn confirm_close(&mut self, cx: &mut Context<Self>) {
        let Some(guard) = self.close_guard.take() else {
            return;
        };
        match guard.target {
            // Found again rather than remembered: if the tab has gone, or
            // another connection's list is in front, there is nothing here
            // this question was asking about.
            CloseTarget::Tab { connection, id } => {
                if let Some(index) = self.guarded_tab_index(connection, id) {
                    self.close_tab_now(index, cx);
                }
            }
            CloseTarget::TabGroup(scope) => self.close_tab_scope_now(scope, cx),
            CloseTarget::Connection(id) => self.close_connection_tab_now(id, cx),
        }
        cx.notify();
    }

    /// Leave everything as it was.
    pub(crate) fn cancel_close(&mut self, cx: &mut Context<Self>) {
        self.close_guard = None;
        cx.notify();
    }

    /// The tab set belonging to a connection, wherever it is currently kept.
    pub(crate) fn connection_tabs(&self, id: ConnectionId) -> Option<&Tabs> {
        if self.workspace.active_id() == Some(id) {
            Some(&self.tabs)
        } else {
            self.stashed_tabs.get(&id)
        }
    }

    /// The same, to write to.
    ///
    /// What an answer that crossed to the server and back has to go through:
    /// by the time one lands, the connection it was sent on may have been put
    /// away, and `self.tabs` belongs to whoever is in front now.
    pub(crate) fn connection_tabs_mut(&mut self, id: ConnectionId) -> Option<&mut Tabs> {
        if self.workspace.active_id() == Some(id) {
            Some(&mut self.tabs)
        } else {
            self.stashed_tabs.get_mut(&id)
        }
    }

    /// Close a connection tab and take every staged batch under it, no
    /// questions asked.
    ///
    /// Reached only once [`Self::close_connection_tab`] has folded the open
    /// draft away and had its question answered, so it does not stash again.
    pub(crate) fn close_connection_tab_now(&mut self, id: ConnectionId, cx: &mut Context<Self>) {
        if !self.workspace.is_open(id) {
            return;
        }
        let was_active = self.workspace.active_id() == Some(id);

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
            // With the rest of the cursor state, and for a sharper reason: the
            // cell editor is drawn from `editing_cell` alone, so one left
            // behind is a box still on screen over a table belonging to
            // another connection -- and the next thing to move the focus
            // commits it into whatever draft was promoted with it.
            self.editing_cell = None;
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
    pub(crate) fn table_matches_filter(
        &self,
        table: &dbui_app::domain::Table,
        query: &str,
    ) -> bool {
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
        let first = self
            .sidebar_visible_items()
            .into_iter()
            .find_map(|item| match item {
                SidebarItem::Table { table, .. } => Some(table),
                SidebarItem::Schema { .. }
                | SidebarItem::Group { .. }
                | SidebarItem::Object { .. } => None,
            });
        let Some(table) = first else {
            return;
        };
        self.focus = Focus::Sidebar;
        self.open_table_tab(table, cx);
    }

    pub(crate) fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_refreshes += 1;
        self.reload_catalog(true, cx);
    }

    /// Re-read the catalog because a statement changed it, without saying so.
    ///
    /// The DDL's own verdict -- "CREATE TABLE users · OK" -- is what the user
    /// is reading, and "Catalog refreshed" landing on top of it a moment later
    /// is how that feedback went missing. A refresh nobody asked for keeps
    /// quiet, failure included: the tree simply stays as it was, and ⌘R is
    /// still there to be told about it.
    pub(crate) fn refresh_catalog_quietly(&mut self, cx: &mut Context<Self>) {
        self.reload_catalog(false, cx);
    }

    fn reload_catalog(&mut self, narrate: bool, cx: &mut Context<Self>) {
        let Some(driver) = self.workspace.active_driver() else {
            return;
        };
        let Some(id) = self.workspace.active_id() else {
            return;
        };

        if narrate {
            self.status = Status::busy("Refreshing…");
        }
        let task = commands::refresh_catalog(&self.runtime, driver);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                match landed {
                    Some(Ok(catalog)) => {
                        if let Some(entry) = this.workspace.get_mut(id) {
                            entry.catalog = Some(catalog);
                        }
                        if narrate {
                            this.status = Status::info("Catalog refreshed");
                        }
                    }
                    Some(Err(error)) if narrate => this.status = Status::error(error.to_string()),
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // -- tables and queries -----------------------------------------------

    pub(crate) fn open_table_tab(&mut self, table: TableRef, cx: &mut Context<Self>) {
        // Both before the switch, and in this order. ⌘P and ⌘⇧F reach here
        // with the cell editor still open, and an editor closed on the far
        // side of the switch is an edit landing on the table it was never
        // typed into; an editor closed after the stash is a value staged
        // without it.
        self.finish_cell_edit(cx);
        // Same as `open_sql_tab`: what was typed reaches the staged batch
        // before the tab it belongs to stops being the one in front.
        self.stash_current_draft(cx);
        self.tabs.open_table(table.clone());
        self.workspace.open_table = Some(table);
        self.selected_cell = None;
        self.persist_session();
        self.load_active_table(cx);
    }

    /// Reload the front table tab.
    ///
    /// Every command that rereads a table -- refresh, paging, the filters, the
    /// page size, sorting, opening the tab -- comes through here, so this is
    /// where the open draft is folded into the staged batch. The load that
    /// lands sets `draft` to `None`, and what was typed lives only in that
    /// draft's inputs: a path that reloaded without stashing first destroyed
    /// it with no message and nothing to recover it from. Staging is by
    /// primary key rather than by position, so the edit goes on counting and
    /// goes on committing even when the page that comes back no longer holds
    /// the row.
    ///
    /// The cell editor is finished first for the reason it is in
    /// `save_pending_edits`: it folds into the draft, and the draft is what is
    /// folded away here, so the other order stashes the value without it.
    pub(crate) fn load_active_table(&mut self, cx: &mut Context<Self>) {
        self.finish_cell_edit(cx);
        self.stash_current_draft(cx);
        // Written on every reload, refusal or not, so a complaint answered by
        // the next one cannot outlive it.
        self.refused_draft = self.draft_error();
        let Some(tab_id) = self.tabs.active_id() else {
            return;
        };
        self.load_table(tab_id, cx);
    }

    fn load_table(&mut self, tab_id: crate::tabs::TabId, cx: &mut Context<Self>) {
        // The connection is taken with the driver and carried through the
        // spawn: it is what the rows coming back belong to, and by the time
        // they land it may no longer be the one in front.
        let (Some(driver), Some(connection)) =
            (self.workspace.active_driver(), self.workspace.active_id())
        else {
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
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.set_error(None);
        }

        // No timeout: a page load has never had one, and the connection's
        // query timeout is a promise about the statements the user types.
        let (stop_handle, stop) = commands::stop_signal(None);
        let run = self.next_run;
        self.next_run += 1;
        let load_key = (Some(connection), tab_id);
        // The load this one replaces -- a page turned twice, or a tab brought
        // back to the front before its first load came in -- would only be
        // thrown away as stale when it landed. Stopped instead: on a big
        // table its count can run for minutes, holding a connection and the
        // footer's "Loading" all the while.
        if let Some((old, handle)) = self.table_loads.insert(load_key, (run, stop_handle)) {
            handle.stop();
            self.abandoned.insert(old);
            self.release_load();
        }
        let task = commands::open_table(
            &self.runtime,
            driver,
            table.clone(),
            page,
            where_clause.clone(),
            sort,
            stop,
        );
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if this
                    .table_loads
                    .get(&load_key)
                    .is_some_and(|(held, _)| *held == run)
                {
                    this.table_loads.remove(&load_key);
                }
                // Its tab closed and already let go of it.
                if this.abandoned.remove(&run) {
                    return;
                }
                this.finish_tab_load(
                    connection,
                    tab_id,
                    load_seq,
                    |this, is_current, is_active| match landed {
                        Some(Ok(contents)) if is_current => {
                            // Taken here rather than under `is_active`: a
                            // complaint this reload cannot say is one the next
                            // load says instead, against a tab it was never
                            // about -- and the post-commit reload is the one
                            // load that does not overwrite it first.
                            let refused = this.refused_draft.take();
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
                            this.apply_column_widths(tab_id);
                            if is_active {
                                // The draft this load just cleared may have
                                // been one nothing could be staged from. ⌘S
                                // answers that complaint by name; a reload
                                // that swallowed it taught the user their
                                // typing simply disappears.
                                this.status = match refused {
                                    Some(message) => {
                                        Status::error(format!("Edit not kept — {message}"))
                                    }
                                    None => Status::Idle,
                                };
                                this.focus = Focus::Grid;
                            }
                        }
                        // Kept on the tab, not just in the footer: a table
                        // that failed to load behind a tab the user has since
                        // moved away from still failed to load.
                        Some(Err(error)) if is_current => {
                            // A load that failed kept the draft, complaint and
                            // all, so there is nothing here to report and
                            // nothing to leave lying about.
                            this.refused_draft = None;
                            this.put_run_failure(tab_id, &error, &[], 0, is_active);
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
        connection: ConnectionId,
        tab_id: crate::tabs::TabId,
        load_seq: u64,
        apply: impl FnOnce(&mut Self, bool, bool),
    ) {
        // Nothing is written anywhere but the front tab list, so a load whose
        // connection has been swapped out from under it is dropped. Tab ids
        // count from zero within each connection, and `load_is_current` asked
        // against the list in front would vouch for a stranger's tab of the
        // same number and the same load count -- which is another table's rows
        // written over it, and the half-typed draft on it thrown away. Nothing
        // was staged on a load, so there is nothing to reconcile by dropping
        // one; the tab keeps what it was already showing.
        let in_front = self.workspace.active_id() == Some(connection);
        let is_current = in_front && self.tabs.load_is_current(tab_id, load_seq);
        let is_active = in_front && self.tabs.active_id() == Some(tab_id);
        apply(self, is_current, is_active);
        self.release_load();
    }

    #[cfg(test)]
    pub(crate) fn abandoned_is_empty(&self) -> bool {
        self.abandoned.is_empty()
    }

    /// One load or run fewer in flight; the footer stops saying "busy" once
    /// none are.
    fn release_load(&mut self) {
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

        if matches!(self.tabs.active(), Some(WorkspaceTab::Sql { .. })) {
            // By name, so the first column of that name is the one sorted.
            // Only reached from callers that have nothing better -- the
            // header itself knows which column it is and uses
            // `toggle_sort_at`, because a query can name two of them alike.
            let Some(at) = self
                .tabs
                .active()
                .and_then(|tab| tab.result())
                .and_then(|view| view.set.columns.iter().position(|info| info.name == column))
            else {
                return;
            };
            self.sort_query_result(at, cx);
            return;
        }

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

    /// Click a header, when the header knows which column it is.
    ///
    /// The index, not the name: a query result can carry two columns of the
    /// same name (`SELECT a.id, b.id`), and sorting by name would sort the
    /// first of them whichever of the two was clicked.
    pub(crate) fn toggle_sort_at(&mut self, column: usize, cx: &mut Context<Self>) {
        if matches!(self.tabs.active(), Some(WorkspaceTab::Sql { .. })) {
            self.stash_current_draft(cx);
            self.sort_query_result(column, cx);
            return;
        }
        let name = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .and_then(|view| view.set.columns.get(column))
            .map(|info| info.name.clone());
        if let Some(name) = name {
            self.toggle_sort(&name, cx);
        }
    }

    /// Sort a query result without touching the query.
    ///
    /// The rows already fetched are reordered in place. Nothing is sent, the
    /// statement in the editor is left exactly as written, and a third click
    /// puts the server's own order back -- which is why the view keeps where
    /// each row started.
    fn sort_query_result(&mut self, column: usize, cx: &mut Context<Self>) {
        let column_name = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .and_then(|view| view.set.columns.get(column))
            .map(|info| info.name.clone());
        let Some(column_name) = column_name else {
            return;
        };

        let Some(WorkspaceTab::Sql {
            sort,
            result: Some(view),
            selected_row,
            selection,
            draft,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };

        let next = crate::tabs::QuerySort::cycled(*sort, column);
        match &next {
            Some(key) => view.sort_rows(column, key.ascending),
            None => view.restore_server_order(),
        }
        *sort = next;

        // Every row index the rest of the window is holding refers to a
        // position, and the positions just moved. Keeping the selection would
        // point the sidebar at whichever row landed where the old one was.
        *selected_row = None;
        selection.clear();
        *draft = None;
        self.selected_cell = None;
        self.copied_cell = None;
        self.copied_field = None;
        // Positional flashes too: lit rows that were copied from the top of
        // the page would otherwise light whichever rows sorted up there.
        self.cell_flash = None;
        if matches!(
            self.row_flash,
            Some(RowFlash {
                rows: FlashRows::Copied(_),
                ..
            })
        ) {
            self.row_flash = None;
        }

        self.status = match &next {
            Some(key) if key.ascending => Status::info(format!("Sorted by {column_name} ↑")),
            Some(_) => Status::info(format!("Sorted by {column_name} ↓")),
            None => Status::info("Sort cleared"),
        };
        cx.notify();
    }

    /// Drop the sort and go back to the table's own key order.
    pub(crate) fn clear_sort(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Sql {
            sort,
            result: Some(view),
            selected_row,
            selection,
            draft,
            ..
        }) = self.tabs.active_mut()
        {
            if sort.take().is_none() {
                return;
            }
            view.restore_server_order();
            *selected_row = None;
            selection.clear();
            *draft = None;
            self.selected_cell = None;
            self.status = Status::info("Sort cleared");
            cx.notify();
            return;
        }

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

    /// The sort the active tab is showing, named -- for the palette and for
    /// anything reporting it in words.
    ///
    /// Owned rather than borrowed because a query's sort is held by index and
    /// the name is read back off the result to answer this.
    pub(crate) fn active_sort(&self) -> Option<dbui_app::domain::SortKey> {
        match self.tabs.active()? {
            WorkspaceTab::Table { sort, .. } => sort.clone(),
            WorkspaceTab::Sql { sort, result, .. } => {
                let sort = (*sort)?;
                let name = result
                    .as_ref()
                    .and_then(|view| view.set.columns.get(sort.column))
                    .map(|info| info.name.clone())?;
                Some(dbui_app::domain::SortKey {
                    column: name,
                    ascending: sort.ascending,
                })
            }
        }
    }

    /// Which column carries the header arrow, by index into the result's
    /// columns.
    ///
    /// The header draws by index rather than matching on the name, so that a
    /// query returning two columns of one name marks the one that is actually
    /// sorted instead of both of them.
    pub(crate) fn active_sort_column(&self) -> Option<(usize, bool)> {
        match self.tabs.active()? {
            WorkspaceTab::Sql { sort, .. } => sort.map(|key| (key.column, key.ascending)),
            WorkspaceTab::Table { sort, result, .. } => {
                let sort = sort.as_ref()?;
                let at = result
                    .as_ref()?
                    .set
                    .columns
                    .iter()
                    .position(|info| info.name == sort.column)?;
                Some((at, sort.ascending))
            }
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

        let Some(WorkspaceTab::Table { page, .. }) = self.tabs.active_mut() else {
            return;
        };
        // Compared against the tab's own page, not the result's. The *next*
        // page is computed from what is on screen so a burst of clicks cannot
        // run ahead of the rows -- but the early return has to ask "is the tab
        // already asking for this?", or a load that failed leaves the tab
        // pointing at a page the result never reached and paging back becomes
        // a no-op.
        if next == *page {
            return;
        }
        *page = next;
        self.load_active_table(cx);
    }

    /// What ⌘↵ means: follow the link under the cursor, or run the statement.
    ///
    /// Both the key binding's action and `on_key` route here. They used to
    /// each have their own idea, and since the action is dispatched first the
    /// window did one thing while the tests proved the other.
    pub(crate) fn run_or_follow_link(&mut self, cx: &mut Context<Self>) {
        if !self.text_undo_has_focus() {
            if let Some((row, column)) = self.selected_cell {
                if self.foreign_key_at(row, column).is_some() {
                    self.follow_foreign_key(row, column, cx);
                    return;
                }
            }
        }
        self.run_query(cx);
    }

    pub(crate) fn run_query(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.resolve_run_sql() else {
            return;
        };
        self.dispatch_statements(statements, cx);
    }

    /// Whether the editor's session has a transaction open, as of its last
    /// statement. Read off the driver, which asked the server.
    pub(crate) fn editor_transaction(&self) -> dbui_app::domain::TransactionState {
        self.workspace
            .active_driver()
            .map(|driver| driver.editor_transaction())
            .unwrap_or_default()
    }

    /// The transaction bar's buttons: end the editor's transaction with
    /// `statement` (`COMMIT` or `ROLLBACK`), as if it had been typed.
    pub(crate) fn end_editor_transaction(&mut self, statement: &str, cx: &mut Context<Self>) {
        self.dispatch_statements(vec![statement.to_string()], cx);
    }

    /// Ask for the plan of the statement under the caret (or each one in
    /// the selection) instead of running it. The plan lands in the result
    /// pane drawn as a tree; see `plan_view`.
    ///
    /// Plain `EXPLAIN`, never `ANALYZE`: the statement itself is not run, so
    /// explaining a `DELETE` deletes nothing.
    pub(crate) fn explain_query(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.resolve_run_sql() else {
            return;
        };
        let driver = self.sql_dialect();
        let explained = statements
            .iter()
            .map(|sql| dbui_app::plan::explain_sql(driver, sql))
            .collect();
        self.dispatch_statements(explained, cx);
    }

    pub(crate) fn run_all_queries(&mut self, cx: &mut Context<Self>) {
        let Some(statements) = self.resolve_run_all_sql() else {
            return;
        };
        self.dispatch_statements(statements, cx);
    }

    /// Put a statement from the history into the editor, ready to run.
    ///
    /// Loaded rather than run: a statement pulled out of history is one the
    /// user wants to look at before it goes anywhere, and some of them are the
    /// DELETE that made them open the history in the first place.
    pub(crate) fn put_sql_in_editor(&mut self, sql: &str, cx: &mut Context<Self>) {
        self.load_sql_into_editor(sql, "Loaded from history — ⌘↵ to run", cx);
    }

    /// Put `sql` in the query tab's editor in place of what is there.
    ///
    /// As one edit, so ⌘Z brings back what it replaced: the tab has one
    /// editor, and whatever was being written in it is not lost to a
    /// mis-picked history row or saved query.
    /// Read a function's, trigger's... definition and open it in a new query
    /// tab, where it can be read, changed and run back.
    pub(crate) fn open_definition(
        &mut self,
        object: dbui_app::domain::DbObject,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };
        let connection = self.workspace.active_id();
        let label = format!("{} {}", object.kind.label(), object.name);
        self.status = Status::busy(format!("Reading {label}…"));
        cx.notify();
        let task = commands::fetch_definition(&self.runtime, driver, object);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                // Opened on the connection it came from, or not at all: a
                // definition dropped into another server's editor is one
                // ⌘↵ away from being created there.
                if this.workspace.active_id() != connection {
                    return;
                }
                match landed {
                    Some(Ok(sql)) => {
                        this.load_sql_into_editor(&sql, &format!("Opened the {label}"), cx);
                        if let Some(WorkspaceTab::Sql { editor, .. }) = this.tabs.active_mut() {
                            editor.move_to(0);
                        }
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

    pub(crate) fn load_sql_into_editor(&mut self, sql: &str, said: &str, cx: &mut Context<Self>) {
        self.open_sql_tab(cx);
        if let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() {
            let end = editor.text().len();
            editor.replace_range(0..end, sql);
        }
        self.focus = Focus::Editor;
        self.status = Status::info(said.to_string());
        cx.notify();
    }

    /// ⌘⇧S on a query tab: name the editor's SQL to keep it.
    pub(crate) fn open_save_query(&mut self, cx: &mut Context<Self>) {
        let has_sql = matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Sql { editor, .. }) if !editor.text().trim().is_empty()
        );
        if !has_sql {
            self.status = Status::info("Write a query first — saving keeps the SQL tab's text");
            cx.notify();
            return;
        }
        self.open_palette(crate::components::palette::PaletteKind::SaveQuery, cx);
    }

    /// Keep the query tab's SQL under `name`.
    pub(crate) fn save_query_as(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(problem) = &self.saved_queries_unreadable {
            self.status = Status::error(format!(
                "Not saved: the saved queries file could not be read ({problem})"
            ));
            cx.notify();
            return;
        }
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return;
        };
        let sql = editor.text().trim().to_string();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        self.saved_queries.save(name, &sql, at);
        self.status = match self.write_saved_queries() {
            Ok(()) => Status::info(format!("Saved “{}” — ⌘⇧O to open it again", name.trim())),
            Err(message) => Status::error(message),
        };
        cx.notify();
    }

    pub(crate) fn delete_saved_query(&mut self, name: &str, cx: &mut Context<Self>) {
        if self.saved_queries_unreadable.is_some() || !self.saved_queries.remove(name) {
            return;
        }
        self.status = match self.write_saved_queries() {
            Ok(()) => Status::info(format!("Deleted “{name}”")),
            Err(message) => Status::error(message),
        };
        cx.notify();
    }

    fn write_saved_queries(&self) -> Result<(), String> {
        dbui_app::saved::saved_queries_path()
            .and_then(|path| dbui_app::saved::save(&path, &self.saved_queries))
            .map_err(|error| error.to_string())
    }

    /// Read the saved queries from disk. Called once at launch.
    pub fn load_saved_queries(&mut self) {
        let loaded =
            dbui_app::saved::saved_queries_path().and_then(|path| dbui_app::saved::load(&path));
        match loaded {
            Ok(saved) => self.saved_queries = saved,
            Err(error) => self.saved_queries_unreadable = Some(error.to_string()),
        }
    }

    /// Drop a ready-made statement into the editor at the caret.
    ///
    /// Inserted rather than pasted over: someone reaching for a `JOIN` in the
    /// middle of a session has the rest of their buffer open, and a template
    /// that replaces it costs them what they were writing. A blank editor is
    /// the same operation either way.
    ///
    /// It goes in on a line of its own, so a template dropped at the end of a
    /// half-typed statement does not silently splice itself into one.
    pub(crate) fn insert_sql_template(&mut self, name: &str, body: &str, cx: &mut Context<Self>) {
        self.open_sql_tab(cx);
        if let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() {
            let existing = editor.text();
            let at_line_start = existing.is_empty()
                || existing[..editor.cursor().min(existing.len())].ends_with('\n');
            let text = if at_line_start {
                body.to_string()
            } else {
                format!("\n{body}")
            };
            editor.insert(&text);
            editor.ensure_editor_caret_visible();
        }
        self.focus = Focus::Editor;
        self.status = Status::info(format!("{name} — fill it in, ⌘↵ to run"));
        cx.notify();
    }

    /// ⌃Space: open the SQL autocomplete popup at the caret with everything
    /// that matches, even when the word is already complete.
    pub(crate) fn trigger_completion(&mut self, cx: &mut Context<Self>) {
        self.completion_auto = false;
        self.refresh_completion(cx);
    }

    /// The fewest characters of a bare word before the popup opens by
    /// itself. One would put a list over every `a` in `WHERE x = a`.
    const AUTO_COMPLETE_MIN: usize = 2;

    /// Open the popup as the user types, when there is something worth
    /// offering: after a `.`, or a word of [`Self::AUTO_COMPLETE_MIN`]
    /// characters that is not a number and not inside a string or comment.
    pub(crate) fn auto_complete(&mut self, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return;
        };
        let caret = editor.cursor();
        let in_text = caret > 0
            && crate::sql_format::highlight_spans(editor.text(), self.sql_dialect())
                .iter()
                .any(|(start, end, style)| {
                    matches!(
                        style,
                        crate::sql_format::SqlStyle::String | crate::sql_format::SqlStyle::Comment
                    ) && *start < caret
                        && caret <= *end
                });
        if in_text {
            return;
        }
        self.completion_auto = true;
        self.refresh_completion(cx);
    }

    /// Rebuild the popup at the caret, in whichever mode it is in.
    pub(crate) fn refresh_completion(&mut self, cx: &mut Context<Self>) {
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
        let mut popup =
            crate::sql_complete::build_popup(&request, catalog, &self.column_cache, &sql, caret);
        if self.completion_auto {
            let worth_it = request.qualifier.is_some()
                || request.prefix.chars().count() >= Self::AUTO_COMPLETE_MIN
                    && !request.prefix.starts_with(|c: char| c.is_ascii_digit());
            // A word typed out in full is not offered back: Enter after
            // `FROM users` is a new line, not a request to write `users` again.
            if let Some(popup) = popup.as_mut() {
                popup
                    .items
                    .retain(|item| !item.label.eq_ignore_ascii_case(&request.prefix));
                popup.selected = 0;
            }
            if !worth_it || popup.as_ref().is_some_and(|popup| popup.items.is_empty()) {
                popup = None;
            }
        }
        self.completion = popup;
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
                        this.refresh_completion(cx);
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
        editor.ensure_editor_caret_visible();
        cx.notify();
    }

    pub(crate) fn dismiss_completion(&mut self, cx: &mut Context<Self>) {
        if self.completion.take().is_some() {
            cx.notify();
        }
    }

    /// The engine the editor's SQL is for, which decides how its strings
    /// read. Not connected, the standard reading.
    pub(crate) fn sql_dialect(&self) -> dbui_app::domain::Driver {
        self.workspace
            .active()
            .map_or(dbui_app::domain::Driver::Postgres, |entry| {
                entry.config.driver
            })
    }

    /// ⌘↵ target: the statements in the selection if there is one, else the
    /// statement under the caret.
    ///
    /// A selection is split like Run All would split it. Sent whole, several
    /// statements went to the server as one prepared statement, which
    /// PostgreSQL refuses outright ("cannot insert multiple commands into a
    /// prepared statement").
    pub(crate) fn resolve_run_sql(&self) -> Option<Vec<String>> {
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active() else {
            return None;
        };
        let dialect = self.sql_dialect();
        if let Some(selected) = editor.selected_text() {
            let statements: Vec<String> = dbui_app::domain::split_statements_for(dialect, selected)
                .into_iter()
                .map(|range| selected[range].trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            return (!statements.is_empty()).then_some(statements);
        }
        let text = editor.text();
        let range = dbui_app::domain::statement_at_for(dialect, text, editor.cursor())?;
        let stmt = text[range].trim();
        if stmt.is_empty() {
            None
        } else {
            Some(vec![stmt.to_string()])
        }
    }

    /// Whether the front tab has a run that Stop would end.
    pub(crate) fn active_run_is_stoppable(&self) -> bool {
        self.tabs.active_id().is_some_and(|tab| {
            self.running
                .contains_key(&(self.workspace.active_id(), tab))
        })
    }

    /// ⌘. -- end the front tab's run, on the server as well as here.
    ///
    /// The handle is taken rather than read, so a second press while the
    /// server is still winding down is quiet instead of a second cancel.
    pub(crate) fn stop_query(&mut self, cx: &mut Context<Self>) {
        // A file being run stops between statements; what ran, stays.
        if let Some(handle) = self.script_stop.take() {
            handle.stop();
            self.status = Status::busy("Stopping the file…");
            cx.notify();
            return;
        }
        let connection = self.workspace.active_id();
        let Some((_, handle)) = self
            .tabs
            .active_id()
            .and_then(|tab| self.running.remove(&(connection, tab)))
        else {
            return;
        };
        handle.stop();
        self.status = Status::busy("Cancelling…");
        cx.notify();
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
        let statements: Vec<String> =
            dbui_app::domain::split_statements_for(self.sql_dialect(), scope)
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

    /// Add statements to the history and write it out.
    ///
    /// Recorded when they are *sent*, not when they land: a statement that
    /// errored is often the one most worth getting back, and one that never
    /// returns is worth seeing too. The outcome is folded in when it arrives.
    fn record_history(&mut self, statements: &[String]) {
        let connection = self.workspace.active_id();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        for sql in statements {
            self.history.record(dbui_app::HistoryEntry {
                sql: sql.clone(),
                connection,
                at,
                ok: true,
            });
        }
        // A history that cannot be written is not worth a message over the
        // query the user is actually looking at.
        let _ = dbui_app::history::history_path()
            .and_then(|path| dbui_app::history::save(&path, &self.history));
    }

    /// Mark the most recent history entry as having failed.
    fn record_history_failure(&mut self, sql: &str) {
        if let Some(entry) = self
            .history
            .entries
            .iter_mut()
            .find(|entry| entry.sql == sql)
        {
            entry.ok = false;
        }
        let _ = dbui_app::history::history_path()
            .and_then(|path| dbui_app::history::save(&path, &self.history));
    }

    pub(crate) fn dispatch_statements(&mut self, statements: Vec<String>, cx: &mut Context<Self>) {
        if statements.is_empty() {
            return;
        }
        // A completion list open at the caret is about the text, not the
        // run; left up, it hangs over the results the run brings back.
        self.completion = None;
        // The server refuses writes on a read-only connection too, but only
        // through a session setting the editor could switch off. Nothing in
        // the batch runs if any of it would write.
        if let Some(write) = statements.iter().find(|sql| dbui_app::domain::writes(sql)) {
            let verb = dbui_app::domain::statement::describe(write).verb;
            if self.refuse_if_read_only(&verb, cx) {
                return;
            }
            // A production connection takes writes, but asks first. The whole
            // batch waits, reads included, so the answer runs it as typed.
            if self.hold_for_production(GuardedWrite::Statements(statements.clone()), cx) {
                return;
            }
        }
        self.record_history(&statements);

        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let Some(connection) = self.workspace.active_id() else {
            return;
        };
        let Some((tab_id, load_seq)) = self.tabs.begin_active_load() else {
            return;
        };

        self.workspace.open_table = None;
        self.selected_cell = None;
        self.loads_in_flight = self.loads_in_flight.saturating_add(1);
        self.status = Status::busy("Running…");
        // The previous failure belonged to the previous run. It goes now, not
        // when the new one lands, so a slow query does not leave a stale
        // complaint sitting over it.
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.set_error(None);
        }

        let sent = statements.clone();
        let timeout = self
            .workspace
            .active()
            .map(|entry| entry.config.query_timeout_secs)
            .filter(|seconds| *seconds > 0)
            .map(|seconds| std::time::Duration::from_secs(u64::from(seconds)));
        let (stop_handle, stop) = commands::stop_signal(timeout);
        // A second run on the same tab replaces the first's handle: that run
        // is superseded -- its result will be dropped as stale -- and Stop
        // now means the one the user is looking at.
        let run = self.next_run;
        self.next_run += 1;
        let run_key = (self.workspace.active_id(), tab_id);
        self.running.insert(run_key, (run, stop_handle));
        let task = commands::run_queries(&self.runtime, driver, statements, stop);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if this
                    .running
                    .get(&run_key)
                    .is_some_and(|(held, _)| *held == run)
                {
                    this.running.remove(&run_key);
                }
                // Its tab closed and already let go of it.
                if this.abandoned.remove(&run) {
                    return;
                }
                let mut catalog_is_stale = false;
                this.finish_tab_load(
                    connection,
                    tab_id,
                    load_seq,
                    |this, is_current, is_active| match landed {
                        Some(Ok(batch)) if is_current => {
                            catalog_is_stale =
                                this.absorb_batch_result(tab_id, batch, &sent, is_active);
                        }
                        // Nothing ran at all -- the pool went away, or the
                        // driver refused before the first statement.
                        Some(Err(error)) if is_current => {
                            this.put_run_failure(tab_id, &error, &sent, 0, is_active);
                        }
                        _ => {}
                    },
                );
                if catalog_is_stale {
                    this.refresh_catalog_quietly(cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Record a failed run on the tab that ran it.
    ///
    /// The error goes on the tab whether or not that tab is in front. A run
    /// whose error only reached the status bar of the active tab was a run
    /// that failed silently the moment the user clicked somewhere else.
    fn put_run_failure(
        &mut self,
        tab_id: crate::tabs::TabId,
        error: &dbui_app::DriverError,
        sent: &[String],
        succeeded: usize,
        is_active: bool,
    ) {
        // The engine names the statement it choked on; when it does not -- a
        // dropped connection, say -- the batch got as far as `succeeded`, so
        // the next one is the one that never finished.
        let sql = error
            .statement()
            .map(str::to_string)
            .or_else(|| sent.get(succeeded).cloned())
            .unwrap_or_default();

        if !sql.is_empty() {
            self.record_history_failure(&sql);
        }

        // Asked for, so not an error: the tab keeps what it had, and the
        // footer says where the run was when it stopped.
        if matches!(error, dbui_app::DriverError::Cancelled { .. }) {
            if is_active {
                self.status = Status::info(if succeeded > 0 {
                    format!("Cancelled after {succeeded} of {} statements", sent.len())
                } else {
                    "Cancelled".to_string()
                });
            }
            return;
        }

        let failure = crate::tabs::StatementError {
            sql,
            message: error.to_string(),
            code: error.code().map(str::to_string),
            succeeded,
            skipped: sent.len().saturating_sub(succeeded + 1),
        };
        if is_active {
            self.status = Status::error(failure.headline());
        }
        if let Some(tab) = self.tabs.get_mut(tab_id) {
            tab.set_error(Some(failure));
        }
    }

    /// Keep every statement's result, not just the last row-producing one.
    ///
    /// Run-all used to discard all but the last grid, which made a batch whose
    /// interesting statement was in the middle unreadable. Each one becomes an
    /// entry in the strip above the grid; the first that produced rows is put
    /// in front, because that is usually the one being looked for.
    fn absorb_batch_result(
        &mut self,
        tab_id: crate::tabs::TabId,
        batch: commands::BatchQueryResult,
        sent: &[String],
        is_active: bool,
    ) -> bool {
        let summary = batch.summary();
        let catalog_is_stale = batch.changed_the_catalog();
        let failure = batch.failure;
        let statements: Vec<crate::tabs::StatementResult> = batch
            .results
            .into_iter()
            .map(|result| {
                let sql = result.statement.clone();
                let one_line = result.summary();
                let rows = match result.outcome {
                    QueryOutcome::Rows(set) => Some(ResultView::new(
                        set,
                        ResultSource::Query { sql: sql.clone() },
                        one_line.clone(),
                        Vec::new(),
                    )),
                    QueryOutcome::Affected(_) => None,
                };
                // Read once, here: a plan is drawn on every frame the tab
                // is in front, and the rows it comes from never change.
                let plan = rows
                    .as_ref()
                    .and_then(|view| dbui_app::Plan::from_result(&view.set));
                crate::tabs::StatementResult {
                    sql,
                    rows,
                    summary: one_line,
                    plan,
                    show_rows: false,
                }
            })
            .collect();

        let front = statements
            .iter()
            .position(|statement| statement.rows.is_some())
            .unwrap_or(0);
        let produced_rows = statements.iter().any(|statement| statement.rows.is_some());
        let count = statements.len();

        if let Some(WorkspaceTab::Sql {
            results,
            active_result,
            selected_row,
            selection,
            draft,
            ..
        }) = self.tabs.get_mut(tab_id)
        {
            *results = statements;
            *active_result = front;
            *selected_row = None;
            selection.clear();
            *draft = None;
        }
        self.show_statement_result(tab_id, front);

        // A batch that stopped early still has whatever ran before it on
        // screen; the error says how far it got and what stopped it.
        // The editor keeps the keyboard when a run failed: the next thing to
        // do is fix the statement, not walk rows it did not return.
        if let Some(error) = &failure {
            self.put_run_failure(tab_id, error, sent, count, is_active);
        } else {
            if let Some(tab) = self.tabs.get_mut(tab_id) {
                tab.set_error(None);
            }
            if is_active {
                self.status = if produced_rows && count == 1 {
                    Status::Idle
                } else {
                    Status::info(summary)
                };
                if produced_rows {
                    self.focus = Focus::Grid;
                }
            }
        }

        catalog_is_stale
    }

    #[cfg(test)]
    pub(crate) fn absorb_batch_result_for_test(
        &mut self,
        tab_id: crate::tabs::TabId,
        batch: commands::BatchQueryResult,
        sent: &[String],
        is_active: bool,
    ) -> bool {
        self.absorb_batch_result(tab_id, batch, sent, is_active)
    }

    /// Dismiss the error panel on the active tab.
    pub(crate) fn clear_tab_error(&mut self, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.active_mut() {
            tab.set_error(None);
        }
        if matches!(self.status, Status::Error(_)) {
            self.status = Status::Idle;
        }
        cx.notify();
    }

    /// Put a failed statement's error on the clipboard, message and all.
    pub(crate) fn copy_tab_error(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self
            .tabs
            .active()
            .and_then(|tab| tab.error())
            .map(|error| error.to_clipboard())
        else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.status = Status::info("Error copied");
        cx.notify();
    }

    /// Put one statement's rows in the grid.
    pub(crate) fn show_statement_result(&mut self, tab_id: crate::tabs::TabId, index: usize) {
        let Some(WorkspaceTab::Sql {
            results,
            active_result,
            result,
            selected_row,
            selection,
            draft,
            sort,
            ..
        }) = self.tabs.get_mut(tab_id)
        else {
            return;
        };
        if index >= results.len() {
            *result = None;
            return;
        }
        *active_result = index;
        *selected_row = None;
        selection.clear();
        *draft = None;
        // Another statement's rows arrive in the order it returned them.
        *sort = None;
        // Moved out of the list and back rather than cloned: a result set can
        // be thousands of rows and only one is on screen at a time. Exactly
        // one of `results[active].rows` and `result` holds it; the caller
        // hands the old one back before asking for a new one.
        *result = results[index].rows.take();
        // A result set aside while it was sorted keeps the order the user put
        // it in, and the sort cleared above would then be a header saying
        // nothing over rows that are plainly in some order. Put the
        // statement's own back, so the two agree.
        if let Some(view) = result.as_mut() {
            view.restore_server_order();
        }
    }

    /// Select a statement from the strip above the grid.
    pub(crate) fn select_statement_result(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tab_id) = self.tabs.active_id() else {
            return;
        };
        // The grid currently owns one of the results; hand it back first.
        if let Some(WorkspaceTab::Sql {
            results,
            active_result,
            result,
            ..
        }) = self.tabs.get_mut(tab_id)
        {
            if let Some(slot) = results.get_mut(*active_result) {
                if slot.rows.is_none() {
                    slot.rows = result.take();
                }
            }
        }
        self.show_statement_result(tab_id, index);
        // Saying what the selected statement did is the whole reason the
        // strip keeps the ones that returned no rows.
        if let Some(WorkspaceTab::Sql { results, .. }) = self.tabs.get_mut(tab_id) {
            if let Some(statement) = results.get(index) {
                let summary = statement.summary.clone();
                self.status = Status::info(summary);
            }
        }
        cx.notify();
    }

    pub(crate) fn refresh_result(&mut self, cx: &mut Context<Self>) {
        self.result_refreshes += 1;
        match self.tabs.active() {
            Some(WorkspaceTab::Table { .. }) => self.load_active_table(cx),
            Some(WorkspaceTab::Sql { .. }) => self.run_query(cx),
            None => self.refresh_catalog(cx),
        }
    }

    pub(crate) fn set_table_pane(&mut self, pane: crate::tabs::TablePane, cx: &mut Context<Self>) {
        let mut needs_indexes = false;
        if let Some(WorkspaceTab::Table {
            pane: tab_pane,
            indexes,
            ..
        }) = self.tabs.active_mut()
        {
            *tab_pane = pane;
            // Read when first asked for, not with every page of rows: most
            // visits to a table never look at its indexes.
            needs_indexes = pane == crate::tabs::TablePane::Structure && indexes.is_none();
        }
        if needs_indexes {
            self.load_indexes(cx);
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

    /// Set rows-per-page from the preset dropdown.
    ///
    /// Writes the draft first and then goes through [`Self::apply_page_size`]
    /// rather than setting `page.limit` directly: the field and the page have
    /// to agree afterwards, and a preset that moved the page without moving
    /// the number beside it would leave the box lying about what was loaded.
    pub(crate) fn set_page_size(&mut self, limit: u32, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            page_size_draft, ..
        }) = self.tabs.active_mut()
        else {
            return;
        };
        *page_size_draft = crate::text_input::TextInput::with_text(limit.to_string(), false);
        self.apply_page_size(cx);
    }

    /// `(page number, page count)` for the table on screen, both 1-based.
    ///
    /// `None` for a SQL result, and for a table whose total the engine would
    /// not give up: a page number without a count is half a fact, and "Page 2
    /// of ?" is not worth the width.
    pub(crate) fn page_position(&self) -> Option<(u64, u64)> {
        let view = self.tabs.active()?.result()?;
        let ResultSource::Table {
            page, total_rows, ..
        } = &view.source
        else {
            return None;
        };
        let total = u64::try_from((*total_rows)?).ok()?;
        Some(page_position_of(page.offset, page.limit, total))
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
        // The marks belong to the row they were taken from; carrying one onto
        // the next row would claim a copy that never happened.
        self.copied_field = None;
        self.copied_cell = None;
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
                // A stash that worked answers whatever the last one complained
                // about. Leaving the complaint up would strand it under a
                // draft that has since been fixed -- and `draft_error` would
                // go on refusing a commit on the strength of it.
                if let Some(draft) = draft.as_mut() {
                    if draft.message.as_ref().is_some_and(|(ok, _)| !ok) {
                        draft.message = None;
                    }
                }
            }
            Err(message) => {
                if let Some(draft) = draft.as_mut() {
                    draft.message = Some((false, message));
                }
                cx.notify();
            }
        }
    }

    /// What the open draft is refusing to stage, if anything.
    ///
    /// [`stash_current_draft`] can only report into the sidebar, which is a
    /// small red line the eye slides off -- and on a table with no primary key
    /// it is the *only* sign that typing into a field led nowhere. ⌘S reads it
    /// back so the refusal is stated where the user is looking when they ask
    /// for the commit.
    ///
    /// [`stash_current_draft`]: DbUi::stash_current_draft
    fn draft_error(&self) -> Option<String> {
        let draft = match self.tabs.active() {
            Some(WorkspaceTab::Table { draft, .. }) | Some(WorkspaceTab::Sql { draft, .. }) => {
                draft.as_ref()?
            }
            None => return None,
        };
        match draft.message.as_ref() {
            Some((false, message)) => Some(message.clone()),
            _ => None,
        }
    }

    // -- grid selection -----------------------------------------------------

    /// Whether the grid's shortcuts apply right now.
    ///
    /// Clicking a row hands the keyboard to the detail sidebar so the fields
    /// are typeable, and ⌘A there still means "the rows I clicked" until an
    /// actual field takes focus — otherwise selecting rows and pressing ⌘A
    /// would do nothing, which reads as the shortcut being broken.
    ///
    /// An open cell editor takes them all back: it is a text box sitting on
    /// the grid, and `focus` stays [`Focus::Grid`] while it is up.
    pub(crate) fn grid_owns_keys(&self) -> bool {
        if self.editing_cell.is_some() {
            return false;
        }
        self.focus == Focus::Grid || (self.focus == Focus::Detail && self.detail_input.is_none())
    }

    /// The active connection's environment tag.
    pub(crate) fn active_environment(&self) -> dbui_app::domain::Environment {
        self.workspace
            .active()
            .map(|entry| entry.config.environment)
            .unwrap_or_default()
    }

    /// Whether the active connection refuses writes.
    pub(crate) fn is_read_only(&self) -> bool {
        self.workspace
            .active()
            .map(|entry| entry.config.read_only)
            .unwrap_or(false)
    }

    /// Refuse a write and say why. Returns true when the caller should stop.
    ///
    /// Checked at each place that would actually write rather than only where
    /// changes are staged: staging is harmless, and refusing it would make a
    /// read-only connection feel broken rather than protected.
    pub(crate) fn refuse_if_read_only(&mut self, what: &str, cx: &mut Context<Self>) -> bool {
        if !self.is_read_only() {
            return false;
        }
        self.status = Status::error(format!(
            "{what} refused — this connection is marked read only"
        ));
        cx.notify();
        true
    }

    /// Whether a text editor currently owns the text keys — ⌘Z for its own
    /// undo stack, and ⌘C / ⌘V / ⌘D for the characters rather than the rows.
    ///
    /// Everywhere else ⌘Z means "discard the staged batch". The batch belongs
    /// to the tab rather than to any one surface, so it has to work from the
    /// tree as well as the grid — the change bubble is on screen either way,
    /// and a shortcut that does nothing while the thing it undoes is visible
    /// reads as broken.
    ///
    /// The inline cell editor is the exception inside [`Focus::Grid`]: it is a
    /// real text box, so while it is open ⌘Z has a keystroke to undo and the
    /// batch is emphatically not what the user is asking to throw away.
    pub(crate) fn text_undo_has_focus(&self) -> bool {
        if self.editing_cell.is_some() {
            return true;
        }
        match self.focus {
            Focus::Editor
            | Focus::Find
            | Focus::Filter
            | Focus::PageSize
            | Focus::SidebarSearch => true,
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

        // ⌥-click opens what the cell points at. A plain click cannot: an
        // editable column that happens to be a foreign key still has to be
        // selectable, copyable and editable like any other.
        if modifiers.alt {
            if let Some(column) = column {
                if self.foreign_key_at(row, column).is_some() {
                    self.follow_foreign_key(row, column, cx);
                    return;
                }
            }
        }

        // Clicking away from an open editor commits it, the way a spreadsheet
        // does. Leaving the box open over a row the user has moved on from is
        // how an edit gets lost, or lands somewhere it was never typed.
        if self.editing_cell.is_some() && self.editing_cell != column.map(|c| (row, c)) {
            self.commit_cell_edit(cx);
        }

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

    // -- dragging a tab along the strip -------------------------------------

    /// Start moving the window from the titlebar's drag strip.
    ///
    /// macOS only in effect: everywhere else the platform still owns window
    /// movement and there is nothing for us to track. See `mac_window`.
    pub(crate) fn begin_titlebar_drag(&mut self) {
        self.titlebar_drag = true;
        #[cfg(target_os = "macos")]
        crate::mac_window::begin_window_drag();
    }

    /// The pointer has moved with the titlebar in hand.
    ///
    /// No `notify`: the window moves at the window-server level, and nothing
    /// this view draws has changed.
    ///
    /// The move itself waits for the next turn of the main loop. Made here,
    /// inside the mouse event, a move that carries the window onto a screen
    /// of another scale has AppKit report the new scale on the spot -- while
    /// gpui still holds the window for the event -- and gpui drops the report.
    /// The drawable went to 2x, the layout stayed at 1x, and the whole app was
    /// drawn at half size in the top-left corner of the window. `cx.defer` is
    /// not late enough: it still runs inside the event.
    pub(crate) fn drag_titlebar(&mut self, cx: &mut Context<Self>) {
        if !self.titlebar_drag {
            return;
        }
        #[cfg(target_os = "macos")]
        cx.spawn(async move |_, _| crate::mac_window::drag_window())
            .detach();
        #[cfg(not(target_os = "macos"))]
        let _ = cx;
    }

    pub(crate) fn end_titlebar_drag(&mut self) {
        self.titlebar_drag = false;
        #[cfg(target_os = "macos")]
        crate::mac_window::end_window_drag();
    }

    pub(crate) fn begin_tab_drag(&mut self, id: crate::tabs::TabId, x: Pixels) {
        self.tab_drag = Some(TabDrag {
            id,
            start_x: x,
            last_x: x,
            moved: false,
        });
    }

    /// The pointer has crossed the tab at `index` with a tab in hand.
    pub(crate) fn drag_tab_over(&mut self, index: usize, x: Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.as_mut() else {
            return;
        };
        if !drag.moved {
            if f32::from(x - drag.start_x).abs() < TAB_DRAG_SLOP {
                return;
            }
            drag.moved = true;
        }
        let Some(from) = self.tabs.items.iter().position(|tab| tab.id() == drag.id) else {
            self.tab_drag = None;
            return;
        };
        if index == from || index >= self.tabs.items.len() {
            return;
        }
        // Only ever in the direction of travel, and only on a pointer that
        // has actually travelled; see `last_x`.
        let forward = index > from;
        if forward && x <= drag.last_x {
            return;
        }
        if !forward && x >= drag.last_x {
            return;
        }
        drag.last_x = x;
        self.tabs.reorder(from, index);
        // Not persisted here: a drag crosses a slot at a time, and the
        // session file is written whole and synchronously. The strip that
        // matters is the one the pointer is let go over, so `end_tab_drag`
        // writes it once.
        cx.notify();
    }

    /// Let go. The tab that was dragged is the tab left in front.
    pub(crate) fn end_tab_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.take() else {
            return;
        };
        self.drag_pointer = None;
        if drag.moved {
            match self.tabs.items.iter().position(|tab| tab.id() == drag.id) {
                // `activate_tab` writes the session on its way through.
                Some(index) if index != self.tabs.active => self.activate_tab(index, cx),
                // Dragging the tab that was already in front leaves it there,
                // so there is nothing to activate -- but the order around it
                // changed, and that still has to be written.
                _ => self.persist_session(),
            }
        }
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

    /// Move the cell cursor without touching which rows are selected.
    ///
    /// Right-clicking inside a range needs exactly this: the menu should be
    /// about the cell under the pointer, and still act on every row the user
    /// picked.
    pub(crate) fn focus_cell(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        if self.editing_cell.is_some() && self.editing_cell != Some((row, column)) {
            self.commit_cell_edit(cx);
        }
        self.selected_cell = Some((row, column));
        self.focus = Focus::Grid;
        cx.notify();
    }

    /// ← / → — move the cell cursor along the selected row.
    ///
    /// Without this the grid is only half keyboard-drivable: rows can be
    /// walked but the cell within one can only be reached with the pointer,
    /// which leaves everything keyed off the selected cell — editing in place,
    /// following a foreign key — out of reach.
    pub(crate) fn move_selected_cell(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        // The columns as drawn, by result index. Stepping through the
        // result's own order instead would walk a reordered grid sideways in
        // whatever order the server happened to send, and would stop on
        // columns the user has hidden.
        let drawn: Vec<usize> = tab
            .display_columns()
            .into_iter()
            .map(|(at, _)| at)
            .collect();
        if drawn.is_empty() {
            return;
        }
        let selected_row = tab.selected_row();

        let row = match self.selected_cell {
            Some((row, _)) => row,
            // No cell yet: start at the row the detail sidebar is describing.
            None => match selected_row {
                Some(row) => row,
                None => return,
            },
        };
        // Where the cell sits *on screen*, so a step is a step the eye can
        // follow. A column that has since been hidden is not on screen at
        // all, and starts the walk from the near edge instead.
        let at = self
            .selected_cell
            .and_then(|(_, column)| drawn.iter().position(|drawn| *drawn == column));
        let next = match at {
            Some(at) => {
                let step = if delta < 0 {
                    at + drawn.len() - 1
                } else {
                    at + 1
                };
                drawn[step % drawn.len()]
            }
            // Arriving from a row selection lands on the first column going
            // right and the last going left.
            None if delta < 0 => drawn[drawn.len() - 1],
            None => drawn[0],
        };

        self.selected_cell = Some((row, next));
        self.reveal_column(next);
        self.focus = Focus::Grid;
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
        self.reveal_row(next, delta > 0);
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
                self.status = Status::info(format!(
                    "{count} {plural} staged for deletion — ⌘S to commit"
                ));
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

    // -- following a foreign key --------------------------------------------

    /// Where the cell at `(row, column)` points, if anywhere.
    pub(crate) fn foreign_key_at(
        &self,
        row: usize,
        column: usize,
    ) -> Option<(dbui_app::domain::ForeignKey, dbui_app::domain::Value)> {
        let view = self.tabs.active()?.result()?;
        let info = view.set.columns.get(column)?;
        let key = view
            .structure
            .iter()
            .find(|meta| meta.name == info.name)?
            .references
            .clone()?;
        let value = view.set.rows.get(row)?.get(column)?.clone();
        // A null reference points at nothing, which is not a row to open.
        if value.is_null() {
            return None;
        }
        Some((key, value))
    }

    /// Open the referenced table, filtered to the row this cell points at.
    ///
    /// A filter rather than a jump to an offset: the referenced row is
    /// somewhere in a table that may have millions of rows, and the only thing
    /// known about it is its key.
    pub(crate) fn follow_foreign_key(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        let Some((key, value)) = self.foreign_key_at(row, column) else {
            return;
        };

        let Some(driver) = self.active_driver_kind() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };
        let predicate = format!(
            "{} = {}",
            driver.quote_identifier(&key.references_column),
            value.sql_literal(driver)
        );

        let target = key.references.clone();
        self.open_table_tab(target.clone(), cx);
        if let Some(WorkspaceTab::Table {
            where_clause,
            where_draft,
            filters_open,
            page,
            ..
        }) = self.tabs.active_mut()
        {
            *where_clause = predicate.clone();
            *where_draft = crate::text_input::TextInput::with_text(predicate, false);
            *filters_open = true;
            page.offset = 0;
        }
        self.status = Status::info(format!("Following {} → {}", key.column, target.qualified()));
        self.load_active_table(cx);
    }

    // -- editing a cell in place --------------------------------------------

    /// Start editing one cell in the grid.
    ///
    /// Refused for a primary key (the row's identity is not editable) and for
    /// a value with newlines in it, which a one-line box would silently
    /// flatten -- those go to the sidebar, which has room for them.
    pub(crate) fn begin_cell_edit(&mut self, row: usize, column: usize, cx: &mut Context<Self>) {
        if !matches!(self.tabs.active(), Some(WorkspaceTab::Table { .. })) {
            self.status = Status::info("Only a table's rows can be edited");
            cx.notify();
            return;
        }

        // Selecting builds the draft this edit will be written into.
        self.select_row(row, cx);

        let field = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            }) => draft
                .fields
                .get(column)
                .map(|(name, input, is_pk)| (name.clone(), input.text().to_string(), *is_pk)),
            _ => None,
        };
        let Some((name, text, is_pk)) = field else {
            return;
        };

        // The two cells that cannot be opened in place still answer the
        // gesture: a double-click on one puts its value on the clipboard.
        // Landing on "you cannot edit this" and nothing else is what makes a
        // JSON column feel unreachable -- the document is right there, and the
        // only thing anyone wanted was to take it somewhere.
        self.selected_cell = Some((row, column));
        if is_pk {
            self.copy_focused_cell(cx);
            self.status = Status::info(format!(
                "{name} is part of the primary key — copied it instead"
            ));
            cx.notify();
            return;
        }
        if text.contains('\n') {
            self.copy_focused_cell(cx);
            // Selected, not just focused: the sidebar field is then a text
            // field like any other, with the whole value ready for ⌘C.
            if let Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            }) = self.tabs.active_mut()
            {
                if let Some((_, input, _)) = draft.fields.get_mut(column) {
                    input.select_all();
                }
            }
            self.detail_input = Some(DetailInput::Field(column));
            self.focus = Focus::Detail;
            self.reveal_detail_field(column);
            self.status = Status::info(format!(
                "{name} is multi-line — copied it; edit it in the sidebar"
            ));
            cx.notify();
            return;
        }

        self.cell_editor = crate::text_input::TextInput::with_text(text, false);
        self.cell_editor.select_all();
        self.editing_cell = Some((row, column));
        self.selected_cell = Some((row, column));
        self.detail_input = None;
        self.focus = Focus::Grid;
        cx.notify();
    }

    /// Commit an open cell editor, if there is one.
    ///
    /// Called from every route that moves the focus elsewhere -- another cell,
    /// a sidebar field, the tree, a menu -- so the editor never outlives the
    /// thing it was opened on.
    pub(crate) fn finish_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing_cell.is_some() {
            self.commit_cell_edit(cx);
        }
    }

    /// Write what was typed back into the draft, which stages it.
    ///
    /// Only if the draft is still the one built over the row the box was
    /// opened on. The row was being thrown away and the column matched on its
    /// own, so a commit arriving after the draft had moved -- or after another
    /// table had been put in front, which a reload now does -- wrote the text
    /// into whatever row sat at that column and staged it as an UPDATE against
    /// a row nobody had typed into.
    pub(crate) fn commit_cell_edit(&mut self, cx: &mut Context<Self>) {
        let Some((row, column)) = self.editing_cell.take() else {
            return;
        };
        let typed = self.cell_editor.text().to_string();
        let tab_id = self.tabs.active_id();
        let mut changed = false;
        if let Some(WorkspaceTab::Table {
            draft: Some(draft), ..
        }) = self.tabs.active_mut()
        {
            if draft.rows == [row] {
                if let Some((_, input, _)) = draft.fields.get_mut(column) {
                    changed = input.text() != typed;
                    let multiline = input.is_multiline();
                    *input = crate::text_input::TextInput::with_text(typed, multiline);
                }
            }
        }
        // Only a real change lights up: pressing Enter on an untouched value
        // staged nothing, and a flash would claim otherwise.
        if let Some(tab) = tab_id.filter(|_| changed) {
            self.cell_flash = Some((tab, row, column, Instant::now()));
        }
        self.focus = Focus::Grid;
        cx.notify();
    }

    pub(crate) fn cancel_cell_edit(&mut self, cx: &mut Context<Self>) {
        if self.editing_cell.take().is_some() {
            self.focus = Focus::Grid;
            cx.notify();
        }
    }

    /// Commit and step to the next editable column on the same row.
    pub(crate) fn commit_cell_and_advance(&mut self, backward: bool, cx: &mut Context<Self>) {
        let Some((row, column)) = self.editing_cell else {
            return;
        };
        self.commit_cell_edit(cx);

        let count = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            }) => draft.fields.len(),
            _ => return,
        };
        if count == 0 {
            return;
        }
        let mut next = column;
        for _ in 0..count {
            next = if backward {
                (next + count - 1) % count
            } else {
                (next + 1) % count
            };
            let editable = match self.tabs.active() {
                Some(WorkspaceTab::Table {
                    draft: Some(draft), ..
                }) => draft
                    .fields
                    .get(next)
                    .is_some_and(|(_, input, is_pk)| !is_pk && !input.text().contains('\n')),
                _ => false,
            };
            if editable {
                self.begin_cell_edit(row, next, cx);
                return;
            }
        }
    }

    // -- column widths ------------------------------------------------------

    pub(crate) fn begin_column_drag(&mut self, column: usize, x: Pixels, cx: &mut Context<Self>) {
        let width = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .and_then(|view| view.widths.get(column).copied())
            .unwrap_or(metrics::column_min_width());
        self.column_drag = Some((column, x, width));
        cx.notify();
    }

    pub(crate) fn drag_column(&mut self, x: Pixels, cx: &mut Context<Self>) {
        let Some((column, start_x, start_width)) = self.column_drag else {
            return;
        };
        let next = (start_width + f32::from(x - start_x))
            .clamp(metrics::column_min_width(), metrics::column_max_width());

        let Some(WorkspaceTab::Table {
            result: Some(view),
            column_widths,
            ..
        }) = self.tabs.active_mut()
        else {
            // A query result has no tab-level store, so its widths last only
            // as long as the result does.
            if let Some(view) = self.tabs.active_mut().and_then(|tab| match tab {
                WorkspaceTab::Sql { result, .. } => result.as_mut(),
                WorkspaceTab::Table { result, .. } => result.as_mut(),
            }) {
                if let Some(width) = view.widths.get_mut(column) {
                    *width = next;
                }
            }
            cx.notify();
            return;
        };

        if let Some(width) = view.widths.get_mut(column) {
            *width = next;
        }
        // Remembered by name so the next page keeps the width.
        if let Some(info) = view.set.columns.get(column) {
            column_widths.insert(info.name.clone(), next);
        }
        cx.notify();
    }

    pub(crate) fn end_column_drag(&mut self, cx: &mut Context<Self>) {
        if self.column_drag.take().is_some() {
            cx.notify();
        }
    }

    // -- dragging a column to another position ------------------------------

    /// A press landed on a header. Whether it is a sort or a move is not
    /// known yet -- that is decided by whether the pointer travels.
    pub(crate) fn begin_column_move(&mut self, column: usize, x: Pixels) {
        self.column_move = Some(ColumnMove {
            column,
            start_x: x,
            last_x: x,
            moved: false,
        });
    }

    /// The pointer has crossed the header of `target` with a column in hand.
    pub(crate) fn drag_column_over(&mut self, target: usize, x: Pixels, cx: &mut Context<Self>) {
        let Some(mut drag) = self.column_move else {
            return;
        };
        if !drag.moved {
            if f32::from(x - drag.start_x).abs() < HEADER_DRAG_SLOP {
                return;
            }
            drag.moved = true;
            self.column_move = Some(drag);
            cx.notify();
        }
        if target == drag.column {
            return;
        }

        let Some(view) = self.tabs.active().and_then(|tab| tab.result()) else {
            return;
        };
        let position = |column: usize| view.order.iter().position(|slot| *slot == column);
        let (Some(from), Some(to)) = (position(drag.column), position(target)) else {
            return;
        };
        // Only ever in the direction of travel; see `ColumnMove`.
        let forward = to > from;
        if forward && x <= drag.last_x {
            return;
        }
        if !forward && x >= drag.last_x {
            return;
        }
        drag.last_x = x;
        self.column_move = Some(drag);

        let Some(tab) = self.tabs.active_mut() else {
            return;
        };
        let moved = tab
            .result_mut()
            .is_some_and(|view| view.move_column(drag.column, target));
        if !moved {
            return;
        }
        // Remembered by name, so the next page -- and the next launch -- open
        // with the columns where the user left them.
        if let WorkspaceTab::Table {
            result: Some(view),
            column_order,
            ..
        } = tab
        {
            *column_order = view.order_names();
        }
        // Written once on release rather than once a heading, for the reason
        // in `drag_tab_over`.
        cx.notify();
    }

    /// Let go of the header. A press that never travelled was a click, and a
    /// click on a header sorts by it.
    pub(crate) fn end_column_move(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.column_move.take() else {
            return;
        };
        self.drag_pointer = None;
        if !drag.moved {
            let name = self
                .tabs
                .active()
                .and_then(|tab| tab.result())
                .and_then(|view| view.set.columns.get(drag.column))
                .map(|info| info.name.clone());
            if name.is_some() {
                // By index: the header knows which column it is, and a query
                // can return two of that name.
                self.toggle_sort_at(drag.column, cx);
                return;
            }
        } else {
            self.persist_session();
        }
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn reapply_column_widths_for_test(&mut self, tab_id: crate::tabs::TabId) {
        self.apply_column_widths(tab_id);
    }

    /// Put the dragged widths back onto a freshly loaded result.
    fn apply_column_widths(&mut self, tab_id: crate::tabs::TabId) {
        let Some(WorkspaceTab::Table {
            result: Some(view),
            column_widths,
            column_order,
            ..
        }) = self.tabs.get_mut(tab_id)
        else {
            return;
        };
        for (index, info) in view.set.columns.iter().enumerate() {
            if let Some(width) = column_widths.get(&info.name) {
                if let Some(slot) = view.widths.get_mut(index) {
                    *slot = *width;
                }
            }
        }
        // Same bargain as the widths: a fresh page arrives in the server's
        // order, and the layout the user arranged is put back over it.
        view.apply_order(column_order);
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

    /// Stage a copy of every selected row.
    ///
    /// The copies land as new rows under the others and are committed with the
    /// rest of the batch, so a duplicate can be edited — or discarded — before
    /// anything reaches the server.
    pub(crate) fn duplicate_selected_rows(&mut self, cx: &mut Context<Self>) {
        self.stash_current_draft(cx);

        // Duplicating the staged row that is open is the obvious reading of
        // ⌘D while filling one in.
        if let Some(index) = self.tabs.active().and_then(|tab| tab.editing_insert()) {
            self.duplicate_staged_insert(index, cx);
            return;
        }

        let Some(WorkspaceTab::Table {
            result: Some(view),
            selection,
            ..
        }) = self.tabs.active()
        else {
            self.status = Status::info("Duplicating needs a table tab");
            cx.notify();
            return;
        };

        let rows = selection.ordered();
        if rows.is_empty() {
            self.status = Status::info("Select a row to duplicate");
            cx.notify();
            return;
        }

        let columns = view.set.columns.clone();
        let structure = view.structure.clone();
        let copies: Vec<crate::tabs::PendingRowInsert> = rows
            .iter()
            .filter_map(|row| view.set.rows.get(*row))
            .map(|values| {
                crate::tabs::PendingRowInsert::duplicating(&columns, &structure, &values.0)
            })
            .collect();

        let count = copies.len();
        self.stage_inserts(copies, cx);
        let plural = if count == 1 { "row" } else { "rows" };
        self.status = Status::info(format!("{count} {plural} duplicated — ⌘S to commit"));
        cx.notify();
    }

    /// ⌘D while a staged row is open: copy that one.
    fn duplicate_staged_insert(&mut self, index: usize, cx: &mut Context<Self>) {
        let copy = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                pending_inserts,
                result: Some(view),
                ..
            }) => pending_inserts.get(index).map(|row| {
                let mut copy =
                    crate::tabs::PendingRowInsert::blank(&view.set.columns, &view.structure);
                for (slot, (_, input, _)) in copy.fields.iter_mut().zip(row.fields.iter()) {
                    slot.1 =
                        crate::text_input::TextInput::with_text(input.text().to_string(), true);
                }
                copy
            }),
            _ => None,
        };
        let Some(copy) = copy else { return };
        self.stage_inserts(vec![copy], cx);
        self.status = Status::info("Row duplicated — ⌘S to commit");
        cx.notify();
    }

    /// Add staged rows and open the first for editing.
    fn stage_inserts(&mut self, rows: Vec<crate::tabs::PendingRowInsert>, cx: &mut Context<Self>) {
        if rows.is_empty() {
            return;
        }
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
        let first = pending_inserts.len();
        pending_inserts.extend(rows);
        *editing_insert = Some(first);
        *change_bubble_expanded = true;
        selection.clear();
        *selected_row = None;
        *draft = None;

        self.detail_open = true;
        self.detail_input = None;
        self.focus = Focus::Detail;
        cx.notify();
    }

    /// Stage rows read off the clipboard.
    ///
    /// The inverse of ⌘C: columns are matched by the header names it wrote, so
    /// rows copied out of one table paste into another that shares column
    /// names. Anything the clipboard names that this table does not have is
    /// ignored rather than refused — pasting three of five columns is a
    /// reasonable thing to want.
    pub(crate) fn paste_rows(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            self.status = Status::info("Nothing on the clipboard");
            cx.notify();
            return;
        };

        let Some(pasted) = crate::row_export::parse_tsv(&text) else {
            self.status = Status::info("The clipboard does not hold rows copied from a table");
            cx.notify();
            return;
        };

        self.stage_named_rows(pasted, "Pasted", cx);
    }

    /// Stage rows that name their columns -- pasted, or read from a file --
    /// as inserts into the front table.
    ///
    /// Columns are matched by name, exactly first and then ignoring case,
    /// since a spreadsheet round trip often changes a header's case. A name
    /// the table does not have is ignored rather than refused: three of five
    /// columns is a reasonable thing to bring in. So is a name that appears
    /// twice -- the first one is used and the repeat ignored and counted,
    /// where it used to overwrite the first cell without a word.
    pub(crate) fn stage_named_rows(
        &mut self,
        incoming: crate::row_export::PastedRows,
        verb: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(WorkspaceTab::Table {
            result: Some(view), ..
        }) = self.tabs.active()
        else {
            self.status = Status::info("Adding rows needs a table tab");
            cx.notify();
            return;
        };
        let columns = view.set.columns.clone();
        let structure = view.structure.clone();

        let mut taken: Vec<String> = Vec::new();
        let matched: Vec<Option<String>> = incoming
            .columns
            .iter()
            .map(|name| {
                let column = columns
                    .iter()
                    .find(|column| column.name == *name)
                    .or_else(|| {
                        columns
                            .iter()
                            .find(|column| column.name.eq_ignore_ascii_case(name))
                    })
                    .map(|column| column.name.clone())?;
                if taken.contains(&column) {
                    return None;
                }
                taken.push(column.clone());
                Some(column)
            })
            .collect();

        if matched.iter().all(Option::is_none) {
            self.status = Status::error("None of those column names are in this table".to_string());
            cx.notify();
            return;
        }

        let staged: Vec<crate::tabs::PendingRowInsert> = incoming
            .rows
            .iter()
            .map(|cells| {
                let mut row = crate::tabs::PendingRowInsert::blank(&columns, &structure);
                for (cell, column) in cells.iter().zip(matched.iter()) {
                    if let Some(column) = column {
                        row.set_from_text(column, cell);
                    }
                }
                row
            })
            .collect();

        let count = staged.len();
        let ignored = matched.iter().filter(|column| column.is_none()).count();
        self.stage_inserts(staged, cx);

        let plural = if count == 1 { "row" } else { "rows" };
        self.status = Status::info(if ignored > 0 {
            format!("{verb} {count} {plural} — {ignored} column(s) ignored")
        } else {
            format!("{verb} {count} {plural} — ⌘S to commit")
        });
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

    /// The staged inserts of one tab as values ready to bind, or the first
    /// parse failure.
    ///
    /// Taken by index rather than off the tab in front: ⌘S answering a close
    /// guard commits the guarded tab, and the × on a tab pill raises that
    /// guard without bringing the tab forward.
    pub(crate) fn collect_batch_inserts(
        &self,
        index: usize,
    ) -> Result<Vec<dbui_app::RowInsert>, String> {
        let Some(WorkspaceTab::Table {
            pending_inserts, ..
        }) = self.tabs.items.get(index)
        else {
            return Ok(Vec::new());
        };
        pending_inserts
            .iter()
            .map(|row| row.to_values().map(|values| dbui_app::RowInsert { values }))
            .collect()
    }

    /// Bring one detail field into view.
    ///
    /// Sending someone to "the sidebar" is only an instruction if the sidebar
    /// then shows the field. On a wide table the panel is scrolled twenty
    /// fields away, and focusing something off-screen looks exactly like
    /// nothing having happened.
    pub(crate) fn reveal_detail_field(&self, field: usize) {
        let Some(draft) = self.tabs.active().and_then(|tab| match tab {
            WorkspaceTab::Table { draft, .. } | WorkspaceTab::Sql { draft, .. } => draft.as_ref(),
        }) else {
            return;
        };

        // The panel renders, in order: the bulk banner when there is one, the
        // field search, then the fields the search left visible. The child
        // index the scroll handle knows is that same count.
        let search = draft.field_search.text().to_ascii_lowercase();
        let lead = usize::from(draft.is_bulk()) + 1;
        let Some(position) = draft
            .fields
            .iter()
            .enumerate()
            .filter(|(_, (name, _, _))| {
                search.is_empty() || name.to_ascii_lowercase().contains(&search)
            })
            .position(|(index, _)| index == field)
        else {
            return;
        };
        self.detail_scroll.scroll_to_item(lead + position);
    }

    /// Copy one detail-sidebar field's value.
    ///
    /// The key's field is the reason this exists: it is drawn read-only,
    /// because the row's identity is not editable, and a box you cannot put a
    /// caret in is also a box you cannot select out of. Reaching a key by
    /// hand -- to paste it into a query, a ticket, a message -- is one of the
    /// most ordinary things anyone does with a row.
    pub(crate) fn copy_detail_field(&mut self, index: usize, cx: &mut Context<Self>) {
        let field = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            })
            | Some(WorkspaceTab::Sql {
                draft: Some(draft), ..
            }) => draft
                .fields
                .get(index)
                .map(|(name, input, _)| (name.clone(), input.text().to_string())),
            _ => None,
        };
        let Some((name, text)) = field else {
            return;
        };
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_field = Some(index);
        self.status = Status::info(format!("Copied {name}"));
        cx.notify();
    }

    /// ⌘C over the grid: the focused cell if there is one, else the rows.
    ///
    /// Clicking a cell is asking about that cell, so copying after it should
    /// hand back that cell rather than the forty columns around it. A range
    /// of rows is a different question and still copies as rows, and ⌘⇧C
    /// always does.
    pub(crate) fn copy_cell_or_rows(&mut self, cx: &mut Context<Self>) {
        let one_row = self
            .tabs
            .active()
            .is_some_and(|tab| tab.selection().ordered().len() <= 1);
        if one_row && self.copy_focused_cell(cx) {
            return;
        }
        self.copy_selected_rows(crate::row_export::RowFormat::Tsv, cx);
    }

    /// Put the focused cell's value on the clipboard, and say whether there
    /// was one.
    ///
    /// What is copied is what the grid shows -- a staged edit wins over the
    /// stored value -- except that it is never truncated, and NULL copies as
    /// nothing at all, the way it does in every other export here.
    pub(crate) fn copy_focused_cell(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((row, column)) = self.selected_cell else {
            return false;
        };
        let staged = self.collect_batch_edits();
        let copied = self.tabs.active().and_then(|tab| {
            let view = tab.result()?;
            let name = view.set.columns.get(column)?.name.clone();
            let pending = tab
                .staged_edit_for_row(row, &staged)
                .and_then(|edit| edit.changes.iter().find(|change| change.column == name))
                .map(|change| change.new_text.clone());
            let text = match pending {
                Some(text) => text,
                None => crate::row_export::cell_text(view.set.rows.get(row)?.get(column)?),
            };
            Some((name, text))
        });
        let Some((name, text)) = copied else {
            return false;
        };

        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text));
        self.copied_cell = Some((row, column));
        self.copied_at = Some(Instant::now());
        self.status = Status::info(format!("Copied {name}"));
        cx.notify();
        true
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
        // Copied in the order the columns are drawn: a paste that comes back
        // in a different order than the grid showed is a paste nobody can
        // line up against what they selected.
        let lay_out = |row: &dbui_app::domain::Row| -> Vec<dbui_app::domain::Value> {
            view.order
                .iter()
                .filter_map(|index| row.0.get(*index).cloned())
                .collect()
        };
        let rows: Vec<Vec<dbui_app::domain::Value>> = if selected.is_empty() {
            view.set.rows.iter().map(lay_out).collect()
        } else {
            selected
                .iter()
                .filter_map(|index| view.set.rows.get(*index))
                .map(lay_out)
                .collect()
        };

        if rows.is_empty() {
            self.status = Status::info("Nothing to copy");
            cx.notify();
            return;
        }

        let columns: Vec<dbui_app::domain::ColumnInfo> = view
            .ordered_columns()
            .map(|(_, info)| info.clone())
            .collect();
        let table = tab.table_ref().cloned();
        let driver = self
            .active_driver_kind()
            .unwrap_or(dbui_app::domain::Driver::Postgres);
        let text = crate::row_export::render(format, &columns, &rows, driver, table.as_ref());

        let count = rows.len();
        let plural = if count == 1 { "row" } else { "rows" };
        let copied = if selected.is_empty() {
            (0..count).collect()
        } else {
            selected
        };
        self.row_flash = Some(RowFlash {
            tab: tab.id(),
            rows: FlashRows::Copied(copied),
            started: Instant::now(),
        });
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
    /// Explain a refused ⌘S, naming where the staged work actually is.
    ///
    /// ⌘S commits the tab it is pressed on. Pressed on a tab with nothing
    /// staged -- a query tab, or a table the user has not touched -- while a
    /// dot is lit on another one, a bare "nothing to commit" reads as the key
    /// having failed. The batch is per-tab because each one commits in its own
    /// transaction against its own table; the fix is to say which tab to press
    /// it on, not to quietly commit a table the user is not looking at.
    fn nothing_here_because(&self, reason: &str) -> String {
        let active = self.tabs.active;
        let mut count = 0usize;
        let mut where_it_is: Option<String> = None;
        for (index, tab) in self.tabs.items.iter().enumerate() {
            if index == active {
                continue;
            }
            let staged = tab.pending_change_count();
            if staged > 0 {
                count += staged;
                where_it_is.get_or_insert_with(|| tab.label());
            }
        }

        match where_it_is {
            Some(label) => {
                format!("{reason} — {count} staged on “{label}”. Switch to that tab and press ⌘S.",)
            }
            None => reason.to_string(),
        }
    }

    /// Where the tab a `CloseTarget::Tab` guard is asking about sits now, if
    /// it is still in front of the user at all.
    ///
    /// The tab list can be swapped out while the question is still on screen:
    /// ⌘⌥] carries another connection's tabs in without bringing the guard
    /// down. Matching on connection as well as id is what stops "discard the
    /// changes on members?" from being answered against a stranger's tab.
    fn guarded_tab_index(
        &self,
        connection: Option<ConnectionId>,
        id: crate::tabs::TabId,
    ) -> Option<usize> {
        if self.workspace.active_id() != connection {
            return None;
        }
        self.tabs.items.iter().position(|tab| tab.id() == id)
    }

    /// Which tab a ⌘S pressed under an open guard is answering, or the reason
    /// it cannot answer at all. `None` when no guard is up, and ⌘S means what
    /// it has always meant: the tab in front.
    ///
    /// The guard's own caption offers ⌘S as the way to keep the work, so ⌘S
    /// has to act on the batch the question is quoting. That only resolves
    /// when the question is about one tab that is still there and still
    /// holding work: a group of tabs, or a whole connection, is several
    /// batches -- each committing in its own transaction against its own
    /// table -- and there is no single one of them to send.
    fn guarded_commit_target(&self) -> Option<Result<usize, String>> {
        let guard = self.close_guard.as_ref()?;
        let label = &guard.label;
        let changes = guard.changes;
        Some(match guard.target {
            CloseTarget::Tab { connection, id } => match self.guarded_tab_index(connection, id) {
                Some(index) if self.tabs.items[index].pending_change_count() > 0 => Ok(index),
                Some(_) => Err(format!("Nothing left to commit on “{label}”")),
                None => Err(format!(
                    "“{label}” is not the tab in front — switch back to it and press ⌘S."
                )),
            },
            CloseTarget::TabGroup(_) => Err(format!(
                "⌘S commits one tab at a time — {changes} staged across {label}. \
                 Switch to each tab and press ⌘S."
            )),
            CloseTarget::Connection(_) => Err(format!(
                "⌘S commits one tab at a time — {changes} staged on “{label}”. \
                 Switch to each tab and press ⌘S."
            )),
        })
    }

    pub(crate) fn save_pending_edits(&mut self, cx: &mut Context<Self>) {
        if self.refuse_if_read_only("Commit", cx) {
            return;
        }

        // An open guard is asking about one tab, and its own caption offers ⌘S
        // as the answer, so ⌘S has to commit that tab. The × on a tab pill
        // raises the guard without bringing the tab forward, and committing
        // whatever is in front instead saves the wrong table and then takes
        // the question away along with the work it was quoting.
        let guarded = match self.guarded_commit_target() {
            None => None,
            Some(Ok(index)) => Some(index),
            // Nothing here for ⌘S to send. Say where the work is, the way a
            // ⌘S on the wrong tab does, and leave the question standing
            // rather than committing a table on a guess.
            Some(Err(message)) => {
                self.status = Status::info(message);
                cx.notify();
                return;
            }
        };
        let target = guarded.unwrap_or(self.tabs.active);

        if matches!(
            self.tabs.items.get(target),
            Some(WorkspaceTab::Table { saving: true, .. })
        ) {
            // Pressing ⌘S again while the first batch is still in flight is
            // what someone does when the first press looked like it did
            // nothing. Saying the commit is already running is the answer to
            // that; a second silent return is what taught them to doubt it.
            self.status = Status::info("Already committing — one moment");
            cx.notify();
            return;
        }

        // Fold the open editors in first so Save catches in-progress edits --
        // the sidebar draft, and the box still open over a cell. Committing a
        // batch that leaves out the value the user typed a moment ago is how
        // an edit silently goes missing.
        //
        // Only when the tab being committed is the one in front, though: both
        // of these act on the active tab, so under a guard raised on another
        // one they would fold a half-typed value into someone else's batch.
        // `close_tab` draws the same line before it counts.
        if target == self.tabs.active {
            self.finish_cell_edit(cx);
            self.stash_current_draft(cx);

            // A draft that would not stage stops the commit here, and says why.
            // Otherwise the batch it was left out of reads as empty, and ⌘S
            // answers "no changes to commit" to a screen full of typing -- which
            // is how a table with no primary key came to look like a broken key
            // rather than a table that cannot be edited.
            if let Some(message) = self.draft_error() {
                self.status = Status::error(format!("Cannot commit — {message}"));
                cx.notify();
                return;
            }
        }

        // The staged inserts are turned into values here rather than later:
        // a row that will not parse has to stop the commit before anything is
        // sent, not halfway through the transaction.
        let inserts = match self.collect_batch_inserts(target) {
            Ok(inserts) => inserts,
            Err(message) => {
                self.status = Status::error(message);
                cx.notify();
                return;
            }
        };

        let (table, edits, deletes, tab_id) = match self.tabs.items.get(target) {
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
                let message = self.nothing_here_because("A query tab has nothing to commit");
                self.status = Status::info(message);
                cx.notify();
                return;
            }
            None => {
                self.status = Status::info("No tab open to commit");
                cx.notify();
                return;
            }
        };

        if edits.is_empty() && deletes.is_empty() && inserts.is_empty() {
            let message = self.nothing_here_because("No changes to commit on this tab");
            self.status = Status::info(message);
            cx.notify();
            return;
        }

        // Checked after the batch so an unsaved-but-disconnected tab says the
        // useful thing rather than "no changes".
        let (Some(driver), Some(connection)) =
            (self.workspace.active_driver(), self.workspace.active_id())
        else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let count = edits.len() + deletes.len() + inserts.len();
        // Last, so the question is only ever asked about a batch that would
        // actually go -- never in place of "no changes" or "not connected".
        if self.hold_for_production(GuardedWrite::Commit { changes: count }, cx) {
            return;
        }
        if let Some(WorkspaceTab::Table { saving, .. }) = self.tabs.get_mut(tab_id) {
            *saving = true;
        }
        // Answering "discard or keep?" with ⌘S is answering it: the batch the
        // guard was standing in front of -- this one, since a guard pointing
        // anywhere else refused the commit above -- is on its way to the
        // server, so the question and the count it quoted are stale.
        self.close_guard = None;
        // A chrome dropdown is what let ⌘S reach here in the first place when
        // one was open; it has no further part in a commit that is already on
        // its way, so it goes with the guard rather than hanging over rows
        // that are no longer what it was drawn on top of.
        self.close_chrome_menus();

        // A commit answered from the guard lands while another tab is in
        // front, so the user goes on working while it is in the air. Stamp
        // the line it puts up -- which commit wrote it, and what it says --
        // so the answer can be dropped rather than written over a query they
        // started since, or a refusal they have just read.
        self.commit_stamp = self.commit_stamp.wrapping_add(1);
        let stamp = self.commit_stamp;
        let line = Status::busy(format!("Committing {count} change(s)…"));
        self.status = line.clone();
        // Taken now because a rollback has to name the tab it rolled back on,
        // and by the time one lands that tab can be behind another.
        let label = self
            .tabs
            .items
            .get(target)
            .map(|tab| tab.label())
            .unwrap_or_default();
        cx.notify();

        let runtime = self.runtime.clone();
        let saved_keys: Vec<Vec<(String, Value)>> =
            edits.iter().map(|edit| edit.pk.clone()).collect();
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
                // ⌘⌥] can carry another connection's whole tab list into
                // `self.tabs` while the commit is in the air, and tab ids
                // start again at zero for each connection -- so `tab_id`
                // looked up in whatever list is in front names a stranger's
                // tab of the same number. `CloseTarget::Tab` carries a
                // connection for this reason; so does this.
                //
                // Resolved wherever this connection's tabs are kept, stashed
                // included, rather than only while it is in front: the rows
                // are on the server either way, and a tab left holding a
                // batch that has already landed would send the whole of it
                // again -- inserts and all -- on the next ⌘S. The `saving`
                // flag comes off by the same route, so a tab whose connection
                // was put away while its commit was in flight is not left
                // answering every later ⌘S with "Already committing".
                let in_front = this.workspace.active_id() == Some(connection);
                if let Some(tabs) = this.connection_tabs_mut(connection) {
                    if let Some(WorkspaceTab::Table { saving, .. }) = tabs.get_mut(tab_id) {
                        *saving = false;
                    }
                }
                let is_active = in_front && this.tabs.active_id() == Some(tab_id);
                // The tab in front answers back as it always has. A commit
                // whose tab is not in front -- answered from under the guard,
                // or simply left behind by ⌘⌥] -- speaks only while the busy
                // line it put up is still the one on screen.
                //
                // That line is its own, so replacing it cannot write over
                // anything the user is reading: a line they are reading is by
                // definition not this one. Staying silent instead leaves the
                // footer claiming the commit is still running long after it
                // finished, and nothing ever says that it worked.
                let answers_back = is_active || (this.commit_stamp == stamp && this.status == line);
                match landed {
                    Some(Ok(saved)) => {
                        if let Some(tabs) = this.connection_tabs_mut(connection) {
                            if let Some(WorkspaceTab::Table {
                                pending_edits,
                                pending_deletes,
                                pending_inserts,
                                editing_insert,
                                change_bubble_expanded,
                                selection,
                                draft,
                                ..
                            }) = tabs.get_mut(tab_id)
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
                        }
                        if answers_back {
                            // Named when the tab is not the one in front, so
                            // the answer is not read against whatever table
                            // is -- the reason the rollback arm names it too.
                            this.status = Status::info(if is_active {
                                format!("Committed {saved} change(s)")
                            } else {
                                format!("“{label}” — committed {saved} change(s)")
                            });
                        }
                        // The reread goes out on the front connection's driver
                        // and lands in the front connection's tab list, so it
                        // can only be asked for while this commit's connection
                        // is still the one in front. Behind, it would read this
                        // table through a stranger's socket. The tab keeps the
                        // rows it already had until it is opened again.
                        if in_front {
                            this.load_table(tab_id, cx);
                        }
                        // Lit by key, so the glow lands on the rows as the
                        // reload redraws them.
                        this.row_flash = Some(RowFlash {
                            tab: tab_id,
                            rows: FlashRows::Committed(saved_keys),
                            started: Instant::now(),
                        });
                    }
                    Some(Err(error)) => {
                        // Transaction rolled back — leave everything staged.
                        //
                        // Said whatever else has reached the line since, unlike
                        // the answer above: this is the answer to something the
                        // user asked for and was told was under way, and never
                        // hearing it failed reads as it having worked. Named
                        // when the tab is not the one in front, so the failure
                        // is not read against whatever table is.
                        this.status = Status::error(if is_active {
                            error.to_string()
                        } else {
                            format!("“{label}” — {error}")
                        });
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
        self.reveal_row(next, delta > 0);
        if stay_on_grid {
            if let Some(column) = column {
                self.selected_cell = Some((next, column));
            }
            self.focus = Focus::Grid;
            cx.notify();
        }
    }

    // -- keeping the keyboard cursor on screen ------------------------------

    /// Scroll the grid so `row` is visible.
    ///
    /// `uniform_list` has no "nearest" strategy: once it decides a scroll is
    /// needed it honours whichever it was given, so a bare `Top` would fling a
    /// row that fell off the *bottom* all the way up. Passing the direction the
    /// cursor moved makes each strategy the minimal scroll for that direction --
    /// down pins the row to the bottom edge, up pins it to the top -- which is
    /// what keeps holding an arrow key reading as one row at a time.
    pub(crate) fn reveal_row(&self, row: usize, downward: bool) {
        self.grid_scroll.scroll_to_item(
            row,
            if downward {
                ScrollStrategy::Bottom
            } else {
                ScrollStrategy::Top
            },
        );
    }

    /// Scroll the grid sideways so `column` is visible.
    ///
    /// The horizontal pane scrolls whole rows, not cells, so there is no item
    /// index to scroll to -- the offset has to be computed from the widths the
    /// header is already drawing with. Nothing happens until the pane has been
    /// laid out at least once, which is also the only time the arrow keys can
    /// have moved anything.
    pub(crate) fn reveal_column(&self, column: usize) {
        let Some(tab) = self.tabs.active() else {
            return;
        };
        let Some(view) = tab.result() else {
            return;
        };

        // Walk the columns as they are drawn: a hidden one takes up no room,
        // and a column the user dragged left is no longer where the result
        // put it.
        let mut start = f32::from(metrics::row_number_width());
        let mut width = None;
        for (index, _) in tab.display_columns() {
            let w = view
                .widths
                .get(index)
                .copied()
                .unwrap_or(metrics::column_min_width());
            if index == column {
                width = Some(w);
                break;
            }
            start += w;
        }
        let Some(width) = width else {
            return;
        };

        let viewport = f32::from(self.grid_h_scroll.bounds().size.width);
        if viewport <= 0. {
            return;
        }
        let offset = self.grid_h_scroll.offset();
        let scrolled = -f32::from(offset.x);

        if let Some(next) = h_offset_for(start, width, viewport, scrolled) {
            self.grid_h_scroll.set_offset(point(px(-next), offset.y));
        }
    }

    /// Scroll the schema tree so the item at `index` in
    /// [`Self::sidebar_visible_items`] is visible.
    pub(crate) fn reveal_sidebar_cursor(&self) {
        let Some(cursor) = self.sidebar_cursor.as_ref() else {
            return;
        };
        // The tree renders exactly this list, in this order, so the position
        // here is the child index the scroll handle knows.
        if let Some(index) = self
            .sidebar_visible_items()
            .iter()
            .position(|item| item == cursor)
        {
            self.sidebar_scroll.scroll_to_item(index);
        }
    }

    // -- the connection form ----------------------------------------------

    pub(crate) fn open_new_connection(&mut self, cx: &mut Context<Self>) {
        self.connection_picker_open = false;
        self.modal = Some(ConnectionForm::new());
        self.focus = Focus::Sidebar;
        cx.notify();
    }

    /// Save a new connection and bring it to the front, tabs and all.
    ///
    /// [`Workspace::add`] moves the front to the connection it has just made,
    /// and `self.tabs` still holds the list belonging to whoever was in front
    /// before it. Left there, it is one connection's tabs standing under
    /// another's name: the next switch files them away under the new id, and
    /// an answer still in flight for the connection that was in front -- a
    /// commit looking for the tab whose batch it is clearing -- cannot find
    /// its tabs at all, so the batch stays staged after it has landed and the
    /// tab goes on refusing every later ⌘S as already committing. It makes
    /// the same swap [`Self::select_connection`] does, through the same
    /// helper, for the same reason.
    ///
    /// [`Workspace::add`]: dbui_app::Workspace::add
    fn add_connection(
        &mut self,
        config: dbui_app::domain::ConnectionConfig,
        cx: &mut Context<Self>,
    ) -> ConnectionId {
        let previous = self.workspace.active_id();
        let id = self.workspace.add(config);
        self.swap_front_tabs(previous, cx);
        id
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
                    self.add_connection(config, cx);
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
        let open = !self.connection_picker_open;
        self.close_chrome_menus();
        self.connection_picker_open = open;
        cx.notify();
    }

    /// Shut every titlebar/toolbar dropdown.
    ///
    /// They are drawn `deferred` and dismissed on a press outside themselves,
    /// so two open at once would both be floating over the window with only
    /// one of them under the pointer. Opening any one therefore closes the
    /// rest rather than trusting each to notice the other.
    pub(crate) fn close_chrome_menus(&mut self) {
        self.connection_picker_open = false;
        self.settings_menu_open = false;
        self.detail_menu_open = false;
        self.page_size_menu_open = false;
    }

    pub(crate) fn toggle_settings_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.settings_menu_open;
        self.close_chrome_menus();
        self.settings_menu_open = open;
        cx.notify();
    }

    pub(crate) fn close_settings_menu(&mut self, cx: &mut Context<Self>) {
        if self.settings_menu_open {
            self.settings_menu_open = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_detail_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.detail_menu_open;
        self.close_chrome_menus();
        self.detail_menu_open = open;
        cx.notify();
    }

    pub(crate) fn close_detail_menu(&mut self, cx: &mut Context<Self>) {
        if self.detail_menu_open {
            self.detail_menu_open = false;
            cx.notify();
        }
    }

    pub(crate) fn toggle_page_size_menu(&mut self, cx: &mut Context<Self>) {
        let open = !self.page_size_menu_open;
        self.close_chrome_menus();
        self.page_size_menu_open = open;
        cx.notify();
    }

    pub(crate) fn close_page_size_menu(&mut self, cx: &mut Context<Self>) {
        if self.page_size_menu_open {
            self.page_size_menu_open = false;
            cx.notify();
        }
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
            self.add_connection(config, cx);
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
            self.workspace.open_table = self.tabs.active().and_then(|tab| tab.table_ref().cloned());
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
        // How many times the session has been written, so a test can hold the
        // line that a drag writes once on release and not once a slot.
        #[cfg(test)]
        self.session_writes.set(self.session_writes.get() + 1);

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

        self.workspace.restore_open(
            session.tabs.iter().map(|tab| tab.connection),
            session.active,
        );

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

    /// True while some panel is holding the keyboard for itself.
    ///
    /// This has to mirror the early returns at the top of [`Self::on_key`],
    /// in the same order, and is kept next to them for that reason: the two
    /// drifting apart is how a shortcut starts firing underneath a panel that
    /// believes the keyboard is its own.
    fn keyboard_is_claimed(&self) -> bool {
        self.palette.is_some()
            || self.confirm.is_some()
            || self.production_guard.is_some()
            || self.activity.is_some()
            || self.close_guard.is_some()
            || self.context_menu.is_some()
            || self.modal.is_some()
            || self.connection_picker_open
            || self.settings_menu_open
            || self.detail_menu_open
            || self.page_size_menu_open
            || self.schema_sheet.is_some()
    }

    pub(crate) fn on_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Every keystroke in the app arrives here, which makes this the one
        // place caps lock has to be put back into the typed character. See
        // [`crate::text_input::with_capslock`] for why it is missing.
        let capslocked = crate::text_input::with_capslock(&event.keystroke, window.capslock().on);
        let keystroke = capslocked.as_ref().unwrap_or(&event.keystroke);
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

        // The structure sheet is modal the same way: a DDL statement is not
        // something to be set off from underneath it.
        if self.schema_sheet.is_some() {
            self.handle_schema_sheet_key(keystroke, cx);
            return;
        }

        // The production question owns it too, and is checked before the
        // close guard: ⌘S from under that guard is what can raise this one.
        if self.production_guard.is_some() {
            match key {
                "escape" => self.cancel_production_write(cx),
                // Unmodified, for the reason the close guard gives below: the
                // ⌘↵ that raised this question must not also answer it.
                "enter" if !command => self.confirm_production_write(cx),
                _ => {}
            }
            return;
        }

        // The activity panel is modal; Escape closes it, and nothing
        // underneath hears a key while it is up.
        if self.activity.is_some() {
            if key == "escape" {
                let confirming = self
                    .activity
                    .as_ref()
                    .is_some_and(|panel| panel.confirming.is_some());
                if confirming {
                    if let Some(panel) = self.activity.as_mut() {
                        panel.confirming = None;
                    }
                    cx.notify();
                } else {
                    self.close_activity(cx);
                }
            }
            return;
        }

        // The close guard owns it for the same reason. Enter goes through with
        // the close rather than cancelling it: the prompt is only up because
        // the user asked to close, and it names the count they are agreeing to
        // lose. Escape is the way back.
        if self.close_guard.is_some() {
            match key {
                "escape" => self.cancel_close(cx),
                // Unmodified only, the way the confirmation prompt guards its
                // own Enter. ⌘↵ is the run-query shortcut, and now that its
                // action is not registered under the guard the keystroke
                // arrives here -- where confirming would throw away exactly
                // the batch the guard is asking about.
                "enter" if !command => self.confirm_close(cx),
                _ => {}
            }
            return;
        }

        if self.context_menu.is_some() {
            match key {
                "escape" => {
                    self.close_context_menu(cx);
                    return;
                }
                // Unmodified Enter only, and swallowed either way: with
                // `RunQuery` unregistered under the menu, ⌘↵ reaches here,
                // and it must neither run the highlighted item nor fall
                // through to the shortcuts below and follow a foreign key out
                // from under the menu the user is still reading.
                "up" | "down" | "enter" => {
                    if !command {
                        self.handle_context_menu_key(key, cx);
                    }
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

        // A dropdown owns the keyboard while it is up, the same way the modal
        // above does -- otherwise Escape reaches past it and closes the tab
        // behind the menu the user was trying to dismiss.
        if self.connection_picker_open
            || self.settings_menu_open
            || self.detail_menu_open
            || self.page_size_menu_open
        {
            if key == "escape" {
                self.close_chrome_menus();
                cx.notify();
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
                "h" if shift => {
                    self.open_palette(PaletteKind::History, cx);
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
                // ⌘C / ⌘V over the grid copy and paste *rows* -- or, with a
                // cell focused, that cell. Inside an editor they are still the
                // text operations they always were.
                //
                // ⌘⇧C is the way back to the rows while a cell is focused,
                // which otherwise only the row-number gutter and the menu
                // reach.
                "c" if shift && !self.text_undo_has_focus() => {
                    self.copy_selected_rows(crate::row_export::RowFormat::Tsv, cx);
                    return;
                }
                "c" if !self.text_undo_has_focus() => {
                    self.copy_cell_or_rows(cx);
                    return;
                }
                "v" if !self.text_undo_has_focus() => {
                    self.paste_rows(cx);
                    return;
                }
                "d" if !self.text_undo_has_focus() => {
                    self.duplicate_selected_rows(cx);
                    return;
                }
                // Unshifted only: ⌘⇧Z is redo, and a staged batch has nothing
                // to redo — so it falls through rather than discarding twice.
                "z" if !shift && !self.text_undo_has_focus() => {
                    self.discard_pending_edits(cx);
                    return;
                }
                "enter" if !shift => {
                    self.run_or_follow_link(cx);
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
                // ⌘E opens a query tab, ⌘⇧E offers something to write in
                // it. The shifted arm has to come first.
                "e" if shift => {
                    self.open_palette(PaletteKind::Templates, cx);
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

        // A cell editor owns the keyboard while it is open: it is a text box
        // sitting on top of the grid, and the grid's own shortcuts would fire
        // through it otherwise.
        if self.editing_cell.is_some() {
            match key {
                "escape" => {
                    self.cancel_cell_edit(cx);
                    return;
                }
                "enter" if !command => {
                    self.commit_cell_edit(cx);
                    return;
                }
                "tab" if !command => {
                    self.commit_cell_and_advance(shift, cx);
                    return;
                }
                _ => {
                    if self.cell_editor.handle_key(keystroke, cx) {
                        cx.notify();
                        return;
                    }
                }
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

        if self.focus == Focus::Find && self.handle_find_key(keystroke, cx) {
            return;
        }

        if self.focus == Focus::Editor {
            // ⌘G walks the matches from the editor too, the bar open or not
            // in focus: find, look, type a fix, ⌘G to the next one.
            if command && key == "g" && self.editor_find.is_some() {
                self.find_step(!shift, cx);
                return;
            }

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

            // GPUI names the key "space" on every platform; the literal is
            // what a synthesized keystroke may carry.
            if (key == "space" || key == " ") && keystroke.modifiers.control {
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
                // What only a code editor does, ahead of the keys every text
                // field shares: ⌘/ comments lines out, and brackets and
                // quotes come in pairs. The form fields and filters keep the
                // plain behaviour -- a `(` that grows a `)` in a password
                // box is a bug, not a convenience.
                let plain = !command && !keystroke.modifiers.control && !keystroke.modifiers.alt;
                let handled = if command && key == "/" {
                    editor.toggle_line_comment();
                    true
                } else if plain && key == "backspace" {
                    editor.backspace_paired();
                    true
                } else if plain
                    && keystroke
                        .key_char
                        .as_deref()
                        .is_some_and(|typed| editor.type_paired(typed))
                {
                    true
                } else {
                    editor.handle_key(keystroke, cx)
                };
                if handled {
                    // Typing past the right edge pans the editor instead of
                    // writing where the user cannot see.
                    editor.ensure_editor_caret_visible();
                    // A space ends the word being completed, so the popup
                    // goes with it rather than going stale; anything else
                    // that edits the word re-filters it.
                    let edits_word = key.len() == 1 || key == "backspace" || key == "delete";
                    // A name character or a `.` opens it by itself; deleting
                    // does not, the way no editor pops a list on backspace.
                    let types_name = !command
                        && !keystroke.modifiers.control
                        && keystroke.key_char.as_deref().is_some_and(|typed| {
                            typed
                                .chars()
                                .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
                        });
                    if self.completion.is_some() && !command && key == "space" {
                        self.dismiss_completion(cx);
                    } else if self.completion.is_some() && !command && edits_word {
                        self.refresh_completion(cx);
                    } else if self.completion.is_none() && types_name {
                        self.auto_complete(cx);
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
            if key == "enter" {
                if let Some((row, column)) = self.selected_cell {
                    self.begin_cell_edit(row, column, cx);
                    return;
                }
            }
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
                "left" => {
                    self.move_selected_cell(-1, cx);
                    return;
                }
                "right" => {
                    self.move_selected_cell(1, cx);
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

    /// Put a computed literal into a detail field, as though it were typed.
    ///
    /// The calendar button's "now" and "today" need this: they are values, not
    /// write tokens, so they go through the same path a keystroke would rather
    /// than through [`Self::set_detail_special_value`], which exists to say
    /// "this column has no value" in three different ways.
    pub(crate) fn set_detail_field_text(
        &mut self,
        index: usize,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let applied = match self.tabs.active_mut() {
            Some(WorkspaceTab::Table {
                draft: Some(draft), ..
            })
            | Some(WorkspaceTab::Sql {
                draft: Some(draft), ..
            }) => match draft.fields.get_mut(index) {
                // The key is not editable here for the same reason it has no
                // value menu: changing it is changing which row this is.
                Some((_, _, true)) => false,
                Some((_, input, false)) => {
                    input.set_text(&text);
                    true
                }
                None => false,
            },
            _ => false,
        };

        self.detail_value_menu = None;
        if applied {
            self.detail_input = Some(DetailInput::Field(index));
            self.focus = Focus::Detail;
        }
        cx.notify();
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
        // Picking MIXED from the menu is the user saying "never mind, leave
        // each row as it was" -- so it drops what was staged for that column
        // as well as putting the token back. That is not the same as the
        // MIXED a fresh draft *shows* because the rows disagree, which has no
        // opinion and must preserve what is staged; the difference is that
        // this one was asked for.
        if applied && token == crate::tabs::MIXED {
            self.unstage_column(index, cx);
        }

        self.detail_value_menu = None;
        if applied {
            self.detail_input = Some(DetailInput::Field(index));
            self.focus = Focus::Detail;
        }
        cx.notify();
    }

    /// Drop any staged change to one column across the drafted rows.
    fn unstage_column(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(WorkspaceTab::Table {
            draft: Some(draft),
            result: Some(view),
            pending_edits,
            ..
        }) = self.tabs.active_mut()
        else {
            return;
        };
        let Some((column, _, _)) = draft.fields.get(index) else {
            return;
        };
        let column = column.clone();
        let keys = draft.row_keys(view);

        for edit in pending_edits.iter_mut() {
            if keys.iter().any(|pk| edit.matches_pk(pk)) {
                edit.changes.retain(|change| change.column != column);
            }
        }
        // A row with nothing left to change is no longer a pending edit.
        pending_edits.retain(|edit| !edit.changes.is_empty());
        cx.notify();
    }
}

/// `(page number, page count)` from an offset, a page size and a total.
///
/// Both 1-based, and the number is clamped into the count: an offset past the
/// end -- which is what a page held open while rows were deleted under it looks
/// like -- would otherwise report "page 4 of 3".
fn page_position_of(offset: u64, limit: u32, total: u64) -> (u64, u64) {
    let limit = u64::from(limit.max(1));
    // A table with no rows still has one page: the one being looked at.
    let pages = total.div_ceil(limit).max(1);
    let current = (offset / limit) + 1;
    (current.min(pages), pages)
}

fn draft_is_open(tab: &WorkspaceTab) -> bool {
    matches!(
        tab,
        WorkspaceTab::Table { draft: Some(_), .. } | WorkspaceTab::Sql { draft: Some(_), .. }
    )
}

/// Where the grid has to be scrolled sideways to show a column, or `None` if
/// it is already on screen.
///
/// `start` and `width` place the column in the full width of the row; `scrolled`
/// is how far the pane has already been panned. The result is the *minimal*
/// move: a column off the left edge comes to the left edge and one off the
/// right comes to the right, so walking with ← and → travels a column at a time
/// rather than jumping the pane around.
fn h_offset_for(start: f32, width: f32, viewport: f32, scrolled: f32) -> Option<f32> {
    if start < scrolled {
        Some(start)
    } else if start + width > scrolled + viewport {
        // A column wider than the pane cannot be shown whole; its left edge is
        // the half worth showing, so it is clamped rather than pushed past.
        Some((start + width - viewport).max(0.).min(start))
    } else {
        None
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
        let glass = self.translucent && !window.is_fullscreen();
        let vibrancy = glass.then_some(self.theme.is_light);
        if vibrancy != self.vibrancy {
            if glass != self.glass {
                window.set_background_appearance(if glass {
                    WindowBackgroundAppearance::Transparent
                } else {
                    WindowBackgroundAppearance::Opaque
                });
            }
            #[cfg(target_os = "macos")]
            crate::mac_window::set_vibrancy(vibrancy);
            self.vibrancy = vibrancy;
        }
        self.glass = glass;
        #[cfg(target_os = "macos")]
        if glass {
            crate::mac_window::set_vibrancy_blur(f64::from(self.glass_blur));
        }

        let modal = self.modal.is_some().then(|| self.render_modal(cx));
        let palette = self.render_palette(cx);
        let change_bubble = self.render_change_bubble(cx);
        let context_menu = self.render_context_menu(window, cx);
        let confirm = self.render_confirm(cx);
        let close_guard = self.render_close_guard(cx);
        let production_guard = self.render_production_guard(cx);
        let activity = self.render_activity(cx);
        let schema_sheet = self.render_schema_sheet(cx);
        let drag_ghost = self.render_drag_ghost();

        div()
            .size_full()
            .flex()
            .flex_col()
            // Translucent, the one tint is laid here and the titlebar, rail
            // and status bar draw nothing of their own over it, so the glass
            // reads as one sheet rather than three panes of it.
            .bg(if glass {
                self.glass_tint()
            } else {
                self.theme.background
            })
            .text_color(self.theme.text)
            .font_family(metrics::UI_FONT)
            .text_size(metrics::text_size())
            .key_context("DbUi")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            // The pointer leaves the 5px grab strip on the first frame of a
            // drag, so the tracking lives on the root instead.
            .when(
                self.titlebar_drag
                    || self.change_bubble_drag.is_some()
                    || self.editor_drag.is_some()
                    || self.row_drag.is_some()
                    || self.column_drag.is_some()
                    || self.column_move.is_some()
                    || self.tab_drag.is_some()
                    || self.sidebar_drag.is_some()
                    || self.detail_drag.is_some(),
                |root| {
                    root.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        this.drag_titlebar(cx);
                        // The carried copy follows every move, not just the
                        // ones that cross into a new slot.
                        let carrying = this.tab_drag.as_ref().is_some_and(|drag| drag.moved)
                            || this.column_move.is_some_and(|drag| drag.moved);
                        if this.tab_drag.is_some() || this.column_move.is_some() {
                            this.drag_pointer = Some(event.position);
                            if carrying {
                                cx.notify();
                            }
                        }
                        if this.change_bubble_drag.is_some() {
                            this.drag_change_bubble(event.position.y, window, cx);
                        }
                        if this.editor_drag.is_some() {
                            this.drag_editor(event.position.y, window, cx);
                        }
                        if this.column_drag.is_some() {
                            this.drag_column(event.position.x, cx);
                        }
                        if this.sidebar_drag.is_some() {
                            this.drag_sidebar(event.position.x, cx);
                        }
                        if this.detail_drag.is_some() {
                            this.drag_detail(event.position.x, cx);
                        }
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_titlebar_drag();
                            this.end_change_bubble_drag(cx);
                            this.end_editor_drag(cx);
                            this.end_row_drag(cx);
                            this.end_column_drag(cx);
                            this.end_column_move(cx);
                            this.end_tab_drag(cx);
                            this.end_sidebar_drag(cx);
                            this.end_detail_drag(cx);
                        }),
                    )
                    // Releasing outside the window has to end the drag too, or
                    // the next pointer move over the grid extends a selection
                    // nobody is still holding.
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, cx| {
                            this.end_titlebar_drag();
                            this.end_change_bubble_drag(cx);
                            this.end_editor_drag(cx);
                            this.end_row_drag(cx);
                            this.end_column_drag(cx);
                            this.end_column_move(cx);
                            this.end_tab_drag(cx);
                            this.end_sidebar_drag(cx);
                            this.end_detail_drag(cx);
                        }),
                    )
                },
            )
            // A bound action runs before `on_key` does and stops the
            // keystroke there, so an action left registered while a panel is
            // up is wrong twice over: it fires behind the panel, and the
            // panel never sees the key it was waiting for. ⌘↵ under the close
            // guard followed a foreign key into a tab nobody could see while
            // the guard's own Enter handling was skipped. Not registering
            // them is the only thing that lets the keystroke through to the
            // panel -- stopping propagation inside `on_key` is too late.
            .when(!self.keyboard_is_claimed(), |root| {
                root.on_action(cx.listener(|this, _: &crate::NewConnection, _window, cx| {
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
                .on_action(cx.listener(|this, _: &crate::FindReplace, _window, cx| {
                    if this.tabs.active().is_some_and(|tab| tab.is_sql()) {
                        this.open_editor_find(true, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &crate::SearchTables, _window, cx| {
                    this.focus_sidebar_search(cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectAllRows, _window, cx| {
                    this.select_all_rows(cx)
                }))
                .on_action(cx.listener(|this, _: &crate::DeleteRows, _window, cx| {
                    this.delete_selected_rows(cx)
                }))
                .on_action(cx.listener(|this, _: &crate::DuplicateRows, _window, cx| {
                    this.duplicate_selected_rows(cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::PasteRows, _window, cx| this.paste_rows(cx)),
                )
                .on_action(cx.listener(|this, _: &crate::ExportCsv, _window, cx| {
                    this.export_rows(crate::row_export::RowFormat::Csv, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::ExportJson, _window, cx| {
                    this.export_rows(crate::row_export::RowFormat::Json, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::ExportSql, _window, cx| {
                    this.export_rows(crate::row_export::RowFormat::Insert, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::ImportCsv, _window, cx| this.import_csv(cx)),
                )
                .on_action(cx.listener(|this, _: &crate::DiscardChanges, _window, cx| {
                    this.discard_pending_edits(cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::OpenSql, _window, cx| this.open_sql_tab(cx)),
                )
                .on_action(
                    cx.listener(|this, _: &crate::Refresh, _window, cx| this.refresh_result(cx)),
                )
                .on_action(
                    cx.listener(|this, _: &crate::RunQuery, _window, cx| {
                        this.run_or_follow_link(cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &crate::RunAllQueries, _window, cx| {
                    this.run_all_queries(cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::ExplainQuery, _window, cx| {
                        this.explain_query(cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &crate::ServerActivity, _window, cx| {
                    this.open_activity(cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::DumpDatabase, _window, cx| {
                        this.dump_database(cx)
                    }),
                )
                .on_action(
                    cx.listener(|this, _: &crate::RunSqlFile, _window, cx| this.run_sql_file(cx)),
                )
                .on_action(
                    cx.listener(|this, _: &crate::StopQuery, _window, cx| this.stop_query(cx)),
                )
                .on_action(
                    cx.listener(|this, _: &crate::SaveQuery, _window, cx| this.open_save_query(cx)),
                )
                .on_action(cx.listener(|this, _: &crate::OpenSavedQuery, _window, cx| {
                    this.open_palette(PaletteKind::SavedQueries, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::NewTable, _window, cx| {
                    this.create_table_sheet(None, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &crate::CloseTab, _window, cx| this.close_active_tab(cx)),
                )
                .on_action(cx.listener(|this, _: &crate::NextTab, _window, cx| this.next_tab(cx)))
                .on_action(cx.listener(|this, _: &crate::PrevTab, _window, cx| this.prev_tab(cx)))
                .on_action(
                    cx.listener(|this, _: &crate::CloseConnection, _window, cx| {
                        this.close_active_connection_tab(cx)
                    }),
                )
                .on_action(cx.listener(|this, _: &crate::NextConnection, _window, cx| {
                    this.cycle_connection_tab(true, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::PrevConnection, _window, cx| {
                    this.cycle_connection_tab(false, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab1, _window, cx| {
                    this.select_tab_number(1, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab2, _window, cx| {
                    this.select_tab_number(2, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab3, _window, cx| {
                    this.select_tab_number(3, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab4, _window, cx| {
                    this.select_tab_number(4, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab5, _window, cx| {
                    this.select_tab_number(5, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab6, _window, cx| {
                    this.select_tab_number(6, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab7, _window, cx| {
                    this.select_tab_number(7, cx)
                }))
                .on_action(cx.listener(|this, _: &crate::SelectTab8, _window, cx| {
                    this.select_tab_number(8, cx)
                }))
                .on_action(cx.listener(
                    |this, _: &crate::SelectTab9, _window, cx| this.select_tab_number(9, cx),
                ))
            })
            // ⌘S survives the close guard, because the guard's own caption
            // promises it -- "⌘S commits them and keeps this open." It also
            // survives the four chrome dropdowns: those are transient menus,
            // not a question standing between the user and their data, so a
            // save pressed over one is still a save. `save_pending_edits`
            // closes them once the commit is actually sent, which is what
            // keeps a dropdown from hanging open over work that already went.
            .when(
                // Never under the production question, though: that is the
                // one ⌘S is waiting on, and pressing it again is not an answer.
                self.production_guard.is_none()
                    && (!self.keyboard_is_claimed()
                        || self.close_guard.is_some()
                        || self.connection_picker_open
                        || self.settings_menu_open
                        || self.detail_menu_open
                        || self.page_size_menu_open),
                |root| {
                    root.on_action(cx.listener(|this, _: &crate::CommitChanges, _window, cx| {
                        this.save_pending_edits(cx)
                    }))
                },
            )
            // Zoom stays live under everything. The panels size themselves
            // with `metrics::scaled`, so they grow along with the rest of the
            // window, and a dialog too small to read is worth enlarging.
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
                    .child(self.render_sidebar_resize(cx))
                    .child(self.render_content_card(window, cx)),
            )
            .children(change_bubble)
            .child(self.render_status_bar(cx))
            .children(modal)
            .children(palette)
            .children(context_menu)
            .children(confirm)
            .children(close_guard)
            .children(activity)
            .children(production_guard)
            .children(schema_sheet)
            // Over everything: it is the pointer's, and the pointer can be
            // anywhere.
            .children(drag_ghost)
    }
}

/// How much of the tint setting a light theme applies. See `glass_tint`.
const LIGHT_TINT_STRENGTH: f32 = 0.4;

/// `from` moved `amount` of the way to `to`.
fn mix(from: gpui::Rgba, to: gpui::Rgba, amount: f32) -> gpui::Rgba {
    let lerp = |a: f32, b: f32| a + (b - a) * amount;
    gpui::Rgba {
        r: lerp(from.r, to.r),
        g: lerp(from.g, to.g),
        b: lerp(from.b, to.b),
        a: lerp(from.a, to.a),
    }
}

/// Past this the blur stops changing anything visible: the desktop is already
/// a wash of its average colour.
const GLASS_BLUR_MAX: u32 = 80;

/// Move a setting `direction` notches of `step`, landing on a multiple of it
/// and staying within `0..=max`.
fn step_setting(value: u32, direction: i32, step: u32, max: u32) -> u32 {
    let notch = value / step;
    let notch = if direction < 0 {
        // A value between notches steps down to the one below it, not past it.
        if value % step == 0 {
            notch.saturating_sub(1)
        } else {
            notch
        }
    } else {
        notch + 1
    };
    (notch * step).min(max)
}

impl DbUi {
    /// The grid and the detail panel. Opaque, they simply fill the space
    /// beside the rail; translucent, they sit in a rounded card on the glass.
    fn render_content_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let content = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .overflow_hidden()
            .child(self.render_main(window, cx))
            .children(self.render_detail_resize(cx))
            .child(self.render_detail_sidebar(cx));
        if !self.glass {
            return content.into_any_element();
        }
        // GPUI clips children to a rectangle, not to rounded corners, so the
        // card is padded far enough that a square child's corner stays inside
        // the curve: an inset of r·(1 − 1/√2), a little under a third of r.
        let radius = metrics::scaled(10.);
        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .mr(metrics::scaled(6.))
            .p(radius * 0.3)
            .rounded(radius)
            .bg(self.theme.background)
            .border_1()
            .border_color(self.theme.border)
            .shadow_sm()
            .overflow_hidden()
            .child(content)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status bar says "Page 2 of 3", so both halves have to survive a
    /// total that does not divide by the page size.
    #[test]
    fn a_page_number_counts_from_one_and_a_partial_page_still_counts() {
        assert_eq!(page_position_of(0, 500, 1400), (1, 3));
        assert_eq!(page_position_of(500, 500, 1400), (2, 3));
        assert_eq!(page_position_of(1000, 500, 1400), (3, 3));
        assert_eq!(page_position_of(0, 500, 1000), (1, 2), "an exact fit");
    }

    #[test]
    fn a_setting_steps_by_notches_and_stays_in_range() {
        assert_eq!(step_setting(35, 1, 5, 100), 40);
        assert_eq!(step_setting(35, -1, 5, 100), 30);
        assert_eq!(step_setting(0, -1, 5, 100), 0, "no wrap below zero");
        assert_eq!(step_setting(100, 1, 5, 100), 100, "capped at the top");
        assert_eq!(step_setting(33, 1, 5, 100), 35, "off-notch snaps up");
        assert_eq!(step_setting(33, -1, 5, 100), 30, "off-notch snaps down");
    }

    /// An empty table is on page one of one, not page one of zero.
    #[test]
    fn an_empty_table_is_still_one_page() {
        assert_eq!(page_position_of(0, 500, 0), (1, 1));
    }

    /// Rows deleted under a page held open leave the offset past the end.
    #[test]
    fn an_offset_past_the_end_reports_the_last_page_not_a_missing_one() {
        assert_eq!(page_position_of(5_000, 500, 1400), (3, 3));
    }

    /// `limit` reaches this straight off the tab, and a zero would divide by it.
    #[test]
    fn a_zero_page_size_does_not_divide_by_zero() {
        assert_eq!(page_position_of(0, 0, 10), (1, 10));
    }

    /// A result over `names`, in the order given and with no rows.
    fn result_over(names: &[&str]) -> ResultView {
        let columns = names
            .iter()
            .map(|name| ColumnInfo {
                name: (*name).to_string(),
                type_name: "text".into(),
            })
            .collect();
        ResultView::new(
            ResultSet {
                columns,
                rows: Vec::new(),
                truncated: false,
            },
            ResultSource::Query { sql: String::new() },
            String::new(),
            Vec::new(),
        )
    }

    fn drawn(view: &ResultView) -> Vec<String> {
        view.order_names()
    }

    #[test]
    fn a_column_lands_where_the_one_it_was_dropped_on_was() {
        let mut view = result_over(&["id", "name", "email"]);
        assert!(view.move_column(2, 0), "email, carried onto id");
        assert_eq!(drawn(&view), ["email", "id", "name"]);
        assert!(
            view.move_column(2, 1),
            "and then onto name, which is now last"
        );
        assert_eq!(drawn(&view), ["id", "name", "email"]);
        assert!(
            !view.move_column(1, 1),
            "a column dropped on itself is a no-op"
        );
    }

    /// The order is remembered by name, so a reload that changed the columns
    /// keeps what it can and puts the rest back where the server had them.
    #[test]
    fn a_saved_order_survives_columns_coming_and_going() {
        let mut view = result_over(&["id", "name", "email"]);
        view.apply_order(&["email".into(), "id".into(), "name".into()]);
        assert_eq!(drawn(&view), ["email", "id", "name"]);

        // `name` is gone and `created` is new.
        let mut next = result_over(&["id", "email", "created"]);
        next.apply_order(&["email".into(), "id".into(), "name".into()]);
        assert_eq!(
            drawn(&next),
            ["email", "id", "created"],
            "the columns still there keep their places, and the new one goes last"
        );

        let mut untouched = result_over(&["id", "name"]);
        untouched.apply_order(&[]);
        assert_eq!(
            drawn(&untouched),
            ["id", "name"],
            "no saved order, no change"
        );
    }

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

    #[test]
    fn a_rail_follows_the_pointer_whatever_the_zoom() {
        // Widths are unzoomed, so 60 real pixels of travel is 60 of width at
        // 100% and only 30 at 200% -- which is what puts the edge back under
        // the pointer that dragged it.
        assert_eq!(
            panel_width_for(SIDEBAR_WIDTH_DEFAULT, px(60.), 1.0, 100., 600.),
            SIDEBAR_WIDTH_DEFAULT + 60.
        );
        assert_eq!(
            panel_width_for(SIDEBAR_WIDTH_DEFAULT, px(60.), 2.0, 100., 600.),
            SIDEBAR_WIDTH_DEFAULT + 30.
        );
    }

    #[test]
    fn a_rail_cannot_be_dragged_past_either_stop() {
        assert_eq!(
            panel_width_for(SIDEBAR_WIDTH_DEFAULT, px(-9000.), 1.0, 150., 560.),
            150.,
            "it must never close by being dragged"
        );
        assert_eq!(
            panel_width_for(DETAIL_WIDTH_DEFAULT, px(9000.), 1.0, 180., 720.),
            720.,
            "and never take the whole window"
        );
    }

    // -- keeping the cell cursor on screen ------------------------------------

    #[test]
    fn a_column_already_on_screen_does_not_move_the_pane() {
        // Columns at 0..100 and 100..200 in a 300-wide pane, nothing panned.
        assert_eq!(h_offset_for(0., 100., 300., 0.), None);
        assert_eq!(h_offset_for(100., 100., 300., 0.), None);
        // And one that ends exactly on the edge is still whole.
        assert_eq!(h_offset_for(200., 100., 300., 0.), None);
    }

    #[test]
    fn a_column_off_an_edge_moves_the_pane_the_minimum() {
        // Off the right: its right edge comes to the pane's right edge, which
        // is one column of travel and not a jump to the top of the row.
        assert_eq!(h_offset_for(300., 100., 300., 0.), Some(100.));
        // Off the left: its left edge comes to the pane's left edge.
        assert_eq!(h_offset_for(50., 100., 300., 200.), Some(50.));
    }

    /// A column too wide to fit shows its left edge, which is where the values
    /// start -- pinning its right edge instead would scroll the content away.
    #[test]
    fn a_column_wider_than_the_pane_shows_its_start() {
        assert_eq!(h_offset_for(100., 900., 300., 0.), Some(100.));
    }

    // -- restoring the tree ---------------------------------------------------

    fn catalog_of(names: &[&str]) -> Catalog {
        Catalog {
            objects: Vec::new(),
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
