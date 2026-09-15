//! The status bar: what just happened, and where in the table you are.
//!
//! Two halves with different jobs. On the left, one line about the last thing
//! the app did, led by a light in the colour of how it went -- a message that
//! has to be read to find out whether it was good news is a message that gets
//! skipped. On the right, where in the result the view is, and the arrows to
//! move it: the same pair as the toolbar, at the other end of a window that is
//! often tall enough for the toolbar to be nowhere near the last row you read.

use super::icon_button;
use crate::root::{DbUi, ResultSource, Status};
use crate::theme::metrics;
use crate::update::UpdateAction;
use gpui::{div, prelude::*, AnyElement, Context, Rgba, SharedString};

impl DbUi {
    pub(crate) fn render_status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let update = self.update_chip();
        let position = self.page_position();
        let paging = self.paging_state();
        let theme = &self.theme;

        let (message, color): (SharedString, Rgba) = match &self.status {
            Status::Idle => (self.idle_message(), theme.text_muted),
            Status::Busy(text) => (text.clone(), theme.warning),
            Status::Info(text) => (text.clone(), theme.text_muted),
            Status::Error(text) => (text.clone(), theme.danger),
        };
        // The light says how it went; the text says what it was. Idle is the
        // green one -- "nothing is wrong" is the useful reading of an app with
        // nothing to report.
        let light = match &self.status {
            Status::Idle => theme.success,
            Status::Busy(_) => theme.warning,
            Status::Info(_) => theme.accent,
            Status::Error(_) => theme.danger,
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

        let summary = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .map(|view| SharedString::from(view.summary.clone()));

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
            .child(super::dot(light))
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
                bar.child(div().text_color(theme.warning).child("more rows available"))
            })
            .children(detail.map(|text| {
                div()
                    .max_w(gpui::px(520.))
                    .overflow_hidden()
                    .font_family(metrics::MONO_FONT)
                    .text_color(theme.text_muted)
                    .child(text)
            }))
            .children(summary.map(|text| div().text_color(theme.text_muted).child(text)))
            .children(position.map(|(page, pages)| {
                div()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("Page {page} of {pages}")))
            }))
            .children(
                paging.map(|(at_start, at_end)| self.render_status_paging(at_start, at_end, cx)),
            )
    }

    /// The prev/next pair, sized down to fit a 26px bar.
    fn render_status_paging(
        &self,
        at_start: bool,
        at_end: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let arrow = |id: &'static str, glyph: &'static str, disabled: bool| {
            icon_button(id, glyph, theme, false)
                .h(metrics::status_height())
                .w(metrics::status_height())
                .when(disabled, |button| {
                    button.text_color(theme.text_faint).cursor_default()
                })
        };

        div()
            .flex()
            .items_center()
            .flex_shrink_0()
            .child(
                arrow("status-page-prev", "‹", at_start).when(!at_start, |button| {
                    button.on_click(cx.listener(|this, _, _window, cx| this.page(false, cx)))
                }),
            )
            .child(
                arrow("status-page-next", "›", at_end).when(!at_end, |button| {
                    button.on_click(cx.listener(|this, _, _window, cx| this.page(true, cx)))
                }),
            )
            .into_any_element()
    }

    /// `(at first page, at last page)` for the table on screen.
    ///
    /// `None` when there is nothing to page -- a SQL result is one page by
    /// definition, and arrows over it would be two dead buttons.
    fn paging_state(&self) -> Option<(bool, bool)> {
        let view = self.tabs.active()?.result()?;
        let ResultSource::Table {
            page, total_rows, ..
        } = &view.source
        else {
            return None;
        };
        let at_end = total_rows
            .map(|total| page.offset + u64::from(page.limit) >= total.max(0) as u64)
            .unwrap_or(false);
        Some((page.offset == 0, at_end))
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

        // The row count moved to the right-hand group, next to the arrows
        // that change it. With nothing left to report, the left half says so
        // -- the light beside it is the part being read anyway.
        match &view.source {
            ResultSource::Table { .. } | ResultSource::Query { .. } => SharedString::from("Ready"),
        }
    }
}
