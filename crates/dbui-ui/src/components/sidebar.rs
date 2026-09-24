//! The left rail: schema / table tree for the active connection.
//!
//! Connections are chosen from the titlebar picker; this surface only walks
//! the catalog of whatever is currently connected.

use super::context_menu::ContextTarget;
use super::{caption, motion};
use crate::root::{DbUi, Focus, SidebarItem};
use crate::theme::metrics;
use dbui_app::domain::{ConnectionId, ObjectKind};
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
            // Functions, triggers and the rest, each kind folded into a group
            // under the tables. Not while filtering: the filter is for tables,
            // and a group that matched nothing would only be noise.
            if query.is_empty() {
                for kind in ObjectKind::ALL {
                    let objects: Vec<_> = catalog.objects_of(&schema.name, kind).collect();
                    if objects.is_empty() {
                        continue;
                    }
                    items.push(SidebarItem::Group {
                        connection: id,
                        schema: schema.name.clone(),
                        kind,
                    });
                    if entry.is_expanded(&SidebarItem::group_key(&schema.name, kind)) {
                        items.extend(objects.into_iter().map(|object| SidebarItem::Object {
                            connection: id,
                            object: object.clone(),
                        }));
                    }
                }
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
        self.reveal_sidebar_cursor();
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
            SidebarItem::Group {
                connection,
                schema,
                kind,
            } => {
                self.toggle_schema(connection, &SidebarItem::group_key(&schema, kind), cx);
            }
            SidebarItem::Object { object, .. } => {
                self.open_definition(object, cx);
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
                    self.reveal_sidebar_cursor();
                    cx.notify();
                } else if !expand && is_expanded {
                    self.toggle_schema(connection, &name, cx);
                }
            }
            SidebarItem::Group {
                connection,
                ref schema,
                kind,
            } => {
                let key = SidebarItem::group_key(schema, kind);
                let is_expanded = self
                    .workspace
                    .get(connection)
                    .is_some_and(|entry| entry.is_expanded(&key));
                if expand != is_expanded {
                    self.toggle_schema(connection, &key, cx);
                }
            }
            // Left from an object goes up to its group, the way it goes from
            // a table to its schema.
            SidebarItem::Object { connection, .. } => {
                if !expand {
                    let items = self.sidebar_visible_items();
                    if let Some(pos) = items.iter().position(|i| i == &item) {
                        if let Some(group) = items[..pos].iter().rev().find(
                            |i| matches!(i, SidebarItem::Group { connection: c, .. } if *c == connection),
                        ) {
                            self.sidebar_cursor = Some(group.clone());
                            self.reveal_sidebar_cursor();
                            cx.notify();
                        }
                    }
                }
            }
            SidebarItem::Table { connection, .. } => {
                if !expand {
                    let items = self.sidebar_visible_items();
                    if let Some(pos) = items.iter().position(|i| i == &item) {
                        for i in (0..pos).rev() {
                            if let SidebarItem::Schema { connection: c, .. } = &items[i] {
                                if *c == connection {
                                    self.sidebar_cursor = Some(items[i].clone());
                                    self.reveal_sidebar_cursor();
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
        self.finish_cell_edit(cx);
        self.sidebar_cursor = Some(item);
        self.focus = Focus::Sidebar;
        self.reveal_sidebar_cursor();
        cx.notify();
    }

    /// The strip between the schema tree and the grid.
    pub(crate) fn render_sidebar_resize(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        super::vertical_resize_handle("sidebar-resize", self.sidebar_drag.is_some(), &self.theme)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.begin_sidebar_drag(event.position.x, cx);
                }),
            )
    }

    pub(crate) fn render_sidebar(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;

        div()
            .w(px(self.sidebar_width * metrics::zoom()))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .when(!self.glass, |rail| {
                rail.bg(theme.panel).border_r_1().border_color(theme.border)
            })
            // The database name and its reload used to head this rail. They
            // are in the titlebar now, beside the connection they belong to,
            // which is what lets the tree start at the top of the panel.
            .child(self.render_sidebar_filter(cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("sidebar-scroll")
                            .track_scroll(&self.sidebar_scroll)
                            .size_full()
                            .min_h(px(0.))
                            .overflow_y_scroll()
                            .py_1()
                            .children(self.render_sidebar_body(cx)),
                    )
                    .child(super::scrollbar::vertical_scrollbar(
                        "sidebar-scrollbar",
                        self.sidebar_scroll.clone(),
                        &self.theme,
                    )),
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
        let chrome = self.chrome_theme();
        let theme = &chrome;

        div()
            .flex()
            .items_center()
            .gap_1()
            .px_2()
            .py_1p5()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(super::text_field::marked_text_field(
                        "sidebar-filter",
                        &self.sidebar_filter,
                        super::text_field::InputTarget::SidebarFilter,
                        focused,
                        Some("Search tables  ⌘⇧F"),
                        super::icons::search_icon(theme.text_faint).into_any_element(),
                        theme,
                        cx,
                    )),
            )
            .when(has_text, |strip| {
                strip.child(
                    div()
                        .id("sidebar-filter-clear")
                        .w(metrics::scaled(20.))
                        .h(metrics::scaled(20.))
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
        let chrome = self.chrome_theme();
        let theme = &chrome;

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
                .child(caption(
                    "Select the connection in the titlebar to connect.",
                    theme,
                ))
                .into_any_element()];
        }

        self.render_tree(id, cx)
    }

    /// The schemas and tables of one connection.
    fn render_tree(&self, id: ConnectionId, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let chrome = self.chrome_theme();
        let theme = &chrome;
        let cursor = self.sidebar_cursor.clone();
        // The cursor is only drawn while the tree owns the keyboard. Left
        // showing at all times it is a second highlight competing with the
        // open table's, and the arrow keys it belongs to are going somewhere
        // else entirely.
        let cursor_shown = self.focus == Focus::Sidebar;
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

        let active_table = self.tabs.active().and_then(|tab| tab.table_ref().cloned());
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
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.hover))
                    .children((is_cursor && cursor_shown).then(|| cursor_marker(theme)))
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
                            .w(metrics::scaled(12.))
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

                let row = div()
                    .id(("table", schema_index * 10_000 + table_index))
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .pl(metrics::scaled(28.))
                    .pr_3()
                    .py_1()
                    .cursor_pointer()
                    .when(is_open, |row| row.bg(theme.selection))
                    .hover(|row| row.bg(theme.hover))
                    .children((is_cursor && cursor_shown).then(|| cursor_marker(theme)))
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
                    );
                // Unfolding a schema pours its tables in. Not while filtering:
                // every keystroke reshuffles which rows are drawn, and a tree
                // that re-fades under each letter is one that flickers.
                if query.is_empty() {
                    rows.push(
                        motion::cascade(
                            ("table-in", schema_index * 10_000 + table_index),
                            row,
                            table_index,
                        )
                        .into_any_element(),
                    );
                } else {
                    rows.push(row.into_any_element());
                }
            }

            if query.is_empty() {
                rows.extend(self.render_object_groups(id, schema_index, &schema.name, cx));
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

impl DbUi {
    /// The folded groups of functions, triggers... under one schema's
    /// tables, and the objects of the groups that are open. Has to agree
    /// with `sidebar_visible_items`, like the rest of the tree.
    fn render_object_groups(
        &self,
        id: ConnectionId,
        schema_index: usize,
        schema: &str,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let chrome = self.chrome_theme();
        let theme = &chrome;
        let cursor_shown = self.focus == Focus::Sidebar;
        let Some(entry) = self.workspace.get(id) else {
            return Vec::new();
        };
        let Some(catalog) = entry.catalog.as_ref() else {
            return Vec::new();
        };
        let mut rows = Vec::new();

        for kind in ObjectKind::ALL {
            let objects: Vec<_> = catalog.objects_of(schema, kind).collect();
            if objects.is_empty() {
                continue;
            }
            let expanded = entry.is_expanded(&SidebarItem::group_key(schema, kind));
            let group = SidebarItem::Group {
                connection: id,
                schema: schema.to_string(),
                kind,
            };
            let is_cursor = self.sidebar_cursor.as_ref() == Some(&group);
            let row_key = schema_index * 16 + kind as usize;
            rows.push(
                div()
                    .id(("object-group", row_key))
                    .relative()
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl(metrics::scaled(24.))
                    .pr_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.hover))
                    .children((is_cursor && cursor_shown).then(|| cursor_marker(theme)))
                    .on_click(cx.listener({
                        let group = group.clone();
                        let key = SidebarItem::group_key(schema, kind);
                        move |this, _, _window, cx| {
                            this.set_sidebar_cursor(group.clone(), cx);
                            this.toggle_schema(id, &key, cx);
                        }
                    }))
                    .child(
                        div()
                            .w(metrics::scaled(12.))
                            .text_color(theme.text_faint)
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text_muted)
                            .child(kind.plural()),
                    )
                    .child(caption(objects.len().to_string(), theme))
                    .into_any_element(),
            );
            if !expanded {
                continue;
            }
            for (object_index, object) in objects.into_iter().enumerate() {
                let item = SidebarItem::Object {
                    connection: id,
                    object: object.clone(),
                };
                let is_cursor = self.sidebar_cursor.as_ref() == Some(&item);
                rows.push(
                    div()
                        .id(("object", row_key * 100_000 + object_index))
                        .relative()
                        .flex()
                        .items_center()
                        .gap_2()
                        .pl(metrics::scaled(42.))
                        .pr_3()
                        .py_1()
                        .cursor_pointer()
                        .hover(|row| row.bg(theme.hover))
                        .children((is_cursor && cursor_shown).then(|| cursor_marker(theme)))
                        .on_click(cx.listener({
                            let item = item.clone();
                            let object = object.clone();
                            move |this, _, _window, cx| {
                                this.set_sidebar_cursor(item.clone(), cx);
                                this.open_definition(object.clone(), cx);
                            }
                        }))
                        .child(
                            div()
                                .flex_shrink_0()
                                .w(metrics::scaled(18.))
                                .text_size(metrics::scaled(9.))
                                .text_color(theme.value_structured)
                                .child(object_badge(kind)),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_color(theme.text)
                                .child(SharedString::from(object.name.clone())),
                        )
                        .children(object.detail.clone().map(|detail| {
                            div()
                                .min_w(px(0.))
                                .truncate()
                                .text_size(metrics::scaled(11.))
                                .text_color(theme.text_faint)
                                .child(SharedString::from(detail))
                        }))
                        .into_any_element(),
                );
            }
        }
        rows
    }
}

/// Two letters for an object's kind, where a table row has its icon.
fn object_badge(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::Function => "FN",
        ObjectKind::Procedure => "PR",
        ObjectKind::Trigger => "TG",
        ObjectKind::Sequence => "SQ",
        ObjectKind::Type => "TY",
        ObjectKind::Extension => "EX",
    }
}

/// Where the arrow keys are, drawn as an edge rather than a fill.
///
/// A fill would be a second selection: the tree already fills the row of the
/// table on screen, and the two are different rows as soon as a tab is open.
/// An edge marker sits alongside that instead of arguing with it -- and a row
/// that is both reads as both.
fn cursor_marker(theme: &crate::theme::Theme) -> AnyElement {
    div()
        .absolute()
        .left_0()
        .top_0()
        .bottom_0()
        .w(px(2.))
        .bg(theme.accent)
        .into_any_element()
}
