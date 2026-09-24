//! Server activity: who is connected, what each is running, and a way to
//! stop one.
//!
//! The question it answers is usually "why is everything slow?", and the
//! answer is usually one statement that has been running for twenty minutes
//! or a session sitting idle inside a transaction, holding locks. So the list
//! puts busy sessions first, shows how long each statement has run, and can
//! hide the idle ones. It refreshes itself every two seconds while open.
//!
//! Cancelling a statement and ending a session both ask once, on the row,
//! before anything is sent: they act on somebody else's work.

use super::{button, caption, motion};
use crate::root::{DbUi, Status};
use crate::theme::metrics;
use dbui_app::commands;
use dbui_app::domain::ServerSession;
use gpui::{div, prelude::*, px, AnyElement, Context, MouseButton, SharedString};
use std::time::Duration;

/// Room for "Cancel it? Yes No" without clipping.
const ACTIONS_WIDTH: f32 = 190.;

/// How often an open panel reads the list again.
const REFRESH_EVERY: Duration = Duration::from_secs(2);

pub struct ActivityPanel {
    pub sessions: Vec<ServerSession>,
    /// False until the first answer lands, so an empty list is not claimed
    /// before it is known.
    pub loaded: bool,
    pub error: Option<String>,
    pub show_idle: bool,
    /// A row waiting on "are you sure?": its id, and whether it is the
    /// whole session (true) or just its statement.
    pub confirming: Option<(i64, bool)>,
    /// Bumped each time the panel opens; a refresh loop from an earlier
    /// opening sees the number change and stops.
    pub(crate) generation: u64,
}

impl ActivityPanel {
    /// The rows drawn: everything, or everything but the idle.
    pub fn visible(&self) -> Vec<&ServerSession> {
        self.sessions
            .iter()
            .filter(|session| self.show_idle || !session.is_idle() || session.is_self)
            .collect()
    }
}

/// "3.2 s", "4 min", "2 h 5 min" -- how long a statement has been going.
fn running_for(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.1} s")
    } else if seconds < 120.0 {
        format!("{seconds:.0} s")
    } else if seconds < 7200.0 {
        format!("{:.0} min", seconds / 60.0)
    } else {
        let minutes = (seconds / 60.0) as u64;
        format!("{} h {} min", minutes / 60, minutes % 60)
    }
}

/// One line of a statement: newlines and runs of spaces folded.
fn one_line(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl DbUi {
    pub(crate) fn open_activity(&mut self, cx: &mut Context<Self>) {
        if self.workspace.active_driver().is_none() {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        }
        if self
            .active_driver_kind()
            .is_some_and(|driver| driver.is_file_based())
        {
            self.status = Status::info("A SQLite file has no server, so no other sessions");
            cx.notify();
            return;
        }
        self.activity_generation = self.activity_generation.wrapping_add(1);
        self.activity = Some(ActivityPanel {
            sessions: Vec::new(),
            loaded: false,
            error: None,
            show_idle: false,
            confirming: None,
            generation: self.activity_generation,
        });
        self.close_chrome_menus();
        cx.notify();
        self.refresh_activity_loop(self.activity_generation, cx);
    }

    pub(crate) fn close_activity(&mut self, cx: &mut Context<Self>) {
        self.activity = None;
        cx.notify();
    }

    /// Read the list now, then every [`REFRESH_EVERY`] until the panel that
    /// started this closes.
    fn refresh_activity_loop(&mut self, generation: u64, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| loop {
            let task = this
                .update(cx, |this, _| {
                    let open = this
                        .activity
                        .as_ref()
                        .is_some_and(|panel| panel.generation == generation);
                    let driver = this.workspace.active_driver();
                    match (open, driver) {
                        (true, Some(driver)) => {
                            Some(commands::fetch_sessions(&this.runtime, driver))
                        }
                        _ => None,
                    }
                })
                .ok()
                .flatten();
            let Some(task) = task else {
                break;
            };
            let landed = task.await;
            let still_open = this
                .update(cx, |this, cx| {
                    let Some(panel) = this
                        .activity
                        .as_mut()
                        .filter(|panel| panel.generation == generation)
                    else {
                        return false;
                    };
                    match landed {
                        Some(Ok(sessions)) => {
                            panel.sessions = sessions;
                            panel.error = None;
                        }
                        Some(Err(error)) => panel.error = Some(error.to_string()),
                        None => return false,
                    }
                    panel.loaded = true;
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !still_open {
                break;
            }
            cx.background_executor().timer(REFRESH_EVERY).await;
        })
        .detach();
    }

    /// Read the list once, now -- after a cancel, so the row moves on
    /// without waiting for the next tick.
    fn refresh_activity_once(&mut self, cx: &mut Context<Self>) {
        let (Some(driver), Some(generation)) = (
            self.workspace.active_driver(),
            self.activity.as_ref().map(|panel| panel.generation),
        ) else {
            return;
        };
        let task = commands::fetch_sessions(&self.runtime, driver);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if let (Some(panel), Some(Ok(sessions))) = (
                    this.activity
                        .as_mut()
                        .filter(|panel| panel.generation == generation),
                    landed,
                ) {
                    panel.sessions = sessions;
                    panel.loaded = true;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn ask_end_session(&mut self, id: i64, terminate: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.activity.as_mut() {
            panel.confirming = Some((id, terminate));
        }
        cx.notify();
    }

    pub(crate) fn end_session(&mut self, id: i64, terminate: bool, cx: &mut Context<Self>) {
        if let Some(panel) = self.activity.as_mut() {
            panel.confirming = None;
        }
        // Somebody else's work, on a connection marked safe to browse.
        if self.refuse_if_read_only("Ending a session", cx) {
            return;
        }
        let Some(driver) = self.workspace.active_driver() else {
            return;
        };
        let what = if terminate {
            format!("Ended session {id}")
        } else {
            format!("Cancelled the statement on session {id}")
        };
        let task = commands::end_session(&self.runtime, driver, id, terminate);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.status = match landed {
                    Some(Ok(())) => Status::info(what),
                    Some(Err(error)) => Status::error(error.to_string()),
                    None => return,
                };
                this.refresh_activity_once(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn toggle_activity_idle(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = self.activity.as_mut() {
            panel.show_idle = !panel.show_idle;
        }
        cx.notify();
    }

    pub(crate) fn render_activity(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let panel = self.activity.as_ref()?;
        let theme = &self.theme;
        let visible = panel.visible();
        let active = panel
            .sessions
            .iter()
            .filter(|session| !session.is_idle())
            .count();
        let headline = format!(
            "{} session{} · {active} busy",
            panel.sessions.len(),
            if panel.sessions.len() == 1 { "" } else { "s" }
        );

        let cell = |width: f32| div().w(metrics::scaled(width)).flex_shrink_0().truncate();

        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_size(metrics::scaled(10.))
            .text_color(theme.text_faint)
            .child(cell(64.).child("ID"))
            .child(cell(96.).child("USER"))
            .child(cell(96.).child("DATABASE"))
            .child(cell(150.).child("STATE"))
            .child(cell(64.).text_right().child("TIME"))
            .child(div().flex_1().child("STATEMENT"))
            .child(div().w(metrics::scaled(ACTIONS_WIDTH)).flex_shrink_0());

        let rows: Vec<AnyElement> = visible
            .iter()
            .enumerate()
            .map(|(index, session)| {
                let id = session.id;
                let busy = !session.is_idle();
                // Long-running is what the panel exists to find.
                let slow = busy && session.running_for.is_some_and(|s| s >= 10.0);
                let idle_in_tx = session.state.contains("idle in transaction");
                let confirming = panel.confirming.filter(|(target, _)| *target == id);

                let actions: AnyElement = if session.is_self {
                    caption("this app", theme).into_any_element()
                } else if let Some((_, terminate)) = confirming {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .text_size(metrics::scaled(11.))
                                .text_color(theme.warning)
                                .child(if terminate { "End it?" } else { "Cancel it?" }),
                        )
                        .child(
                            button(("activity-yes", index), "Yes", theme, true)
                                .bg(theme.danger)
                                .border_color(theme.danger)
                                .on_click(cx.listener(move |this, _, _window, cx| {
                                    this.end_session(id, terminate, cx)
                                })),
                        )
                        .child(button(("activity-no", index), "No", theme, false).on_click(
                            cx.listener(|this, _, _window, cx| {
                                if let Some(panel) = this.activity.as_mut() {
                                    panel.confirming = None;
                                }
                                cx.notify();
                            }),
                        ))
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .gap_1()
                        // Idle in a transaction there is no statement to
                        // cancel; ending the session is what lets go.
                        .when(busy && !session.state.starts_with("idle"), |row| {
                            row.child(
                                button(("activity-cancel", index), "Cancel", theme, false)
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.ask_end_session(id, false, cx)
                                    })),
                            )
                        })
                        .child(
                            button(("activity-kill", index), "End", theme, false).on_click(
                                cx.listener(move |this, _, _window, cx| {
                                    this.ask_end_session(id, true, cx)
                                }),
                            ),
                        )
                        .into_any_element()
                };

                div()
                    .id(("activity-row", index))
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_4()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme.divider)
                    .when(slow || idle_in_tx, |row| {
                        row.bg(gpui::Rgba {
                            a: 0.08,
                            ..theme.warning
                        })
                    })
                    .child(
                        cell(64.)
                            .text_color(theme.value_number)
                            .child(SharedString::from(id.to_string())),
                    )
                    .child(cell(96.).child(SharedString::from(session.user.clone())))
                    .child(
                        cell(96.)
                            .text_color(theme.text_muted)
                            .child(SharedString::from(session.database.clone())),
                    )
                    .child(
                        cell(150.)
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_color(if idle_in_tx {
                                        theme.warning
                                    } else if busy {
                                        theme.success
                                    } else {
                                        theme.text_faint
                                    })
                                    .child(SharedString::from(session.state.clone())),
                            )
                            .children(session.waiting_on.clone().map(|waiting| {
                                div()
                                    .truncate()
                                    .text_size(metrics::scaled(10.))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(waiting))
                            })),
                    )
                    .child(
                        cell(64.)
                            .text_right()
                            .text_color(if slow {
                                theme.warning
                            } else {
                                theme.text_muted
                            })
                            .child(SharedString::from(
                                session
                                    .running_for
                                    .filter(|_| busy)
                                    .map(running_for)
                                    .unwrap_or_default(),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .font_family(metrics::MONO_FONT)
                                    .text_color(if busy { theme.text } else { theme.text_faint })
                                    .child(SharedString::from(one_line(&session.query))),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics::scaled(10.))
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(session.client.clone())),
                            ),
                    )
                    .child(
                        div()
                            .w(metrics::scaled(ACTIONS_WIDTH))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .child(actions),
                    )
                    .into_any_element()
            })
            .collect();

        let body: AnyElement = if let Some(error) = &panel.error {
            div()
                .p_4()
                .text_color(theme.danger)
                .child(SharedString::from(error.clone()))
                .into_any_element()
        } else if !panel.loaded {
            div()
                .p_4()
                .child(caption("Reading sessions…", theme))
                .into_any_element()
        } else if rows.is_empty() {
            div()
                .p_4()
                .child(caption(
                    "Nothing is running. Show idle to see every session.",
                    theme,
                ))
                .into_any_element()
        } else {
            div()
                .id("activity-rows")
                .flex_1()
                .min_h(px(0.))
                .overflow_y_scroll()
                .children(rows)
                .into_any_element()
        };

        let show_idle = panel.show_idle;
        Some(
            motion::dialog(
                "activity-in",
                div()
                    .id("activity-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("activity-panel")
                            .w(gpui::relative(0.9))
                            .max_w(metrics::scaled(1100.))
                            .h(gpui::relative(0.8))
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .bg(theme.elevated)
                            .border_1()
                            .border_color(theme.border)
                            .text_size(metrics::text_size_small())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_4()
                                    .py_3()
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(
                                        div()
                                            .text_size(metrics::scaled(15.))
                                            .child("Server activity"),
                                    )
                                    .child(caption(headline, theme))
                                    .child(div().flex_1())
                                    .child(caption("Refreshes every 2 s", theme))
                                    .child(
                                        button(
                                            "activity-idle",
                                            if show_idle { "Hide idle" } else { "Show idle" },
                                            theme,
                                            false,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _window, cx| {
                                                this.toggle_activity_idle(cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        button("activity-close", "Close", theme, false).on_click(
                                            cx.listener(|this, _, _window, cx| {
                                                this.close_activity(cx)
                                            }),
                                        ),
                                    ),
                            )
                            .child(header)
                            .child(body),
                    ),
                px(0.),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_at_a_glance() {
        assert_eq!(running_for(0.25), "0.2 s");
        assert_eq!(running_for(42.0), "42 s");
        assert_eq!(running_for(600.0), "10 min");
        assert_eq!(running_for(7_500.0), "2 h 5 min");
    }

    #[test]
    fn statements_fold_to_one_line() {
        assert_eq!(
            one_line("SELECT *\n  FROM t\n WHERE a = 1"),
            "SELECT * FROM t WHERE a = 1"
        );
    }
}
