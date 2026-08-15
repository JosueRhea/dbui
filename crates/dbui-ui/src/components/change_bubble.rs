//! Floating batch-change bubble above the status bar.

use super::button;
use crate::root::DbUi;
use crate::tabs::{FieldChange, PendingRowDelete, PendingRowEdit, WorkspaceTab};
use crate::text_diff::{line_diff, DiffLine};
use crate::theme::{metrics, Theme};
use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString};

/// A one-line before/after has to fit beside its column name, and a diff line
/// has to fit the bubble. Past this the tail is dropped -- the bubble is a
/// summary, and the detail sidebar holds the full value.
const MAX_LINE_CHARS: usize = 120;

/// Enough to show a small edit in full without the bubble swallowing the
/// window. A larger change says how much more there is.
const MAX_DIFF_LINES: usize = 12;

impl DbUi {
    pub(crate) fn render_change_bubble(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let batch = self.collect_batch_edits();
        let deletes = self.collect_batch_deletes();
        if batch.is_empty() && deletes.is_empty() {
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
        let count = batch.len() + deletes.len();
        let mut label = if count == 1 {
            "1 change".to_string()
        } else {
            format!("{count} changes")
        };
        // Deletions are the half of the batch worth naming in the collapsed
        // state: an edit can be re-edited, a delete cannot be un-deleted.
        if !deletes.is_empty() {
            label.push_str(&format!(
                " · {} to delete",
                deletes.len()
            ));
        }

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

        let save_label = if saving { "Committing…" } else { "Commit  ⌘S" };
        let save = if saving {
            button("save-changes", save_label, theme, true)
                .opacity(0.7)
                .cursor_default()
        } else {
            button("save-changes", save_label, theme, true)
                .on_click(cx.listener(|this, _, _window, cx| this.save_pending_edits(cx)))
        };

        if expanded {
            bubble = bubble.child(resize_handle(self.change_bubble_drag.is_some(), theme, cx));
        }

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
                    .w_full()
                    .min_w(px(0.))
                    .h(self.change_bubble_height)
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme.divider)
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(batch.iter().map(|edit| render_edit_group(edit, theme)))
                    .children(deletes.iter().map(|row| render_delete_row(row, theme))),
            );
        }

        Some(bubble.into_any_element())
    }
}

/// The bubble's top edge: drag it up for more diff, down for less.
///
/// Only the pointer-down lives here. Once the drag starts the pointer is off
/// this 5px strip immediately, so the root view owns the move and release.
fn resize_handle(dragging: bool, theme: &Theme, cx: &mut Context<DbUi>) -> AnyElement {
    div()
        .id("change-bubble-resize")
        .w_full()
        .h(px(5.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .cursor_row_resize()
        .when(dragging, |strip| strip.bg(theme.accent))
        .hover(|strip| strip.bg(theme.hover))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                this.begin_change_bubble_drag(event.position.y, cx);
            }),
        )
        .child(
            div()
                .w(px(28.))
                .h(px(2.))
                .rounded_full()
                .bg(theme.text_faint),
        )
        .into_any_element()
}

fn render_edit_group(edit: &PendingRowEdit, theme: &Theme) -> AnyElement {
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
        .children(
            edit.changes
                .iter()
                .map(|change| render_change(change, theme)),
        )
        .into_any_element()
}

/// A staged deletion: the row's key, struck through, in the removal colour.
fn render_delete_row(row: &PendingRowDelete, theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .min_w(px(0.))
        .flex()
        .items_center()
        .gap_2()
        .overflow_hidden()
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .text_color(theme.danger)
        .child(div().flex_shrink_0().child("−"))
        .child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .text_ellipsis()
                .line_through()
                .child(SharedString::from(one_line(&row.label))),
        )
        .child(div().flex_shrink_0().child("DELETE ROW"))
        .into_any_element()
}

fn render_change(change: &FieldChange, theme: &Theme) -> AnyElement {
    let multiline = change.old_text.contains('\n') || change.new_text.contains('\n');
    let diff = if multiline {
        line_diff(&change.old_text, &change.new_text)
    } else {
        None
    };

    match diff {
        Some(lines) if !lines.is_empty() => div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_1()
            .child(column_label(&change.column, theme))
            .child(render_diff_lines(&lines, theme))
            .into_any_element(),
        // A scalar, or a document too large to diff: the old inline form, but
        // flattened onto one line so a stray newline cannot grow the row.
        _ => div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .gap_2()
            .overflow_hidden()
            .font_family(metrics::MONO_FONT)
            .text_size(metrics::text_size_small())
            .child(column_label(&change.column, theme))
            .child(div().text_color(theme.text_faint).child(":"))
            .child(
                div()
                    .text_color(theme.danger)
                    .child(SharedString::from(one_line(&change.old_text))),
            )
            .child(div().text_color(theme.text_faint).child("→"))
            .child(
                div()
                    .text_color(theme.success)
                    .child(SharedString::from(one_line(&change.new_text))),
            )
            .into_any_element(),
    }
}

fn column_label(column: &str, theme: &Theme) -> AnyElement {
    div()
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .text_color(theme.text)
        .child(SharedString::from(column.to_string()))
        .into_any_element()
}

fn render_diff_lines(lines: &[DiffLine], theme: &Theme) -> AnyElement {
    let hidden = lines.len().saturating_sub(MAX_DIFF_LINES);

    let mut body = div()
        .w_full()
        .min_w(px(0.))
        .flex()
        .flex_col()
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .children(lines.iter().take(MAX_DIFF_LINES).map(|line| {
            let (marker, text, color) = match line {
                DiffLine::Removed(text) => ("-", text, theme.danger),
                DiffLine::Added(text) => ("+", text, theme.success),
            };
            div()
                .w_full()
                .min_w(px(0.))
                .flex()
                .gap_1()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_color(color)
                .child(div().flex_shrink_0().child(marker))
                .child(
                    div()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(SharedString::from(one_line(text))),
                )
        }));

    if hidden > 0 {
        body = body.child(
            div()
                .text_color(theme.text_faint)
                .child(SharedString::from(format!("… {hidden} more line(s)"))),
        );
    }

    body.into_any_element()
}

/// Collapse a value onto one line and cap it, the way the grid renders a cell:
/// a newline here would paint over the row below it.
fn one_line(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.chars().count() >= MAX_LINE_CHARS {
            out.push('…');
            return out;
        }
        match ch {
            '\n' => out.push('⏎'),
            '\t' => out.push(' '),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}
