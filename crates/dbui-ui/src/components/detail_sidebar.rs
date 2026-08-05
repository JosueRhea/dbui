//! Right-hand detail panel for the selected row.

use super::caption;
use super::text_field::{text_field, DetailInput, InputTarget};
use crate::json_format::{self, JsonStyle};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::{metrics, Theme};
use dbui_app::domain::Value;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, SharedString,
};

const DETAIL_WIDTH_BASE: f32 = 280.;
const STRIP_WIDTH_BASE: f32 = 24.;

impl DbUi {
    pub(crate) fn render_detail_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;

        if !self.detail_open {
            return div()
                .id("detail-strip")
                .w(px(STRIP_WIDTH_BASE * metrics::zoom()))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .bg(theme.panel)
                .border_l_1()
                .border_color(theme.border)
                .child(
                    div()
                        .id("detail-strip-toggle")
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_color(theme.text_faint)
                        .hover(|strip| strip.text_color(theme.text_muted))
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_detail(cx)))
                        .child("◂"),
                );
        }

        let open_menu = self.detail_value_menu;
        let body = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft,
                selected_row,
                result,
                ..
            })
            | Some(WorkspaceTab::Sql {
                draft,
                selected_row,
                result,
                ..
            }) => {
                if let Some(draft) = draft.as_ref() {
                    let originals = result
                        .as_ref()
                        .and_then(|view| view.set.rows.get(draft.row_index))
                        .map(|row| row.0.as_slice())
                        .unwrap_or(&[]);
                    render_table_draft(draft, originals, open_menu, self.detail_input, theme, cx)
                } else if selected_row.is_some() {
                    caption("Loading row…", theme).into_any_element()
                } else {
                    empty_selection(theme)
                }
            }
            None => empty_selection(theme),
        };

        div()
            .id("detail-sidebar")
            .w(px(DETAIL_WIDTH_BASE * metrics::zoom()))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .h(metrics::toolbar_height())
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.divider)
                    .child(div().text_size(px(13.)).child("Details"))
                    .child(
                        div()
                            .id("detail-collapse")
                            .px_1()
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .hover(|icon| icon.text_color(theme.text))
                            .on_click(cx.listener(|this, _, _window, cx| this.toggle_detail(cx)))
                            .child("▸"),
                    ),
            )
            .child(
                div()
                    .id("detail-body")
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .w_full()
                    .overflow_x_hidden()
                    .overflow_y_scroll()
                    .map(|mut el| {
                        // Horizontal trackpad over fields must not remap onto
                        // this vertical sidebar scroll.
                        el.style().restrict_scroll_to_axis = Some(true);
                        el
                    })
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(body),
            )
    }
}

fn empty_selection(theme: &Theme) -> AnyElement {
    div()
        .py_6()
        .flex()
        .items_center()
        .justify_center()
        .child(caption("No row selected.", theme))
        .into_any_element()
}

fn render_table_draft(
    draft: &crate::tabs::RowDraft,
    originals: &[Value],
    open_menu: Option<usize>,
    detail_input: Option<DetailInput>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let search = draft.field_search.text().to_ascii_lowercase();
    let search_focused = detail_input == Some(DetailInput::Search);

    let fields: Vec<AnyElement> = draft
        .fields
        .iter()
        .enumerate()
        .filter(|(_, (name, _, _))| {
            search.is_empty() || name.to_ascii_lowercase().contains(&search)
        })
        .map(|(index, (name, input, is_pk))| {
            let focused = detail_input == Some(DetailInput::Field(index));
            let original = originals.get(index);
            let allow_empty = original.map(allows_empty_token).unwrap_or(true);
            div()
                .id(("detail-field", index))
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(field_header(
                    index,
                    name,
                    *is_pk,
                    open_menu == Some(index),
                    allow_empty,
                    theme,
                    cx,
                ))
                .child(if *is_pk {
                    read_only_field(input.text(), true, theme).into_any_element()
                } else {
                    text_field(
                        ("detail-field-input", index),
                        input,
                        InputTarget::DetailField(index),
                        focused,
                        None,
                        theme,
                        cx,
                    )
                })
                .into_any_element()
        })
        .collect();

    let message = draft.message.as_ref().map(|(ok, text)| {
        div()
            .text_size(px(11.))
            .text_color(if *ok { theme.success } else { theme.danger })
            .child(SharedString::from(text.clone()))
    });

    div()
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .min_w(px(0.))
        .child(text_field(
            "detail-field-search",
            &draft.field_search,
            InputTarget::DetailSearch,
            search_focused,
            Some("Search for field…"),
            theme,
            cx,
        ))
        .children(fields)
        .children(message)
        .into_any_element()
}

fn field_header(
    index: usize,
    name: &str,
    is_pk: bool,
    menu_open: bool,
    allow_empty: bool,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let label_color = if is_pk {
        theme.warning
    } else {
        theme.text_muted
    };

    let mut header = div()
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .text_color(label_color)
                .text_size(metrics::text_size_small())
                .child(SharedString::from(name.to_string())),
        );

    if !is_pk {
        header = header.child(
            div()
                .id(("detail-value-menu-btn", index))
                .px_1()
                .rounded_sm()
                .text_size(metrics::text_size_small())
                .text_color(if menu_open {
                    theme.text
                } else {
                    theme.text_faint
                })
                .cursor_pointer()
                .hover(|btn| btn.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.toggle_detail_value_menu(index, cx);
                }))
                .child("▾"),
        );
    }

    if menu_open && !is_pk {
        header = header.child(special_value_menu(index, allow_empty, theme, cx));
    }

    header.into_any_element()
}

fn special_value_menu(
    index: usize,
    allow_empty: bool,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    let items: &[(&'static str, &str, bool)] = &[
        ("NULL", "SQL NULL", true),
        ("EMPTY", "Empty string", allow_empty),
        ("DEFAULT", "Column default", true),
    ];
    for (item_index, &(token, hint, enabled)) in items.iter().enumerate() {
        if !enabled {
            continue;
        }
        rows.push(
            div()
                .id(("detail-value-menu-item", index * 8 + item_index))
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|row| row.bg(theme.hover))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.set_detail_special_value(index, token, cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .font_family(metrics::MONO_FONT)
                                .text_color(theme.text)
                                .child(token),
                        )
                        .child(
                            div()
                                .text_size(metrics::text_size_small())
                                .text_color(theme.text_faint)
                                .child(hint),
                        ),
                )
                .into_any_element(),
        );
    }

    deferred(
        div()
            .id(("detail-value-menu", index))
            .absolute()
            .top_full()
            .right_0()
            .mt_1()
            .min_w(px(160.))
            .flex()
            .flex_col()
            .py_1()
            .rounded_md()
            .bg(theme.elevated)
            .border_1()
            .border_color(theme.border)
            .occlude()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_detail_value_menu(cx);
            }))
            .children(rows),
    )
    .into_any_element()
}

fn allows_empty_token(value: &Value) -> bool {
    matches!(
        value,
        Value::Text(_)
            | Value::Json(_)
            | Value::Uuid(_)
            | Value::Temporal(_)
            | Value::Decimal(_)
            | Value::Null
            | Value::Default
    )
}

fn read_only_field(text: &str, muted: bool, theme: &Theme) -> AnyElement {
    let display = json_format::display_text(text);
    let color = if muted {
        theme.text_faint
    } else {
        theme.text
    };

    if !display.contains('\n') && !display.contains('\r') {
        return div()
            .w_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .h(px(28.))
            .px_2()
            .rounded_md()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .font_family(metrics::MONO_FONT)
            .overflow_hidden()
            .text_color(color)
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(SharedString::from(display)),
            )
            .into_any_element();
    }

    let spans = json_format::highlight_spans(&display);
    let lines: Vec<&str> = display.split('\n').collect();
    let visible = lines.len().min(8).max(1);
    let line_h = px(18.);
    // Lines + `py_1` + `border_1`; without the border the last line was clipped.
    let height = px(18. * visible as f32 + 8. + 2.);

    let mut consumed = 0usize;
    let painted: Vec<AnyElement> = lines
        .into_iter()
        .take(8)
        .map(|line| {
            let line_start = consumed;
            let line_end = consumed + line.len();
            let line_range = line_start..line_end;
            consumed = line_end + 1;

            let line_styles = spans
                .as_ref()
                .map(|all| json_format::styles_on_line(all, &line_range))
                .unwrap_or_default();

            div()
                .h(line_h)
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .children(read_only_line_chunks(
                    line,
                    &line_styles,
                    if muted { theme.text_faint } else { theme.text },
                    theme,
                ))
                .into_any_element()
        })
        .collect();

    div()
        .w_full()
        .min_w(px(0.))
        .h(height)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.background)
        .border_1()
        .border_color(theme.border)
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .overflow_hidden()
        .child(
            div()
                .w_full()
                .h_full()
                .min_w(px(0.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(painted),
        )
        .into_any_element()
}

fn read_only_line_chunks(
    line: &str,
    styles: &[(usize, usize, JsonStyle)],
    fallback: gpui::Rgba,
    theme: &Theme,
) -> Vec<AnyElement> {
    if line.is_empty() {
        return vec![div().child(SharedString::from(" ")).into_any_element()];
    }
    if styles.is_empty() {
        return vec![div()
            .text_color(fallback)
            .child(SharedString::from(line.to_string()))
            .into_any_element()];
    }

    let mut cuts = vec![0usize, line.len()];
    for &(start, end, _) in styles {
        cuts.push(start.min(line.len()));
        cuts.push(end.min(line.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::new();
    for window in cuts.windows(2) {
        let start = window[0];
        let end = window[1];
        if start >= end {
            continue;
        }
        let color = styles
            .iter()
            .find(|&&(s, e, _)| start >= s && start < e)
            .map(|&(_, _, style)| style.color(theme))
            .unwrap_or(fallback);
        out.push(
            div()
                .text_color(color)
                .child(SharedString::from(line[start..end].to_string()))
                .into_any_element(),
        );
    }
    out
}
