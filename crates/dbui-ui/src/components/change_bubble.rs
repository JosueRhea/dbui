//! Floating batch-change bubble above the status bar.

use super::button;
use crate::root::DbUi;
use crate::tabs::{PendingRowEdit, WorkspaceTab};
use crate::theme::metrics;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};

impl DbUi {
    pub(crate) fn render_change_bubble(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let batch = self.collect_batch_edits();
        if batch.is_empty() {
            return None;
        }

        let (expanded, saving) = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                change_bubble_expanded,
                saving,
                ..
            }) => (*change_bubble_expanded, *saving),
            _ => (false, false),
        };
        let theme = &self.theme;
        let count = batch.len();
        let label = if count == 1 {
            "1 change".to_string()
        } else {
            format!("{count} changes")
        };

        let mut bubble = div()
            .id("change-bubble")
            .flex()
            .flex_col()
            .mx_3()
            .mb_2()
            .flex_shrink_0()
            .rounded_lg()
            .bg(theme.elevated)
            .border_1()
            .border_color(theme.border)
            .overflow_hidden();

        let discard = if saving {
            button("discard-changes", "Discard", theme, false)
                .opacity(0.5)
                .cursor_default()
        } else {
            button("discard-changes", "Discard", theme, false).on_click(cx.listener(
                |this, _, _window, cx| this.discard_pending_edits(cx),
            ))
        };

        let save_label = if saving { "Saving…" } else { "Save" };
        let save = if saving {
            button("save-changes", save_label, theme, true)
                .opacity(0.7)
                .cursor_default()
        } else {
            button("save-changes", save_label, theme, true)
                .on_click(cx.listener(|this, _, _window, cx| this.save_pending_edits(cx)))
        };

        bubble = bubble.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .child(
                    div()
                        .id("change-bubble-toggle")
                        .flex()
                        .items_center()
                        .gap_2()
                        .flex_1()
                        .min_w(px(0.))
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.toggle_change_bubble(cx)
                        }))
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(if expanded { "▾" } else { "▸" }),
                        )
                        .child(div().text_color(theme.text).child(SharedString::from(label))),
                )
                .child(discard)
                .child(save),
        );

        if expanded {
            bubble = bubble.child(
                div()
                    .id("change-bubble-details")
                    .max_h(px(180.))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme.divider)
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(batch.iter().map(|edit| render_edit_group(edit, theme))),
            );
        }

        Some(bubble.into_any_element())
    }
}

fn render_edit_group(edit: &PendingRowEdit, theme: &crate::theme::Theme) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(metrics::text_size_small())
                .text_color(theme.text_muted)
                .child(SharedString::from(edit.label.clone())),
        )
        .children(edit.changes.iter().map(|change| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .font_family(metrics::MONO_FONT)
                .text_size(metrics::text_size_small())
                .child(
                    div()
                        .text_color(theme.text)
                        .child(SharedString::from(change.column.clone())),
                )
                .child(div().text_color(theme.text_faint).child(":"))
                .child(
                    div()
                        .text_color(theme.danger)
                        .child(SharedString::from(change.old_text.clone())),
                )
                .child(div().text_color(theme.text_faint).child("→"))
                .child(
                    div()
                        .text_color(theme.success)
                        .child(SharedString::from(change.new_text.clone())),
                )
                .into_any_element()
        }))
        .into_any_element()
}
