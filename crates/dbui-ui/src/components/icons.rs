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
        .child(div().w(px(8.)).h(px(2.)).rounded(px(1.)).bg(color))
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

/// Stacked-discs mark for a database / connection.
pub(crate) fn database_icon(color: Rgba) -> impl IntoElement {
    let band = move |top: f32| {
        div()
            .absolute()
            .left_0()
            .top(px(top))
            .w_full()
            .h(px(1.))
            .bg(color)
    };
    div()
        .w(px(11.))
        .h(px(12.))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(0.5))
                .w_full()
                .h(px(11.))
                .rounded(px(4.))
                .border_1()
                .border_color(color),
        )
        .child(band(4.))
        .child(band(8.))
}

/// Magnifier for a search field.
pub(crate) fn search_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(12.))
        .h(px(12.))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .left_0()
                .top_0()
                .w(px(9.))
                .h(px(9.))
                .rounded_full()
                .border_1()
                .border_color(color),
        )
        // The handle, drawn as a stubby diagonal-ish tail. A real diagonal
        // needs a rotation this element set does not have, so two short bars
        // stand in for one -- at 12px the difference does not survive.
        .child(
            div()
                .absolute()
                .left(px(7.5))
                .top(px(8.))
                .w(px(3.5))
                .h(px(1.5))
                .rounded(px(0.75))
                .bg(color),
        )
}

/// Settings mark: three rails with a knob on each.
///
/// Not a gear. A gear needs eight teeth to read as one, and eight teeth need
/// four of them on the diagonals -- which this element set cannot draw without
/// a rotation. Four teeth on a thin ring is the crosshair every scope in every
/// game uses, and that is exactly what it looked like. Sliders say "settings"
/// at 14px with no ambiguity and no diagonals.
pub(crate) fn settings_icon(color: Rgba) -> impl IntoElement {
    let rail = move |top: f32| {
        div()
            .absolute()
            .left_0()
            .top(px(top))
            .w(px(14.))
            .h(px(1.5))
            .rounded(px(0.75))
            .bg(color)
    };
    let knob = move |left: f32, top: f32| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(3.))
            .h(px(5.))
            .rounded(px(1.))
            .bg(color)
    };
    div()
        .w(px(14.))
        .h(px(14.))
        .flex_none()
        .relative()
        .child(rail(1.75))
        .child(rail(6.25))
        .child(rail(10.75))
        .child(knob(8.5, 0.))
        .child(knob(2.5, 4.5))
        .child(knob(6.5, 9.))
}

/// Funnel for the filter toggle.
pub(crate) fn funnel_icon(color: Rgba) -> impl IntoElement {
    let bar = move |left: f32, top: f32, w: f32| {
        div()
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(w))
            .h(px(1.5))
            .rounded(px(0.75))
            .bg(color)
    };
    div()
        .w(px(12.))
        .h(px(11.))
        .flex_none()
        .relative()
        .child(bar(0.5, 1., 11.))
        .child(bar(2.5, 4.5, 7.))
        .child(bar(4.5, 8., 3.))
}

/// Three vertical bars for the column picker.
pub(crate) fn columns_icon(color: Rgba) -> impl IntoElement {
    let bar = move |left: f32| {
        div()
            .absolute()
            .left(px(left))
            .top_0()
            .w(px(2.5))
            .h(px(11.))
            .rounded(px(1.))
            .bg(color)
    };
    div()
        .w(px(12.))
        .h(px(11.))
        .flex_none()
        .relative()
        .child(bar(0.))
        .child(bar(4.75))
        .child(bar(9.5))
}

/// Calendar page, for a date / timestamp field.
pub(crate) fn calendar_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(12.))
        .h(px(12.))
        .flex_none()
        .relative()
        .rounded(px(2.))
        .border_1()
        .border_color(color)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(2.5))
                .w_full()
                .h(px(1.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(2.))
                .top(px(5.5))
                .w(px(2.))
                .h(px(2.))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(6.))
                .top(px(5.5))
                .w(px(2.))
                .h(px(2.))
                .bg(color),
        )
}

/// A filled square, the stop sign every media control agrees on.
pub(crate) fn stop_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(14.))
        .h(px(14.))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .child(div().w(px(9.)).h(px(9.)).rounded(px(2.)).bg(color))
}

/// Plus, for "add a row" and "new tab".
pub(crate) fn plus_icon(color: Rgba) -> impl IntoElement {
    div()
        .w(px(11.))
        .h(px(11.))
        .flex_none()
        .relative()
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(4.75))
                .w(px(11.))
                .h(px(1.5))
                .rounded(px(0.75))
                .bg(color),
        )
        .child(
            div()
                .absolute()
                .left(px(4.75))
                .top_0()
                .w(px(1.5))
                .h(px(11.))
                .rounded(px(0.75))
                .bg(color),
        )
}

/// A circular arrow, for reloading.
///
/// Drawn as a path rather than borrowed from a font: `↻` comes out of SF Pro
/// a size smaller and a weight lighter than the drawn icons it sits beside.
#[derive(IntoElement)]
pub(crate) struct RefreshIcon {
    color: Rgba,
    /// Rotation in whole turns -- the button spins once when pressed.
    turn: f32,
}

impl RefreshIcon {
    pub(crate) fn new(color: Rgba) -> Self {
        Self { color, turn: 0. }
    }

    pub(crate) fn turn(mut self, turn: f32) -> Self {
        self.turn = turn;
        self
    }
}

impl RenderOnce for RefreshIcon {
    fn render(self, _: &mut gpui::Window, _: &mut gpui::App) -> impl IntoElement {
        const SIZE: f32 = 14.;
        const RADIUS: f32 = 5.;
        const STROKE: f32 = 1.5;
        // The arc runs clockwise from just above three o'clock, round the
        // bottom, to twelve -- leaving the top-right open for the arrowhead
        // to point into.
        const START: f32 = -10.;
        const END: f32 = 260.;
        let Self { color, turn } = self;
        gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let spin = turn * 360.;
                let centre = bounds.center();
                let at = |degrees: f32| {
                    let radians = (degrees + spin).to_radians();
                    gpui::point(
                        centre.x + px(RADIUS * radians.cos()),
                        centre.y + px(RADIUS * radians.sin()),
                    )
                };

                let mut arc = gpui::PathBuilder::stroke(px(STROKE));
                arc.move_to(at(START));
                arc.arc_to(
                    gpui::point(px(RADIUS), px(RADIUS)),
                    px(0.),
                    true,
                    true,
                    at(END),
                );
                if let Ok(path) = arc.build() {
                    window.paint_path(path, color);
                }

                // The head sits on the arc's end, pointing along it:
                // clockwise, which at twelve o'clock is to the right.
                let end = (END + spin).to_radians();
                let (out_x, out_y) = (end.cos(), end.sin());
                let (ahead_x, ahead_y) = (-out_y, out_x);
                let along =
                    |distance: f32| gpui::point(px(ahead_x * distance), px(ahead_y * distance));
                let across =
                    |distance: f32| gpui::point(px(out_x * distance), px(out_y * distance));
                let tip = at(END) + along(3.2);
                let base = at(END) - along(0.6);
                let mut head = gpui::PathBuilder::fill();
                head.add_polygon(&[tip, base + across(2.7), base - across(2.7)], true);
                if let Ok(path) = head.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .w(px(SIZE))
        .h(px(SIZE))
        .flex_none()
    }
}
