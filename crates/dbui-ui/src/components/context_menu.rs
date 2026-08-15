//! Right-click menus, and the typed confirmation that guards the two
//! destructive entries in them.
//!
//! The menu is state on [`DbUi`] like everything else: it is opened by a
//! pointer-down handler, drawn at the top of the window so it can overhang the
//! sidebar, and closed by anything else the user does. Nothing here owns state
//! of its own.

use super::{button, caption};
use crate::root::{DbUi, Focus, Status};
use crate::text_input::TextInput;
use crate::row_export::RowFormat;
use crate::theme::metrics;
use dbui_app::commands;
use dbui_app::domain::{ConnectionId, TableKind, TableRef};
use gpui::{
    div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, Pixels, Point,
    SharedString, Window,
};

/// What the pointer was over.
#[derive(Clone)]
pub enum ContextTarget {
    Table { table: TableRef, kind: TableKind },
    Schema { connection: ConnectionId, name: String },
    /// A row in the result grid.
    Rows,
}

/// One thing a context menu can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    OpenTable,
    ShowStructure,
    CopyName,
    CopyQualifiedName,
    SelectInSqlTab,
    CopyInsert,
    CopyCreate,
    RefreshCatalog,
    ToggleSchema,
    Truncate,
    Drop,
    CopyRowsTsv,
    CopyRowsJson,
    CopyRowsInsert,
    DeleteRows,
    AddRow,
}

impl MenuAction {
    /// Whether picking this needs a typed confirmation first.
    fn is_destructive(self) -> bool {
        matches!(self, MenuAction::Truncate | MenuAction::Drop)
    }
}

enum MenuRow {
    Separator,
    Item {
        action: MenuAction,
        label: SharedString,
    },
}

pub struct ContextMenu {
    pub target: ContextTarget,
    pub position: Point<Pixels>,
    /// Keyboard cursor. Indexes the *selectable* rows, not the separators.
    pub selected: usize,
}

/// A destructive statement waiting for the user to type the relation's name.
///
/// A confirm button on its own is a reflex; typing the name is a sentence the
/// user has to mean. The name is also what tells them *which* table they are
/// about to lose when the menu that started this is long gone.
pub struct ConfirmPrompt {
    pub action: MenuAction,
    pub table: TableRef,
    pub kind: TableKind,
    pub input: TextInput,
    pub running: bool,
    pub error: Option<SharedString>,
}

impl ConfirmPrompt {
    fn new(action: MenuAction, table: TableRef, kind: TableKind) -> Self {
        Self {
            action,
            table,
            kind,
            input: TextInput::new(false),
            running: false,
            error: None,
        }
    }

    pub fn title(&self) -> String {
        let what = match self.kind {
            TableKind::Table => "table",
            TableKind::View => "view",
            TableKind::MaterializedView => "materialized view",
        };
        match self.action {
            MenuAction::Truncate => format!("Truncate {what}"),
            _ => format!("Drop {what}"),
        }
    }

    pub fn body(&self) -> String {
        match self.action {
            MenuAction::Truncate => format!(
                "Every row in {} is deleted. This cannot be undone and is not \
                 part of the staged batch — it runs immediately.",
                self.table.qualified()
            ),
            _ => format!(
                "{} and everything in it is removed from the server. This \
                 cannot be undone.",
                self.table.qualified()
            ),
        }
    }

    /// What has to be typed: the bare name, which is what the tree shows.
    pub fn expected(&self) -> &str {
        &self.table.name
    }

    pub fn armed(&self) -> bool {
        !self.running && self.input.text().trim() == self.expected()
    }
}

fn rows_for(target: &ContextTarget) -> Vec<MenuRow> {
    match target {
        ContextTarget::Rows => vec![
            MenuRow::Item {
                action: MenuAction::CopyRowsTsv,
                label: RowFormat::Tsv.label().into(),
            },
            MenuRow::Item {
                action: MenuAction::CopyRowsJson,
                label: RowFormat::Json.label().into(),
            },
            MenuRow::Item {
                action: MenuAction::CopyRowsInsert,
                label: RowFormat::Insert.label().into(),
            },
            MenuRow::Separator,
            MenuRow::Item {
                action: MenuAction::AddRow,
                label: "New Row".into(),
            },
            MenuRow::Item {
                action: MenuAction::DeleteRows,
                label: "Delete Selected Rows".into(),
            },
        ],
        ContextTarget::Schema { .. } => vec![
            MenuRow::Item {
                action: MenuAction::ToggleSchema,
                label: "Expand / Collapse".into(),
            },
            MenuRow::Item {
                action: MenuAction::CopyName,
                label: "Copy Name".into(),
            },
            MenuRow::Separator,
            MenuRow::Item {
                action: MenuAction::RefreshCatalog,
                label: "Refresh Catalog".into(),
            },
        ],
        ContextTarget::Table { kind, .. } => {
            let mut rows = vec![
                MenuRow::Item {
                    action: MenuAction::OpenTable,
                    label: "Open".into(),
                },
                MenuRow::Item {
                    action: MenuAction::ShowStructure,
                    label: "Show Structure".into(),
                },
                MenuRow::Separator,
                MenuRow::Item {
                    action: MenuAction::CopyName,
                    label: "Copy Name".into(),
                },
                MenuRow::Item {
                    action: MenuAction::CopyQualifiedName,
                    label: "Copy Qualified Name".into(),
                },
                MenuRow::Separator,
                MenuRow::Item {
                    action: MenuAction::SelectInSqlTab,
                    label: "SELECT in New SQL Tab".into(),
                },
                MenuRow::Item {
                    action: MenuAction::CopyInsert,
                    label: "Copy INSERT Template".into(),
                },
                MenuRow::Item {
                    action: MenuAction::CopyCreate,
                    label: "Copy CREATE TABLE".into(),
                },
                MenuRow::Separator,
                MenuRow::Item {
                    action: MenuAction::RefreshCatalog,
                    label: "Refresh Catalog".into(),
                },
                MenuRow::Separator,
            ];
            // There is nothing to truncate in a view -- both engines reject it.
            if *kind == TableKind::Table {
                rows.push(MenuRow::Item {
                    action: MenuAction::Truncate,
                    label: "Truncate Table…".into(),
                });
            }
            rows.push(MenuRow::Item {
                action: MenuAction::Drop,
                label: match kind {
                    TableKind::Table => "Drop Table…".into(),
                    TableKind::View => "Drop View…".into(),
                    TableKind::MaterializedView => "Drop Materialized View…".into(),
                },
            });
            rows
        }
    }
}

fn actions_of(target: &ContextTarget) -> Vec<MenuAction> {
    rows_for(target)
        .iter()
        .filter_map(|row| match row {
            MenuRow::Item { action, .. } => Some(*action),
            MenuRow::Separator => None,
        })
        .collect()
}

/// Menu width, and the margin kept from the window edges so a menu opened near
/// one is still fully on screen.
const MENU_WIDTH: f32 = 232.;
const EDGE_MARGIN: f32 = 8.;

/// Row height plus padding, used to guess the menu's height for the flip.
const ROW_HEIGHT: f32 = 26.;
const SEPARATOR_HEIGHT: f32 = 9.;

impl DbUi {
    pub(crate) fn open_context_menu(
        &mut self,
        target: ContextTarget,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        // A typed confirmation is modal. It already owns the keyboard, and a
        // menu opening behind it would offer a second destructive statement
        // over the top of one the user has not answered yet.
        if self.confirm.is_some() {
            return;
        }
        self.connection_picker_open = false;
        self.detail_value_menu = None;
        self.context_menu = Some(ContextMenu {
            target,
            position,
            selected: 0,
        });
        cx.notify();
    }

    pub(crate) fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn handle_context_menu_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(menu) = self.context_menu.as_ref() else {
            return;
        };
        let actions = actions_of(&menu.target);
        if actions.is_empty() {
            return;
        }

        match key {
            "up" | "down" => {
                if let Some(menu) = self.context_menu.as_mut() {
                    menu.selected = if key == "down" {
                        (menu.selected + 1) % actions.len()
                    } else if menu.selected == 0 {
                        actions.len() - 1
                    } else {
                        menu.selected - 1
                    };
                }
                cx.notify();
            }
            "enter" => {
                let index = self
                    .context_menu
                    .as_ref()
                    .map(|menu| menu.selected)
                    .unwrap_or(0);
                if let Some(action) = actions.get(index).copied() {
                    self.run_context_action(action, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn run_context_action(&mut self, action: MenuAction, cx: &mut Context<Self>) {
        let Some(menu) = self.context_menu.as_ref() else {
            return;
        };
        let target = menu.target.clone();
        self.close_context_menu(cx);

        // A destructive pick opens the confirmation instead of running.
        if action.is_destructive() {
            if self.refuse_if_read_only("That", cx) {
                return;
            }
            if let ContextTarget::Table { table, kind } = target {
                self.confirm = Some(ConfirmPrompt::new(action, table, kind));
                cx.notify();
            }
            return;
        }

        match (&target, action) {
            (ContextTarget::Schema { connection, name }, MenuAction::ToggleSchema) => {
                let (connection, name) = (*connection, name.clone());
                self.toggle_schema(connection, &name, cx);
            }
            (ContextTarget::Schema { name, .. }, MenuAction::CopyName) => {
                self.copy_to_clipboard(name.clone(), "Schema name copied", cx);
            }
            (_, MenuAction::RefreshCatalog) => self.refresh_catalog(cx),
            (_, MenuAction::CopyRowsTsv) => self.copy_selected_rows(RowFormat::Tsv, cx),
            (_, MenuAction::CopyRowsJson) => self.copy_selected_rows(RowFormat::Json, cx),
            (_, MenuAction::CopyRowsInsert) => self.copy_selected_rows(RowFormat::Insert, cx),
            (_, MenuAction::AddRow) => self.add_row(cx),
            (_, MenuAction::DeleteRows) => self.delete_selected_rows(cx),

            (ContextTarget::Table { table, .. }, MenuAction::OpenTable) => {
                let table = table.clone();
                self.open_table_tab(table, cx);
            }
            (ContextTarget::Table { table, .. }, MenuAction::ShowStructure) => {
                let table = table.clone();
                self.open_table_tab(table, cx);
                self.set_table_pane(crate::tabs::TablePane::Structure, cx);
            }
            (ContextTarget::Table { table, .. }, MenuAction::CopyName) => {
                self.copy_to_clipboard(table.name.clone(), "Table name copied", cx);
            }
            (ContextTarget::Table { table, .. }, MenuAction::CopyQualifiedName) => {
                self.copy_to_clipboard(table.qualified(), "Table name copied", cx);
            }
            (ContextTarget::Table { table, .. }, MenuAction::SelectInSqlTab) => {
                let table = table.clone();
                let Some(driver) = self.active_driver_kind() else {
                    self.status = Status::error("Not connected");
                    cx.notify();
                    return;
                };
                let sql = crate::sql_scaffold::select_statement(driver, &table);
                self.open_sql_tab(cx);
                if let Some(crate::tabs::WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() {
                    *editor = TextInput::with_text(sql, true);
                }
                self.focus = Focus::Editor;
                cx.notify();
            }
            (ContextTarget::Table { table, .. }, MenuAction::CopyInsert | MenuAction::CopyCreate) => {
                let table = table.clone();
                self.copy_generated_sql(table, action, cx);
            }
            _ => {}
        }
    }

    /// The engine of the active connection, for quoting.
    pub(crate) fn active_driver_kind(&self) -> Option<dbui_app::domain::Driver> {
        self.workspace
            .active_driver()
            .map(|driver| driver.driver())
    }

    pub(crate) fn copy_to_clipboard(
        &mut self,
        text: impl Into<String>,
        message: &'static str,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.into()));
        self.status = Status::info(message);
        cx.notify();
    }

    /// Copy an INSERT or CREATE scaffold, fetching the columns if they are not
    /// already cached.
    fn copy_generated_sql(
        &mut self,
        table: TableRef,
        action: MenuAction,
        cx: &mut Context<Self>,
    ) {
        let Some(driver_kind) = self.active_driver_kind() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };

        let key = (table.schema.clone(), table.name.clone());
        if let Some(columns) = self.column_cache.get(&key) {
            let sql = render_scaffold(action, driver_kind, &table, columns);
            self.copy_to_clipboard(sql, "SQL copied", cx);
            return;
        }

        let Some(driver) = self.workspace.active_driver() else {
            return;
        };
        self.status = Status::busy(format!("Reading {}…", table.qualified()));
        let task = commands::fetch_columns(&self.runtime, driver, table.clone());
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                match landed {
                    Some(Ok((table, columns))) => {
                        this.column_cache
                            .insert((table.schema.clone(), table.name.clone()), columns.clone());
                        let sql = render_scaffold(action, driver_kind, &table, &columns);
                        this.copy_to_clipboard(sql, "SQL copied", cx);
                    }
                    Some(Err(error)) => {
                        this.status = Status::error(error.to_string());
                        cx.notify();
                    }
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }

    // -- the confirmation ---------------------------------------------------

    pub(crate) fn close_confirm(&mut self, cx: &mut Context<Self>) {
        if self
            .confirm
            .as_ref()
            .is_some_and(|prompt| prompt.running)
        {
            return;
        }
        if self.confirm.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn handle_confirm_key(&mut self, keystroke: &gpui::Keystroke, cx: &mut Context<Self>) {
        let key = keystroke.key.as_str();
        if key == "escape" {
            self.close_confirm(cx);
            return;
        }
        if key == "enter" && !keystroke.modifiers.platform {
            self.run_confirmed_action(cx);
            return;
        }
        if let Some(prompt) = self.confirm.as_mut() {
            if prompt.running {
                return;
            }
            if prompt.input.handle_key(keystroke, cx) {
                prompt.error = None;
                cx.notify();
            }
        }
    }

    pub(crate) fn run_confirmed_action(&mut self, cx: &mut Context<Self>) {
        // Re-checked here as well as at the menu: the flag can be turned on
        // while a confirmation is open.
        if self.refuse_if_read_only("That", cx) {
            self.confirm = None;
            return;
        }
        let Some(prompt) = self.confirm.as_ref() else {
            return;
        };
        if !prompt.armed() {
            if let Some(prompt) = self.confirm.as_mut() {
                let expected = prompt.expected().to_string();
                prompt.error = Some(SharedString::from(format!("Type {expected} to confirm")));
            }
            cx.notify();
            return;
        }

        let (action, table, kind) = (prompt.action, prompt.table.clone(), prompt.kind);
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            self.confirm = None;
            cx.notify();
            return;
        };

        if let Some(prompt) = self.confirm.as_mut() {
            prompt.running = true;
        }
        let verb = if action == MenuAction::Truncate {
            "Truncating"
        } else {
            "Dropping"
        };
        self.status = Status::busy(format!("{verb} {}…", table.qualified()));
        cx.notify();

        let task = if action == MenuAction::Truncate {
            commands::truncate_table(&self.runtime, driver, table.clone())
        } else {
            commands::drop_relation(&self.runtime, driver, table.clone(), kind)
        };

        let dropped = action == MenuAction::Drop;
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                match landed {
                    Some(Ok(_)) => {
                        this.confirm = None;
                        let what = table.qualified();
                        this.status = Status::info(if dropped {
                            format!("Dropped {what}")
                        } else {
                            format!("Truncated {what}")
                        });
                        // The tree still lists a relation that is gone, and a
                        // tab onto it would only fail on its next load.
                        if dropped {
                            this.close_tabs_for_table(&table, cx);
                        }
                        this.refresh_catalog(cx);
                        // A truncated table's open tab is showing rows that no
                        // longer exist.
                        if !dropped {
                            this.refresh_result(cx);
                        }
                    }
                    Some(Err(error)) => {
                        if let Some(prompt) = this.confirm.as_mut() {
                            prompt.running = false;
                            prompt.error = Some(SharedString::from(error.to_string()));
                        }
                        this.status = Status::error(error.to_string());
                        cx.notify();
                    }
                    None => {
                        if let Some(prompt) = this.confirm.as_mut() {
                            prompt.running = false;
                        }
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Close every tab pointing at a relation that no longer exists.
    pub(crate) fn close_tabs_for_table(&mut self, table: &TableRef, cx: &mut Context<Self>) {
        while let Some(index) = self
            .tabs
            .items
            .iter()
            .position(|tab| tab.table_ref() == Some(table))
        {
            self.close_tab(index, cx);
        }
    }

    // -- rendering ----------------------------------------------------------

    pub(crate) fn render_context_menu(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = &self.theme;
        let menu = self.context_menu.as_ref()?;
        let rows = rows_for(&menu.target);
        let selected = menu.selected;

        // Guessed rather than measured: the menu has to be placed before it is
        // laid out, and every row here is one line of the same size.
        let height: f32 = rows
            .iter()
            .map(|row| match row {
                MenuRow::Separator => SEPARATOR_HEIGHT,
                MenuRow::Item { .. } => ROW_HEIGHT,
            })
            .sum::<f32>()
            + 8.;

        let viewport = window.viewport_size();
        let left = f32::from(menu.position.x)
            .min(f32::from(viewport.width) - MENU_WIDTH - EDGE_MARGIN)
            .max(EDGE_MARGIN);
        // Flip above the pointer rather than hang off the bottom edge.
        let top = if f32::from(menu.position.y) + height + EDGE_MARGIN
            > f32::from(viewport.height)
        {
            (f32::from(menu.position.y) - height).max(EDGE_MARGIN)
        } else {
            f32::from(menu.position.y)
        };

        let mut index = 0usize;
        let children: Vec<AnyElement> = rows
            .iter()
            .enumerate()
            .map(|(row_index, row)| match row {
                MenuRow::Separator => div()
                    .my_1()
                    .h(px(1.))
                    .w_full()
                    .bg(theme.divider)
                    .into_any_element(),
                MenuRow::Item { action, label } => {
                    let action = *action;
                    let is_selected = index == selected;
                    index += 1;
                    let danger = action.is_destructive();
                    div()
                        .id(("context-menu-row", row_index))
                        .flex()
                        .items_center()
                        .px_3()
                        .py_1()
                        .cursor_pointer()
                        .text_color(if danger { theme.danger } else { theme.text })
                        .when(is_selected, |row| row.bg(theme.selection))
                        .hover(|row| row.bg(theme.hover))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                                this.run_context_action(action, cx);
                            }),
                        )
                        .child(label.clone())
                        .into_any_element()
                }
            })
            .collect();

        Some(
            // A full-window catcher: clicking anywhere else dismisses, which is
            // what every other menu on the platform does.
            div()
                .id("context-menu-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_context_menu(cx)
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(|this, _: &MouseDownEvent, _window, cx| {
                        this.close_context_menu(cx)
                    }),
                )
                .child(
                    div()
                        .id("context-menu")
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(MENU_WIDTH))
                        .py_1()
                        .rounded_md()
                        .bg(theme.elevated)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_md()
                        .text_size(metrics::text_size())
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .children(children),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn render_confirm(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = &self.theme;
        let prompt = self.confirm.as_ref()?;
        let title = prompt.title();
        let body = prompt.body();
        let expected = prompt.expected().to_string();
        let armed = prompt.armed();
        let running = prompt.running;
        let error = prompt.error.clone();
        let input = &prompt.input;

        let scrim = if theme.is_light {
            gpui::rgba(0x00000044)
        } else {
            gpui::rgba(0x00000088)
        };

        let confirm_button = if armed {
            button("confirm-destructive", "Confirm", theme, true)
                .bg(theme.danger)
                .border_color(theme.danger)
                .on_click(cx.listener(|this, _, _window, cx| this.run_confirmed_action(cx)))
        } else {
            button(
                "confirm-destructive",
                if running { "Running…" } else { "Confirm" },
                theme,
                false,
            )
            .opacity(0.5)
            .cursor_default()
        };

        Some(
            div()
                .id("confirm-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                // Without this the panel stretches to the full height of the
                // window instead of hugging its own text.
                .items_start()
                .pt(px(140.))
                .bg(scrim)
                // Modal to the pointer as well as to the keyboard: the surfaces
                // underneath stay visible, but a click cannot reach them. This
                // was how a right-click still opened the tree's menu behind an
                // unanswered "drop this table".
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("confirm-panel")
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .rounded(px(12.))
                        .bg(theme.elevated)
                        .border_1()
                        .border_color(theme.danger)
                        .child(
                            div()
                                .text_color(theme.danger)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(SharedString::from(body)),
                        )
                        .child(caption(
                            format!("Type “{expected}” to confirm."),
                            theme,
                        ))
                        .child(super::text_field::text_field(
                            "confirm-input",
                            input,
                            super::text_field::InputTarget::ConfirmName,
                            true,
                            Some(&expected),
                            theme,
                            cx,
                        ))
                        .children(error.map(|message| {
                            div()
                                .text_size(metrics::text_size_small())
                                .text_color(theme.danger)
                                .child(message)
                        }))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    button("confirm-cancel", "Cancel", theme, false).on_click(
                                        cx.listener(|this, _, _window, cx| this.close_confirm(cx)),
                                    ),
                                )
                                .child(confirm_button),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn render_scaffold(
    action: MenuAction,
    driver: dbui_app::domain::Driver,
    table: &TableRef,
    columns: &[dbui_app::domain::Column],
) -> String {
    match action {
        MenuAction::CopyCreate => crate::sql_scaffold::create_table(driver, table, columns),
        _ => crate::sql_scaffold::insert_template(driver, table, columns),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_target(kind: TableKind) -> ContextTarget {
        ContextTarget::Table {
            table: TableRef::new("public", "users"),
            kind,
        }
    }

    /// Both engines reject `TRUNCATE` on a view, so the menu must not offer it.
    #[test]
    fn a_view_is_not_offered_truncate() {
        let actions = actions_of(&table_target(TableKind::View));
        assert!(!actions.contains(&MenuAction::Truncate));
        assert!(actions.contains(&MenuAction::Drop));

        let actions = actions_of(&table_target(TableKind::Table));
        assert!(actions.contains(&MenuAction::Truncate));
    }

    /// The keyboard cursor indexes selectable rows, so separators must never
    /// be counted -- otherwise Enter lands on a divider and does nothing.
    #[test]
    fn separators_are_not_selectable() {
        let target = table_target(TableKind::Table);
        let rows = rows_for(&target);
        let separators = rows
            .iter()
            .filter(|row| matches!(row, MenuRow::Separator))
            .count();
        assert!(separators > 0, "the menu is grouped");
        assert_eq!(actions_of(&target).len(), rows.len() - separators);
    }

    #[test]
    fn a_confirmation_arms_only_on_the_exact_name() {
        let mut prompt = ConfirmPrompt::new(
            MenuAction::Drop,
            TableRef::new("public", "users"),
            TableKind::Table,
        );
        assert!(!prompt.armed(), "an empty box must not arm it");
        prompt.input.set_text("user");
        assert!(!prompt.armed());
        prompt.input.set_text("users");
        assert!(prompt.armed());
        // Whitespace is a typo, not a different table.
        prompt.input.set_text("  users  ");
        assert!(prompt.armed());
        // ...but a running action cannot be fired twice.
        prompt.running = true;
        assert!(!prompt.armed());
    }

    /// The grid's menu acts on rows, not on the relation -- offering "Drop
    /// Table" from a right-click on a row is how the wrong thing gets clicked.
    #[test]
    fn the_row_menu_offers_rows_and_nothing_destructive_to_the_table() {
        let actions = actions_of(&ContextTarget::Rows);
        assert!(actions.contains(&MenuAction::CopyRowsTsv));
        assert!(actions.contains(&MenuAction::DeleteRows));
        assert!(!actions.iter().any(|action| action.is_destructive()));
        assert!(!actions.contains(&MenuAction::Drop));
    }

    #[test]
    fn a_schema_menu_has_no_destructive_entries() {
        let target = ContextTarget::Schema {
            connection: ConnectionId::next(),
            name: "public".into(),
        };
        assert!(!actions_of(&target)
            .iter()
            .any(|action| action.is_destructive()));
    }
}
