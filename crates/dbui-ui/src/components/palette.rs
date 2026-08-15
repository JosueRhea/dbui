//! Command palette: go-to-table (⌘P) and actions (⌘⇧P).

use super::icons::{command_mark, table_icon, theme_mark};
use super::text_field::{text_field, InputTarget};
use crate::root::{DbUi, Focus, Status};
use crate::text_input::TextInput;
use crate::theme::{metrics, Theme};
use dbui_app::domain::TableRef;
use gpui::{div, prelude::*, px, AnyElement, Context, ScrollHandle, SharedString};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteKind {
    GoToTable,
    Actions,
    Themes,
}

pub struct Palette {
    pub kind: PaletteKind,
    pub query: TextInput,
    pub selected: usize,
    /// Keeps the selected row visible while arrowing through the list.
    pub list_scroll: ScrollHandle,
}

impl Palette {
    pub fn new(kind: PaletteKind) -> Self {
        Self {
            kind,
            query: TextInput::new(false),
            selected: 0,
            list_scroll: ScrollHandle::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActionId {
    NewConnection,
    ImportTablePlus,
    ConnectActive,
    DisconnectActive,
    CloseConnection,
    NextConnection,
    PrevConnection,
    RefreshCatalog,
    RefreshResult,
    OpenSql,
    RunQuery,
    RunAllQueries,
    GoToTable,
    SearchTables,
    SelectAllRows,
    AddRow,
    DeleteRows,
    CopyRowsTsv,
    CopyRowsJson,
    CopyRowsInsert,
    ClearSort,
    CommitChanges,
    DiscardChanges,
    ToggleFilters,
    ToggleColumns,
    ToggleDetail,
    PagePrev,
    PageNext,
    FocusSidebar,
    ClearSql,
    ChangeTheme,
    CloseTab,
    NextTab,
    PrevTab,
    ZoomIn,
    ZoomOut,
    ZoomReset,
}

struct ActionDef {
    id: ActionId,
    label: &'static str,
    shortcut: Option<&'static str>,
    section: &'static str,
}

const ACTIONS: &[ActionDef] = &[
    // Navigate
    ActionDef {
        id: ActionId::GoToTable,
        label: "Go to Table…",
        shortcut: Some("⌘P"),
        section: "Navigate",
    },
    ActionDef {
        id: ActionId::SearchTables,
        label: "Search Tables",
        shortcut: Some("⌘⇧F"),
        section: "Navigate",
    },
    ActionDef {
        id: ActionId::FocusSidebar,
        label: "Focus Sidebar",
        shortcut: Some("Esc"),
        section: "Navigate",
    },
    ActionDef {
        id: ActionId::NextTab,
        label: "Next Tab",
        shortcut: Some("⌃⇥"),
        section: "Navigate",
    },
    ActionDef {
        id: ActionId::PrevTab,
        label: "Previous Tab",
        shortcut: Some("⌃⇧⇥"),
        section: "Navigate",
    },
    ActionDef {
        id: ActionId::CloseTab,
        label: "Close Tab",
        shortcut: Some("⌘W"),
        section: "Navigate",
    },
    // Connection
    ActionDef {
        id: ActionId::NewConnection,
        label: "New Connection",
        shortcut: Some("⌘N"),
        section: "Connection",
    },
    ActionDef {
        id: ActionId::ImportTablePlus,
        label: "Import from TablePlus",
        shortcut: None,
        section: "Connection",
    },
    ActionDef {
        id: ActionId::ConnectActive,
        label: "Connect Active",
        shortcut: None,
        section: "Connection",
    },
    ActionDef {
        id: ActionId::DisconnectActive,
        label: "Disconnect Active",
        shortcut: None,
        section: "Connection",
    },
    ActionDef {
        id: ActionId::NextConnection,
        label: "Next Connection",
        shortcut: Some("⌘⌥]"),
        section: "Connection",
    },
    ActionDef {
        id: ActionId::PrevConnection,
        label: "Previous Connection",
        shortcut: Some("⌘⌥["),
        section: "Connection",
    },
    ActionDef {
        id: ActionId::CloseConnection,
        label: "Close Connection",
        shortcut: Some("⌘⇧W"),
        section: "Connection",
    },
    ActionDef {
        id: ActionId::RefreshCatalog,
        label: "Refresh Catalog",
        shortcut: None,
        section: "Connection",
    },
    // Query
    ActionDef {
        id: ActionId::OpenSql,
        label: "New SQL Tab",
        shortcut: Some("⌘E"),
        section: "Query",
    },
    ActionDef {
        id: ActionId::RunQuery,
        label: "Run Query",
        shortcut: Some("⌘↵"),
        section: "Query",
    },
    ActionDef {
        id: ActionId::RunAllQueries,
        label: "Run All Queries",
        shortcut: Some("⌘⇧↵"),
        section: "Query",
    },
    ActionDef {
        id: ActionId::ClearSql,
        label: "Clear SQL Editor",
        shortcut: Some("⌘K"),
        section: "Query",
    },
    ActionDef {
        id: ActionId::RefreshResult,
        label: "Refresh Result",
        shortcut: Some("⌘R"),
        section: "Query",
    },
    ActionDef {
        id: ActionId::PagePrev,
        label: "Previous Page",
        shortcut: Some("⌘["),
        section: "Query",
    },
    ActionDef {
        id: ActionId::PageNext,
        label: "Next Page",
        shortcut: Some("⌘]"),
        section: "Query",
    },
    // Rows
    ActionDef {
        id: ActionId::SelectAllRows,
        label: "Select All Rows",
        shortcut: Some("⌘A"),
        section: "Rows",
    },
    ActionDef {
        id: ActionId::AddRow,
        label: "New Row",
        shortcut: None,
        section: "Rows",
    },
    ActionDef {
        id: ActionId::DeleteRows,
        label: "Delete Selected Rows",
        shortcut: Some("⌘⌫"),
        section: "Rows",
    },
    ActionDef {
        id: ActionId::CopyRowsTsv,
        label: "Copy Rows as TSV",
        shortcut: Some("⌘C"),
        section: "Rows",
    },
    ActionDef {
        id: ActionId::CopyRowsJson,
        label: "Copy Rows as JSON",
        shortcut: None,
        section: "Rows",
    },
    ActionDef {
        id: ActionId::CopyRowsInsert,
        label: "Copy Rows as INSERT",
        shortcut: None,
        section: "Rows",
    },
    ActionDef {
        id: ActionId::ClearSort,
        label: "Clear Sort",
        shortcut: None,
        section: "Rows",
    },
    ActionDef {
        id: ActionId::CommitChanges,
        label: "Commit Changes",
        shortcut: Some("⌘S"),
        section: "Rows",
    },
    ActionDef {
        id: ActionId::DiscardChanges,
        label: "Discard Changes",
        shortcut: Some("⌘Z"),
        section: "Rows",
    },
    // View
    ActionDef {
        id: ActionId::ToggleFilters,
        label: "Toggle Filters",
        shortcut: None,
        section: "View",
    },
    ActionDef {
        id: ActionId::ToggleColumns,
        label: "Toggle Columns",
        shortcut: None,
        section: "View",
    },
    ActionDef {
        id: ActionId::ToggleDetail,
        label: "Toggle Row Detail",
        shortcut: None,
        section: "View",
    },
    ActionDef {
        id: ActionId::ChangeTheme,
        label: "Change Theme…",
        shortcut: Some("⌘⇧T"),
        section: "View",
    },
    ActionDef {
        id: ActionId::ZoomIn,
        label: "Zoom In",
        shortcut: Some("⌘+"),
        section: "View",
    },
    ActionDef {
        id: ActionId::ZoomOut,
        label: "Zoom Out",
        shortcut: Some("⌘-"),
        section: "View",
    },
    ActionDef {
        id: ActionId::ZoomReset,
        label: "Actual Size",
        shortcut: Some("⌘0"),
        section: "View",
    },
];

enum PaletteRow {
    Table(TableRef),
    Action {
        id: ActionId,
        enabled: bool,
    },
    Theme {
        id: &'static str,
        label: &'static str,
    },
}

impl PaletteRow {
    fn section(&self) -> &'static str {
        match self {
            PaletteRow::Table(_) => "Tables",
            PaletteRow::Theme { .. } => "Themes",
            PaletteRow::Action { id, .. } => ACTIONS
                .iter()
                .find(|a| a.id == *id)
                .map(|a| a.section)
                .unwrap_or("Actions"),
        }
    }
}

fn section_header(label: &str, theme: &Theme) -> AnyElement {
    div()
        .px_3()
        .pt_2()
        .pb_0p5()
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(theme.text_faint)
        .child(SharedString::from(label.to_uppercase()))
        .into_any_element()
}

fn shortcut_chip(label: &str, theme: &Theme) -> AnyElement {
    div()
        .px_1p5()
        .py_0p5()
        .rounded(px(4.))
        .bg(theme.background)
        .text_size(metrics::text_size_small())
        .text_color(theme.text_faint)
        .child(SharedString::from(label.to_string()))
        .into_any_element()
}

fn legend_key(label: &str, theme: &Theme) -> AnyElement {
    div()
        .px_1()
        .rounded(px(3.))
        .bg(theme.background)
        .text_size(px(10.))
        .text_color(theme.text_faint)
        .child(SharedString::from(label.to_string()))
        .into_any_element()
}

fn legend_item(key: &str, caption: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap_1()
        .child(legend_key(key, theme))
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.text_faint)
                .child(SharedString::from(caption.to_string())),
        )
        .into_any_element()
}

impl DbUi {
    pub(crate) fn open_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) {
        // Leaving the theme picker without committing restores the previous theme.
        if self.palette.as_ref().map(|p| p.kind) == Some(PaletteKind::Themes)
            && kind != PaletteKind::Themes
        {
            self.cancel_theme_preview();
        }

        self.connection_picker_open = false;
        self.filter_focus = None;
        self.page_size_focus = false;
        self.detail_input = None;

        let mut palette = Palette::new(kind);
        if kind == PaletteKind::Themes {
            self.theme_prev = Some(self.theme.id.to_string());
            palette.selected = crate::theme::all_themes()
                .iter()
                .position(|t| t.id == self.theme.id)
                .unwrap_or(0);
        }

        self.palette = Some(palette);
        if kind == PaletteKind::Themes {
            self.preview_selected_theme();
        }
        cx.notify();
    }

    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        let was_themes = self.palette.as_ref().map(|p| p.kind) == Some(PaletteKind::Themes);
        self.palette = None;
        if was_themes {
            self.cancel_theme_preview();
        }
        cx.notify();
    }

    fn cancel_theme_preview(&mut self) {
        if let Some(prev) = self.theme_prev.take() {
            self.apply_theme_id(&prev);
        }
    }

    fn preview_theme_id(&mut self, id: &str) {
        self.apply_theme_id(id);
    }

    fn commit_theme_id(&mut self, id: &str, cx: &mut Context<Self>) {
        self.apply_theme_id(id);
        self.theme_prev = None;
        self.palette = None;
        self.persist_theme(cx);
    }

    fn preview_selected_theme(&mut self) {
        let selected = self.palette.as_ref().map(|p| p.selected).unwrap_or(0);
        let rows = self.palette_rows(PaletteKind::Themes);
        if let Some(PaletteRow::Theme { id, .. }) = rows.get(selected) {
            self.preview_theme_id(id);
        }
    }

    pub(crate) fn cmd_find(&mut self, cx: &mut Context<Self>) {
        self.close_palette(cx);

        if matches!(
            self.tabs.active(),
            Some(crate::tabs::WorkspaceTab::Table { .. })
        ) {
            if let Some(crate::tabs::WorkspaceTab::Table {
                filters_open,
                where_draft,
                where_clause,
                ..
            }) = self.tabs.active_mut()
            {
                *filters_open = true;
                if where_draft.text().is_empty() && !where_clause.is_empty() {
                    *where_draft = TextInput::with_text(where_clause.clone(), false);
                }
            }
            self.focus_input(InputTarget::WhereDraft, cx);
            return;
        }

        self.status = Status::info("Open a table to filter");
        cx.notify();
    }

    pub(crate) fn handle_palette_key(
        &mut self,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(palette) = self.palette.as_ref() else {
            return false;
        };
        let kind = palette.kind;
        let key = keystroke.key.as_str();
        let command = keystroke.modifiers.platform;
        let shift = keystroke.modifiers.shift;

        if key == "escape" {
            self.close_palette(cx);
            return true;
        }

        if command {
            match key {
                "p" if shift => {
                    self.open_palette(PaletteKind::Actions, cx);
                    return true;
                }
                "p" => {
                    self.open_palette(PaletteKind::GoToTable, cx);
                    return true;
                }
                _ => {}
            }
        }

        let rows = self.palette_rows(kind);
        let len = rows.len();

        if key == "up" {
            if let Some(palette) = self.palette.as_mut() {
                if len == 0 {
                    palette.selected = 0;
                } else if palette.selected == 0 {
                    palette.selected = len - 1;
                } else {
                    palette.selected -= 1;
                }
            }
            if kind == PaletteKind::Themes {
                self.preview_selected_theme();
            }
            cx.notify();
            return true;
        }
        if key == "down" {
            if let Some(palette) = self.palette.as_mut() {
                if len == 0 {
                    palette.selected = 0;
                } else {
                    palette.selected = (palette.selected + 1) % len;
                }
            }
            if kind == PaletteKind::Themes {
                self.preview_selected_theme();
            }
            cx.notify();
            return true;
        }
        if key == "enter" {
            if kind == PaletteKind::Themes {
                let selected = self.palette.as_ref().map(|p| p.selected).unwrap_or(0);
                let rows = self.palette_rows(PaletteKind::Themes);
                if let Some(PaletteRow::Theme { id, .. }) = rows.get(selected) {
                    self.commit_theme_id(id, cx);
                }
                return true;
            }
            if let Some(row) = rows.get(self.palette.as_ref().map(|p| p.selected).unwrap_or(0)) {
                self.run_palette_row(row, cx);
            }
            return true;
        }

        let _ = shift;
        let mut preview_themes = false;
        if let Some(palette) = self.palette.as_mut() {
            if palette.query.handle_key(keystroke, cx) {
                palette.selected = 0;
                preview_themes = palette.kind == PaletteKind::Themes;
                cx.notify();
            } else {
                return true;
            }
        }
        if preview_themes {
            self.preview_selected_theme();
            cx.notify();
        }
        true
    }

    fn palette_rows(&self, kind: PaletteKind) -> Vec<PaletteRow> {
        let query = self
            .palette
            .as_ref()
            .map(|p| p.query.text().to_lowercase())
            .unwrap_or_default();

        match kind {
            PaletteKind::GoToTable => {
                let Some(entry) = self.workspace.active() else {
                    return Vec::new();
                };
                let Some(catalog) = entry.catalog.as_ref() else {
                    return Vec::new();
                };
                catalog
                    .schemas
                    .iter()
                    .flat_map(|schema| schema.tables.iter())
                    .map(|table| table.reference())
                    .filter(|table| {
                        if query.is_empty() {
                            return true;
                        }
                        let q = table.qualified().to_lowercase();
                        let name = table.name.to_lowercase();
                        q.contains(&query) || name.contains(&query)
                    })
                    .map(PaletteRow::Table)
                    .collect()
            }
            PaletteKind::Actions => ACTIONS
                .iter()
                .filter(|action| {
                    if query.is_empty() {
                        return true;
                    }
                    action.label.to_lowercase().contains(&query)
                })
                .map(|action| PaletteRow::Action {
                    id: action.id,
                    enabled: self.action_enabled(action.id),
                })
                .collect(),
            PaletteKind::Themes => crate::theme::all_themes()
                .iter()
                .filter(|theme| {
                    if query.is_empty() {
                        return true;
                    }
                    let q = query.as_str();
                    theme.label.to_lowercase().contains(q) || theme.id.contains(q)
                })
                .map(|theme| PaletteRow::Theme {
                    id: theme.id,
                    label: theme.label,
                })
                .collect(),
        }
    }

    fn action_enabled(&self, id: ActionId) -> bool {
        let has_active = self.workspace.active_id().is_some();
        let connected = self.workspace.active_driver().is_some();
        let is_table = matches!(
            self.tabs.active(),
            Some(crate::tabs::WorkspaceTab::Table { .. })
        );
        let is_sql = matches!(
            self.tabs.active(),
            Some(crate::tabs::WorkspaceTab::Sql { .. })
        );

        match id {
            ActionId::NewConnection
            | ActionId::ImportTablePlus
            | ActionId::GoToTable
            | ActionId::SearchTables
            | ActionId::FocusSidebar
            | ActionId::ChangeTheme
            | ActionId::OpenSql
            | ActionId::CloseTab
            | ActionId::NextTab
            | ActionId::PrevTab
            | ActionId::ZoomIn
            | ActionId::ZoomOut
            | ActionId::ZoomReset => true,
            ActionId::ConnectActive => {
                has_active
                    && self
                        .workspace
                        .active()
                        .map(|e| !e.status.is_connected())
                        .unwrap_or(false)
            }
            ActionId::DisconnectActive | ActionId::RefreshCatalog => connected,
            ActionId::CloseConnection => has_active,
            // Nothing to step to with one tab open, or none.
            ActionId::NextConnection | ActionId::PrevConnection => {
                self.workspace.open_count() > 1
            }
            ActionId::RefreshResult => connected && (is_table || is_sql),
            ActionId::RunQuery | ActionId::RunAllQueries | ActionId::ClearSql => is_sql,
            ActionId::ToggleFilters
            | ActionId::ToggleColumns
            | ActionId::PagePrev
            | ActionId::PageNext => is_table,
            // Selecting needs rows on screen, from either kind of tab.
            ActionId::SelectAllRows => self
                .tabs
                .active()
                .and_then(|tab| tab.result())
                .is_some_and(|view| !view.set.rows.is_empty()),
            ActionId::DeleteRows => {
                is_table
                    && self
                        .tabs
                        .active()
                        .is_some_and(|tab| !tab.selection().is_empty())
            }
            ActionId::AddRow => {
                is_table && self.tabs.active().and_then(|tab| tab.result()).is_some()
            }
            ActionId::CopyRowsTsv | ActionId::CopyRowsJson | ActionId::CopyRowsInsert => self
                .tabs
                .active()
                .and_then(|tab| tab.result())
                .is_some_and(|view| !view.set.rows.is_empty()),
            ActionId::ClearSort => self.active_sort().is_some(),
            ActionId::CommitChanges | ActionId::DiscardChanges => {
                !self.collect_batch_edits().is_empty() || !self.collect_batch_deletes().is_empty()
            }
            ActionId::ToggleDetail => true,
        }
    }

    fn run_palette_row(&mut self, row: &PaletteRow, cx: &mut Context<Self>) {
        match row {
            PaletteRow::Table(table) => {
                let table = table.clone();
                self.close_palette(cx);
                self.open_table_tab(table, cx);
            }
            PaletteRow::Action { id, enabled } => {
                if !*enabled {
                    return;
                }
                let id = *id;
                self.close_palette(cx);
                self.run_action(id, cx);
            }
            PaletteRow::Theme { id, .. } => {
                self.commit_theme_id(id, cx);
            }
        }
    }

    fn run_action(&mut self, id: ActionId, cx: &mut Context<Self>) {
        match id {
            ActionId::NewConnection => self.open_new_connection(cx),
            ActionId::ImportTablePlus => self.import_tableplus_connections(cx),
            ActionId::ConnectActive => {
                if let Some(id) = self.workspace.active_id() {
                    self.connect(id, cx);
                }
            }
            ActionId::DisconnectActive => {
                if let Some(id) = self.workspace.active_id() {
                    self.disconnect(id, cx);
                }
            }
            ActionId::CloseConnection => self.close_active_connection_tab(cx),
            ActionId::NextConnection => self.cycle_connection_tab(true, cx),
            ActionId::PrevConnection => self.cycle_connection_tab(false, cx),
            ActionId::RefreshCatalog => self.refresh_catalog(cx),
            ActionId::RefreshResult => self.refresh_result(cx),
            ActionId::OpenSql => self.open_sql_tab(cx),
            ActionId::CloseTab => self.close_active_tab(cx),
            ActionId::NextTab => self.next_tab(cx),
            ActionId::PrevTab => self.prev_tab(cx),
            ActionId::RunQuery => self.run_query(cx),
            ActionId::RunAllQueries => self.run_all_queries(cx),
            ActionId::GoToTable => self.open_palette(PaletteKind::GoToTable, cx),
            ActionId::SearchTables => self.focus_sidebar_search(cx),
            ActionId::SelectAllRows => self.select_all_rows(cx),
            ActionId::DeleteRows => self.delete_selected_rows(cx),
            ActionId::AddRow => self.add_row(cx),
            ActionId::CopyRowsTsv => {
                self.copy_selected_rows(crate::row_export::RowFormat::Tsv, cx)
            }
            ActionId::CopyRowsJson => {
                self.copy_selected_rows(crate::row_export::RowFormat::Json, cx)
            }
            ActionId::CopyRowsInsert => {
                self.copy_selected_rows(crate::row_export::RowFormat::Insert, cx)
            }
            ActionId::ClearSort => self.clear_sort(cx),
            ActionId::CommitChanges => self.save_pending_edits(cx),
            ActionId::DiscardChanges => self.discard_pending_edits(cx),
            ActionId::ToggleFilters => self.toggle_filters_open(cx),
            ActionId::ToggleColumns => self.toggle_columns_open(cx),
            ActionId::ToggleDetail => self.toggle_detail(cx),
            ActionId::PagePrev => self.page(false, cx),
            ActionId::PageNext => self.page(true, cx),
            ActionId::FocusSidebar => {
                self.focus = Focus::Sidebar;
                cx.notify();
            }
            ActionId::ClearSql => {
                if let Some(crate::tabs::WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut()
                {
                    editor.clear();
                }
                cx.notify();
            }
            ActionId::ChangeTheme => self.open_palette(PaletteKind::Themes, cx),
            ActionId::ZoomIn => self.zoom_delta(1, cx),
            ActionId::ZoomOut => self.zoom_delta(-1, cx),
            ActionId::ZoomReset => self.zoom_delta(0, cx),
        }
    }

    pub(crate) fn render_palette(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let Some(palette) = self.palette.as_ref() else {
            return None;
        };
        let kind = palette.kind;
        let selected = palette.selected;
        let query = &palette.query;

        let placeholder = match kind {
            PaletteKind::GoToTable => "Search tables…",
            PaletteKind::Actions => "Type a command…",
            PaletteKind::Themes => "Search themes…",
        };

        let rows = self.palette_rows(kind);
        let selected = if rows.is_empty() {
            0
        } else {
            selected.min(rows.len() - 1)
        };
        let list_scroll = palette.list_scroll.clone();

        let list: Vec<AnyElement> = if rows.is_empty() {
            let empty = match kind {
                PaletteKind::GoToTable => {
                    if self.workspace.active_driver().is_none() {
                        "Connect to a database first"
                    } else {
                        "No matching tables"
                    }
                }
                PaletteKind::Actions => "No matching actions",
                PaletteKind::Themes => "No matching themes",
            };
            vec![div()
                .px_4()
                .py_4()
                .text_color(theme.text_muted)
                .child(empty)
                .into_any_element()]
        } else {
            let mut list = Vec::new();
            let mut last_section: Option<&str> = None;
            let mut selected_child: Option<usize> = None;
            for (index, row) in rows.iter().enumerate() {
                let section = row.section();
                if last_section != Some(section) {
                    list.push(section_header(section, theme));
                    last_section = Some(section);
                }

                let is_sel = index == selected;
                if is_sel {
                    selected_child = Some(list.len());
                }
                let row_el = match row {
                    PaletteRow::Table(table) => {
                        let label = SharedString::from(table.qualified());
                        let target = table.clone();
                        palette_row(
                            index,
                            is_sel,
                            true,
                            table_icon(theme.text_muted).into_any_element(),
                            label,
                            None,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                this.close_palette(cx);
                                this.open_table_tab(target.clone(), cx);
                            }),
                        )
                    }
                    PaletteRow::Action { id, enabled } => {
                        let def = ACTIONS.iter().find(|a| a.id == *id).unwrap();
                        let label = SharedString::from(def.label);
                        let shortcut = def.shortcut;
                        let enabled = *enabled;
                        let action_id = *id;
                        palette_row(
                            index,
                            is_sel,
                            enabled,
                            command_mark(if enabled {
                                theme.text_muted
                            } else {
                                theme.text_faint
                            })
                            .into_any_element(),
                            label,
                            shortcut,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                if !enabled {
                                    return;
                                }
                                this.close_palette(cx);
                                this.run_action(action_id, cx);
                            }),
                        )
                    }
                    PaletteRow::Theme { id, label } => {
                        let theme_id = *id;
                        let label = SharedString::from(*label);
                        let active = self.theme_prev.as_deref().unwrap_or(self.theme.id) == *id
                            || self.theme.id == *id;
                        let chip = if active && is_sel {
                            Some("preview")
                        } else if active {
                            Some("current")
                        } else {
                            None
                        };
                        palette_row(
                            index,
                            is_sel,
                            true,
                            theme_mark(theme.accent).into_any_element(),
                            label,
                            chip,
                            theme,
                            cx.listener(move |this, _, _, cx| {
                                this.commit_theme_id(theme_id, cx);
                            }),
                        )
                    }
                };
                list.push(row_el);
            }
            if let Some(child_ix) = selected_child {
                list_scroll.scroll_to_item(child_ix);
            }
            list
        };

        let scrim = if theme.is_light {
            gpui::rgba(0x00000033)
        } else {
            gpui::rgba(0x00000066)
        };

        Some(
            div()
                .id("palette-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .pt(px(72.))
                .bg(scrim)
                .on_click(cx.listener(|this, _, _, cx| this.close_palette(cx)))
                .child(
                    div()
                        .id("palette-panel")
                        .w(px(560.))
                        .max_h(px(480.))
                        .flex()
                        .flex_col()
                        .rounded(px(16.))
                        .bg(theme.elevated)
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_3()
                                .pt_1()
                                .pb_1()
                                .child(div().flex_1().min_w(px(0.)).child(text_field(
                                    "palette-query",
                                    query,
                                    InputTarget::PaletteQuery,
                                    true,
                                    Some(placeholder),
                                    theme,
                                    cx,
                                )))
                                .child(
                                    div()
                                        .id("palette-close")
                                        .w(px(28.))
                                        .h(px(28.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(px(6.))
                                        .cursor_pointer()
                                        .text_color(theme.text_faint)
                                        .hover(|s| s.bg(theme.hover).text_color(theme.text))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.close_palette(cx);
                                        }))
                                        .child("×"),
                                ),
                        )
                        .child(div().h(px(1.)).w_full().bg(theme.divider))
                        .child(
                            div()
                                .id("palette-list")
                                .track_scroll(&list_scroll)
                                .flex_1()
                                .min_h(px(0.))
                                .max_h(px(360.))
                                .overflow_y_scroll()
                                .pb_1()
                                .children(list),
                        )
                        .child(div().h(px(1.)).w_full().bg(theme.divider))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .px_3()
                                .py_1p5()
                                .child(legend_item("↑↓", "Navigate", theme))
                                .child(legend_item("↵", "Confirm", theme))
                                .child(legend_item("esc", "Close", theme)),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn palette_row(
    index: usize,
    selected: bool,
    enabled: bool,
    icon: AnyElement,
    label: SharedString,
    chip: Option<&'static str>,
    theme: &Theme,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    div()
        .id(("palette-row", index))
        .mx_2()
        .px_2()
        .py_1()
        .rounded(px(6.))
        .flex()
        .items_center()
        .gap_2()
        .cursor_pointer()
        .when(selected, |r| r.bg(theme.selection))
        .when(!enabled, |r| r.text_color(theme.text_faint))
        .hover(|r| {
            r.bg(if selected {
                theme.selection
            } else {
                theme.hover
            })
        })
        .on_click(on_click)
        .child(icon)
        .child(div().flex_1().min_w(px(0.)).child(label))
        .children(chip.map(|s| shortcut_chip(s, theme)))
        .into_any_element()
}
