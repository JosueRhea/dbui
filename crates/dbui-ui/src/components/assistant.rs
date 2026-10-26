//! The assistant panel: a chat with the user's own Claude Code, given the
//! active connection's tables to look at and its SQL to run.
//!
//! The process, the tools and the gate every statement passes live in
//! [`dbui_app::assistant`]; this is the panel, one conversation per
//! connection, and the Approve / Deny a write waits on in Ask mode.

use super::text_field::{sized_text_field, FieldHeight, InputTarget};
use super::{button, caption, icon_button};
use crate::root::{DbUi, Focus, Status};
use crate::theme::{metrics, Theme};
use dbui_app::assistant::{AccessMode, Entry, Event, Launch, Session, Thread, ToolState};
use dbui_app::domain::ConnectionId;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};

const PANEL_WIDTH: f32 = 380.;

/// One connection's conversation and the process behind it.
#[derive(Default)]
pub(crate) struct AssistantChat {
    pub(crate) thread: Thread,
    session: Option<Session>,
    /// Bumped per process, so a pump left over from an earlier one cannot
    /// write into the conversation that replaced it.
    generation: u64,
}

impl DbUi {
    pub(crate) fn toggle_assistant(&mut self, cx: &mut Context<Self>) {
        self.assistant_open = !self.assistant_open;
        if self.assistant_open {
            self.focus_input(InputTarget::AssistantPrompt, cx);
        } else if self.focus == Focus::Assistant {
            self.focus = Focus::Grid;
        }
        cx.notify();
    }

    pub(crate) fn set_assistant_mode(&mut self, mode: AccessMode, cx: &mut Context<Self>) {
        self.assistant_mode = mode;
        for chat in self.assistant_chats.values() {
            if let Some(session) = &chat.session {
                session.set_mode(mode);
            }
        }
        cx.notify();
    }

    pub(crate) fn send_assistant_message(&mut self, cx: &mut Context<Self>) {
        let text = self.assistant_input.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(entry) = self.workspace.active() else {
            self.status = Status::error("Connect to a database to use the assistant");
            cx.notify();
            return;
        };
        let Some(driver) = entry.status.driver().cloned() else {
            self.status = Status::error("Connect to a database to use the assistant");
            cx.notify();
            return;
        };
        let id = entry.config.id;
        let config = entry.config.clone();
        if self
            .assistant_chats
            .get(&id)
            .is_some_and(|chat| chat.thread.busy)
        {
            return;
        }

        let mode = self.assistant_mode;
        let chat = self.assistant_chats.entry(id).or_default();
        if chat.session.is_none() || chat.thread.ended {
            chat.generation += 1;
            chat.thread.ended = false;
            let (session, mut events) = Session::start(
                &self.runtime,
                Launch {
                    driver,
                    config,
                    mode,
                },
            );
            chat.session = Some(session);
            let generation = chat.generation;
            cx.spawn(async move |this, cx| {
                while let Some(event) = events.recv().await {
                    let alive = this.update(cx, |this, cx| {
                        this.on_assistant_event(id, generation, event, cx)
                    });
                    if alive.is_err() {
                        break;
                    }
                }
            })
            .detach();
        }
        if let Some(session) = &chat.session {
            session.send(&text);
        }
        chat.thread.push_user(text);
        self.assistant_input.clear();
        self.assistant_scroll.scroll_to_bottom();
        cx.notify();
    }

    fn on_assistant_event(
        &mut self,
        id: ConnectionId,
        generation: u64,
        event: Event,
        cx: &mut Context<Self>,
    ) {
        let Some(chat) = self.assistant_chats.get_mut(&id) else {
            return;
        };
        if chat.generation != generation {
            return;
        }
        if matches!(event, Event::Exited { .. }) {
            chat.session = None;
        }
        chat.thread.apply(event);
        self.assistant_scroll.scroll_to_bottom();
        cx.notify();
    }

    pub(crate) fn answer_assistant(&mut self, call: u64, approve: bool, cx: &mut Context<Self>) {
        if let Some(session) = self.active_assistant().and_then(|c| c.session.as_ref()) {
            session.answer(call, approve);
        }
        cx.notify();
    }

    pub(crate) fn stop_assistant(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.active_assistant().and_then(|c| c.session.as_ref()) {
            session.interrupt();
        }
        cx.notify();
    }

    /// Forget the conversation; the next message starts a fresh process.
    pub(crate) fn new_assistant_chat(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.workspace.active_id() {
            self.assistant_chats.remove(&id);
        }
        cx.notify();
    }

    /// A closed connection's agent has nothing left to talk to.
    pub(crate) fn end_assistant(&mut self, id: ConnectionId) {
        if let Some(chat) = self.assistant_chats.get_mut(&id) {
            chat.session = None;
            chat.thread.apply(Event::Exited { error: None });
        }
    }

    fn active_assistant(&self) -> Option<&AssistantChat> {
        self.assistant_chats.get(&self.workspace.active_id()?)
    }

    pub(crate) fn render_assistant(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let entry = self.workspace.active();
        let connected = entry.is_some_and(|e| e.status.driver().is_some());
        let read_only = entry.is_some_and(|e| e.config.read_only);
        let production = entry.is_some_and(|e| e.config.environment.confirms_writes());
        let chat = self.active_assistant();
        let busy = chat.is_some_and(|c| c.thread.busy);
        let model = chat.and_then(|c| c.thread.model.clone());
        let mode = self.assistant_mode;

        let mut body: Vec<AnyElement> = Vec::new();
        match chat {
            _ if !connected => body.push(
                caption("Connect to a database to use the assistant.", theme).into_any_element(),
            ),
            Some(chat) if !chat.thread.entries.is_empty() => {
                for (index, entry) in chat.thread.entries.iter().enumerate() {
                    body.push(render_entry(index, entry, theme, cx));
                }
                if busy
                    && !matches!(
                        chat.thread.entries.last(),
                        Some(Entry::Tool {
                            state: ToolState::AwaitingApproval,
                            ..
                        })
                    )
                {
                    body.push(caption("Thinking…", theme).into_any_element());
                }
            }
            _ => body.push(
                caption(
                    "Ask about this database: what's in it, how tables relate, or what the data says. Runs on the Claude Code you have installed.",
                    theme,
                )
                .into_any_element(),
            ),
        }

        let note = if read_only {
            Some("This connection is read only: writes are always refused.")
        } else if production && mode == AccessMode::FullAccess {
            Some("Production: writes still ask first.")
        } else {
            None
        };

        let modes = div()
            .flex()
            .gap_1()
            .children(
                AccessMode::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, option)| {
                        let active = option == mode;
                        div()
                            .id(("assistant-mode", index))
                            .px_2()
                            .py_0p5()
                            .rounded(px(4.))
                            .border_1()
                            .cursor_pointer()
                            .text_size(metrics::text_size_small())
                            .when(active, |pill| {
                                pill.bg(theme.accent)
                                    .border_color(theme.accent)
                                    .text_color(theme.text_on_accent)
                            })
                            .when(!active, |pill| {
                                pill.border_color(theme.border)
                                    .text_color(theme.text_muted)
                                    .hover(|pill| pill.bg(theme.hover).text_color(theme.text))
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_assistant_mode(option, cx)
                            }))
                            .child(option.label())
                    }),
            );

        let focused = self.focus == Focus::Assistant;
        let field = sized_text_field(
            "assistant-prompt",
            &self.assistant_input,
            InputTarget::AssistantPrompt,
            focused,
            Some("Ask the assistant…"),
            FieldHeight::Capped,
            theme,
            cx,
        );
        let action = if busy {
            button("assistant-stop", "Stop", theme, false)
                .on_click(cx.listener(|this, _, _, cx| this.stop_assistant(cx)))
        } else {
            button("assistant-send", "Send", theme, true)
                .on_click(cx.listener(|this, _, _, cx| this.send_assistant_message(cx)))
        };

        div()
            .id("assistant-panel")
            .w(metrics::scaled(PANEL_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .text_size(metrics::text_size())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(metrics::toolbar_height())
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_color(theme.text).child("Assistant"))
                    .children(model.map(|model| {
                        div()
                            .text_color(theme.text_faint)
                            .text_size(metrics::text_size_small())
                            .child(model)
                    }))
                    .child(div().flex_1())
                    .child(
                        icon_button("assistant-new", "+", theme, false)
                            .on_click(cx.listener(|this, _, _, cx| this.new_assistant_chat(cx))),
                    )
                    .child(
                        icon_button("assistant-close", "×", theme, false)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_assistant(cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(modes)
                    .child(caption(note.unwrap_or(mode.description()), theme)),
            )
            .child(
                div()
                    .id("assistant-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.assistant_scroll)
                    .child(div().flex().flex_col().gap_2().p_3().children(body)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(field)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(div().flex_1().child(caption("↵ send · ⇧↵ new line", theme)))
                            .child(action),
                    ),
            )
            .into_any_element()
    }
}

fn render_entry(index: usize, entry: &Entry, theme: &Theme, cx: &mut Context<DbUi>) -> AnyElement {
    match entry {
        Entry::User(text) => div()
            .flex()
            .justify_end()
            .child(
                div()
                    .max_w(metrics::scaled(PANEL_WIDTH - 60.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(theme.selection)
                    .text_color(theme.text)
                    .child(text.clone()),
            )
            .into_any_element(),
        Entry::Assistant(text) => render_text(text, theme),
        Entry::Notice { text, error } => div()
            .text_size(metrics::text_size_small())
            .text_color(if *error {
                theme.danger
            } else {
                theme.text_muted
            })
            .child(text.clone())
            .into_any_element(),
        Entry::Tool {
            call,
            tool,
            detail,
            state,
        } => {
            let call = *call;
            let (status, color): (SharedString, _) = match state {
                ToolState::Running => ("running…".into(), theme.text_faint),
                ToolState::AwaitingApproval => ("needs approval".into(), theme.warning),
                ToolState::Done(summary) => (summary.clone().into(), theme.success),
                ToolState::Failed(_) => ("refused".into(), theme.danger),
            };
            let waiting = matches!(state, ToolState::AwaitingApproval);
            let label = match tool.as_str() {
                "run_query" => "Query".to_string(),
                "describe_table" => format!("Describe {}", detail.as_deref().unwrap_or("")),
                "list_tables" => "List tables".to_string(),
                other => other.to_string(),
            };
            let sql = (tool == "run_query").then(|| detail.clone()).flatten();
            div()
                .id(("assistant-tool", index))
                .flex()
                .flex_col()
                .gap_1()
                .p_2()
                .rounded_md()
                .border_1()
                .border_color(if waiting { theme.warning } else { theme.border })
                .bg(theme.elevated)
                .text_size(metrics::text_size_small())
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(div().flex_1().text_color(theme.text_muted).child(label))
                        .child(div().text_color(color).child(status)),
                )
                .children(sql.map(|sql| {
                    div()
                        .font_family(metrics::MONO_FONT)
                        .text_color(theme.text)
                        .child(sql)
                }))
                .children(match state {
                    ToolState::Failed(reason) => Some(
                        div()
                            .text_color(theme.danger)
                            .child(reason.clone())
                            .into_any_element(),
                    ),
                    _ => None,
                })
                .children(waiting.then(|| {
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            button(("assistant-deny", index), "Deny", theme, false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.answer_assistant(call, false, cx)
                                }),
                            ),
                        )
                        .child(
                            button(("assistant-approve", index), "Run it", theme, true).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.answer_assistant(call, true, cx)
                                }),
                            ),
                        )
                }))
                .into_any_element()
        }
    }
}

/// Prose, with fenced code drawn as code.
fn render_text(text: &str, theme: &Theme) -> AnyElement {
    let blocks = text.split("```").enumerate().filter_map(|(index, part)| {
        if index % 2 == 0 {
            let prose = part.trim();
            (!prose.is_empty()).then(|| {
                div()
                    .text_color(theme.text)
                    .child(prose.to_string())
                    .into_any_element()
            })
        } else {
            // The fence's first line names the language.
            let code = part
                .split_once('\n')
                .map_or(part, |(_, rest)| rest)
                .trim_end();
            Some(
                div()
                    .p_2()
                    .rounded_md()
                    .bg(theme.background)
                    .border_1()
                    .border_color(theme.border)
                    .font_family(metrics::MONO_FONT)
                    .text_size(metrics::text_size_small())
                    .text_color(theme.text)
                    .child(code.to_string())
                    .into_any_element(),
            )
        }
    });
    div()
        .flex()
        .flex_col()
        .gap_2()
        .children(blocks)
        .into_any_element()
}
