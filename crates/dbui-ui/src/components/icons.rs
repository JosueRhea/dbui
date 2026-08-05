//! Small geometric icons — Unicode stand-ins render as tofu with SF Pro.

use dbui_app::domain::TableKind;
use gpui::{div, prelude::*, px, IntoElement, Rgba};

/// Grid-with-header mark for a base table.
pub(crate) fn table_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(12.))
        .h(px(11.))
        .flex_none()
        .relative()
        .rounded(px(1.5))
        .border_1()
        .border_color(color)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .w_full()
                .h(px(3.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(6.5))
                .w_full()
                .h(px(1.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(3.5))
                .top(px(3.))
                .w(px(1.))
                .h(px(8.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(7.5))
                .top(px(3.))
                .w(px(1.))
                .h(px(8.))
                .bg(color),
        )
}

/// Open grid (no filled header) for views / materialized views.
pub(crate) fn view_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(12.))
        .h(px(11.))
        .flex_none()
        .relative()
        .rounded(px(1.5))
        .border_1()
        .border_color(color)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(3.))
                .w_full()
                .h(px(1.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(6.5))
                .w_full()
                .h(px(1.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(5.5))
                .top_0()
                .w(px(1.))
                .h(px(11.))
                .bg(color),
        )
}

/// SQL editor tab: three staggered lines suggesting a query.
pub(crate) fn sql_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(12.))
        .h(px(11.))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .left(px(1.))
                .top(px(2.))
                .w(px(10.))
                .h(px(1.5))
                .rounded(px(0.5))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(1.))
                .top(px(5.))
                .w(px(7.))
                .h(px(1.5))
                .rounded(px(0.5))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(1.))
                .top(px(8.))
                .w(px(9.))
                .h(px(1.5))
                .rounded(px(0.5))
                .bg(color),
        )
}

/// Small rounded command mark for palette action rows.
pub(crate) fn command_mark(color: Rgba) -> impl IntoElement {
    div()
        .w(px(18.))
        .h(px(18.))
        .flex_none()
        .rounded(px(5.))
        .border_1()
        .border_color(color)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(px(8.))
                .h(px(2.))
                .rounded(px(1.))
                .bg(color),
        )
}

/// Swatch for theme rows in the palette.
pub(crate) fn theme_mark(color: Rgba) -> impl IntoElement {
    div()
        .w(px(18.))
        .h(px(18.))
        .flex_none()
        .rounded(px(5.))
        .border_1()
        .border_color(color)
        .flex()
        .items_center()
        .justify_center()
        .child(div().w(px(8.)).h(px(8.)).rounded_full().bg(color))
}

pub(crate) fn kind_icon(kind: TableKind, color: Rgba) -> gpui::AnyElement {
    match kind {
        TableKind::Table => table_icon(color).into_any_element(),
        TableKind::View | TableKind::MaterializedView => view_icon(color).into_any_element(),
    }
}
