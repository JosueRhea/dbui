//! Workspace tab bar across the top of the main pane.

use super::{caption, dot};
use super::icons::{sql_icon, table_icon};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{div, prelude::*, AnyElement, Context, SharedString};

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
                    // Staged work the tab is holding, marked where the user
                    // decides which tab to close.
                    .children((changes > 0).then(|| dot(theme.warning)))
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
