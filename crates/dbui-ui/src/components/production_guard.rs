//! "Write to production?" -- the question in front of a write on a connection
//! tagged [`Environment::Production`].
//!
//! Read-only refuses writes outright; this is for the server that has to take
//! writes, but where one sent by reflex -- ⌘S on the wrong window, ⌘↵ on the
//! wrong tab -- is the expensive kind of mistake. It names the connection and
//! what is about to be sent, and nothing goes until it is answered.
//!
//! DROP and TRUNCATE are not routed through here: they already ask for the
//! table's name to be typed, which is a stronger question than this one.
//!
//! [`Environment::Production`]: dbui_app::domain::Environment::Production

use super::{button, caption, motion};
use crate::root::DbUi;
use crate::theme::metrics;
use gpui::{div, prelude::*, AnyElement, Context, MouseButton, SharedString};

/// What is waiting on the answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardedWrite {
    /// The staged batch on the tab ⌘S was pressed on.
    Commit { changes: usize },
    /// Statements from the SQL editor, at least one of which writes.
    Statements(Vec<String>),
}

pub struct ProductionGuard {
    pub write: GuardedWrite,
    /// The connection's name, as the tab strip shows it.
    pub connection: SharedString,
}

impl ProductionGuard {
    pub fn title(&self) -> String {
        format!("Write to production — “{}”?", self.connection)
    }

    pub fn body(&self) -> String {
        match &self.write {
            GuardedWrite::Commit { changes } => format!(
                "{changes} staged change{} will be committed.",
                if *changes == 1 { "" } else { "s" }
            ),
            GuardedWrite::Statements(statements) => {
                let writes: Vec<_> = statements
                    .iter()
                    .filter(|sql| dbui_app::domain::writes(sql))
                    .collect();
                let first = writes
                    .first()
                    .map(|sql| first_line(sql))
                    .unwrap_or_default();
                match writes.len() {
                    1 => format!("This statement writes: {first}"),
                    n => format!("{n} of these statements write, starting with: {first}"),
                }
            }
        }
    }

    pub fn confirm_label(&self) -> &'static str {
        match self.write {
            GuardedWrite::Commit { .. } => "Commit to Production",
            GuardedWrite::Statements(_) => "Run on Production",
        }
    }
}

/// One line of a statement, short enough to read at a glance.
fn first_line(sql: &str) -> String {
    let line = sql.trim().lines().next().unwrap_or_default().trim();
    let mut out: String = line.chars().take(80).collect();
    if line.chars().count() > 80 || sql.trim().lines().nth(1).is_some() {
        out.push('…');
    }
    out
}

impl DbUi {
    pub(crate) fn render_production_guard(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let guard = self.production_guard.as_ref()?;
        let theme = &self.theme;
        let red = theme
            .environment_color(dbui_app::domain::Environment::Production)
            .unwrap_or(theme.danger);
        let scrim = if theme.is_light {
            gpui::rgba(0x00000033)
        } else {
            gpui::rgba(0x00000066)
        };

        Some(
            motion::dialog(
                "production-guard-in",
                div()
                    .id("production-guard-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .bg(scrim)
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("production-guard-panel")
                            .w(metrics::scaled(460.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .rounded(gpui::px(12.))
                            .bg(theme.elevated)
                            .border_1()
                            .border_color(red)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded(gpui::px(4.))
                                            .bg(red)
                                            .text_color(gpui::rgb(0xffffff))
                                            .text_size(metrics::scaled(10.))
                                            .child("PRODUCTION"),
                                    )
                                    .child(
                                        div()
                                            .text_color(theme.text)
                                            .font_weight(gpui::FontWeight::MEDIUM)
                                            .child(SharedString::from(guard.title())),
                                    ),
                            )
                            .child(
                                div()
                                    .text_color(theme.text_muted)
                                    .font_family(metrics::MONO_FONT)
                                    .child(SharedString::from(guard.body())),
                            )
                            .child(caption(
                                "↵ goes ahead. Esc cancels; nothing is sent.",
                                theme,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        button("production-guard-cancel", "Cancel", theme, false)
                                            .on_click(cx.listener(|this, _, _window, cx| {
                                                this.cancel_production_write(cx)
                                            })),
                                    )
                                    .child(
                                        button(
                                            "production-guard-confirm",
                                            guard.confirm_label(),
                                            theme,
                                            true,
                                        )
                                        .bg(red)
                                        .border_color(red)
                                        .on_click(
                                            cx.listener(|this, _, _window, cx| {
                                                this.confirm_production_write(cx)
                                            }),
                                        ),
                                    ),
                            ),
                    ),
                metrics::scaled(140.),
            )
            .into_any_element(),
        )
    }

    /// Stop `write` for a question when the connection in front is tagged
    /// production. Returns true when the caller should stop; the answer runs
    /// the write again, and this lets that second pass through.
    pub(crate) fn hold_for_production(
        &mut self,
        write: GuardedWrite,
        cx: &mut Context<Self>,
    ) -> bool {
        if std::mem::take(&mut self.production_confirmed) {
            return false;
        }
        let Some(entry) = self.workspace.active() else {
            return false;
        };
        if !entry.config.environment.confirms_writes() {
            return false;
        }
        self.production_guard = Some(ProductionGuard {
            write,
            connection: entry.config.name.clone().into(),
        });
        cx.notify();
        true
    }

    pub(crate) fn confirm_production_write(&mut self, cx: &mut Context<Self>) {
        let Some(guard) = self.production_guard.take() else {
            return;
        };
        self.production_confirmed = true;
        match guard.write {
            GuardedWrite::Commit { .. } => self.save_pending_edits(cx),
            GuardedWrite::Statements(statements) => self.dispatch_statements(statements, cx),
        }
        // Spent or not, the pass is for this one write only.
        self.production_confirmed = false;
        cx.notify();
    }

    pub(crate) fn cancel_production_write(&mut self, cx: &mut Context<Self>) {
        if self.production_guard.take().is_some() {
            self.status = crate::root::Status::info("Cancelled — nothing was sent");
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(write: GuardedWrite) -> ProductionGuard {
        ProductionGuard {
            write,
            connection: "prod-db".into(),
        }
    }

    #[test]
    fn a_commit_counts_its_changes() {
        assert_eq!(
            guard(GuardedWrite::Commit { changes: 1 }).body(),
            "1 staged change will be committed."
        );
        assert_eq!(
            guard(GuardedWrite::Commit { changes: 3 }).body(),
            "3 staged changes will be committed."
        );
    }

    #[test]
    fn statements_name_the_first_write_not_the_first_statement() {
        let body = guard(GuardedWrite::Statements(vec![
            "SELECT 1".into(),
            "DELETE FROM orders\nWHERE id = 4".into(),
            "UPDATE t SET a = 1".into(),
        ]))
        .body();
        assert_eq!(
            body,
            "2 of these statements write, starting with: DELETE FROM orders…"
        );
    }
}
