//! The result grid.

use crate::root::{DbUi, ResultView};
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use dbui_app::domain::ValueKind;
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, MouseButton, MouseDownEvent,
    SharedString, Window,
};
use std::collections::HashSet;

const CELL_CHARS: usize = 200;

impl DbUi {
    pub(crate) fn render_grid(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_index = self.tabs.active;

        let hidden_columns = match self.tabs.items.get(active_index) {
            Some(WorkspaceTab::Table { hidden_columns, .. }) => hidden_columns.clone(),
            _ => HashSet::new(),
        };

        let Some(view) = self
            .tabs
            .items
            .get(active_index)
            .and_then(|tab| tab.result())
        else {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        };

        if view.set.columns.is_empty() {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        }

        let is_table_tab = matches!(
            self.tabs.items.get(active_index),
            Some(WorkspaceTab::Table { .. })
        );

        let visible: Vec<(usize, _)> = view
            .set
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| !is_table_tab || !hidden_columns.contains(&column.name))
            .collect();

        if visible.is_empty() {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        }

        let row_count = view.set.rows.len();
        let total_width: f32 = visible
            .iter()
            .map(|(index, _)| {
                view.widths
                    .get(*index)
                    .copied()
                    .unwrap_or(metrics::column_min_width())
            })
            .sum::<f32>()
            + f32::from(metrics::row_number_width());

        let header = render_header(view, &visible, &self.theme, total_width);

        // Virtualized rows (fast). Parent H-scrolls; list only scrolls vertically.
        // `overflow_hidden` then `overflow_x_scroll` keeps Y clipped so the list
        // gets a bounded height (otherwise it grows with content and neither
        // axis scrolls).
        let body = uniform_list(
            "result-rows",
            row_count,
            cx.processor(|this, range: std::ops::Range<usize>, _window, cx| {
                let active_index = this.tabs.active;
                let Some(tab) = this.tabs.items.get(active_index) else {
                    return Vec::new();
                };
                let Some(view) = tab.result() else {
                    return Vec::new();
                };
                let visible: Vec<(usize, _)> = view
                    .set
                    .columns
                    .iter()
                    .enumerate()
                    .filter(|(_, column)| match tab {
                        WorkspaceTab::Table { hidden_columns, .. } => {
                            !hidden_columns.contains(&column.name)
                        }
                        WorkspaceTab::Sql { .. } => true,
                    })
                    .collect();
                let lead_row = tab.selected_row();
                let theme = &this.theme;
                let total_width: f32 = visible
                    .iter()
                    .map(|(index, _)| {
                        view.widths
                            .get(*index)
                            .copied()
                            .unwrap_or(metrics::column_min_width())
                    })
                    .sum::<f32>()
                    + f32::from(metrics::row_number_width());

                range
                    .map(|index| {
                        let row = &view.set.rows[index];
                        let stripe = theme.stripe(index);
                        let row_selected = tab.selection().contains(index)
                            || lead_row == Some(index);
                        let staged_delete = tab.row_is_staged_for_delete(index);

                        let cells: Vec<AnyElement> = visible
                            .iter()
                            .map(|(column, _)| {
                                let column = *column;
                                let width = view
                                    .widths
                                    .get(column)
                                    .copied()
                                    .unwrap_or(metrics::column_min_width());
                                let value = row.get(column);
                                let kind = value.map(|v| v.kind()).unwrap_or(ValueKind::Null);
                                let is_null = value.map(|v| v.is_null()).unwrap_or(true);
                                let text: SharedString = if is_null {
                                    "NULL".into()
                                } else {
                                    value
                                        .map(|v| v.to_cell(CELL_CHARS))
                                        .unwrap_or_default()
                                        .into()
                                };
                                let is_selected = this.selected_cell == Some((index, column));

                                div()
                                    .id(("cell", index * 1_000 + column))
                                    .w(px(width))
                                    .flex_shrink_0()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .overflow_hidden()
                                    .border_r_1()
                                    .border_color(theme.divider)
                                    .when(is_selected || row_selected, |cell| {
                                        cell.bg(theme.selection).border_color(theme.accent)
                                    })
                                    .when(kind.right_aligned(), |cell| cell.justify_end())
                                    .text_color(theme.value_color(kind))
                                    .when(is_null, |cell| cell.text_color(theme.value_null))
                                    // A row on its way out is drawn as one:
                                    // struck through, in the colour the change
                                    // bubble uses for a removal.
                                    .when(staged_delete, |cell| {
                                        cell.line_through().text_color(theme.danger)
                                    })
                                    .cursor_pointer()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.grid_pointer_down(
                                                index,
                                                Some(column),
                                                event.modifiers,
                                                cx,
                                            );
                                        }),
                                    )
                                    .child(text)
                                    .into_any_element()
                            })
                            .collect();

                        div()
                            .id(("row", index))
                            .flex()
                            .w(px(total_width))
                            .h(metrics::row_height())
                            .when_some(stripe, |row, tint| row.bg(tint))
                            .when(row_selected, |row| row.bg(theme.selection))
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.grid_pointer_down(index, None, event.modifiers, cx);
                                }),
                            )
                            // Drag-select. The press marks the anchor; crossing
                            // a row with the button down grows the range to it.
                            .on_mouse_move(cx.listener(
                                move |this, _: &gpui::MouseMoveEvent, _, cx| {
                                    this.grid_drag_over(index, cx);
                                },
                            ))
                            .child(
                                div()
                                    .w(metrics::row_number_width())
                                    .flex_shrink_0()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .justify_end()
                                    .px_2()
                                    .text_color(if staged_delete {
                                        theme.danger
                                    } else {
                                        theme.text_faint
                                    })
                                    .text_size(metrics::text_size_small())
                                    .border_r_1()
                                    .border_color(theme.divider)
                                    .child(SharedString::from(if staged_delete {
                                        "−".to_string()
                                    } else {
                                        (index + 1).to_string()
                                    })),
                            )
                            .children(cells)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .w(px(total_width))
        .flex_1()
        .min_h(px(0.));

        div()
            .id("grid-scroll")
            .flex_1()
            .h_full()
            .min_h(px(0.))
            .min_w(px(0.))
            .w_full()
            .overflow_hidden()
            .font_family(metrics::MONO_FONT)
            .child(
                div()
                    .id("grid-h-scroll")
                    .size_full()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .overflow_hidden()
                    .overflow_x_scroll()
                    // Without this, GPUI remaps vertical wheel deltas onto X when
                    // the container only scrolls horizontally — so a vertical
                    // trackpad gesture also pans the grid sideways.
                    .map(|mut el| {
                        el.style().restrict_scroll_to_axis = Some(true);
                        el
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .h_full()
                            .min_h(px(0.))
                            .w(px(total_width))
                            .child(header)
                            .child(body),
                    ),
            )
            .into_any_element()
    }
}

fn empty_state(theme: &crate::theme::Theme, connected: bool) -> impl IntoElement {
    let message = if connected {
        "Pick a table, or press ⌘E to write a query."
    } else {
        "Connect to a database to get started."
    };

    div()
        .flex_1()
        .min_h(px(0.))
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme.text_faint)
        .child(message)
}

fn render_header(
    view: &ResultView,
    visible: &[(usize, &dbui_app::domain::ColumnInfo)],
    theme: &crate::theme::Theme,
    total_width: f32,
) -> AnyElement {
    let columns: Vec<AnyElement> = visible
        .iter()
        .map(|(index, column)| {
            let width = view
                .widths
                .get(*index)
                .copied()
                .unwrap_or(metrics::column_min_width());

            let is_key = view
                .structure
                .iter()
                .any(|meta| meta.name == column.name && meta.is_primary_key);

            div()
                .w(px(width))
                .flex_shrink_0()
                .h_full()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .overflow_hidden()
                .border_r_1()
                .border_color(theme.border)
                .when(is_key, |header| header.text_color(theme.warning))
                .child(SharedString::from(column.name.clone()))
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(column.type_name.to_lowercase())),
                )
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .w(px(total_width))
        .h(metrics::header_height())
        .flex_shrink_0()
        .bg(theme.elevated)
        .text_color(theme.text_muted)
        .text_size(metrics::text_size_small())
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .w(metrics::row_number_width())
                .flex_shrink_0()
                .h_full()
                .border_r_1()
                .border_color(theme.border),
        )
        .children(columns)
        .into_any_element()
}
