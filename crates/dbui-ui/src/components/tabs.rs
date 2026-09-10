//! Workspace tab bar across the top of the main pane.

use super::context_menu::ContextTarget;
use super::icons::{sql_icon, table_icon};
use super::{button, caption, dot};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{
    div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, MouseMoveEvent,
    SharedString,
};

impl DbUi {
    pub(crate) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.active;

        // The front tab's count comes the long way round so the dot and the
        // change bubble can never disagree: the bubble folds the open draft in
        // before counting, and a tab whose dot lit up only after the selection
        // moved would be a tab that looked clean while the bubble said
        // otherwise. Every other tab has already had its draft folded away.
        let active_changes = self.collect_batch_edits().len()
            + self.collect_batch_deletes().len()
            + self
                .tabs
                .active()
                .map(|tab| tab.pending_inserts().len())
                .unwrap_or(0);
        let theme = &self.theme;

        let tabs: Vec<AnyElement> = self
            .tabs
            .items
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = index == active;
                let id = tab.id();
                let label = tab.label();
                let changes = if is_active {
                    active_changes
                } else {
                    tab.pending_change_count()
                };
                let icon_color = if is_active {
                    theme.text_muted
                } else {
                    theme.text_faint
                };
                let icon = match tab {
                    WorkspaceTab::Table { .. } => table_icon(icon_color).into_any_element(),
                    WorkspaceTab::Sql { .. } => sql_icon(icon_color).into_any_element(),
                };

                let dragging = self
                    .tab_drag
                    .as_ref()
                    .is_some_and(|drag| drag.id == id && drag.moved);

                div()
                    // Keyed on the tab, not the slot: dragging renumbers the
                    // slots, and an id that moved with them would hand the
                    // press and the release to two different elements.
                    .id(("workspace-tab", id as usize))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h_full()
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if is_active {
                        theme.accent
                    } else {
                        gpui::rgba(0x00000000)
                    })
                    .text_color(if is_active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .hover(|row| row.bg(theme.hover))
                    // The tab in hand is lifted off the strip, so it is clear
                    // which one the rest are making room for.
                    .when(dragging, |row| row.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.activate_tab(index, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.begin_tab_drag(id, event.position.x);
                            cx.notify();
                        }),
                    )
                    // Crossing another tab with one in hand is what reorders
                    // the strip; the tab follows the pointer a slot at a time
                    // rather than being dropped somewhere at the end.
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                            this.drag_tab_over(index, event.position.x, cx);
                        }),
                    )
                    // Right-clicking a tab does not move the focus to it: the
                    // menu names the tab it was opened on, so pointing at one
                    // to close the rest must not first make it the one kept
                    // by accident.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                            this.open_context_menu(ContextTarget::Tab { id }, event.position, cx);
                        }),
                    )
                    .child(icon)
                    .child(SharedString::from(label))
                    // Staged work the tab is holding, marked where the user
                    // decides which tab to close.
                    .children((changes > 0).then(|| dot(theme.warning)))
                    .child(
                        div()
                            .id(("workspace-tab-close", id as usize))
                            .px_1()
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .hover(|icon| icon.text_color(theme.danger))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, cx);
                            }))
                            .child("×"),
                    )
                    .into_any_element()
            })
            .collect();

        // The strip scrolls; the refresh button does not. Keeping it pinned
        // outside the scroller is what makes it reachable no matter how many
        // tabs are open -- a reload the user has to scroll to find is a
        // reload they will reach for the keyboard instead.
        let strip = if tabs.is_empty() {
            div()
                .id("workspace-tab-bar-empty")
                .flex()
                .items_center()
                .px_3()
                .h_full()
                .flex_1()
                .min_w(px(0.))
                .child(caption("No tabs open", theme))
        } else {
            div()
                .id("workspace-tab-bar")
                .flex()
                .items_center()
                .h_full()
                .flex_1()
                .min_w(px(0.))
                .overflow_x_scroll()
                .children(tabs)
        };

        div()
            .flex()
            .items_center()
            .h(metrics::toolbar_height())
            .flex_shrink_0()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .child(strip)
            .child(
                div().flex_shrink_0().px_2().child(
                    button("refresh-result", "\u{21bb}", theme, false)
                        .px_2()
                        .on_click(cx.listener(|this, _, _window, cx| this.refresh_result(cx))),
                ),
            )
    }
}
