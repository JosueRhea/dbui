//! The toolbar over the grid: pane switcher, filters, paging, row summary.
//!
//! It sits above the content rather than under it. What the buttons here do is
//! decide what the grid shows, and a control that changes what you are looking
//! at belongs on the same edge as the thing it changes -- reading a table and
//! then hunting along the bottom of the window for "Structure" was two
//! journeys for one thought.

use super::icons::{columns_icon, funnel_icon, plus_icon, table_icon, view_icon, RefreshIcon};
use super::text_field::{text_field, InputTarget};
use super::{
    caption, icon_button, menu_row, menu_surface, motion, toolbar_button, toolbar_icon_color,
};
use crate::root::{DbUi, Focus, ResultSource};
use crate::tabs::{TablePane, WorkspaceTab};
use crate::theme::metrics;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString,
};

/// What the rows-per-page dropdown offers. The field beside it still takes any
/// number -- these are the ones worth one press rather than five keystrokes.
const PAGE_SIZE_PRESETS: [u32; 5] = [100, 200, 500, 1_000, 5_000];

impl DbUi {
    pub(crate) fn render_toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let Some(tab) = self.tabs.active() else {
            return div()
                .flex()
                .items_center()
                .px_3()
                .h(metrics::toolbar_height())
                .flex_shrink_0()
                .bg(theme.panel)
                .border_b_1()
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
            WorkspaceTab::Sql { .. } => (
                false,
                TablePane::Data,
                false,
                false,
                false,
                true,
                true,
                None,
                0,
            ),
        };

        let page_size_focused = self.focus == Focus::PageSize && self.page_size_focus;
        let data_active = pane == TablePane::Data;
        let structure_active = pane == TablePane::Structure;

        div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .h(metrics::toolbar_height())
            .flex_shrink_0()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .when(is_table, |bar| {
                bar.child(
                    toolbar_button(
                        "pane-data",
                        table_icon(toolbar_icon_color(theme, data_active)),
                        "Data",
                        theme,
                        data_active,
                    )
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.set_table_pane(TablePane::Data, cx);
                    })),
                )
                .child(
                    toolbar_button(
                        "pane-structure",
                        view_icon(toolbar_icon_color(theme, structure_active)),
                        "Structure",
                        theme,
                        structure_active,
                    )
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.set_table_pane(TablePane::Structure, cx);
                    })),
                )
                .child(
                    toolbar_button(
                        "toggle-filters",
                        funnel_icon(toolbar_icon_color(theme, filters_open)),
                        "Filters",
                        theme,
                        filters_open,
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_filters_open(cx))),
                )
                .child(
                    toolbar_button(
                        "toggle-columns",
                        columns_icon(toolbar_icon_color(theme, columns_open)),
                        "Columns",
                        theme,
                        columns_open,
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle_columns_open(cx))),
                )
                .child(
                    toolbar_button(
                        "add-row",
                        plus_icon(toolbar_icon_color(theme, false)),
                        "Row",
                        theme,
                        false,
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.add_row(cx))),
                )
            })
            .child(div().flex_1().min_w(px(0.)))
            .child(
                div()
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .text_size(metrics::text_size_small())
                    .child(summary),
            )
            // Reloading is about the result, so it sits with the controls that
            // page through it rather than in the tab strip it used to.
            .child(
                icon_button(
                    "refresh-result",
                    motion::spin(
                        "refresh-result-spin",
                        RefreshIcon::new(theme.text_muted),
                        self.result_refreshes,
                    ),
                    theme,
                    false,
                )
                .on_click(cx.listener(|this, _, _window, cx| this.refresh_result(cx))),
            )
            .when(paging, |bar| {
                let draft = page_size_draft.expect("table tab has page size draft");
                bar.child(page_button(
                    "toolbar-page-prev",
                    "‹",
                    theme,
                    at_start,
                    cx.listener(|this, _, _window, cx| this.page(false, cx)),
                ))
                .child(page_button(
                    "toolbar-page-next",
                    "›",
                    theme,
                    at_end,
                    cx.listener(|this, _, _window, cx| this.page(true, cx)),
                ))
                .child(self.render_page_size(
                    draft,
                    page_size_focused,
                    page_limit,
                    cx,
                ))
            })
    }

    /// Rows per page: a field you can type into, wearing a select's chevron.
    ///
    /// Both halves earn their place. The presets are what almost everyone
    /// wants and are one press; the field is the only way to ask for 37, and
    /// removing it to make the control a plain dropdown would have taken a
    /// working feature away to match a picture.
    fn render_page_size(
        &self,
        draft: &crate::text_input::TextInput,
        focused: bool,
        limit: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let open = self.page_size_menu_open;

        let rows: Vec<AnyElement> = PAGE_SIZE_PRESETS
            .iter()
            .map(|&size| {
                menu_row(
                    ("page-size-preset", size as usize),
                    SharedString::from(size.to_string()),
                    (size == limit).then_some("•"),
                    theme,
                )
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.close_page_size_menu(cx);
                    this.set_page_size(size, cx);
                }))
                .into_any_element()
            })
            .collect();

        div()
            .relative()
            .flex_shrink_0()
            .flex()
            .items_center()
            .child(
                div()
                    .w(metrics::scaled(60.))
                    .flex_shrink_0()
                    .child(text_field(
                        "page-size-input",
                        draft,
                        InputTarget::PageSize,
                        focused,
                        Some(&limit.to_string()),
                        theme,
                        cx,
                    )),
            )
            .child(
                icon_button("page-size-menu-btn", "⌄", theme, open).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_page_size_menu(cx);
                    }),
                ),
            )
            .children(open.then(|| {
                deferred(motion::menu(
                    "page-size-menu-in",
                    menu_surface("page-size-menu", theme)
                        .top_full()
                        .right_0()
                        .mt_1()
                        .w(metrics::scaled(140.))
                        .text_size(metrics::text_size_small())
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.close_page_size_menu(cx);
                        }))
                        .child(
                            div()
                                .px_3()
                                .pb_1()
                                .text_color(theme.text_faint)
                                .child("Rows / page"),
                        )
                        .children(rows),
                    metrics::scaled(4.),
                ))
            }))
            .into_any_element()
    }
}

fn page_button(
    id: &'static str,
    label: &'static str,
    theme: &crate::theme::Theme,
    disabled: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let base = icon_button(id, label, theme, false);
    if disabled {
        base.text_color(theme.text_faint).cursor_default()
    } else {
        base.on_click(on_click)
    }
}
