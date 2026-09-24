//! The custom titlebar.
//!
//! The system one is hidden (`appears_transparent`) so the window reads as one
//! surface rather than a chrome bar stuck on top of an app. The left inset
//! keeps clear of the traffic lights, which are still drawn by the platform.
//!
//! One bar, three groups. Connection switching lives on the left: one chip per
//! open connection, TablePlus-style, the front one carrying the chevron that
//! drops down every connection the user has saved. The workspace tab strip
//! follows it, so "which server" and "which table on it" are the same glance
//! rather than two rows apart. On the right sit the two things that are about
//! the app rather than the data: the search box and the settings menu.
//!
//! A chip is an open connection; the dropdown is every connection the user has
//! saved. Closing a chip therefore does not delete anything, which is why the
//! `×` and the picker's `✎`/`⏻` are different gestures with different reach.

use super::icons::{database_icon, search_icon, settings_icon, RefreshIcon};
use super::{caption, dot, icon_button, menu_row, menu_surface, motion};
use crate::components::palette::PaletteKind;
use crate::root::DbUi;
use crate::theme::{metrics, Theme};
use dbui_app::ConnectionStatus;
use gpui::{
    deferred, div, prelude::*, px, AnyElement, ClickEvent, Context, MouseButton, MouseDownEvent,
    MouseUpEvent, SharedString, Window, WindowControlArea,
};

impl DbUi {
    pub(crate) fn render_titlebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        // Everything holding `&self` is built before the theme borrow, which
        // is what lets the bar be assembled in one expression below.
        let connections = self.render_connection_chips(cx);
        let tab_strip = self.render_tab_bar(cx);
        let settings = self.render_settings_menu(cx);
        let settings_open = self.settings_menu_open;
        let chrome = self.chrome_theme();
        let theme = &chrome;
        let environment = self.active_environment();
        let environment_color = theme.environment_color(environment);

        div()
            .id("titlebar")
            .relative()
            .flex()
            .items_center()
            .h(metrics::titlebar_height())
            .flex_shrink_0()
            .pl(metrics::traffic_light_inset())
            .pr_2()
            .gap_2()
            .when(!self.glass, |bar| {
                bar.bg(theme.panel).border_b_1().border_color(theme.border)
            })
            .text_color(theme.text_muted)
            .text_size(metrics::text_size_small())
            .child(connections)
            // The tag, where the user looks to tell which server they are on,
            // and a line in its colour right across the window under the bar:
            // production should be visible from across the room.
            .children(environment_color.map(|color| {
                div()
                    .flex_shrink_0()
                    .px_2()
                    .py_0p5()
                    .rounded(px(4.))
                    .bg(gpui::Rgba { a: 0.16, ..color })
                    .border_1()
                    .border_color(color)
                    .text_color(color)
                    .text_size(metrics::scaled(10.))
                    .child(environment.label().to_uppercase())
            }))
            .children(environment_color.map(|color| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.))
                    .bg(color)
            }))
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
                    .text_size(metrics::scaled(10.))
                    .child("READ ONLY")
            }))
            // Which side of the bar a control belongs to is the only thing
            // separating two strips of tabs from one long one.
            .child(
                div()
                    .w(px(1.))
                    .h(metrics::scaled(18.))
                    .flex_shrink_0()
                    .bg(theme.divider),
            )
            .child(tab_strip)
            // The leftover width, and the only part of the bar that moves the
            // window. It was tried as a layer behind the whole titlebar, to
            // win back the strips above and below the tabs -- but a layer is a
            // sibling of the controls, not an ancestor, so it saw their
            // presses too and a tab drag moved the window again. A gap that
            // overlaps nothing cannot have that problem.
            .child(
                div()
                    .id("titlebar-drag")
                    .flex_1()
                    .min_w(metrics::scaled(16.))
                    .h_full()
                    .window_control_area(WindowControlArea::Drag)
                    // Press here; move and release on the root view. The
                    // pointer leaves this strip on the first frame of a drag,
                    // the same way every other handle in the window works.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _window, _cx| {
                            this.begin_titlebar_drag();
                        }),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseUpEvent, _window, _cx| {
                            this.end_titlebar_drag();
                        }),
                    )
                    .on_click(move |event: &ClickEvent, window: &mut Window, _cx| {
                        if event.click_count() == 2 {
                            window.titlebar_double_click();
                        }
                    }),
            )
            .child(self.render_titlebar_search(cx))
            // Reloading the catalog is about the connection, not the result,
            // so it sits with the other controls that are about the app rather
            // than about the rows -- and next to the box you would have gone
            // looking for a missing table in.
            .child(
                icon_button(
                    "refresh-catalog",
                    motion::spin(
                        "refresh-catalog-spin",
                        RefreshIcon::new(theme.text_muted),
                        self.catalog_refreshes,
                    ),
                    theme,
                    false,
                )
                .on_click(cx.listener(|this, _, _window, cx| this.refresh_catalog(cx))),
            )
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(
                        icon_button(
                            "titlebar-settings",
                            settings_icon(if settings_open {
                                theme.text
                            } else {
                                theme.text_muted
                            }),
                            theme,
                            settings_open,
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                this.toggle_settings_menu(cx);
                            }),
                        ),
                    )
                    .children(settings),
            )
    }

    /// The search box, which is a button wearing a text field's clothes.
    ///
    /// Nothing is typed here: pressing it opens the palette, which is where
    /// the query actually lives. Drawing it as a field is what makes the
    /// shortcut discoverable to someone who has never pressed it.
    fn render_titlebar_search(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let chrome = self.chrome_theme();
        let theme = &chrome;
        div()
            .id("titlebar-search")
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .h(metrics::control_height())
            .w(metrics::scaled(220.))
            .flex_shrink_0()
            .rounded_md()
            .cursor_pointer()
            .bg(theme.elevated)
            .border_1()
            .border_color(theme.border)
            .hover(|field| field.border_color(theme.accent))
            .on_click(
                cx.listener(|this, _, _window, cx| this.open_palette(PaletteKind::GoToTable, cx)),
            )
            .child(search_icon(theme.text_faint))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .text_color(theme.text_faint)
                    .child("Search…"),
            )
            // The real binding, not the one the mock drew: a shortcut printed
            // on a button has to be the one that works.
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(metrics::scaled(10.))
                    .text_color(theme.text_faint)
                    .child("⌘P"),
            )
    }

    /// One chip per open connection, in tab order.
    ///
    /// The strip is allowed to shrink and scroll rather than push the tab
    /// strip and the drag area off the end of the bar.
    fn render_connection_chips(&self, cx: &mut Context<Self>) -> AnyElement {
        let chrome = self.chrome_theme();
        let theme = &chrome;
        let active = self.workspace.active_id();
        let picker_open = self.connection_picker_open;

        if self.workspace.open_count() == 0 {
            return div()
                .id("connection-chips-empty")
                .relative()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .h(metrics::control_height())
                .flex_shrink_0()
                .rounded_md()
                .cursor_pointer()
                .border_1()
                .border_color(theme.border)
                .hover(|chip| chip.bg(theme.hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_connection_picker(cx);
                    }),
                )
                .child(caption("No connection open", theme))
                .child(div().text_color(theme.text_faint).child("⌄"))
                .children(picker_open.then(|| self.render_connection_picker(cx)))
                .into_any_element();
        }

        let chips: Vec<AnyElement> = self
            .workspace
            .open_entries()
            .map(|entry| {
                let id = entry.id();
                let key = id.0 as usize;
                let is_active = active == Some(id);
                let light = status_color(&entry.status, theme);
                let name = SharedString::from(entry.config.name.clone());
                let driver = entry.config.driver;
                let tag = theme.environment_color(entry.config.environment);
                // Staged work anywhere under this connection, including the
                // tabs that are not in front -- ⌘⇧W closes all of them.
                let changes: usize = self
                    .connection_tabs(id)
                    .map(|tabs| {
                        tabs.items
                            .iter()
                            .map(|tab| tab.pending_change_count())
                            .sum()
                    })
                    .unwrap_or(0);

                div()
                    .id(("connection-chip", key))
                    .relative()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(metrics::control_height())
                    .max_w(metrics::scaled(220.))
                    .rounded_md()
                    .cursor_pointer()
                    .border_1()
                    .border_color(if is_active {
                        theme.border
                    } else {
                        gpui::rgba(0x00000000)
                    })
                    .when(is_active, |chip| chip.bg(theme.elevated))
                    .when(!is_active, |chip| chip.hover(|s| s.bg(theme.hover)))
                    .text_color(if is_active {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    // The front chip is the picker's button; the rest just
                    // switch. Both are one press, which is why neither waits
                    // for the release.
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            if is_active {
                                this.toggle_connection_picker(cx);
                            } else {
                                this.open_connection_tab(id, cx);
                            }
                        }),
                    )
                    // The engine's own colour on the mark, and the connection
                    // state on the dot beside it: one says what this is, the
                    // other whether it is reachable.
                    .child(database_icon(theme.driver_color(driver)))
                    .child(div().truncate().child(name))
                    // Every tagged chip carries its colour, not just the one
                    // in front: switching to production should be seen
                    // before it is clicked, not after.
                    .children(tag.map(|color| {
                        div()
                            .absolute()
                            .left(px(6.))
                            .right(px(6.))
                            .bottom(px(-1.))
                            .h(px(2.))
                            .rounded_full()
                            .bg(color)
                    }))
                    .children((changes > 0).then(|| dot(theme.warning)))
                    .child(dot(light))
                    .when(is_active, |chip| {
                        chip.child(
                            div()
                                .flex_shrink_0()
                                .text_size(metrics::scaled(10.))
                                .text_color(theme.text_faint)
                                .child("⌄"),
                        )
                    })
                    .child(
                        div()
                            .id(("connection-chip-close", key))
                            .px_1()
                            .flex_shrink_0()
                            .text_color(theme.text_faint)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.danger))
                            // Mouse-down rather than click, to match the chip
                            // itself -- otherwise the chip activates on the way
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
                    .children((is_active && picker_open).then(|| self.render_connection_picker(cx)))
                    .into_any_element()
            })
            .collect();

        div()
            .id("connection-chips")
            .flex()
            .items_center()
            .gap_1()
            .min_w(px(0.))
            .overflow_x_scroll()
            .children(chips)
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
                                .child(div().truncate().text_color(theme.text).child(name))
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
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, _, cx| {
                                            if !event.standard_click() {
                                                return;
                                            }
                                            cx.stop_propagation();
                                            this.close_connection_picker(cx);
                                            this.disconnect(id, cx);
                                        },
                                    ))
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

        deferred(motion::menu(
            "connection-picker-menu-in",
            menu_surface("connection-picker-menu", theme)
                .top_full()
                .left_0()
                .mt_1()
                .w(metrics::scaled(340.))
                .max_h(metrics::scaled(360.))
                .py_0()
                .overflow_hidden()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.close_connection_picker(cx);
                }))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(
                            div()
                                .id("connection-picker-list")
                                .track_scroll(&self.picker_scroll)
                                .size_full()
                                .min_h(px(0.))
                                .overflow_y_scroll()
                                .py_1()
                                .children(rows),
                        )
                        .child(super::scrollbar::vertical_scrollbar(
                            "picker-scrollbar",
                            self.picker_scroll.clone(),
                            theme,
                        )),
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
            metrics::scaled(4.),
        ))
        .into_any_element()
    }

    /// The gear menu: the app's own settings, as opposed to the data's.
    ///
    /// Everything here is also an action with a shortcut. The menu exists so
    /// that none of them is *only* a shortcut -- the theme picker in
    /// particular was previously reachable by ⌘⇧T and nothing else.
    fn render_settings_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.settings_menu_open {
            return None;
        }
        let theme = &self.theme;
        let zoom = metrics::zoom_pct();

        let separator = || {
            div()
                .my_1()
                .h(px(1.))
                .w_full()
                .flex_shrink_0()
                .bg(theme.divider)
        };

        Some(
            deferred(motion::menu(
                "settings-menu-in",
                menu_surface("settings-menu", theme)
                    .top_full()
                    .right_0()
                    .mt_1()
                    .w(metrics::scaled(240.))
                    .text_size(metrics::text_size_small())
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                        this.close_settings_menu(cx);
                    }))
                    .child(
                        menu_row("settings-theme", "Change Theme…", Some("⌘⇧T"), theme).on_click(
                            cx.listener(|this, _, _window, cx| {
                                this.close_settings_menu(cx);
                                this.open_palette(PaletteKind::Themes, cx);
                            }),
                        ),
                    )
                    .child(
                        menu_row("settings-commands", "Command Palette…", Some("⌘⇧P"), theme)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.close_settings_menu(cx);
                                this.open_palette(PaletteKind::Actions, cx);
                            })),
                    )
                    .child(separator())
                    .child(
                        menu_row(
                            "settings-new-connection",
                            "New Connection…",
                            Some("⌘N"),
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _window, cx| {
                            this.close_settings_menu(cx);
                            this.open_new_connection(cx);
                        })),
                    )
                    .child(
                        menu_row("settings-refresh-catalog", "Refresh Catalog", None, theme)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.close_settings_menu(cx);
                                this.refresh_catalog(cx);
                            })),
                    )
                    .child(separator())
                    .child(
                        menu_row(
                            "settings-translucent",
                            "Translucent Window",
                            Some(if self.translucent { "On" } else { "Off" }),
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_translucent(cx))),
                    )
                    // Only while it is on: they tune a glass that is not there
                    // otherwise, and would read as broken controls.
                    .when(self.translucent, |menu| {
                        menu.child(stepper_row(
                            "settings-glass-opacity",
                            "Tint",
                            format!("{}%", self.glass_opacity_pct),
                            theme,
                            cx.listener(|this, _, _window, cx| this.step_glass_opacity(-1, cx)),
                            cx.listener(|this, _, _window, cx| this.step_glass_opacity(1, cx)),
                        ))
                        .child(stepper_row(
                            "settings-glass-blur",
                            "Blur",
                            self.glass_blur.to_string(),
                            theme,
                            cx.listener(|this, _, _window, cx| this.step_glass_blur(-1, cx)),
                            cx.listener(|this, _, _window, cx| this.step_glass_blur(1, cx)),
                        ))
                    })
                    .child(
                        menu_row("settings-zoom-in", "Zoom In", Some("⌘+"), theme)
                            .on_click(cx.listener(|this, _, _window, cx| this.zoom_delta(1, cx))),
                    )
                    .child(
                        menu_row("settings-zoom-out", "Zoom Out", Some("⌘−"), theme)
                            .on_click(cx.listener(|this, _, _window, cx| this.zoom_delta(-1, cx))),
                    )
                    .child(
                        menu_row(
                            "settings-zoom-reset",
                            SharedString::from(format!("Actual Size  ({zoom}%)")),
                            Some("⌘0"),
                            theme,
                        )
                        .on_click(cx.listener(|this, _, _window, cx| this.zoom_delta(0, cx))),
                    ),
                metrics::scaled(4.),
            ))
            .into_any_element(),
        )
    }
}

/// A settings row that is a value with − and + beside it rather than a
/// command. The row itself does nothing on click, so a press that misses
/// the buttons does not close the menu under the user mid-adjustment.
fn stepper_row(
    id: &'static str,
    label: &'static str,
    value: String,
    theme: &Theme,
    on_less: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
    on_more: impl Fn(&ClickEvent, &mut Window, &mut gpui::App) + 'static,
) -> impl IntoElement {
    let button = |suffix: &'static str, glyph: &'static str| {
        div()
            .id((id, usize::from(suffix == "more")))
            .w(metrics::scaled(20.))
            .h(metrics::scaled(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .cursor_pointer()
            .text_color(theme.text_muted)
            .hover(|button| button.bg(theme.hover).text_color(theme.text))
            .child(glyph)
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .pl_3()
        .pr_2()
        .py_0p5()
        .text_color(theme.text)
        .child(div().pl_3().child(label))
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .child(button("less", "−").on_click(on_less))
                .child(
                    div()
                        .w(metrics::scaled(36.))
                        .flex()
                        .justify_center()
                        .text_size(metrics::text_size_small())
                        .text_color(theme.text_muted)
                        .child(value),
                )
                .child(button("more", "+").on_click(on_more)),
        )
}

fn status_color(status: &ConnectionStatus, theme: &Theme) -> gpui::Rgba {
    match status {
        ConnectionStatus::Connected(_) => theme.success,
        ConnectionStatus::Connecting => theme.warning,
        ConnectionStatus::Failed(_) => theme.danger,
        ConnectionStatus::Disconnected => theme.text_faint,
    }
}
