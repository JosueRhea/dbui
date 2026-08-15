//! The custom titlebar.
//!
//! The system one is hidden (`appears_transparent`) so the window reads as one
//! surface rather than a chrome bar stuck on top of an app. The left inset
//! keeps clear of the traffic lights, which are still drawn by the platform.
//!
//! Connection switching lives here (not in the sidebar): one tab per open
//! connection, TablePlus-style, with a `+` that drops down the list of saved
//! connections to open. The rest of the bar still owns native chrome — drag to
//! move, double-click to zoom.
//!
//! A tab is an open connection; the dropdown is every connection the user has
//! saved. Closing a tab therefore does not delete anything, which is why the
//! `×` and the picker's `✎`/`⏻` are different gestures with different reach.

use super::{caption, dot};
use crate::root::DbUi;
use crate::theme::{metrics, Theme};
use dbui_app::ConnectionStatus;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, SharedString, Window, WindowControlArea,
};
use std::cell::Cell;
use std::rc::Rc;

impl DbUi {
    pub(crate) fn render_titlebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let picker_open = self.connection_picker_open;
        let tabs = self.render_connection_tabs(cx);
        let theme = &self.theme;

        let should_move = Rc::new(Cell::new(false));
        let should_move_down = should_move.clone();
        let should_move_up = should_move.clone();
        let should_move_out = should_move.clone();
        let should_move_move = should_move.clone();

        div()
            .id("titlebar")
            .relative()
            .flex()
            .items_center()
            .h(metrics::titlebar_height())
            .flex_shrink_0()
            .pl(metrics::traffic_light_inset())
            .pr_3()
            .gap_2()
            .bg(theme.panel)
            .border_b_1()
            .border_color(theme.border)
            .text_color(theme.text_muted)
            .text_size(metrics::text_size_small())
            .child(tabs)
            // A read-only connection says so where the user is already
            // looking to tell which server they are on.
            .children(self.is_read_only().then(|| {
                div()
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(theme.elevated)
                    .border_1()
                    .border_color(theme.warning)
                    .text_color(theme.warning)
                    .text_size(px(10.))
                    .child("READ ONLY")
            }))
            .child(
                // The `+` is the only way to reach a connection that is not
                // already a tab, so it stays put rather than scrolling away
                // with the strip when the bar is full.
                div()
                    .id("connection-picker")
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .justify_center()
                    .w(px(24.))
                    .h(px(22.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(|s| s.bg(theme.hover).text_color(theme.text))
                    .when(picker_open, |s| s.bg(theme.selection))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_connection_picker(cx);
                        }),
                    )
                    .child("+")
                    .children(picker_open.then(|| self.render_connection_picker(cx))),
            )
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .h_full()
                    .window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(
                        MouseButton::Left,
                        move |_event: &MouseDownEvent, _window: &mut Window, _cx| {
                            should_move_down.set(true);
                        },
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        move |_event: &MouseUpEvent, _window: &mut Window, _cx| {
                            should_move_up.set(false);
                        },
                    )
                    .on_mouse_up_out(
                        MouseButton::Left,
                        move |_event: &MouseUpEvent, _window: &mut Window, _cx| {
                            should_move_out.set(false);
                        },
                    )
                    .on_mouse_move(move |_event: &MouseMoveEvent, window: &mut Window, _cx| {
                        if should_move_move.get() {
                            should_move_move.set(false);
                            start_titlebar_drag(window);
                        }
                    })
                    .on_click(move |event: &ClickEvent, window: &mut Window, _cx| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    }),
            )
    }

    /// One tab per open connection, in tab order.
    ///
    /// The strip is the only thing here allowed to grow, and it scrolls rather
    /// than pushing the `+` and the drag area off the end of the bar.
    fn render_connection_tabs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let active = self.workspace.active_id();

        if self.workspace.open_count() == 0 {
            return div()
                .flex()
                .items_center()
                .px_2()
                .flex_shrink_0()
                .child(caption("No connection open", theme))
                .into_any_element();
        }

        let tabs: Vec<AnyElement> = self
            .workspace
            .open_entries()
            .map(|entry| {
                let id = entry.id();
                let key = id.0 as usize;
                let is_active = active == Some(id);
                let light = status_color(&entry.status, theme);
                let name = SharedString::from(entry.config.name.clone());

                div()
                    .id(("connection-tab", key))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .pl_2()
                    .pr_1()
                    .py_1()
                    .max_w(px(200.))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(is_active, |tab| tab.bg(theme.selection))
                    .when(!is_active, |tab| tab.hover(|s| s.bg(theme.hover)))
                    .text_color(if is_active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_connection_tab(id, cx);
                        }),
                    )
                    .child(dot(light))
                    .child(div().truncate().child(name))
                    .child(
                        div()
                            .id(("connection-tab-close", key))
                            .px_1()
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.danger))
                            // Mouse-down rather than click, to match the tab
                            // itself -- otherwise the tab activates on the way
                            // down and only then closes on the way up.
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                    cx.stop_propagation();
                                    this.close_connection_tab(id, cx);
                                }),
                            )
                            .child("×"),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("connection-tabs")
            .flex()
            .items_center()
            .gap_1()
            .min_w(px(0.))
            .overflow_x_scroll()
            .children(tabs)
            .into_any_element()
    }

    fn render_connection_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let active = self.workspace.active_id();

        let mut rows: Vec<AnyElement> = Vec::new();

        if self.workspace.is_empty() {
            rows.push(
                div()
                    .px_3()
                    .py_2()
                    .child(caption("No connections yet", theme))
                    .into_any_element(),
            );
        } else {
            for entry in self.workspace.entries() {
                let id = entry.id();
                let is_active = active == Some(id);
                let is_open = self.workspace.is_open(id);
                let connected = entry.status.is_connected();
                let light = status_color(&entry.status, theme);
                let name = SharedString::from(entry.config.name.clone());
                let summary = SharedString::from(entry.config.summary());

                rows.push(
                    div()
                        .id(("picker-connection", id.0 as usize))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .mx_1()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(is_active, |row| row.bg(theme.selection))
                        .hover(|row| row.bg(theme.hover))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            if !event.standard_click() {
                                return;
                            }
                            this.pick_connection(id, cx);
                        }))
                        .child(dot(light))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .truncate()
                                        .text_color(theme.text)
                                        .child(name),
                                )
                                .child(caption(summary, theme).truncate()),
                        )
                        // Says which of these already have a tab, so clicking
                        // one reads as "go there" rather than "open a second".
                        .when(is_open && !is_active, |row| {
                            row.child(caption("open", theme))
                        })
                        .when(connected, |row| {
                            row.child(
                                div()
                                    .id(("picker-disconnect", id.0 as usize))
                                    .px_1()
                                    .text_color(theme.text_faint)
                                    .cursor_pointer()
                                    .hover(|s| s.text_color(theme.danger))
                                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                        if !event.standard_click() {
                                            return;
                                        }
                                        cx.stop_propagation();
                                        this.close_connection_picker(cx);
                                        this.disconnect(id, cx);
                                    }))
                                    .child("⏻"),
                            )
                        })
                        .child(
                            div()
                                .id(("picker-edit", id.0 as usize))
                                .px_1()
                                .text_color(theme.text_faint)
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme.text))
                                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                    if !event.standard_click() {
                                        return;
                                    }
                                    cx.stop_propagation();
                                    this.edit_connection(id, cx);
                                }))
                                .child("✎"),
                        )
                        .into_any_element(),
                );
            }
        }

        deferred(
            div()
                .id("connection-picker-menu")
                .absolute()
                .top_full()
                .left_0()
                .mt_1()
                .w(px(320.))
                .max_h(px(360.))
                .flex()
                .flex_col()
                .rounded(px(10.))
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .occlude()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_connection_picker(cx);
                }))
                .child(
                    div()
                        .id("connection-picker-list")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .py_1()
                        .children(rows),
                )
                .child(div().h(px(1.)).w_full().bg(theme.divider))
                .child(
                    div()
                        .id("picker-new-connection")
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .text_color(theme.text)
                        .hover(|s| s.bg(theme.hover))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.close_connection_picker(cx);
                            this.open_new_connection(cx);
                        }))
                        .child("New Connection…"),
                ),
        )
        .into_any_element()
    }
}

fn status_color(status: &ConnectionStatus, theme: &Theme) -> gpui::Rgba {
    match status {
        ConnectionStatus::Connected(_) => theme.success,
        ConnectionStatus::Connecting => theme.warning,
        ConnectionStatus::Failed(_) => theme.danger,
        ConnectionStatus::Disconnected => theme.text_faint,
    }
}

fn start_titlebar_drag(window: &mut Window) {
    #[cfg(target_os = "macos")]
    {
        let _ = window;
        crate::mac_window::perform_window_drag();
    }
    #[cfg(not(target_os = "macos"))]
    {
        window.start_window_move();
    }
}
