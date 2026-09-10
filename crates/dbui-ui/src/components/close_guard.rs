//! "Discard staged changes?" — the guard in front of closing something that
//! is holding uncommitted work.
//!
//! Staged edits live on the tab, so closing one is the one gesture in the app
//! that can silently throw away work the server never saw. The change bubble
//! says the work exists; this says it is about to go. Everything that closes
//! without a question -- dropping the table the tab was showing, say -- calls
//! the `_now` variant instead of going through here.

use super::{button, caption};
use crate::root::DbUi;
use crate::tabs::TabId;
use crate::theme::metrics;
use dbui_app::domain::ConnectionId;
use gpui::{div, prelude::*, AnyElement, Context, MouseButton, SharedString};

/// What a confirmed discard goes on to close.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseTarget {
    /// A table or SQL tab, by index in the active connection's tab list.
    Tab(usize),
    /// Several tabs at once, from the tab bar's own menu.
    TabGroup(TabScope),
    Connection(ConnectionId),
}

/// Which tabs one of the bulk closes is aimed at.
///
/// Named by [`TabId`] rather than by index: the guard can sit open while a
/// load lands or another tab closes, and an index that shifted under it would
/// close whatever slid into the slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabScope {
    /// Everything but the tab the menu was opened on.
    Others(TabId),
    /// Everything sitting to the right of it.
    ToRight(TabId),
    All,
}

pub struct CloseGuard {
    pub target: CloseTarget,
    /// What is being closed, named the way the user sees it named.
    pub label: SharedString,
    pub changes: usize,
}

impl DbUi {
    pub(crate) fn render_close_guard(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let guard = self.close_guard.as_ref()?;
        let theme = &self.theme;

        let plural = if guard.changes == 1 {
            "change"
        } else {
            "changes"
        };
        let title = format!("Discard {} staged {plural}?", guard.changes);
        // A group close is several tabs, and the singular sentence would name
        // them as one thing the user never opened.
        let body = if matches!(guard.target, CloseTarget::TabGroup(_)) {
            format!(
                "{} have work that has not been committed. Closing them throws \
                 the whole batch away.",
                guard.label
            )
        } else {
            format!(
                "“{}” has work that has not been committed. Closing it throws the \
                 whole batch away.",
                guard.label
            )
        };

        let scrim = if theme.is_light {
            gpui::rgba(0x00000033)
        } else {
            gpui::rgba(0x00000066)
        };

        Some(
            div()
                .id("close-guard-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .justify_center()
                .items_start()
                .pt(metrics::scaled(140.))
                .bg(scrim)
                // Modal to the pointer as well as the keyboard, for the same
                // reason the destructive confirmation is: a question about
                // losing work must not be answerable by clicking past it.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                .child(
                    div()
                        .id("close-guard-panel")
                        .w(metrics::scaled(440.))
                        .flex()
                        .flex_col()
                        .gap_3()
                        .p_4()
                        .rounded(gpui::px(12.))
                        .bg(theme.elevated)
                        .border_1()
                        .border_color(theme.warning)
                        .child(
                            div()
                                .text_color(theme.warning)
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .text_color(theme.text_muted)
                                .child(SharedString::from(body)),
                        )
                        .child(caption(
                            "⌘S commits them and keeps this open. Esc cancels.",
                            theme,
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    button("close-guard-cancel", "Keep Open", theme, false)
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.cancel_close(cx)
                                        })),
                                )
                                .child(
                                    button("close-guard-discard", "Discard & Close", theme, true)
                                        .bg(theme.danger)
                                        .border_color(theme.danger)
                                        .on_click(cx.listener(|this, _, _window, cx| {
                                            this.confirm_close(cx)
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}
