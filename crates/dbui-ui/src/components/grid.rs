//! The result grid.

use crate::root::{DbUi, ResultView};
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use dbui_app::domain::ValueKind;
use gpui::{
    div, prelude::*, px, uniform_list, AnyElement, Context, MouseButton, MouseDownEvent,
    SharedString, Window,
};

const CELL_CHARS: usize = 200;

impl DbUi {
    pub(crate) fn render_grid(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active_index = self.tabs.active;

        let Some(tab) = self.tabs.items.get(active_index) else {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        };
        let Some(view) = tab.result() else {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        };

        if view.set.columns.is_empty() {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        }

        let visible = tab.display_columns();

        if visible.is_empty() {
            return empty_state(&self.theme, self.workspace.active_driver().is_some())
                .into_any_element();
        }

        // Staged inserts are drawn under the stored rows: indices past the
        // result belong to `pending_inserts`, which is what lets one list
        // render both without the virtualizer knowing the difference.
        let stored_rows = view.set.rows.len();
        let insert_count = self
            .tabs
            .items
            .get(active_index)
            .map(|tab| tab.pending_inserts().len())
            .unwrap_or(0);
        let row_count = stored_rows + insert_count;
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

        let sort = self.active_sort_column();
        // Both kinds of tab sort, by two different means. A table tab sends
        // the order to the server and pages through it; a query tab reorders
        // the rows already fetched, because a query's order is whatever its
        // own ORDER BY says and re-reading it with one bolted on would be
        // rewriting the user's SQL behind their back.
        let moving = self
            .column_move
            .filter(|drag| drag.moved)
            .map(|drag| drag.column);
        let header = render_header(view, &visible, &self.theme, total_width, sort, moving, cx);

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
                let visible = tab.display_columns();
                let lead_row = tab.selected_row();
                // The whole staged batch, resolved once per repaint rather
                // than per cell: a cell the user has edited should show what
                // it will become, not what the server last said.
                let staged = this.collect_batch_edits();
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

                let stored_rows = view.set.rows.len();

                range
                    .map(|index| {
                        if index >= stored_rows {
                            return render_insert_row(
                                this,
                                tab,
                                index - stored_rows,
                                &visible,
                                total_width,
                                cx,
                            );
                        }
                        let row = &view.set.rows[index];
                        let stripe = theme.stripe(index);
                        let row_selected = tab.selection().contains(index)
                            || lead_row == Some(index);
                        let staged_delete = tab.row_is_staged_for_delete(index);
                        let staged_edit = tab.staged_edit_for_row(index, &staged);

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
                                // A staged change wins over the stored value:
                                // after typing into a cell, seeing the old
                                // value still there reads as the edit having
                                // been dropped.
                                let pending = staged_edit.and_then(|edit| {
                                    view.set.columns.get(column).and_then(|info| {
                                        edit.changes
                                            .iter()
                                            .find(|change| change.column == info.name)
                                    })
                                });
                                let text: SharedString = match pending {
                                    Some(change) => one_line_cell(&change.new_text).into(),
                                    None if is_null => "NULL".into(),
                                    None => value
                                        .map(|v| v.to_cell(CELL_CHARS))
                                        .unwrap_or_default()
                                        .into(),
                                };
                                let is_selected = this.selected_cell == Some((index, column));
                                let just_copied = this.copied_cell == Some((index, column));
                                let editing = this.editing_cell == Some((index, column));
                                let links = this.foreign_key_at(index, column).is_some();

                                if editing {
                                    return div()
                                        .id(("cell", index * 1_000 + column))
                                        .w(px(width))
                                        .flex_shrink_0()
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .border_r_1()
                                        .border_color(theme.accent)
                                        .child(super::text_field::text_field(
                                            "cell-editor",
                                            &this.cell_editor,
                                            super::text_field::InputTarget::CellEditor,
                                            true,
                                            None,
                                            theme,
                                            cx,
                                        ))
                                        // A press anywhere else closes it and
                                        // keeps what was typed. Handlers on the
                                        // other surfaces only cover the ones
                                        // that have handlers; this covers the
                                        // rest of the window.
                                        .on_mouse_down_out(cx.listener(
                                            |this, _: &MouseDownEvent, _, cx| {
                                                this.finish_cell_edit(cx);
                                            },
                                        ))
                                        // A press *inside* is the field's own:
                                        // placing a caret, double-clicking a
                                        // word, starting a drag-selection. The
                                        // field has already handled it by the
                                        // time this runs -- children go first
                                        // -- and stopping here is what keeps it
                                        // from also reaching the row, which
                                        // reads a press carrying no column as
                                        // "the user moved on" and commits.
                                        .on_mouse_down(
                                            MouseButton::Left,
                                            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                                cx.stop_propagation();
                                            }),
                                        )
                                        // Same for the right button, which
                                        // would otherwise open the row menu --
                                        // and that closes the editor too.
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(|_, _: &MouseDownEvent, _, cx| {
                                                cx.stop_propagation();
                                            }),
                                        )
                                        .into_any_element();
                                }

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
                                    // A copy answers where it was asked for.
                                    // The status bar is at the bottom of the
                                    // window, which on a wide table is
                                    // nowhere near the cell that was clicked.
                                    //
                                    // Tinted rather than outlined: a cell only
                                    // draws its right-hand border, so a border
                                    // colour alone is one hairline the eye
                                    // never lands on.
                                    .when(just_copied, |cell| {
                                        cell.bg(gpui::Rgba {
                                            a: 0.22,
                                            ..theme.success
                                        })
                                        .border_color(theme.success)
                                    })
                                    .when(kind.right_aligned(), |cell| cell.justify_end())
                                    .text_color(theme.value_color(kind))
                                    .when(is_null && pending.is_none(), |cell| {
                                        cell.text_color(theme.value_null)
                                    })
                                    .when(pending.is_some(), |cell| {
                                        cell.text_color(theme.success)
                                    })
                                    // A value that points at another table is
                                    // underlined, the way a link is anywhere
                                    // else.
                                    .when(links && pending.is_none(), |cell| {
                                        cell.underline().text_color(theme.value_structured)
                                    })
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
                                            // A second click on the same cell
                                            // opens it, the way a grid does.
                                            if event.click_count >= 2 {
                                                this.begin_cell_edit(index, column, cx);
                                                return;
                                            }
                                            this.grid_pointer_down(
                                                index,
                                                Some(column),
                                                event.modifiers,
                                                cx,
                                            );
                                        }),
                                    )
                                    // The row behind this has the same handler
                                    // minus the column, which is what a menu
                                    // entry about *this cell* needs -- so the
                                    // press is claimed here and the column
                                    // carried through.
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            // Right-clicking outside the
                                            // selection moves it; inside one,
                                            // only the cell cursor moves, so a
                                            // menu opened over a range still
                                            // acts on the whole range.
                                            let inside = this
                                                .tabs
                                                .active()
                                                .is_some_and(|tab| tab.selection().contains(index));
                                            if inside {
                                                this.focus_cell(index, column, cx);
                                            } else {
                                                this.grid_pointer_down(
                                                    index,
                                                    Some(column),
                                                    gpui::Modifiers::default(),
                                                    cx,
                                                );
                                                this.end_row_drag(cx);
                                            }
                                            this.open_context_menu(
                                                crate::components::context_menu::ContextTarget::Rows,
                                                event.position,
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
                            .on_mouse_down(
                                MouseButton::Right,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    // Right-clicking outside the selection
                                    // moves it, the way every list does --
                                    // otherwise the menu acts on rows the
                                    // pointer is nowhere near.
                                    let inside = this
                                        .tabs
                                        .active()
                                        .is_some_and(|tab| tab.selection().contains(index));
                                    if !inside {
                                        this.grid_pointer_down(
                                            index,
                                            None,
                                            gpui::Modifiers::default(),
                                            cx,
                                        );
                                        this.end_row_drag(cx);
                                    }
                                    this.open_context_menu(
                                        crate::components::context_menu::ContextTarget::Rows,
                                        event.position,
                                        cx,
                                    );
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
        // So the arrow keys can put a row back on screen -- see
        // `DbUi::reveal_row`.
        .track_scroll(self.grid_scroll.clone())
        .w(px(total_width))
        .flex_1()
        .min_h(px(0.));

        // The bars are siblings of the scroller, not children of it: a child
        // would scroll away with the rows. They come after it so that their
        // prepaint reads the sizes this frame's layout just settled.
        let vertical_bar = super::scrollbar::vertical_scrollbar(
            "grid-v-scrollbar",
            self.grid_scroll.0.borrow().base_handle.clone(),
            &self.theme,
        )
        // Clear of the header, which does not scroll with the rows.
        .top(metrics::header_height());
        let horizontal_bar = super::scrollbar::horizontal_scrollbar(
            "grid-h-scrollbar",
            self.grid_h_scroll.clone(),
            &self.theme,
        );

        div()
            .id("grid-scroll")
            .relative()
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
                    .track_scroll(&self.grid_h_scroll)
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
            .child(vertical_bar)
            .child(horizontal_bar)
            .into_any_element()
    }
}

/// One staged insert, drawn as a row under the stored ones.
///
/// Marked `+` in the gutter and tinted with the success colour, so a row that
/// is not on the server yet never looks like one that is.
fn render_insert_row(
    this: &DbUi,
    tab: &WorkspaceTab,
    insert_index: usize,
    visible: &[(usize, &dbui_app::domain::ColumnInfo)],
    total_width: f32,
    cx: &mut Context<DbUi>,
) -> gpui::Stateful<gpui::Div> {
    let theme = &this.theme;
    let inserts = tab.pending_inserts();
    let Some(row) = inserts.get(insert_index) else {
        return div().id(("insert-row-missing", insert_index));
    };
    let being_edited = tab.editing_insert() == Some(insert_index);

    let cells: Vec<AnyElement> = visible
        .iter()
        .map(|(column, info)| {
            let width = this
                .tabs
                .active()
                .and_then(|tab| tab.result())
                .and_then(|view| view.widths.get(*column).copied())
                .unwrap_or(metrics::column_min_width());
            let text: SharedString = row
                .fields
                .iter()
                .find(|(name, _, _)| name == &info.name)
                .map(|(_, input, _)| input.text().to_string())
                .unwrap_or_default()
                .into();

            div()
                .w(px(width))
                .flex_shrink_0()
                .h_full()
                .flex()
                .items_center()
                .px_2()
                .overflow_hidden()
                .whitespace_nowrap()
                .border_r_1()
                .border_color(theme.divider)
                .text_color(theme.text_muted)
                .child(text)
                .into_any_element()
        })
        .collect();

    div()
        .id(("insert-row", insert_index))
        .flex()
        .w(px(total_width))
        .h(metrics::row_height())
        .bg(theme.selection)
        .when(being_edited, |row| row.bg(theme.hover))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.edit_insert(insert_index, cx);
            }),
        )
        .child(
            div()
                .w(metrics::row_number_width())
                .flex_shrink_0()
                .h_full()
                .flex()
                .items_center()
                .justify_end()
                .px_2()
                .text_color(theme.success)
                .text_size(metrics::text_size_small())
                .border_r_1()
                .border_color(theme.divider)
                .child("+"),
        )
        .children(cells)
}

/// A staged value flattened to fit one grid row, the way a stored value is.
fn one_line_cell(text: &str) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if flat.chars().count() > CELL_CHARS {
        flat.chars().take(CELL_CHARS).collect()
    } else {
        flat
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
    // `sort` is the sorted column's index into the result, not its name: a
    // query can return two columns of one name and only one of them is sorted.
    sort: Option<(usize, bool)>,
    // `moving` is the column being carried, once the press has travelled far
    // enough to be a drag rather than a click on the heading.
    moving: Option<usize>,
    cx: &mut Context<DbUi>,
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
            let sorted = sort.filter(|(at, _)| *at == *index);

            div()
                .id(("header", *index))
                .relative()
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
                .when(sorted.is_some(), |header| header.text_color(theme.text))
                .cursor_pointer()
                .hover(|style| style.bg(theme.hover))
                // The one in hand is lit, so a drag over a wide table still
                // shows which column is moving.
                .when(moving == Some(*index), |header| {
                    header.bg(theme.selection).text_color(theme.text)
                })
                // A press is not a sort yet: it becomes one on release, if
                // the pointer never travelled. Sorting on the way down would
                // re-run the query under a column on its way somewhere else.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener({
                        let column = *index;
                        move |this, event: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_column_move(column, event.position.x);
                            cx.notify();
                        }
                    }),
                )
                // Crossing another heading with a column in hand is what
                // moves it -- a slot at a time, the way the tab strip does.
                .on_mouse_move(cx.listener({
                    let column = *index;
                    move |this, event: &gpui::MouseMoveEvent, _, cx| {
                        this.drag_column_over(column, event.position.x, cx);
                    }
                }))
                .child(SharedString::from(column.name.clone()))
                .child(
                    div()
                        .text_size(metrics::scaled(9.))
                        .text_color(theme.text_faint)
                        .child(SharedString::from(column.type_name.to_lowercase())),
                )
                .children(sorted.map(|(_, ascending)| {
                    div()
                        .flex_shrink_0()
                        .text_color(theme.accent)
                        .child(if ascending { "↑" } else { "↓" })
                }))
                // The grab strip for resizing, on the column's right edge.
                // `absolute` so it sits over the border rather than taking
                // width from the header it belongs to.
                .child(
                    div()
                        .id(("column-resize", *index))
                        .absolute()
                        .top_0()
                        .right(px(-2.))
                        .w(px(5.))
                        .h_full()
                        .cursor_col_resize()
                        .hover(|strip| strip.bg(theme.accent))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let column = *index;
                                move |this, event: &MouseDownEvent, _, cx| {
                                    // Or the press would sort the column the
                                    // user is trying to widen.
                                    cx.stop_propagation();
                                    this.begin_column_drag(column, event.position.x, cx);
                                }
                            }),
                        ),
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
