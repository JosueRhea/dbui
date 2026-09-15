//! The staged-changes panel, above the status bar.
//!
//! Everything waiting for the next commit, as a table: one line per column
//! that will be written, with the row it belongs to, what it says now and what
//! it will say. The free-form diff this replaced read well for one edit and
//! badly for forty -- the eye had nothing to run down, so checking that a bulk
//! edit had touched only the column you meant meant reading every line. Four
//! aligned columns make that one glance.
//!
//! A value that gained or lost lines still gets its line diff, drawn under the
//! row it belongs to: "3 lines became 4" is not something a before/after pair
//! on one line can say.

use super::button;
use crate::root::DbUi;
use crate::tabs::{FieldChange, PendingRowDelete, PendingRowEdit, WorkspaceTab};
use crate::text_diff::{line_diff, DiffLine};
use crate::theme::{metrics, Theme};
use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString};

/// A one-line before/after has to fit its cell, and a diff line has to fit the
/// panel. Past this the tail is dropped -- the panel is a summary, and the
/// detail sidebar holds the full value.
const MAX_LINE_CHARS: usize = 120;

/// Enough to show a small edit in full without the panel swallowing the
/// window. A larger change says how much more there is.
const MAX_DIFF_LINES: usize = 12;

/// Width of the `Row` and `Column` cells. Fixed rather than proportional: they
/// hold identifiers, which are short and roughly all the same length, while
/// the values beside them are whatever the data is.
const KEY_CELL_WIDTH: f32 = 150.;

impl DbUi {
    pub(crate) fn render_change_bubble(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let batch = self.collect_batch_edits();
        let deletes = self.collect_batch_deletes();
        let inserts: Vec<String> = self
            .tabs
            .active()
            .map(|tab| {
                tab.pending_inserts()
                    .iter()
                    .map(|row| row.label())
                    .collect()
            })
            .unwrap_or_default();
        if batch.is_empty() && deletes.is_empty() && inserts.is_empty() {
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
        let count = batch.len() + deletes.len() + inserts.len();
        let mut label = if count == 1 {
            "1 change".to_string()
        } else {
            format!("{count} changes")
        };
        // Deletions are the half of the batch worth naming in the collapsed
        // state: an edit can be re-edited, a delete cannot be un-deleted.
        if !inserts.is_empty() {
            label.push_str(&format!(" · {} new", inserts.len()));
        }
        if !deletes.is_empty() {
            label.push_str(&format!(" · {} to delete", deletes.len()));
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
            button("discard-changes", "Discard", theme, false)
                .on_click(cx.listener(|this, _, _window, cx| this.discard_pending_edits(cx)))
        };

        let save_label = if saving {
            "Committing…"
        } else {
            "Commit  ⌘S"
        };
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
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_change_bubble(cx)))
                        .child(div().text_color(theme.text_muted).child(if expanded {
                            "⌄"
                        } else {
                            "›"
                        }))
                        .child(div().text_color(theme.text).child("Staged changes"))
                        .child(
                            div()
                                .text_size(metrics::text_size_small())
                                .text_color(theme.text_muted)
                                .child(SharedString::from(label)),
                        ),
                )
                .child(discard)
                .child(save),
        );

        if expanded {
            let mut rows: Vec<AnyElement> = Vec::new();
            for label in &inserts {
                rows.push(render_insert_row(label, theme));
            }
            for edit in &batch {
                rows.extend(render_edit_rows(edit, theme));
            }
            for row in &deletes {
                rows.push(render_delete_row(row, theme));
            }

            bubble = bubble.child(
                div()
                    .relative()
                    .w_full()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .h(self.change_bubble_height)
                    .border_t_1()
                    .border_color(theme.divider)
                    .child(table_header(theme))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h(px(0.))
                            .child(
                                div()
                                    .id("change-bubble-details")
                                    .track_scroll(&self.change_bubble_scroll)
                                    .size_full()
                                    .min_w(px(0.))
                                    .overflow_y_scroll()
                                    .flex()
                                    .flex_col()
                                    .children(rows),
                            )
                            .child(super::scrollbar::vertical_scrollbar(
                                "change-bubble-scrollbar",
                                self.change_bubble_scroll.clone(),
                                theme,
                            )),
                    ),
            );
        }

        Some(bubble.into_any_element())
    }
}

/// The panel's top edge: drag it up for more of the batch, down for less.
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

fn table_header(theme: &Theme) -> AnyElement {
    let heading = |label: &'static str, fixed: bool| {
        let cell = div().px_2().truncate();
        if fixed {
            cell.w(metrics::scaled(KEY_CELL_WIDTH)).flex_shrink_0()
        } else {
            cell.flex_1().min_w(px(0.))
        }
        .child(label)
    };

    div()
        .flex()
        .items_center()
        .w_full()
        .h(metrics::scaled(24.))
        .flex_shrink_0()
        .px_3()
        .bg(theme.panel)
        .border_b_1()
        .border_color(theme.divider)
        .text_size(metrics::text_size_small())
        .text_color(theme.text_faint)
        // Lines up with the status dot each row leads with.
        .child(div().w(metrics::scaled(14.)).flex_shrink_0())
        .child(heading("Row", true))
        .child(heading("Column", true))
        .child(heading("From", false))
        .child(heading("To", false))
        .into_any_element()
}

/// One line of the table, tinted by what it is going to do.
struct ChangeRow<'a> {
    tint: gpui::Rgba,
    /// The dot at the head of the line.
    marker: gpui::Rgba,
    row: &'a str,
    column: &'a str,
    from: Option<&'a str>,
    to: &'a str,
    /// What `to` should be drawn in -- the success colour for a value, the
    /// danger colour for a row on its way out.
    to_color: gpui::Rgba,
    strike_from: bool,
}

fn render_row(row: ChangeRow<'_>, theme: &Theme) -> AnyElement {
    let value_cell = |text: Option<&str>, color: gpui::Rgba, strike: bool| {
        div()
            .flex_1()
            .min_w(px(0.))
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_color(color)
            .when(strike, |cell| cell.line_through())
            .child(SharedString::from(
                text.map(one_line).unwrap_or_else(|| "—".to_string()),
            ))
    };

    let key_cell = |text: &str, color: gpui::Rgba| {
        div()
            .w(metrics::scaled(KEY_CELL_WIDTH))
            .flex_shrink_0()
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .text_color(color)
            .child(SharedString::from(one_line(text)))
    };

    div()
        .flex()
        .items_center()
        .w_full()
        .min_w(px(0.))
        .h(metrics::scaled(24.))
        .flex_shrink_0()
        .px_3()
        .bg(row.tint)
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .child(
            div()
                .w(metrics::scaled(14.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .child(super::dot(row.marker)),
        )
        .child(key_cell(row.row, theme.text_muted))
        .child(key_cell(row.column, theme.text))
        .child(value_cell(row.from, theme.danger, row.strike_from))
        .child(value_cell(Some(row.to), row.to_color, false))
        .into_any_element()
}

/// A tint weak enough to read through. The row's own colours carry the
/// meaning; this only groups the line with the ones like it.
fn tint(color: gpui::Rgba) -> gpui::Rgba {
    gpui::Rgba { a: 0.10, ..color }
}

fn render_edit_rows(edit: &PendingRowEdit, theme: &Theme) -> Vec<AnyElement> {
    let mut out = Vec::with_capacity(edit.changes.len());
    for change in &edit.changes {
        out.push(render_row(
            ChangeRow {
                tint: tint(theme.success),
                marker: theme.warning,
                row: &edit.label,
                column: &change.column,
                from: Some(&change.old_text),
                to: &change.new_text,
                to_color: theme.success,
                strike_from: false,
            },
            theme,
        ));
        if let Some(lines) = multiline_diff(change) {
            out.push(render_diff_lines(&lines, theme));
        }
    }
    out
}

/// A staged insert: what little is known about a row that does not exist yet.
fn render_insert_row(label: &str, theme: &Theme) -> AnyElement {
    render_row(
        ChangeRow {
            tint: tint(theme.success),
            marker: theme.success,
            row: label,
            column: "—",
            from: None,
            to: "NEW ROW",
            to_color: theme.success,
            strike_from: false,
        },
        theme,
    )
}

/// A staged deletion: the row's key, struck through, in the removal colour.
fn render_delete_row(row: &PendingRowDelete, theme: &Theme) -> AnyElement {
    render_row(
        ChangeRow {
            tint: tint(theme.danger),
            marker: theme.danger,
            row: &row.label,
            column: "—",
            from: Some(&row.label),
            to: "DELETE ROW",
            to_color: theme.danger,
            strike_from: true,
        },
        theme,
    )
}

/// The line diff for a change that gained or lost lines, if there is one.
fn multiline_diff(change: &FieldChange) -> Option<Vec<DiffLine>> {
    if !change.old_text.contains('\n') && !change.new_text.contains('\n') {
        return None;
    }
    line_diff(&change.old_text, &change.new_text).filter(|lines| !lines.is_empty())
}

fn render_diff_lines(lines: &[DiffLine], theme: &Theme) -> AnyElement {
    let hidden = lines.len().saturating_sub(MAX_DIFF_LINES);

    let mut body = div()
        .w_full()
        .min_w(px(0.))
        .flex()
        .flex_col()
        // Indented past the dot and the two key cells, so the diff reads as
        // belonging to the row above it rather than as more rows.
        .pl(metrics::scaled(KEY_CELL_WIDTH * 2. + 26.))
        .pr_3()
        .py_1()
        .bg(tint(theme.accent))
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
