//! Filter strip: one freeform WHERE input for table tabs.

use super::button_with_focus;
use super::text_field::{text_field, InputTarget};
use crate::root::{DbUi, FilterFocus, Focus};
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{div, prelude::*, px, Context};

impl DbUi {
    pub(crate) fn render_filter_strip(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;

        let Some(WorkspaceTab::Table {
            filters_open,
            where_draft,
            ..
        }) = self.tabs.active()
        else {
            return div().id("filter-strip-empty");
        };

        if !*filters_open {
            return div().id("filter-strip-closed");
        }

        let filter_focus = self.filter_focus.filter(|_| self.focus == Focus::Filter);
        let where_focused = filter_focus == Some(FilterFocus::Where);
        let apply_focused = filter_focus == Some(FilterFocus::Apply);
        let clear_focused = filter_focus == Some(FilterFocus::Clear);

        div()
            .id("filter-strip")
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .flex_shrink_0()
            .bg(theme.elevated)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .text_color(theme.text_muted)
                    .font_family(metrics::MONO_FONT)
                    .text_size(metrics::text_size_small())
                    .child("WHERE"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .child(text_field(
                        "where-input",
                        where_draft,
                        InputTarget::WhereDraft,
                        where_focused,
                        Some("id = 1 AND …"),
                        theme,
                        cx,
                    )),
            )
            .child(
                button_with_focus("apply-filters", "Apply", theme, true, apply_focused)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.filter_focus = Some(FilterFocus::Apply);
                        this.focus = Focus::Filter;
                        this.apply_filters(cx);
                    })),
            )
            .child(
                button_with_focus("clear-filters", "Clear", theme, false, clear_focused)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.filter_focus = Some(FilterFocus::Clear);
                        this.focus = Focus::Filter;
                        this.clear_filters(cx);
                    })),
            )
    }
}
