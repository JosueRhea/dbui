//! Rendering, split by surface.
//!
//! Each module is an `impl DbUi` block holding the render methods for one part
//! of the window. They read state and attach listeners; they never own state.
//! That lives on [`DbUi`](crate::root::DbUi), which is what keeps "what the app
//! knows" in one file instead of spread across the widgets that draw it.

mod bottom_bar;
mod change_bubble;
mod connection_form;
pub(crate) mod context_menu;
mod detail_sidebar;
mod filters;
mod grid;
mod icons;
mod main_pane;
pub(crate) mod palette;
mod sidebar;
mod status_bar;
mod tabs;
pub(crate) mod text_field;
mod titlebar;

pub use connection_form::{ConnectionForm, FormAction};
pub use text_field::DetailInput;

use crate::theme::Theme;
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
        .h(px(26.))
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
        .text_size(px(11.))
        .child(text.into())
}

/// A filled dot -- the connection-status light in the sidebar.
pub(crate) fn dot(color: gpui::Rgba) -> Div {
    div().w(px(7.)).h(px(7.)).rounded_full().bg(color)
}
