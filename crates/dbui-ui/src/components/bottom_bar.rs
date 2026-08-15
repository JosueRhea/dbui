//! Toolbar under the grid: pane switcher, filters, paging, row summary.

use super::text_field::{text_field, InputTarget};
use super::{button, caption};
use crate::root::{DbUi, Focus, ResultSource};
use crate::tabs::{TablePane, WorkspaceTab};
use crate::theme::metrics;
use gpui::{div, prelude::*, px, ClickEvent, Context, SharedString, Window};

fn mode_button(
    id: &'static str,
    label: &'static str,
    theme: &crate::theme::Theme,
    active: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .h(px(26.))
        .rounded_md()
        .cursor_pointer()
        .bg(if active { theme.selection } else { theme.elevated })
        .text_color(if active { theme.text } else { theme.text_muted })
        .border_1()
        .border_color(if active { theme.accent } else { theme.border })
        .hover(|style| style.bg(if active { theme.selection } else { theme.hover }))
        .on_click(on_click)
        .child(label)
}

fn page_button(
    id: &'static str,
    label: &'static str,
    theme: &crate::theme::Theme,
    disabled: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let base = button(id, label, theme, false);
    if disabled {
        base.text_color(theme.text_faint).cursor_default()
    } else {
        base.on_click(on_click)
    }
}

impl DbUi {
    pub(crate) fn render_bottom_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let Some(tab) = self.tabs.active() else {
            return div()
                .flex()
                .items_center()
                .px_3()
                .h(metrics::toolbar_height())
                .flex_shrink_0()
                .bg(theme.panel)
                .border_t_1()
                .border_color(theme.border)
                .child(caption("No tab selected", theme));
        };

        let base_summary = tab
            .result()
            .map(|view| view.summary.clone())
            .unwrap_or_else(|| "No data".to_string());
        // Only worth saying past one row: a single selected row is the row the
        // detail sidebar is already describing.
        let selected = tab.selection().len();
        let summary: SharedString = if selected > 1 {
            SharedString::from(format!("{base_summary} · {selected} selected"))
        } else {
            SharedString::from(base_summary)
        };

        let (
            is_table,
            pane,
            filters_open,
            columns_open,
            paging,
            at_start,
            at_end,
            page_size_draft,
            page_limit,
        ) = match tab {
            WorkspaceTab::Table {
                pane,
                filters_open,
                columns_open,
                page,
                page_size_draft,
                result,
                ..
            } => {
                let (at_start, at_end) = match result.as_ref().map(|view| &view.source) {
                    Some(ResultSource::Table {
                        page: tab_page,
                        total_rows,
                        ..
                    }) => (
                        tab_page.offset == 0,
                        total_rows
                            .map(|total| {
                                tab_page.offset + u64::from(tab_page.limit) >= total.max(0) as u64
                            })
                            .unwrap_or(false),
                    ),
                    Some(_) => (page.offset == 0, false),
                    None => (true, true),
                };
                (
                    true,
                    *pane,
                    *filters_open,
                    *columns_open,
                    true,
                    at_start,
                    at_end,
                    Some(page_size_draft),
                    page.limit,
                )
            }
            WorkspaceTab::Sql { .. } => {
                (false, TablePane::Data, false, false, false, true, true, None, 0)
            }
        };

        let page_size_focused = self.focus == Focus::PageSize && self.page_size_focus;

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(metrics::toolbar_height())
            .flex_shrink_0()
            .bg(theme.panel)
            .border_t_1()
            .border_color(theme.border)
            .when(is_table, |bar| {
                bar.child(mode_button(
                    "pane-data",
                    "Data",
                    theme,
                    pane == TablePane::Data,
                    cx.listener(|this, _, _window, cx| {
                        this.set_table_pane(TablePane::Data, cx);
                    }),
                ))
                .child(mode_button(
                    "pane-structure",
                    "Structure",
                    theme,
                    pane == TablePane::Structure,
                    cx.listener(|this, _, _window, cx| {
                        this.set_table_pane(TablePane::Structure, cx);
                    }),
                ))
            })
            .when(is_table, |bar| {
                bar.child(mode_button(
                    "toggle-filters",
                    "Filters",
                    theme,
                    filters_open,
                    cx.listener(|this, _, _window, cx| this.toggle_filters_open(cx)),
                ))
                .child(mode_button(
                    "toggle-columns",
                    "Columns",
                    theme,
                    columns_open,
                    cx.listener(|this, _, _window, cx| this.toggle_columns_open(cx)),
                ))
                .child(
                    button("add-row", "+ Row", theme, false).on_click(
                        cx.listener(|this, _, _window, cx| this.add_row(cx)),
                    ),
                )
            })
            .child(
                div()
                    .flex_1()
                    .text_center()
                    .text_color(theme.text_muted)
                    .text_size(metrics::text_size_small())
                    .child(summary),
            )
            .when(paging, |bar| {
                let draft = page_size_draft.expect("table tab has page size draft");
                bar.child(caption("Rows / page", theme))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_shrink_0()
                            .child(text_field(
                                "page-size-input",
                                draft,
                                InputTarget::PageSize,
                                page_size_focused,
                                Some(&page_limit.to_string()),
                                theme,
                                cx,
                            )),
                    )
                    .child(page_button(
                        "bottom-page-prev",
                        "‹",
                        theme,
                        at_start,
                        cx.listener(|this, _, _window, cx| this.page(false, cx)),
                    ))
                    .child(page_button(
                        "bottom-page-next",
                        "›",
                        theme,
                        at_end,
                        cx.listener(|this, _, _window, cx| this.page(true, cx)),
                    ))
            })
    }
}
