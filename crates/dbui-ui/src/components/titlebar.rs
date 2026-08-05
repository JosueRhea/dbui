//! The custom titlebar.
//!
//! The system one is hidden (`appears_transparent`) so the window reads as one
//! surface rather than a chrome bar stuck on top of an app. The left inset
//! keeps clear of the traffic lights, which are still drawn by the platform.
//!
//! Connection switching lives here (not in the sidebar): a compact picker and
//! dropdown, TablePlus-style. The rest of the bar still owns native chrome —
//! drag to move, double-click to zoom.

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
        let theme = &self.theme;
        let picker_open = self.connection_picker_open;

        let (label, summary, light) = match self.workspace.active() {
            Some(entry) => {
                let light = match &entry.status {
                    ConnectionStatus::Connected(_) => theme.success,
                    ConnectionStatus::Connecting => theme.warning,
                    ConnectionStatus::Failed(_) => theme.danger,
                    ConnectionStatus::Disconnected => theme.text_faint,
                };
                (
                    SharedString::from(entry.config.name.clone()),
                    Some(SharedString::from(entry.config.summary())),
                    light,
                )
            }
            None => (
                SharedString::from("No connection"),
                None,
                theme.text_faint,
            ),
        };

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
            .child(
                div()
                    .id("connection-picker")
                    .relative()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.hover))
                    .when(picker_open, |s| s.bg(theme.selection))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.toggle_connection_picker(cx);
                        }),
                    )
                    .child(dot(light))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .max_w(px(280.))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(theme.text)
                                    .child(label),
                            )
                            .children(summary.map(|text| {
                                div()
                                    .truncate()
                                    .text_color(theme.text_faint)
                                    .child(text)
                            })),
                    )
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child(if picker_open { "▴" } else { "▾" }),
                    )
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
