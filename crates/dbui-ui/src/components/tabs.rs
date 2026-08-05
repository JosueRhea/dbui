//! Workspace tab bar across the top of the main pane.

use super::caption;
use super::icons::{sql_icon, table_icon};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{div, prelude::*, AnyElement, Context, SharedString};

impl DbUi {
    pub(crate) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let active = self.tabs.active;

        let tabs: Vec<AnyElement> = self
            .tabs
            .items
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = index == active;
                let label = tab.label();
                let icon_color = if is_active {
                    theme.text_muted
                } else {
                    theme.text_faint
                };
                let icon = match tab {
                    WorkspaceTab::Table { .. } => table_icon(icon_color).into_any_element(),
                    WorkspaceTab::Sql { .. } => sql_icon(icon_color).into_any_element(),
                };

                div()
                    .id(("workspace-tab", index))
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
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.activate_tab(index, cx);
                    }))
                    .child(icon)
                    .child(SharedString::from(label))
                    .child(
                        div()
                            .id(("workspace-tab-close", index))
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

        if tabs.is_empty() {
            return div()
                .id("workspace-tab-bar-empty")
                .flex()
                .items_center()
                .px_3()
                .h(metrics::toolbar_height())
                .flex_shrink_0()
                .bg(theme.panel)
                .border_b_1()
                .border_color(theme.border)
                .child(caption("No tabs open", theme));
        }

        div()
            .flex()
            .items_center()
            .h(metrics::toolbar_height())
            .flex_shrink_0()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .id("workspace-tab-bar")
            .overflow_x_scroll()
            .children(tabs)
    }
}
