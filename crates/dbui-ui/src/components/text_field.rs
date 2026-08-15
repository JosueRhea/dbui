//! Shared text field: caret, selection, mouse, scroll.
//!
//! Single-line for filters / search / page size; multiline for detail cells.

use crate::highlight;
use crate::json_format;
use crate::root::DbUi;
use crate::text_input::{self, selection_on_line, TextInput};
use crate::theme::{metrics, Theme};
use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, prelude::*, px, AnyElement, Bounds, Context, ElementId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollWheelEvent, SharedString,
};

const SINGLE_LINE_HEIGHT: f32 = 28.;
const MAX_VISIBLE_LINES: usize = 8;
/// `py_1` top + bottom, in unzoomed pixels (`py_1` is 0.25rem and rem tracks zoom).
const FIELD_PAD_Y: f32 = 8.;
/// `border_1` top + bottom. Borders are not zoomed, and leaving this out of the
/// field height is what used to make the scrollport shorter than its own text.
const FIELD_BORDER_Y: f32 = 2.;

/// Which editable string the shared field is talking to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    WhereDraft,
    DetailSearch,
    DetailField(usize),
    PageSize,
    PaletteQuery,
    /// The table filter above the schema tree.
    SidebarFilter,
    /// The name typed to arm a destructive statement.
    ConfirmName,
    /// One column of the new row being staged.
    InsertField(usize),
    /// The one-line editor open over a grid cell.
    CellEditor,
}

impl DbUi {
    pub(crate) fn input_mut(&mut self, target: InputTarget) -> Option<&mut TextInput> {
        use crate::tabs::WorkspaceTab;
        match target {
            InputTarget::WhereDraft => match self.tabs.active_mut() {
                Some(WorkspaceTab::Table { where_draft, .. }) => Some(where_draft),
                _ => None,
            },
            InputTarget::DetailSearch => match self.tabs.active_mut() {
                Some(WorkspaceTab::Table {
                    draft: Some(draft), ..
                })
                | Some(WorkspaceTab::Sql {
                    draft: Some(draft), ..
                }) => Some(&mut draft.field_search),
                _ => None,
            },
            InputTarget::DetailField(index) => match self.tabs.active_mut() {
                Some(WorkspaceTab::Table {
                    draft: Some(draft), ..
                })
                | Some(WorkspaceTab::Sql {
                    draft: Some(draft), ..
                }) => draft.fields.get_mut(index).map(|(_, input, _)| input),
                _ => None,
            },
            InputTarget::PageSize => match self.tabs.active_mut() {
                Some(WorkspaceTab::Table {
                    page_size_draft, ..
                }) => Some(page_size_draft),
                _ => None,
            },
            InputTarget::PaletteQuery => self.palette.as_mut().map(|p| &mut p.query),
            InputTarget::SidebarFilter => Some(&mut self.sidebar_filter),
            InputTarget::ConfirmName => self.confirm.as_mut().map(|prompt| &mut prompt.input),
            InputTarget::CellEditor => Some(&mut self.cell_editor),
            InputTarget::InsertField(index) => match self.tabs.active_mut() {
                Some(crate::tabs::WorkspaceTab::Table {
                    pending_inserts,
                    editing_insert: Some(open),
                    ..
                }) => pending_inserts
                    .get_mut(*open)
                    .and_then(|row| row.fields.get_mut(index).map(|(_, input, _)| input)),
                _ => None,
            },
        }
    }

    pub(crate) fn focus_input(&mut self, target: InputTarget, cx: &mut Context<Self>) {
        // Moving the keyboard into another box closes the one open over a
        // grid cell, rather than leaving two editors on screen at once.
        if target != InputTarget::CellEditor {
            self.finish_cell_edit(cx);
        }
        match target {
            InputTarget::WhereDraft => {
                self.filter_focus = Some(crate::root::FilterFocus::Where);
                self.focus = crate::root::Focus::Filter;
                self.detail_input = None;
                self.page_size_focus = false;
            }
            InputTarget::DetailSearch => {
                self.detail_input = Some(DetailInput::Search);
                self.focus = crate::root::Focus::Detail;
                self.filter_focus = None;
                self.page_size_focus = false;
            }
            InputTarget::DetailField(index) => {
                self.detail_input = Some(DetailInput::Field(index));
                self.focus = crate::root::Focus::Detail;
                self.filter_focus = None;
                self.page_size_focus = false;
            }
            InputTarget::PageSize => {
                self.page_size_focus = true;
                self.focus = crate::root::Focus::PageSize;
                self.filter_focus = None;
                self.detail_input = None;
            }
            InputTarget::PaletteQuery => {
                self.filter_focus = None;
                self.page_size_focus = false;
                self.detail_input = None;
            }
            InputTarget::SidebarFilter => {
                self.focus = crate::root::Focus::SidebarSearch;
                self.filter_focus = None;
                self.page_size_focus = false;
                self.detail_input = None;
            }
            // The prompt is modal: it already owns the keyboard, and moving
            // `focus` would leave the surface underneath looking active.
            InputTarget::ConfirmName => {}
            InputTarget::CellEditor => {
                self.focus = crate::root::Focus::Grid;
                self.detail_input = None;
                self.filter_focus = None;
                self.page_size_focus = false;
            }
            InputTarget::InsertField(index) => {
                self.detail_input = Some(DetailInput::Field(index));
                self.focus = crate::root::Focus::Detail;
                self.filter_focus = None;
                self.page_size_focus = false;
            }
        }
        cx.notify();
    }
}

/// Which detail-sidebar editor owns the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailInput {
    Search,
    Field(usize),
}

/// Full-featured field bound to an [`InputTarget`].
pub(crate) fn text_field(
    id: impl Into<ElementId>,
    input: &TextInput,
    target: InputTarget,
    focused: bool,
    placeholder: Option<&str>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let scroll_id: ElementId = match target {
        InputTarget::WhereDraft => "where-scroll".into(),
        InputTarget::DetailSearch => "detail-search-scroll".into(),
        InputTarget::PageSize => "page-size-scroll".into(),
        InputTarget::PaletteQuery => "palette-query-scroll".into(),
        InputTarget::SidebarFilter => "sidebar-filter-scroll".into(),
        InputTarget::ConfirmName => "confirm-name-scroll".into(),
        InputTarget::InsertField(index) => ("insert-field-scroll", index).into(),
        InputTarget::CellEditor => "cell-editor-scroll".into(),
        InputTarget::DetailField(index) => ("detail-field-scroll", index).into(),
    };

    if input.is_multiline() {
        multiline_text_field(
            id,
            scroll_id,
            input,
            target,
            focused,
            placeholder,
            theme,
            cx,
        )
    } else {
        single_line_text_field(
            id,
            scroll_id,
            input,
            target,
            focused,
            placeholder,
            theme,
            cx,
        )
    }
}

fn single_line_text_field(
    id: impl Into<ElementId>,
    scroll_id: impl Into<ElementId>,
    input: &TextInput,
    target: InputTarget,
    focused: bool,
    placeholder: Option<&str>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let empty = input.text().is_empty();
    let hit_slot = input.hit_bounds_slot();
    let scroll_handle = input.scroll_handle().clone();
    let borderless = matches!(target, InputTarget::PaletteQuery);
    let line_h = if borderless {
        px(36. * metrics::zoom())
    } else {
        px(SINGLE_LINE_HEIGHT * metrics::zoom())
    };
    let char_w = text_input::char_width();
    // Pan with `left` — no Overflow::Scroll (that was the vertical bounce).
    let scroll_x = scroll_handle.offset().x;

    // Connection-form chrome: fixed height, flex-centered glyphs, overflow clip.
    let field = div()
        .id(id)
        .w_full()
        .min_w(px(0.))
        .flex_shrink_0()
        .relative()
        .h(line_h)
        .px_2()
        .cursor_text()
        .overflow_hidden()
        .font_family(if borderless {
            metrics::UI_FONT
        } else {
            metrics::MONO_FONT
        })
        .text_size(metrics::text_size());

    let field = if borderless {
        field.bg(gpui::rgba(0x0000_0000)).border_0()
    } else {
        field
            .rounded_md()
            .bg(theme.background)
            .border_1()
            .border_color(if focused { theme.accent } else { theme.border })
    };

    field
        .child(
            div()
                .id(scroll_id)
                .size_full()
                .overflow_hidden()
                .relative()
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .left(scroll_x)
                        .h_full()
                        .flex()
                        .items_center()
                        .child(render_single_line(
                            input,
                            focused,
                            empty,
                            placeholder,
                            theme,
                        )),
                ),
        )
        // `px_2` scales with the rem, so the inset has to as well.
        .child(hit_canvas(hit_slot, px(8. * metrics::zoom()), px(0.)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.focus_input(target, cx);
                let Some(input) = this.input_mut(target) else {
                    return;
                };
                let offset = input.offset_for_mouse(event.position, px(0.), line_h, char_w);
                input.click_at(offset, event.modifiers.shift, event.click_count);
                if event.click_count <= 1 {
                    input.begin_selecting();
                } else {
                    input.end_selecting();
                }
                input.ensure_caret_visible();
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            let Some(input) = this.input_mut(target) else {
                return;
            };
            if !input.is_selecting() {
                return;
            }
            let offset = input.offset_for_mouse(event.position, px(0.), line_h, char_w);
            input.select_to(offset);
            input.ensure_caret_visible();
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                if let Some(input) = this.input_mut(target) {
                    input.end_selecting();
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                if let Some(input) = this.input_mut(target) {
                    input.end_selecting();
                }
                cx.notify();
            }),
        )
        .into_any_element()
}

fn multiline_text_field(
    id: impl Into<ElementId>,
    scroll_id: impl Into<ElementId>,
    input: &TextInput,
    target: InputTarget,
    focused: bool,
    placeholder: Option<&str>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let empty = input.text().is_empty();
    let hit_slot = input.hit_bounds_slot();
    let scroll_handle = input.scroll_handle().clone();
    let line_h = text_input::field_line_height();
    let char_w = text_input::field_char_width();
    let line_count = input.layout().lines.len().max(1);
    let visible = line_count.min(MAX_VISIBLE_LINES);
    // The scrollport is sized from the same number as the text it holds, so a
    // field that fits its lines has *no* vertical scroll range. When that was
    // off by the border's 2px, `ensure_caret_visible` kept flipping the offset
    // between 0 and -2 on every keystroke -- the vertical bounce.
    let port_h = px(f32::from(line_h) * visible as f32);
    let height = px(f32::from(port_h) + FIELD_PAD_Y * metrics::zoom() + FIELD_BORDER_Y);

    div()
        .id(id)
        .w_full()
        .min_w(px(0.))
        .flex_shrink_0()
        .relative()
        .flex()
        .items_start()
        .h(height)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.background)
        .border_1()
        .border_color(if focused { theme.accent } else { theme.border })
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .cursor_text()
        .overflow_hidden()
        .child(
            div()
                .id(scroll_id)
                .track_scroll(&scroll_handle)
                .flex_1()
                .min_w(px(0.))
                .h(port_h)
                .overflow_x_scroll()
                .scrollbar_width(px(0.))
                .when(line_count > MAX_VISIBLE_LINES, |el| el.overflow_y_scroll())
                .map(|mut el| {
                    el.style().restrict_scroll_to_axis = Some(true);
                    el
                })
                .when(line_count > MAX_VISIBLE_LINES, {
                    let scroll_handle = scroll_handle.clone();
                    move |el| {
                        el.on_scroll_wheel(move |event: &ScrollWheelEvent, window, cx| {
                            let delta = event.delta.pixel_delta(window.line_height());
                            if delta.y.abs() <= delta.x.abs() {
                                return;
                            }
                            let max_y = scroll_handle.max_offset().height;
                            if max_y <= px(0.) {
                                return;
                            }
                            let y = scroll_handle.offset().y;
                            let can_scroll = if delta.y < px(0.) {
                                y > -max_y
                            } else if delta.y > px(0.) {
                                y < px(0.)
                            } else {
                                false
                            };
                            if can_scroll {
                                cx.stop_propagation();
                            }
                        })
                    }
                })
                .child(render_multiline(
                    input,
                    focused,
                    empty,
                    placeholder,
                    theme,
                    char_w,
                )),
        )
        // Matches `px_2` / `py_1`, so the measured viewport is the scrollport.
        .child(hit_canvas(
            hit_slot,
            px(8. * metrics::zoom()),
            px(4. * metrics::zoom()),
        ))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                this.focus_input(target, cx);
                let Some(input) = this.input_mut(target) else {
                    return;
                };
                let offset = input.offset_for_mouse(event.position, px(0.), line_h, char_w);
                input.click_at(offset, event.modifiers.shift, event.click_count);
                if event.click_count <= 1 {
                    input.begin_selecting();
                } else {
                    input.end_selecting();
                }
                input.ensure_caret_visible();
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
            let Some(input) = this.input_mut(target) else {
                return;
            };
            if !input.is_selecting() {
                return;
            }
            let offset = input.offset_for_mouse(event.position, px(0.), line_h, char_w);
            input.select_to(offset);
            input.ensure_caret_visible();
            cx.notify();
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                if let Some(input) = this.input_mut(target) {
                    input.end_selecting();
                }
                cx.notify();
            }),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                if let Some(input) = this.input_mut(target) {
                    input.end_selecting();
                }
                cx.notify();
            }),
        )
        .into_any_element()
}

fn hit_canvas(
    slot: Rc<Cell<Option<Bounds<Pixels>>>>,
    pad_x: Pixels,
    pad_y: Pixels,
) -> impl IntoElement {
    // Paint writes into a Cell — never `entity.update` (that re-rendered every
    // detail field every frame and made typing crawl).
    canvas(
        move |bounds, _, _| {
            let mut bounds = bounds;
            bounds.origin.x += pad_x;
            bounds.origin.y += pad_y;
            bounds.size.width = (bounds.size.width - pad_x * 2.).max(px(0.));
            bounds.size.height = (bounds.size.height - pad_y * 2.).max(px(0.));
            if slot.get() != Some(bounds) {
                slot.set(Some(bounds));
            }
        },
        |_bounds, _, _, _| {},
    )
    .absolute()
    .size_full()
}

fn render_single_line(
    input: &TextInput,
    focused: bool,
    empty: bool,
    placeholder: Option<&str>,
    theme: &Theme,
) -> AnyElement {
    let caret_h = px(14. * metrics::zoom());

    if empty && !focused {
        if let Some(placeholder) = placeholder {
            return div()
                .flex()
                .items_center()
                .flex_shrink_0()
                .whitespace_nowrap()
                .text_color(theme.text_faint)
                .child(SharedString::from(placeholder.to_string()))
                .into_any_element();
        }
    }

    let text = input.text().replace(['\n', '\r'], " ");
    let selection = input.selection();
    let cursor = input.cursor().min(text.len());
    let caret_color = if focused {
        theme.accent
    } else {
        gpui::rgba(0x00000000)
    };

    let start = selection.start.min(text.len());
    let end = selection.end.min(text.len());

    if start != end {
        let before = &text[..start];
        let selected = &text[start..end];
        let after = &text[end..];
        return div()
            .flex()
            .items_center()
            .flex_shrink_0()
            .whitespace_nowrap()
            .text_color(theme.text)
            .child(SharedString::from(before.to_string()))
            .child(
                div()
                    .bg(theme.selection)
                    .flex()
                    .items_center()
                    .child(SharedString::from(selected.to_string())),
            )
            .child(SharedString::from(after.to_string()))
            .into_any_element();
    }

    if !focused {
        return div()
            .flex()
            .items_center()
            .flex_shrink_0()
            .whitespace_nowrap()
            .text_color(theme.text)
            .child(SharedString::from(text))
            .into_any_element();
    }

    let (before, after) = text.split_at(cursor);
    div()
        .flex()
        .items_center()
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_color(theme.text)
        .child(SharedString::from(before.to_string()))
        .child(text_input::caret_element(caret_color, caret_h))
        .child(SharedString::from(after.to_string()))
        .into_any_element()
}

fn render_multiline(
    input: &TextInput,
    focused: bool,
    empty: bool,
    placeholder: Option<&str>,
    theme: &Theme,
    char_w: f32,
) -> AnyElement {
    if empty && !focused {
        if let Some(placeholder) = placeholder {
            return div()
                .text_color(theme.text_faint)
                .child(SharedString::from(placeholder.to_string()))
                .into_any_element();
        }
    }

    let layout = input.layout();
    let selection = layout.selection.clone();
    let cursor = input.cursor();
    let has_selection = input.has_selection();
    let caret_color = if focused {
        theme.accent
    } else {
        gpui::rgba(0x00000000)
    };
    let line_h = text_input::field_line_height();
    let json_spans = json_format::highlight_spans(input.text());

    let max_chars = layout
        .lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let content_w = px(max_chars as f32 * char_w + 8.);

    let mut consumed = 0usize;
    let lines: Vec<AnyElement> = layout
        .lines
        .iter()
        .map(|line| {
            let line_start = consumed;
            let line_end = consumed + line.len();
            let line_range = line_start..line_end;
            consumed = line_end + 1;

            let show_caret =
                focused && !has_selection && cursor >= line_start && cursor <= line_end;
            let caret_col = cursor.saturating_sub(line_start).min(line.len());
            let line_styles = json_spans
                .as_ref()
                .map(|spans| json_format::styles_on_line(spans, &line_range))
                .unwrap_or_default();

            let body = highlight::render_highlighted_line(
                line,
                &line_styles,
                selection_on_line(&selection, &line_range),
                show_caret.then_some(caret_col),
                caret_color,
                theme,
                line_h,
                |style| style.color(theme),
            );

            div()
                .h(line_h)
                .w(content_w)
                .flex()
                .items_center()
                .when(line.is_empty(), |row| row.child(div().w(px(1.)).h_full()))
                .child(body)
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_col()
        .flex_shrink_0()
        .w(content_w)
        .children(lines)
        .into_any_element()
}
