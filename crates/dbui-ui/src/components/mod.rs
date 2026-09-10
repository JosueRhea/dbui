//! Rendering, split by surface.
//!
//! Each module is an `impl DbUi` block holding the render methods for one part
//! of the window. They read state and attach listeners; they never own state.
//! That lives on [`DbUi`](crate::root::DbUi), which is what keeps "what the app
//! knows" in one file instead of spread across the widgets that draw it.

mod bottom_bar;
mod change_bubble;
pub(crate) mod close_guard;
mod connection_form;
pub(crate) mod context_menu;
mod detail_sidebar;
mod filters;
mod grid;
mod icons;
mod main_pane;
pub(crate) mod palette;
pub(crate) mod scrollbar;
mod sidebar;
mod status_bar;
mod tabs;
pub(crate) mod text_field;
mod titlebar;

pub use connection_form::{ConnectionForm, FormAction};
pub use text_field::DetailInput;

use crate::theme::{metrics, Theme};
use gpui::{div, prelude::*, px, Div, ElementId, SharedString, Stateful};

/// A clickable button. The caller attaches `.on_click`.
///
/// `primary` fills it with the accent colour -- for the one action a surface
/// exists to perform. Everything else is a quiet outline, so the filled button
/// keeps meaning something.
pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
    primary: bool,
) -> Stateful<Div> {
    button_with_focus(id, label, theme, primary, false)
}

/// Like [`button`], with a visible focus ring for keyboard Tab cycles.
pub(crate) fn button_with_focus(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    theme: &Theme,
    primary: bool,
    focused: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .h(metrics::control_height())
        .rounded_md()
        .cursor_pointer()
        .border_1()
        .child(label.into());

    if primary {
        let ring = if focused {
            theme.text_on_accent
        } else {
            theme.accent
        };
        base.bg(theme.accent)
            .text_color(theme.text_on_accent)
            .border_color(ring)
            .hover(|style| style.bg(theme.accent_hover))
    } else {
        let ring = if focused { theme.accent } else { theme.border };
        base.bg(theme.elevated)
            .text_color(theme.text)
            .border_color(ring)
            .hover(|style| style.bg(theme.hover))
    }
}

/// A one-line label in the muted colour, for captions and empty states.
pub(crate) fn caption(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .text_color(theme.text_muted)
        .text_size(metrics::text_size_small())
        .child(text.into())
}

/// A vertical grab strip for resizing the panel beside it.
///
/// Five pixels wide and otherwise invisible: the panels already draw their own
/// borders, so this only has to be findable by the pointer. It lights up under
/// the cursor and stays lit for as long as the drag lasts -- the pointer
/// leaves the strip on the first frame, and without that the handle would
/// blink off the moment it started working.
///
/// The caller attaches `.on_mouse_down`, which is the half that knows which
/// panel is being dragged.
pub(crate) fn vertical_resize_handle(
    id: impl Into<ElementId>,
    dragging: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(5.))
        .h_full()
        .flex_shrink_0()
        .cursor_col_resize()
        .when(dragging, |strip| strip.bg(theme.accent))
        .hover(|strip| strip.bg(theme.hover))
}

/// A filled dot -- the connection-status light in the sidebar.
pub(crate) fn dot(color: gpui::Rgba) -> Div {
    div().w(px(7.)).h(px(7.)).rounded_full().bg(color)
}
