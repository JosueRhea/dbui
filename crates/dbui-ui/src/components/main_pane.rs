//! The center column: tab bar, filters, content, and bottom bar.

use super::{button, caption};
use crate::root::{DbUi, Focus};
use crate::tabs::{TablePane, WorkspaceTab};
use crate::text_input::{self, selection_on_line};
use crate::theme::metrics;
use gpui::{
    canvas, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, SharedString, Window,
};

impl DbUi {
    pub(crate) fn render_main(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex_1()
            .min_w(px(0.))
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .child(self.render_tab_bar(cx))
            .child(self.render_filter_strip(cx))
            .child(self.render_columns_panel(cx))
            .child(self.render_tab_content(window, cx))
            .child(self.render_bottom_bar(cx))
    }

    fn render_columns_panel(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let Some(WorkspaceTab::Table {
            columns_open,
            result,
            hidden_columns,
            ..
        }) = self.tabs.active()
        else {
            return div().into_any_element();
        };
        if !*columns_open {
            return div().into_any_element();
        }
        let Some(view) = result.as_ref() else {
            return div().into_any_element();
        };

        let rows: Vec<_> = view
            .set
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                let name = column.name.clone();
                let visible = !hidden_columns.contains(&name);
                div()
                    .id(("column-toggle", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.toggle_column_hidden(&name, cx);
                    }))
                    .child(
                        div()
                            .w(px(12.))
                            .h(px(12.))
                            .rounded_sm()
                            .border_1()
                            .border_color(theme.border)
                            .bg(if visible {
                                theme.accent
                            } else {
                                gpui::rgba(0x00000000)
                            }),
                    )
                    .child(SharedString::from(column.name.clone()))
                    .into_any_element()
            })
            .collect();

        div()
            .id("columns-panel")
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .max_h(px(160.))
            .overflow_y_scroll()
            .flex_shrink_0()
            .bg(theme.elevated)
            .border_b_1()
            .border_color(theme.border)
            .child(caption("Columns", theme))
            .children(rows)
            .into_any_element()
    }

    fn render_tab_content(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(tab) = self.tabs.active() else {
            return self.render_empty_state().into_any_element();
        };

        match tab {
            WorkspaceTab::Sql { .. } => div()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(self.render_editor(cx))
                .child(self.render_grid(window, cx))
                .into_any_element(),
            WorkspaceTab::Table { pane, .. } => match pane {
                TablePane::Structure => self.render_structure(cx).into_any_element(),
                TablePane::Data => div()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .child(self.render_grid(window, cx))
                    .into_any_element(),
            },
        }
    }

    fn render_structure(&mut self, _cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;

        let Some(WorkspaceTab::Table { result, .. }) = self.tabs.active() else {
            return self.render_empty_state().into_any_element();
        };

        let Some(view) = result.as_ref() else {
            return self.render_empty_state().into_any_element();
        };

        if view.structure.is_empty() {
            return self.render_empty_state().into_any_element();
        }

        let rows: Vec<AnyElement> = view
            .structure
            .iter()
            .map(|column| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.divider)
                    .when(column.is_primary_key, |row| row.text_color(theme.warning))
                    .child(
                        div()
                            .w(px(160.))
                            .flex_shrink_0()
                            .child(SharedString::from(column.name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text_muted)
                            .child(SharedString::from(column.data_type.clone())),
                    )
                    .child(
                        div()
                            .w(px(48.))
                            .text_color(theme.text_faint)
                            .text_size(metrics::text_size_small())
                            .child(if column.nullable { "null" } else { "not null" }),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("structure-pane")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .font_family(metrics::MONO_FONT)
            .children(rows)
            .into_any_element()
    }

    fn render_empty_state(&self) -> impl IntoElement {
        let theme = &self.theme;
        let message = if self.workspace.active_driver().is_some() {
            "Pick a table, or press ⌘E to write a query."
        } else {
            "Connect to a database to get started."
        };

        div()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.text_faint)
            .child(message)
    }

    /// The SQL editor on the active SQL tab.
    fn render_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let focused = self.focus == Focus::Editor;
        let Some(WorkspaceTab::Sql { editor, .. }) = self.tabs.active_mut() else {
            return div().id("editor-empty").into_any_element();
        };

        let layout = editor.layout();
        let empty = editor.is_empty();
        let selection = layout.selection.clone();
        let cursor = editor.cursor();
        let input_has_selection = editor.has_selection();
        let hit_slot = editor.hit_bounds_slot();

        let mut consumed = 0usize;
        let lines: Vec<AnyElement> = layout
            .lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                let line_start = consumed;
                let line_end = consumed + line.len();
                let line_range = line_start..line_end;
                consumed = line_end + 1;

                let row = div()
                    .flex()
                    .items_center()
                    .h(metrics::editor_line_height())
                    .child(
                        div()
                            .w(px(32.))
                            .flex_shrink_0()
                            .pr_2()
                            .text_color(theme.text_faint)
                            .text_size(metrics::text_size_small())
                            .child(SharedString::from((index + 1).to_string())),
                    );

                let caret_color = if focused {
                    theme.accent
                } else {
                    theme.text_faint
                };

                let show_caret =
                    focused && !input_has_selection && cursor >= line_start && cursor <= line_end;
                let caret_col = cursor.saturating_sub(line_start).min(line.len());

                let body = if let Some(sel) = selection_on_line(&selection, &line_range) {
                    let before = &line[..sel.start];
                    let selected = &line[sel.clone()];
                    let after = &line[sel.end..];

                    div()
                        .flex()
                        .items_center()
                        .child(SharedString::from(before.to_string()))
                        .child(
                            div()
                                .bg(theme.selection)
                                .flex()
                                .items_center()
                                .child(SharedString::from(selected.to_string())),
                        )
                        .child(SharedString::from(after.to_string()))
                        .when(empty && index == layout.caret_line, |r| {
                            r.child(
                                div()
                                    .text_color(theme.text_faint)
                                    .child("SELECT * FROM …"),
                            )
                        })
                } else if show_caret {
                    let (before, after) = line.split_at(caret_col);
                    div()
                        .flex()
                        .items_center()
                        .child(SharedString::from(before.to_string()))
                        .child(text_input::caret_element(caret_color, px(15.)))
                        .child(SharedString::from(after.to_string()))
                        .when(empty && index == layout.caret_line, |r| {
                            r.child(
                                div()
                                    .text_color(theme.text_faint)
                                    .child("SELECT * FROM …"),
                            )
                        })
                } else {
                    div()
                        .flex()
                        .items_center()
                        .child(SharedString::from(line.to_string()))
                };

                row.child(body).into_any_element()
            })
            .collect();

        div()
            .id("editor")
            .flex_shrink_0()
            .h(px(150.))
            .flex()
            .flex_col()
            .bg(theme.background)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .child(caption("SQL", theme))
                    .child(
                        button("run-query", "Run  ⌘↵", theme, true)
                            .on_click(cx.listener(|this, _, _window, cx| this.run_query(cx))),
                    ),
            )
            .child(
                div()
                    .id("editor-body")
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .px_3()
                    .pb_2()
                    .font_family(metrics::MONO_FONT)
                    .text_size(metrics::editor_text_size())
                    .cursor_text()
                    .child(
                        canvas(
                            move |bounds, _, _| {
                                let mut bounds = bounds;
                                bounds.origin.x += text_input::editor_gutter();
                                bounds.size.width = (bounds.size.width
                                    - text_input::editor_gutter())
                                .max(px(0.));
                                if hit_slot.get() != Some(bounds) {
                                    hit_slot.set(Some(bounds));
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                            this.focus = Focus::Editor;
                            let Some(WorkspaceTab::Sql { editor, .. }) = this.tabs.active_mut()
                            else {
                                return;
                            };
                            let offset = editor.offset_for_mouse(
                                event.position,
                                px(0.),
                                metrics::editor_line_height(),
                                text_input::char_width(),
                            );
                            editor.click_at(
                                offset,
                                event.modifiers.shift,
                                event.click_count,
                            );
                            if event.click_count <= 1 {
                                editor.begin_selecting();
                            } else {
                                editor.end_selecting();
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                        let Some(WorkspaceTab::Sql { editor, .. }) = this.tabs.active_mut() else {
                            return;
                        };
                        if !editor.is_selecting() {
                            return;
                        }
                        let offset = editor.offset_for_mouse(
                            event.position,
                            px(0.),
                            metrics::editor_line_height(),
                            text_input::char_width(),
                        );
                        editor.select_to(offset);
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if let Some(WorkspaceTab::Sql { editor, .. }) = this.tabs.active_mut() {
                                editor.end_selecting();
                            }
                            cx.notify();
                        }),
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _, cx| {
                            if let Some(WorkspaceTab::Sql { editor, .. }) = this.tabs.active_mut() {
                                editor.end_selecting();
                            }
                            cx.notify();
                        }),
                    )
                    .children(lines),
            )
            .into_any_element()
    }
}
