//! Workspace tab strip. Drawn in the titlebar, beside the connection chips.
//!
//! Tabs are pills rather than a notched strip: they share the bar with the
//! connection chips and the search box, and one row of controls that all agree
//! on a shape reads as one bar instead of three widgets pushed together. The
//! front tab gets a filled surface and an accent bar along its top edge -- the
//! bar is what survives being read out of the corner of the eye.

use super::context_menu::ContextTarget;
use super::icons::{plus_icon, sql_icon, table_icon};
use super::{caption, dot, motion};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{
    div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, MouseMoveEvent,
    SharedString,
};

impl DbUi {
    pub(crate) fn render_tab_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.tabs.active;

        // The front tab's count comes the long way round so the dot and the
        // change bubble can never disagree: the bubble folds the open draft in
        // before counting, and a tab whose dot lit up only after the selection
        // moved would be a tab that looked clean while the bubble said
        // otherwise. Every other tab has already had its draft folded away.
        let active_changes = self.collect_batch_edits().len()
            + self.collect_batch_deletes().len()
            + self
                .tabs
                .active()
                .map(|tab| tab.pending_inserts().len())
                .unwrap_or(0);
        let chrome = self.chrome_theme();
        let theme = &chrome;

        let tabs: Vec<AnyElement> = self
            .tabs
            .items
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let is_active = index == active;
                let id = tab.id();
                let label = tab.label();
                let changes = if is_active {
                    active_changes
                } else {
                    tab.pending_change_count()
                };
                let icon_color = if is_active {
                    theme.text_muted
                } else {
                    theme.text_faint
                };
                let icon = match tab {
                    WorkspaceTab::Table { .. } => table_icon(icon_color).into_any_element(),
                    WorkspaceTab::Sql { .. } => sql_icon(icon_color).into_any_element(),
                };

                let dragging = self
                    .tab_drag
                    .as_ref()
                    .is_some_and(|drag| drag.id == id && drag.moved);

                let pill = div()
                    // Keyed on the tab, not the slot: dragging renumbers the
                    // slots, and an id that moved with them would hand the
                    // press and the release to two different elements.
                    .id(("workspace-tab", id as usize))
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .pl_2p5()
                    .pr_1()
                    .h(metrics::control_height())
                    .max_w(metrics::scaled(200.))
                    .rounded_md()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if is_active {
                        theme.border
                    } else {
                        gpui::rgba(0x00000000)
                    })
                    .when(is_active, |row| row.bg(theme.elevated))
                    .text_color(if is_active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .when(!is_active, |row| row.hover(|row| row.bg(theme.hover)))
                    // The tab in hand is lifted off the strip -- its copy rides
                    // the pointer -- and what stays behind is the slot the
                    // rest are making room for.
                    .when(dragging, |row| row.bg(theme.selection).opacity(0.4))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.activate_tab(index, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            this.begin_tab_drag(id, event.position.x);
                            cx.notify();
                        }),
                    )
                    // Crossing another tab with one in hand is what reorders
                    // the strip; the tab follows the pointer a slot at a time
                    // rather than being dropped somewhere at the end.
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                            this.drag_tab_over(index, event.position.x, cx);
                        }),
                    )
                    // Right-clicking a tab does not move the focus to it: the
                    // menu names the tab it was opened on, so pointing at one
                    // to close the rest must not first make it the one kept
                    // by accident.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                            this.open_context_menu(ContextTarget::Tab { id }, event.position, cx);
                        }),
                    )
                    .child(icon)
                    .child(div().truncate().child(SharedString::from(label)))
                    // Staged work the tab is holding, marked where the user
                    // decides which tab to close.
                    .children((changes > 0).then(|| dot(theme.warning)))
                    .child(
                        div()
                            .id(("workspace-tab-close", id as usize))
                            .px_1()
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .hover(|icon| icon.text_color(theme.danger))
                            .on_click(cx.listener(move |this, _, _window, cx| {
                                cx.stop_propagation();
                                this.close_tab(index, cx);
                            }))
                            .child("×"),
                    );
                motion::tab(("workspace-tab-in", id as usize), pill).into_any_element()
            })
            .collect();

        // The strip scrolls; the `+` does not. Keeping it pinned outside the
        // scroller is what makes it reachable no matter how many tabs are
        // open -- a new-tab button you have to scroll to find is one nobody
        // presses twice.
        // The underline is aimed from the strip's own record of where each
        // tab was laid out last frame, so it lands on the tab as drawn --
        // whatever its label made its width -- and travels there when the
        // front tab changes, or when the strip shuffles under it.
        let strip_bounds = self.tab_strip_scroll.bounds();
        // Last frame's record is only trusted if it was a strip of this many
        // tabs plus the one trailing element (the underline, or the frame
        // request below). Just after a close or an open it is not, and aiming
        // by it would send the underline to whichever tab used to be there.
        let count = tabs.len();
        let fresh = self.tab_strip_scroll.bounds_for_item(count).is_some()
            && self.tab_strip_scroll.bounds_for_item(count + 1).is_none();
        let target = (count > 0 && fresh)
            .then(|| self.tab_strip_scroll.bounds_for_item(active))
            .flatten()
            .map(|tab| {
                let inset = 8.;
                let left = f32::from(tab.left() - strip_bounds.left());
                let right = f32::from(tab.right() - strip_bounds.left());
                let top = tab.bottom() - strip_bounds.top();
                (
                    motion::Span {
                        left: left + inset,
                        right: (right - inset).max(left + inset),
                    },
                    top,
                )
            });
        if let Some((span, top)) = target {
            match self.tab_indicator.as_mut() {
                Some((slide, at)) => {
                    slide.aim(span);
                    *at = top;
                }
                None => self.tab_indicator = Some((motion::Slide::at(span), top)),
            }
        }
        // Asks for one more frame, which will have fresh bounds to aim by.
        // Carried *inside* the underline when there is one, so the strip
        // keeps exactly one trailing child and the underline is never swapped
        // out -- which would restart its slide.
        let another_frame = || {
            gpui::canvas(
                |_, window, _| window.request_animation_frame(),
                |_, _, _, _| {},
            )
            .size_0()
        };
        let indicator = match (&self.tab_indicator, count > 0) {
            (_, false) => None,
            (Some((slide, top)), true) => Some(
                slide
                    .render(
                        "tab-indicator",
                        div()
                            .absolute()
                            .top(*top + px(3.))
                            .h(px(2.))
                            .rounded_full()
                            .bg(self.theme.accent)
                            .when(!fresh, |bar| bar.child(another_frame())),
                    )
                    .into_any_element(),
            ),
            (None, true) => Some(another_frame().into_any_element()),
        };
        let chrome = self.chrome_theme();
        let theme = &chrome;

        let strip = if tabs.is_empty() {
            div()
                .id("workspace-tab-bar-empty")
                .flex()
                .items_center()
                .px_2()
                .flex_shrink_0()
                .child(caption("No tabs open", theme))
                .into_any_element()
        } else {
            div()
                .id("workspace-tab-bar")
                .track_scroll(&self.tab_strip_scroll)
                .relative()
                .flex()
                .items_center()
                .gap_1()
                // Room below the tabs for the underline, and as much above
                // so they stay centred in the bar.
                .py(px(6.))
                .min_w(px(0.))
                .overflow_x_scroll()
                .children(tabs)
                .children(indicator)
                .into_any_element()
        };

        div()
            .flex()
            .items_center()
            .gap_1()
            .min_w(px(0.))
            .child(strip)
            .child(
                super::icon_button("new-tab", plus_icon(theme.text_muted), theme, false).on_click(
                    cx.listener(|this, _, _window, cx| {
                        this.open_sql_tab(cx);
                    }),
                ),
            )
    }
}
