//! "Parameters" -- the values for a statement's `:name` placeholders, asked
//! for before it runs.
//!
//! Each name gets a field, filled with what it was given last time, so
//! re-running a statement with one value changed is Enter, a few keys, and
//! Enter. Values go in as literals (see `dbui_domain::params`): numbers,
//! NULL and true/false as they are, anything else as a quoted string.

use super::text_field::{text_field, InputTarget};
use super::{button, caption, motion};
use crate::root::DbUi;
use crate::text_input::TextInput;
use crate::theme::metrics;
use dbui_app::domain::params;
use gpui::{div, prelude::*, AnyElement, Context, Keystroke, MouseButton, SharedString};

pub struct ParamSheet {
    pub names: Vec<String>,
    pub inputs: Vec<TextInput>,
    pub focused: usize,
    /// The statements waiting on the values, as the editor resolved them.
    pub statements: Vec<String>,
    /// Asked for by Explain rather than Run.
    pub explain: bool,
}

impl DbUi {
    /// Run (or explain) `statements`, asking first for any `:name` in them.
    /// Returns false when there were none and the caller should go ahead.
    pub(crate) fn ask_for_params(
        &mut self,
        statements: &[String],
        explain: bool,
        cx: &mut Context<Self>,
    ) -> bool {
        let dialect = self.sql_dialect();
        let mut names: Vec<String> = Vec::new();
        for sql in statements {
            for name in params::names(dialect, sql) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            return false;
        }
        let inputs = names
            .iter()
            .map(|name| {
                let mut input = TextInput::with_text(
                    self.param_values.get(name).cloned().unwrap_or_default(),
                    false,
                );
                input.select_all();
                input
            })
            .collect();
        self.param_sheet = Some(ParamSheet {
            names,
            inputs,
            focused: 0,
            statements: statements.to_vec(),
            explain,
        });
        self.completion = None;
        cx.notify();
        true
    }

    /// Fill the values in and send the statements on.
    pub(crate) fn run_with_params(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.param_sheet.take() else {
            return;
        };
        let dialect = self.sql_dialect();
        for (name, input) in sheet.names.iter().zip(&sheet.inputs) {
            self.param_values
                .insert(name.clone(), input.text().to_string());
        }
        let values = &self.param_values;
        let filled: Vec<String> = sheet
            .statements
            .iter()
            .map(|sql| params::substitute(dialect, sql, |name| values.get(name).cloned()))
            .map(|sql| {
                if sheet.explain {
                    dbui_app::plan::explain_sql(dialect, &sql)
                } else {
                    sql
                }
            })
            .collect();
        self.dispatch_statements(filled, cx);
    }

    pub(crate) fn close_param_sheet(&mut self, cx: &mut Context<Self>) {
        self.param_sheet = None;
        cx.notify();
    }

    pub(crate) fn handle_param_sheet_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) {
        let Some(sheet) = self.param_sheet.as_mut() else {
            return;
        };
        match keystroke.key.as_str() {
            "escape" => return self.close_param_sheet(cx),
            // Unmodified, so the ⌘↵ that opened the sheet does not also
            // answer it.
            "enter" if !keystroke.modifiers.platform => return self.run_with_params(cx),
            "tab" => {
                let count = sheet.inputs.len();
                sheet.focused = if keystroke.modifiers.shift {
                    (sheet.focused + count - 1) % count
                } else {
                    (sheet.focused + 1) % count
                };
                if let Some(input) = sheet.inputs.get_mut(sheet.focused) {
                    input.select_all();
                }
                cx.notify();
                return;
            }
            _ => {}
        }
        let focused = sheet.focused;
        if let Some(input) = sheet.inputs.get_mut(focused) {
            if input.handle_key(keystroke, cx) {
                input.ensure_caret_visible();
                cx.notify();
            }
        }
    }

    pub(crate) fn render_param_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.param_sheet.as_ref()?;
        let theme = &self.theme;
        let rows: Vec<AnyElement> = sheet
            .names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(metrics::scaled(120.))
                            .flex_shrink_0()
                            .truncate()
                            .font_family(metrics::MONO_FONT)
                            .text_color(theme.value_structured)
                            .child(SharedString::from(format!(":{name}"))),
                    )
                    .child(div().flex_1().min_w(gpui::px(0.)).child(text_field(
                        ("param-field", index),
                        &sheet.inputs[index],
                        InputTarget::ParamField(index),
                        sheet.focused == index,
                        Some("value"),
                        theme,
                        cx,
                    )))
                    .into_any_element()
            })
            .collect();
        let verb = if sheet.explain { "Explain" } else { "Run" };

        Some(
            motion::dialog(
                "params-in",
                div()
                    .id("params-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .bg(gpui::rgba(0x00000066))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("params-panel")
                            .w(metrics::scaled(460.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .rounded(gpui::px(12.))
                            .bg(theme.elevated)
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child("Parameters"),
                            )
                            .children(rows)
                            .child(caption(
                                "Numbers, NULL, true and false go in as they are; anything \
                                 else as a string. Tab moves, ↵ runs, Esc cancels.",
                                theme,
                            ))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(
                                        button("params-cancel", "Cancel", theme, false).on_click(
                                            cx.listener(|this, _, _window, cx| {
                                                this.close_param_sheet(cx)
                                            }),
                                        ),
                                    )
                                    .child(button("params-run", verb, theme, true).on_click(
                                        cx.listener(|this, _, _window, cx| {
                                            this.run_with_params(cx)
                                        }),
                                    )),
                            ),
                    ),
                metrics::scaled(140.),
            )
            .into_any_element(),
        )
    }
}
