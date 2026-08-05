//! Right-hand detail panel for the selected row.

use super::caption;
use super::text_field::{text_field, DetailInput, InputTarget};
use crate::json_format::{self, JsonStyle};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::{metrics, Theme};
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};

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

        let body = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft,
                selected_row,
                ..
            })
            | Some(WorkspaceTab::Sql {
                draft,
                selected_row,
                ..
            }) => {
                if let Some(draft) = draft.as_ref() {
                    render_table_draft(draft, self.detail_input, theme, cx)
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
            div()
                .id(("detail-field", index))
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_color(if *is_pk {
                            theme.warning
                        } else {
                            theme.text_muted
                        })
                        .text_size(metrics::text_size_small())
                        .child(SharedString::from(name.clone())),
                )
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
    let height = px(18. * visible as f32 + 8.);

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
