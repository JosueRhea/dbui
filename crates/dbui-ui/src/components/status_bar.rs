//! The status bar: what just happened, and what is on screen.

use crate::root::{DbUi, ResultSource, Status};
use crate::theme::metrics;
use crate::update::UpdateAction;
use gpui::{div, prelude::*, Context, Rgba, SharedString};

impl DbUi {
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let update = self.update_chip();
        let theme = &self.theme;

        let (message, color): (SharedString, Rgba) = match &self.status {
            Status::Idle => (self.idle_message(), theme.text_muted),
            Status::Busy(text) => (text.clone(), theme.warning),
            Status::Info(text) => (text.clone(), theme.text_muted),
            Status::Error(text) => (text.clone(), theme.danger),
        };

        let detail = self.selected_cell.and_then(|(row, column)| {
            let view = self.tabs.active()?.result()?;
            let value = view.set.rows.get(row)?.get(column)?;
            let name = view.set.columns.get(column)?.name.clone();
            // An underlined value is not much of an affordance on its own, so
            // the bar says how to open it.
            let opens = if self.foreign_key_at(row, column).is_some() {
                "  ·  ⌘↵ or ⌥-click to open"
            } else {
                ""
            };
            Some(SharedString::from(format!(
                "{name} = {}{opens}",
                value.to_cell(180)
            )))
        });

        let truncated = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .map(|view| view.set.truncated)
            .unwrap_or(false);

        div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .h(metrics::status_height())
            .flex_shrink_0()
            .bg(theme.panel)
            .border_t_1()
            .border_color(theme.border)
            .text_size(metrics::text_size_small())
            .child(div().text_color(color).child(message))
            .child(div().flex_1())
            // Left of the other trailing items: an update is about the app, not
            // about what is on screen, so it should not sit between a value and
            // the row count it belongs to.
            .children(update.map(|(label, action)| {
                let idle = action == UpdateAction::None;
                div()
                    .id("update-chip")
                    .px_2()
                    .rounded_md()
                    .text_color(if idle { theme.text_muted } else { theme.accent })
                    .when(!idle, |chip| {
                        chip.cursor_pointer()
                            .hover(|chip| chip.bg(theme.hover))
                            .on_click(cx.listener(move |this, _, _window, cx| match action {
                                UpdateAction::Download => this.download_update(cx),
                                UpdateAction::Install => this.install_update(cx),
                                UpdateAction::Retry => this.check_for_update(cx),
                                UpdateAction::None => {}
                            }))
                    })
                    .child(SharedString::from(label))
            }))
            .when(truncated, |bar| {
                bar.child(
                    div()
                        .text_color(theme.warning)
                        .child("more rows available"),
                )
            })
            .children(detail.map(|text| {
                div()
                    .max_w(gpui::px(520.))
                    .overflow_hidden()
                    .font_family(metrics::MONO_FONT)
                    .text_color(theme.text_muted)
                    .child(text)
            }))
    }

    fn idle_message(&self) -> SharedString {
        let Some(view) = self.tabs.active().and_then(|tab| tab.result()) else {
            return match self.workspace.active() {
                Some(entry) if entry.status.is_connected() => {
                    SharedString::from(entry.config.summary())
                }
                _ => SharedString::from("Ready"),
            };
        };

        let columns = view.set.columns.len();
        match &view.source {
            ResultSource::Table { .. } | ResultSource::Query { .. } => {
                SharedString::from(format!("{} · {columns} columns", view.summary))
            }
        }
    }
}
