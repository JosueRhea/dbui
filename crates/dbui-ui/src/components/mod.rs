//! Rendering, split by surface.
//!
//! Each module is an `impl DbUi` block holding the render methods for one part
//! of the window. They read state and attach listeners; they never own state.
//! That lives on [`DbUi`](crate::root::DbUi), which is what keeps "what the app
//! knows" in one file instead of spread across the widgets that draw it.

mod change_bubble;
pub(crate) mod close_guard;
pub(crate) mod connection_form;
pub(crate) mod context_menu;
mod detail_sidebar;
mod drag_ghost;
pub(crate) mod editor_find;
mod filters;
mod grid;
mod icons;
mod main_pane;
pub(crate) mod motion;
pub(crate) mod palette;
pub(crate) mod schema_sheet;
pub(crate) mod scrollbar;
mod sidebar;
mod status_bar;
mod tabs;
pub(crate) mod text_field;
mod titlebar;
mod toolbar;
mod transfer;

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

/// A square ghost button holding one glyph or drawn icon.
///
/// The chrome's workhorse: pagination arrows, the titlebar gear, the detail
/// panel's overflow menu. `active` is for the ones that latch -- a menu button
/// stays lit while its menu is open, or pressing it reads as having done
/// nothing.
pub(crate) fn icon_button(
    id: impl Into<ElementId>,
    icon: impl IntoElement,
    theme: &Theme,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .w(metrics::control_height())
        .h(metrics::control_height())
        .flex_shrink_0()
        .rounded_md()
        .cursor_pointer()
        .text_color(if active { theme.text } else { theme.text_muted })
        .when(active, |btn| btn.bg(theme.selection))
        .hover(|style| style.bg(theme.hover).text_color(theme.text))
        .child(icon)
}

/// A toolbar control: an icon, then its label.
///
/// `active` fills it with the accent -- the segmented pane switcher and the
/// latching toggles beside it use the same treatment, so "this is the mode you
/// are in" and "this panel is open" read the same way.
pub(crate) fn toolbar_button(
    id: impl Into<ElementId>,
    icon: impl IntoElement,
    label: impl Into<SharedString>,
    theme: &Theme,
    active: bool,
) -> Stateful<Div> {
    let base = div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .px_2p5()
        .h(metrics::control_height())
        .flex_shrink_0()
        .rounded_md()
        .cursor_pointer()
        .border_1()
        .text_size(metrics::text_size_small())
        .child(icon)
        .child(label.into());

    if active {
        base.bg(theme.accent)
            .border_color(theme.accent)
            .text_color(theme.text_on_accent)
            .hover(|style| style.bg(theme.accent_hover))
    } else {
        base.bg(theme.elevated)
            .border_color(theme.border)
            .text_color(theme.text_muted)
            .hover(|style| style.bg(theme.hover).text_color(theme.text))
    }
}

/// The colour an icon inside a [`toolbar_button`] should be drawn in.
///
/// Icons here are shapes, not glyphs, so they take a colour rather than
/// inheriting one -- and an icon left the wrong colour on a filled button is
/// invisible rather than merely wrong.
pub(crate) fn toolbar_icon_color(theme: &Theme, active: bool) -> gpui::Rgba {
    if active {
        theme.text_on_accent
    } else {
        theme.text_muted
    }
}

/// A column's type, drawn beside its name.
///
/// Small, faint and monospaced: it is what the value *is*, which matters when
/// you are about to type a replacement for it, and never the thing being read.
pub(crate) fn type_badge(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex_shrink_0()
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::scaled(10.))
        .text_color(theme.text_faint)
        .child(text.into())
}

/// The floating surface every dropdown in the app is drawn on.
pub(crate) fn menu_surface(id: impl Into<ElementId>, theme: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .absolute()
        .flex()
        .flex_col()
        .py_1()
        .rounded(px(8.))
        .bg(theme.elevated)
        .border_1()
        .border_color(theme.border)
        .occlude()
}

/// One row of a [`menu_surface`]. The caller attaches `.on_click`.
pub(crate) fn menu_row(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    shortcut: Option<&str>,
    theme: &Theme,
) -> Stateful<Div> {
    let shortcut = shortcut.map(|keys| {
        div()
            .flex_shrink_0()
            .text_size(metrics::text_size_small())
            .text_color(theme.text_faint)
            .child(SharedString::from(keys.to_string()))
    });
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .px_3()
        .py_1()
        .cursor_pointer()
        .text_color(theme.text)
        .hover(|row| row.bg(theme.hover))
        .child(div().child(label.into()))
        .children(shortcut)
}
