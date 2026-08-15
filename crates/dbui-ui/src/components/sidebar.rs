//! The left rail: schema / table tree for the active connection.
//!
//! Connections are chosen from the titlebar picker; this surface only walks
//! the catalog of whatever is currently connected.

use super::context_menu::ContextTarget;
use super::{button, caption};
use crate::root::{DbUi, Focus, SidebarItem};
use crate::theme::metrics;
use dbui_app::domain::ConnectionId;
use gpui::{
    div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString, Window,
};

impl DbUi {
    /// Every row the tree is drawing, in order.
    ///
    /// This and [`DbUi::render_tree`] must agree exactly: the arrow keys walk
    /// this list and the pointer clicks that one, and a cursor that can land
    /// on a row nobody drew is a cursor that vanishes.
    pub(crate) fn sidebar_visible_items(&self) -> Vec<SidebarItem> {
        let mut items = Vec::new();
        let Some(id) = self.workspace.active_id() else {
            return items;
        };
        let Some(entry) = self.workspace.get(id) else {
            return items;
        };
        if !entry.status.is_connected() {
            return items;
        }
        let Some(catalog) = entry.catalog.as_ref() else {
            return items;
        };
        let query = self.sidebar_query();

        for schema in &catalog.schemas {
            let matches: Vec<_> = schema
                .tables
                .iter()
                .filter(|table| self.table_matches_filter(table, &query))
                .collect();
            // While filtering, a schema with nothing in it is noise.
            if !query.is_empty() && matches.is_empty() {
                continue;
            }

            items.push(SidebarItem::Schema {
                connection: id,
                name: schema.name.clone(),
            });
            // A filter overrides the folds: the point of typing is to see the
            // matches, not to be told which folders to open next.
            if query.is_empty() && !entry.is_expanded(&schema.name) {
                continue;
            }
            for table in matches {
                items.push(SidebarItem::Table {
                    connection: id,
                    table: table.reference(),
                });
            }
        }
        items
    }

    pub(crate) fn sidebar_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let items = self.sidebar_visible_items();
        if items.is_empty() {
            return;
        }
        let current = self
            .sidebar_cursor
            .as_ref()
            .and_then(|c| items.iter().position(|i| i == c))
            .unwrap_or(0);
        let next = if delta < 0 {
            if current == 0 {
                items.len() - 1
            } else {
                current - 1
            }
        } else {
            (current + 1) % items.len()
        };
        self.sidebar_cursor = Some(items[next].clone());
        self.focus = Focus::Sidebar;
        cx.notify();
    }

    pub(crate) fn sidebar_activate(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.sidebar_cursor.clone() else {
            let items = self.sidebar_visible_items();
            self.sidebar_cursor = items.into_iter().next();
            cx.notify();
            return;
        };
        match item {
            SidebarItem::Schema { connection, name } => {
                self.toggle_schema(connection, &name, cx);
            }
            SidebarItem::Table { table, .. } => {
                self.open_table_tab(table, cx);
            }
        }
    }

    pub(crate) fn sidebar_expand(&mut self, expand: bool, cx: &mut Context<Self>) {
        let Some(item) = self.sidebar_cursor.clone() else {
            return;
        };
        match item {
            SidebarItem::Schema { connection, name } => {
                let is_expanded = self
                    .workspace
                    .get(connection)
                    .map(|e| e.is_expanded(&name))
                    .unwrap_or(false);
                if expand && !is_expanded {
                    self.toggle_schema(connection, &name, cx);
                    let items = self.sidebar_visible_items();
                    if let Some(pos) = items.iter().position(|i| {
                        matches!(
                            i,
                            SidebarItem::Schema {
                                connection: c,
                                name: n
                            } if *c == connection && n == &name
                        )
                    }) {
                        if let Some(child) = items.get(pos + 1) {
                            if matches!(
                                child,
                                SidebarItem::Table { connection: c, .. } if *c == connection
                            ) {
                                self.sidebar_cursor = Some(child.clone());
                            }
                        }
                    }
                    cx.notify();
                } else if !expand && is_expanded {
                    self.toggle_schema(connection, &name, cx);
                }
            }
            SidebarItem::Table { connection, .. } => {
                if !expand {
                    let items = self.sidebar_visible_items();
                    if let Some(pos) = items.iter().position(|i| i == &item) {
                        for i in (0..pos).rev() {
                            if let SidebarItem::Schema {
                                connection: c, ..
                            } = &items[i]
                            {
                                if *c == connection {
                                    self.sidebar_cursor = Some(items[i].clone());
                                    cx.notify();
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn set_sidebar_cursor(&mut self, item: SidebarItem, cx: &mut Context<Self>) {
        self.sidebar_cursor = Some(item);
        self.focus = Focus::Sidebar;
        cx.notify();
    }

    pub(crate) fn render_sidebar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let header = match self.workspace.active() {
            Some(entry) if entry.status.is_connected() => {
                SharedString::from(entry.config.database.clone())
            }
            Some(entry) => SharedString::from(entry.config.name.clone()),
            None => SharedString::from("Database"),
        };

        div()
            .w(metrics::sidebar_width())
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.panel)
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .h(metrics::toolbar_height())
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.divider)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .truncate()
                            .text_size(px(11.))
                            .text_color(theme.text_faint)
                            .child(header),
                    )
                    .child(
                        button("refresh-catalog", "↻", theme, false)
                            .px_2()
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.refresh_catalog(cx)
                            })),
                    ),
            )
            .child(self.render_sidebar_filter(cx))
            .child(
                div()
                    .id("sidebar-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .py_1()
                    .children(self.render_sidebar_body(cx)),
            )
    }

    /// The table filter. Hidden until there is a catalog to filter.
    fn render_sidebar_filter(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let connected = self
            .workspace
            .active()
            .is_some_and(|entry| entry.status.is_connected());
        if !connected {
            return div().into_any_element();
        }

        let focused = self.focus == Focus::SidebarSearch;
        let has_text = !self.sidebar_filter.is_empty();
        let theme = &self.theme;

        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(super::text_field::text_field(
                        "sidebar-filter",
                        &self.sidebar_filter,
                        super::text_field::InputTarget::SidebarFilter,
                        focused,
                        Some("Search tables  ⌘⇧F"),
                        theme,
                        cx,
                    )),
            )
            .when(has_text, |strip| {
                strip.child(
                    div()
                        .id("sidebar-filter-clear")
                        .w(px(20.))
                        .h(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .text_color(theme.text_faint)
                        .hover(|style| style.bg(theme.hover).text_color(theme.text))
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.clear_sidebar_filter(cx);
                        }))
                        .child("×"),
                )
            })
            .into_any_element()
    }

    fn render_sidebar_body(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;

        if self.workspace.is_empty() {
            return vec![div()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child("No connections yet"),
                )
                .child(caption("Press ⌘N or use the titlebar picker.", theme))
                .into_any_element()];
        }

        let Some(id) = self.workspace.active_id() else {
            return vec![div()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child("No connection selected"),
                )
                .child(caption("Pick one from the titlebar.", theme))
                .into_any_element()];
        };

        let Some(entry) = self.workspace.get(id) else {
            return Vec::new();
        };

        if !entry.status.is_connected() {
            let message = match &entry.status {
                dbui_app::ConnectionStatus::Connecting => "Connecting…",
                dbui_app::ConnectionStatus::Failed(err) => err.as_str(),
                _ => "Not connected",
            };
            return vec![div()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(theme.text_muted)
                        .child(SharedString::from(message.to_string())),
                )
                .child(caption("Select the connection in the titlebar to connect.", theme))
                .into_any_element()];
        }

        self.render_tree(id, cx)
    }

    /// The schemas and tables of one connection.
    fn render_tree(&self, id: ConnectionId, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = &self.theme;
        let cursor = self.sidebar_cursor.clone();
        let Some(entry) = self.workspace.get(id) else {
            return Vec::new();
        };
        let Some(catalog) = entry.catalog.as_ref() else {
            return vec![div()
                .p_4()
                .child(caption("Loading catalog…", theme))
                .into_any_element()];
        };

        if catalog.schemas.is_empty() {
            return vec![div()
                .px_3()
                .py_1()
                .child(caption("No schemas visible to this user", theme))
                .into_any_element()];
        }

        let active_table = self
            .tabs
            .active()
            .and_then(|tab| tab.table_ref().cloned());
        let query = self.sidebar_query();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut matched = 0usize;

        for (schema_index, schema) in catalog.schemas.iter().enumerate() {
            let tables: Vec<&dbui_app::domain::Table> = schema
                .tables
                .iter()
                .filter(|table| self.table_matches_filter(table, &query))
                .collect();
            if !query.is_empty() && tables.is_empty() {
                continue;
            }
            matched += tables.len();

            // Filtering unfolds every schema it kept -- see the matching note
            // in `sidebar_visible_items`, which this has to agree with.
            let expanded = query.is_empty() && entry.is_expanded(&schema.name) || !query.is_empty();
            let name = schema.name.clone();
            let schema_item = SidebarItem::Schema {
                connection: id,
                name: name.clone(),
            };
            let is_cursor = cursor.as_ref() == Some(&schema_item);

            rows.push(
                div()
                    .id(("schema", schema_index))
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .when(is_cursor, |row| row.bg(theme.selection))
                    .hover(|row| row.bg(theme.hover))
                    .on_click(cx.listener({
                        let name = name.clone();
                        move |this, _, _window, cx| {
                            this.set_sidebar_cursor(
                                SidebarItem::Schema {
                                    connection: id,
                                    name: name.clone(),
                                },
                                cx,
                            );
                            this.toggle_schema(id, &name, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let name = name.clone();
                            move |this, event: &MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                                this.open_context_menu(
                                    ContextTarget::Schema {
                                        connection: id,
                                        name: name.clone(),
                                    },
                                    event.position,
                                    cx,
                                );
                            }
                        }),
                    )
                    .child(
                        div()
                            .w(px(12.))
                            .text_color(theme.text_faint)
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(schema.name.clone())),
                    )
                    .child(caption(tables.len().to_string(), theme))
                    .into_any_element(),
            );

            if !expanded {
                continue;
            }

            for (table_index, table) in tables.iter().enumerate() {
                let reference = table.reference();
                let is_open = active_table.as_ref() == Some(&reference);
                let target = reference.clone();
                let table_item = SidebarItem::Table {
                    connection: id,
                    table: reference.clone(),
                };
                let is_cursor = cursor.as_ref() == Some(&table_item);

                rows.push(
                    div()
                        .id(("table", schema_index * 10_000 + table_index))
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl(px(28.))
                        .pr_3()
                        .py_1()
                        .cursor_pointer()
                        .when(is_open || is_cursor, |row| row.bg(theme.selection))
                        .hover(|row| row.bg(theme.hover))
                        .on_click(cx.listener({
                            let target = target.clone();
                            move |this, _, _window, cx| {
                                this.set_sidebar_cursor(
                                    SidebarItem::Table {
                                        connection: id,
                                        table: target.clone(),
                                    },
                                    cx,
                                );
                                this.open_table_tab(target.clone(), cx);
                            }
                        }))
                        // Right-click moves the cursor too: the menu names one
                        // table, and the tree has to show which.
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener({
                                let target = target.clone();
                                let kind = table.kind;
                                move |this, event: &MouseDownEvent, _window, cx| {
                                    cx.stop_propagation();
                                    this.set_sidebar_cursor(
                                        SidebarItem::Table {
                                            connection: id,
                                            table: target.clone(),
                                        },
                                        cx,
                                    );
                                    this.open_context_menu(
                                        ContextTarget::Table {
                                            table: target.clone(),
                                            kind,
                                        },
                                        event.position,
                                        cx,
                                    );
                                }
                            }),
                        )
                        .child(super::icons::kind_icon(
                            table.kind,
                            if table.kind.is_view() {
                                theme.value_structured
                            } else {
                                theme.text_faint
                            },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .text_color(theme.text)
                                .child(SharedString::from(table.name.clone())),
                        )
                        .into_any_element(),
                );
            }
        }

        // A filter that found nothing has to say so: an empty tree otherwise
        // reads as a connection that lost its catalog.
        if !query.is_empty() && matched == 0 {
            return vec![div()
                .px_3()
                .py_2()
                .child(caption(
                    format!("No tables matching “{}”", self.sidebar_filter.text().trim()),
                    theme,
                ))
                .into_any_element()];
        }

        rows
    }
}
