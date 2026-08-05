//! The left rail: schema / table tree for the active connection.
//!
//! Connections are chosen from the titlebar picker; this surface only walks
//! the catalog of whatever is currently connected.

use super::{button, caption};
use crate::root::{DbUi, Focus, SidebarItem};
use crate::theme::metrics;
use dbui_app::domain::ConnectionId;
use gpui::{
    div, prelude::*, px, AnyElement, Context, SharedString, Window,
};

impl DbUi {
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
        for schema in &catalog.schemas {
            items.push(SidebarItem::Schema {
                connection: id,
                name: schema.name.clone(),
            });
            if !entry.is_expanded(&schema.name) {
                continue;
            }
            for table in &schema.tables {
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
        let mut rows: Vec<AnyElement> = Vec::new();

        for (schema_index, schema) in catalog.schemas.iter().enumerate() {
            let expanded = entry.is_expanded(&schema.name);
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
                    .child(caption(schema.tables.len().to_string(), theme))
                    .into_any_element(),
            );

            if !expanded {
                continue;
            }

            for (table_index, table) in schema.tables.iter().enumerate() {
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

        rows
    }
}
