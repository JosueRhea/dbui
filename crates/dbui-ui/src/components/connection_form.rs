//! The new/edit connection sheet.

use super::{button_with_focus, caption, motion};
use crate::root::DbUi;
use crate::text_input::{self, TextInput};
use crate::theme::{metrics, Theme};
use dbui_app::domain::{ConnectionConfig, Driver, Environment, TlsMode};
use gpui::{
    canvas, div, prelude::*, px, AnyElement, App, Context, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, SharedString,
};

/// One editable field of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Name,
    Host,
    Port,
    Username,
    Password,
    Database,
    /// Seconds before a statement is stopped. Blank for no limit.
    Timeout,
    /// A folder in the connection list. Blank for none.
    Group,
    SshHost,
    SshPort,
    SshUser,
    /// A private key file; blank for the agent and `~/.ssh/config`.
    SshKey,
    SshPassword,
}

impl Field {
    /// Whether this field means anything for a file-based engine.
    ///
    /// SQLite has no host, port, user or password. Drawing them would be
    /// asking for values that are then ignored.
    pub fn applies_to(self, driver: Driver) -> bool {
        if !driver.is_file_based() {
            return true;
        }
        // A runaway query is as much a thing on a local file as on a server.
        matches!(
            self,
            Field::Name | Field::Database | Field::Timeout | Field::Group
        )
    }

    /// Part of the SSH tunnel, drawn only while the tunnel is switched on.
    pub fn is_ssh(self) -> bool {
        matches!(
            self,
            Field::SshHost | Field::SshPort | Field::SshUser | Field::SshKey | Field::SshPassword
        )
    }

    /// Tab order, which is also the order they are drawn in.
    pub const ORDER: [Field; 13] = [
        Field::Name,
        Field::Host,
        Field::Port,
        Field::Username,
        Field::Password,
        Field::Database,
        Field::Timeout,
        Field::Group,
        Field::SshHost,
        Field::SshPort,
        Field::SshUser,
        Field::SshKey,
        Field::SshPassword,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Field::Name => "Name",
            Field::Host => "Host",
            Field::Port => "Port",
            Field::Username => "User",
            Field::Password => "Password",
            Field::Database => "Database",
            Field::Timeout => "Timeout",
            Field::Group => "Group",
            Field::SshHost => "SSH host",
            Field::SshPort => "SSH port",
            Field::SshUser => "SSH user",
            Field::SshKey => "Key file",
            Field::SshPassword => "SSH pass",
        }
    }

    /// Shown faint in an empty field, for the one whose empty means something.
    fn placeholder(self) -> Option<&'static str> {
        match self {
            Field::Timeout => Some("None · seconds"),
            Field::Group => Some("None · e.g. Acme"),
            Field::SshUser => Some("From ~/.ssh/config"),
            Field::SshKey => Some("Agent · ~/.ssh/config"),
            Field::SshPassword => Some("Password or key passphrase"),
            _ => None,
        }
    }

    /// Passwords render as bullets.
    fn is_secret(self) -> bool {
        matches!(self, Field::Password | Field::SshPassword)
    }
}

pub struct ConnectionForm {
    /// Carries the parts that are not free text: the id, the driver, the TLS
    /// mode. The text fields carry the rest.
    config: ConnectionConfig,
    fields: Vec<TextInput>,
    /// Tab order: fields first, then Cancel / Test / Save.
    focused: usize,
    pub testing: bool,
    /// `(ok, text)` -- the result of the last Test, or a validation complaint.
    message: Option<(bool, String)>,
    /// Editing an existing connection rather than creating one. Only changes
    /// the wording; saving is the same operation either way.
    editing: bool,
}

/// Which control in the connection sheet owns Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormAction {
    Field,
    Cancel,
    Test,
    Save,
}

impl ConnectionForm {
    pub fn new() -> Self {
        Self::from_config(ConnectionConfig::new(Driver::Postgres), false)
    }

    pub fn editing(config: ConnectionConfig) -> Self {
        Self::from_config(config, true)
    }

    fn from_config(config: ConnectionConfig, editing: bool) -> Self {
        let fields = Field::ORDER
            .iter()
            .map(|field| TextInput::with_text(field_value(&config, *field), false))
            .collect();

        Self {
            config,
            fields,
            focused: 0,
            testing: false,
            message: None,
            editing,
        }
    }

    fn focus_len(&self) -> usize {
        self.fields.len() + 3
    }

    fn cancel_index(&self) -> usize {
        self.fields.len()
    }

    fn test_index(&self) -> usize {
        self.fields.len() + 1
    }

    fn save_index(&self) -> usize {
        self.fields.len() + 2
    }

    pub fn focused_action(&self) -> FormAction {
        let n = self.fields.len();
        match self.focused {
            i if i < n => FormAction::Field,
            i if i == n => FormAction::Cancel,
            i if i == n + 1 => FormAction::Test,
            _ => FormAction::Save,
        }
    }

    pub fn focus_action(&mut self, action: FormAction) {
        self.focused = match action {
            FormAction::Field => 0,
            FormAction::Cancel => self.cancel_index(),
            FormAction::Test => self.test_index(),
            FormAction::Save => self.save_index(),
        };
    }

    pub fn set_message(&mut self, ok: bool, text: impl Into<String>) {
        self.message = Some((ok, text.into()));
    }

    /// Rebuild a config from what has been typed.
    ///
    /// A port that will not parse falls back to the driver's default rather
    /// than to zero: a half-typed port should not silently become an invalid
    /// one, and validation would only report a number the user never entered.
    pub fn to_config(&self) -> ConnectionConfig {
        let mut config = self.config.clone();
        config.name = self.text(Field::Name).trim().to_string();
        config.host = self.text(Field::Host).trim().to_string();
        config.port = self
            .text(Field::Port)
            .trim()
            .parse()
            .unwrap_or_else(|_| config.driver.default_port());
        config.username = self.text(Field::Username).trim().to_string();
        config.password = self.text(Field::Password).to_string();
        config.database = self.text(Field::Database).trim().to_string();
        config.read_only = self.config.read_only;
        // Anything that is not a whole number of seconds is no limit, which
        // is what an empty field says too.
        config.query_timeout_secs = self.text(Field::Timeout).trim().parse().unwrap_or(0);
        config.group = self.text(Field::Group).trim().to_string();
        config.ssh.host = self.text(Field::SshHost).trim().to_string();
        // Same rule as the database port: half-typed is the default, not zero.
        config.ssh.port = self
            .text(Field::SshPort)
            .trim()
            .parse()
            .unwrap_or(dbui_app::domain::SshConfig::DEFAULT_PORT);
        config.ssh.username = self.text(Field::SshUser).trim().to_string();
        config.ssh.key_path = self.text(Field::SshKey).trim().to_string();
        config.ssh.password = self.text(Field::SshPassword).to_string();
        config
    }

    /// Whether `field` is drawn -- and so whether Tab may land in it.
    pub fn shows(&self, field: Field) -> bool {
        field.applies_to(self.config.driver) && (!field.is_ssh() || self.ssh_enabled())
    }

    /// Whether the tunnel is switched on, for an engine that can use one.
    pub fn ssh_enabled(&self) -> bool {
        self.config.ssh.enabled && !self.config.driver.is_file_based()
    }

    /// Switching the tunnel off keeps what was typed in its fields, so
    /// trying a direct connection and coming back costs nothing.
    pub fn toggle_ssh(&mut self) {
        self.config.ssh.enabled = !self.config.ssh.enabled;
        if self.config.ssh.enabled {
            self.focus(Field::SshHost);
        } else if Field::ORDER
            .get(self.focused)
            .is_some_and(|field| field.is_ssh())
        {
            self.focus(Field::Name);
        }
    }

    pub fn driver(&self) -> Driver {
        self.config.driver
    }

    pub fn tls(&self) -> TlsMode {
        self.config.tls
    }

    /// Whether the sheet is showing a complaint rather than a result.
    ///
    /// The renderer reads `message` directly for its colour; this exists so a
    /// test can ask the question without the field being public.
    #[cfg(test)]
    pub fn has_problem(&self) -> bool {
        matches!(self.message, Some((false, _)))
    }

    pub fn read_only(&self) -> bool {
        self.config.read_only
    }

    pub fn toggle_read_only(&mut self) {
        self.config.read_only = !self.config.read_only;
    }

    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// Switching engines re-defaults the port and the user -- the old values
    /// were the other engine's conventions and are almost never right here.
    /// Anything the user actually typed elsewhere is left alone, except the
    /// database across a server/file switch: a database name is not a path to
    /// a file, and a path is not a database name.
    pub fn set_driver(&mut self, driver: Driver) {
        if self.config.driver == driver {
            return;
        }
        let previous = self.config.driver;
        self.config.driver = driver;

        if self.text(Field::Port) == previous.default_port().to_string() {
            self.set_text(Field::Port, driver.default_port().to_string());
        }
        let defaults = ConnectionConfig::new(driver);
        if self.text(Field::Username) == ConnectionConfig::new(previous).username {
            self.set_text(Field::Username, defaults.username);
        }
        if previous.is_file_based() != driver.is_file_based()
            || self.text(Field::Database) == previous.default_database()
        {
            self.set_text(Field::Database, defaults.database);
        }
        if self.text(Field::Name) == format!("New {}", previous.label()) {
            self.set_text(Field::Name, format!("New {}", driver.label()));
        }
        // Focus never stays in a field the new engine hides.
        if Field::ORDER
            .get(self.focused)
            .is_some_and(|field| !self.shows(*field))
        {
            self.focus(Field::Name);
        }
    }

    pub fn set_tls(&mut self, tls: TlsMode) {
        self.config.tls = tls;
    }

    pub fn environment(&self) -> Environment {
        self.config.environment
    }

    pub fn set_environment(&mut self, environment: Environment) {
        self.config.environment = environment;
    }

    pub fn focus(&mut self, field: Field) {
        if let Some(index) = Field::ORDER
            .iter()
            .position(|candidate| *candidate == field)
        {
            self.focused = index;
        }
    }

    pub fn handle_key(&mut self, keystroke: &Keystroke, cx: &App) -> bool {
        if keystroke.key == "tab" && !keystroke.modifiers.platform {
            let count = self.focus_len();
            let step = if keystroke.modifiers.shift {
                count - 1
            } else {
                1
            };
            // Past the fields this engine hides: Tab from a SQLite sheet's
            // Name used to land in the invisible Host box, and what was typed
            // next went nowhere anyone could see.
            loop {
                self.focused = (self.focused + step) % count;
                let hidden = Field::ORDER
                    .get(self.focused)
                    .is_some_and(|field| !self.shows(*field));
                if !hidden {
                    break;
                }
            }
            return true;
        }
        if self.focused >= self.fields.len() {
            return false;
        }
        let field = &mut self.fields[self.focused];
        let handled = field.handle_key(keystroke, cx);
        field.ensure_caret_visible();
        handled
    }

    pub fn field_mut(&mut self, index: usize) -> Option<&mut TextInput> {
        self.fields.get_mut(index)
    }

    fn text(&self, field: Field) -> &str {
        let index = Field::ORDER
            .iter()
            .position(|candidate| *candidate == field)
            .unwrap_or(0);
        self.fields[index].text()
    }

    fn set_text(&mut self, field: Field, value: impl Into<String>) {
        if let Some(index) = Field::ORDER
            .iter()
            .position(|candidate| *candidate == field)
        {
            self.fields[index].set_text(value);
        }
    }
}

impl Default for ConnectionForm {
    fn default() -> Self {
        Self::new()
    }
}

fn field_value(config: &ConnectionConfig, field: Field) -> String {
    match field {
        Field::Name => config.name.clone(),
        Field::Host => config.host.clone(),
        Field::Port => config.port.to_string(),
        Field::Username => config.username.clone(),
        Field::Password => config.password.clone(),
        Field::Database => config.database.clone(),
        // Blank rather than "0": the placeholder says what no limit means.
        Field::Timeout => match config.query_timeout_secs {
            0 => String::new(),
            seconds => seconds.to_string(),
        },
        Field::Group => config.group.clone(),
        Field::SshHost => config.ssh.host.clone(),
        Field::SshPort => config.ssh.port.to_string(),
        Field::SshUser => config.ssh.username.clone(),
        Field::SshKey => config.ssh.key_path.clone(),
        Field::SshPassword => config.ssh.password.clone(),
    }
}

impl DbUi {
    pub(crate) fn render_modal(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let Some(form) = self.modal.as_ref() else {
            return div().into_any_element();
        };

        let title = if form.is_editing() {
            "Edit Connection"
        } else {
            "New Connection"
        };

        let engine = form.config.driver;
        let (ssh_rows, rows): (Vec<_>, Vec<_>) = Field::ORDER
            .iter()
            .enumerate()
            // A file-based engine has no host, port, user or password, and a
            // switched-off tunnel has no fields at all.
            .filter(|(_, field)| form.shows(**field))
            .map(|(index, field)| {
                let focused = form.focused == index;
                let input = &form.fields[index];
                let label = if engine.is_file_based() && *field == Field::Database {
                    "File"
                } else {
                    field.label()
                };
                let hit_slot = input.hit_bounds_slot();
                // Panned with `left`, the way the app's other single-line
                // fields are, so a long path scrolls under the caret instead
                // of pushing the box -- and the sheet -- wider.
                let scroll_x = input.scroll_handle().offset().x;
                let field = *field;

                let row = div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .w(metrics::scaled(72.))
                            .flex_shrink_0()
                            .text_color(theme.text_muted)
                            .child(label),
                    )
                    .child(
                        div()
                            .id(("field", index))
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .relative()
                            .flex()
                            .items_center()
                            .h(metrics::scaled(28.))
                            .px_2()
                            .rounded_md()
                            .bg(theme.background)
                            .border_1()
                            .border_color(if focused { theme.accent } else { theme.border })
                            .font_family(metrics::MONO_FONT)
                            .cursor_text()
                            .child(
                                div().size_full().overflow_hidden().relative().child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .left(scroll_x)
                                        .h_full()
                                        .flex()
                                        .items_center()
                                        .child(render_field_text(input, field, focused, theme)),
                                ),
                            )
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        // Account for horizontal padding so
                                        // clicks line up with glyphs.
                                        let mut bounds = bounds;
                                        bounds.origin.x += px(8.);
                                        bounds.size.width =
                                            (bounds.size.width - px(16.)).max(px(0.));
                                        if hit_slot.get() != Some(bounds) {
                                            hit_slot.set(Some(bounds));
                                        }
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .size_full(),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                                    let Some(form) = this.modal.as_mut() else {
                                        return;
                                    };
                                    form.focus(field);
                                    let Some(input) = form.field_mut(index) else {
                                        return;
                                    };
                                    let offset = input.offset_for_mouse(
                                        event.position,
                                        px(0.),
                                        px(28.),
                                        text_input::char_width(),
                                    );
                                    input.click_at(
                                        offset,
                                        event.modifiers.shift,
                                        event.click_count,
                                    );
                                    // Drag-select only from a single click;
                                    // double/triple already chose a range.
                                    if event.click_count <= 1 {
                                        input.begin_selecting();
                                    } else {
                                        input.end_selecting();
                                    }
                                    input.ensure_caret_visible();
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(
                                move |this, event: &MouseMoveEvent, _, cx| {
                                    let Some(form) = this.modal.as_mut() else {
                                        return;
                                    };
                                    if form.focused != index {
                                        return;
                                    }
                                    let Some(input) = form.field_mut(index) else {
                                        return;
                                    };
                                    if !input.is_selecting() {
                                        return;
                                    }
                                    let offset = input.offset_for_mouse(
                                        event.position,
                                        px(0.),
                                        px(28.),
                                        text_input::char_width(),
                                    );
                                    input.select_to(offset);
                                    input.ensure_caret_visible();
                                    cx.notify();
                                },
                            ))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                                    if let Some(form) = this.modal.as_mut() {
                                        if let Some(input) = form.field_mut(index) {
                                            input.end_selecting();
                                        }
                                    }
                                    cx.notify();
                                }),
                            )
                            .on_mouse_up_out(
                                MouseButton::Left,
                                cx.listener(move |this, _: &MouseUpEvent, _, cx| {
                                    if let Some(form) = this.modal.as_mut() {
                                        if let Some(input) = form.field_mut(index) {
                                            input.end_selecting();
                                        }
                                    }
                                    cx.notify();
                                }),
                            ),
                    );
                (field.is_ssh(), row)
            })
            .partition(|(is_ssh, _)| *is_ssh);
        let rows: Vec<_> = rows.into_iter().map(|(_, row)| row).collect();
        let ssh_rows: Vec<_> = ssh_rows.into_iter().map(|(_, row)| row).collect();

        let driver_choice = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(metrics::scaled(72.))
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .child("Engine"),
            )
            .child(div().flex().gap_2().children(Driver::ALL.map(|driver| {
                let active = form.driver() == driver;
                div()
                    .id(("driver", driver as usize))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .h(metrics::scaled(28.))
                    .rounded_md()
                    .cursor_pointer()
                    .bg(if active {
                        theme.accent
                    } else {
                        theme.background
                    })
                    .text_color(if active {
                        theme.text_on_accent
                    } else {
                        theme.text_muted
                    })
                    .border_1()
                    .border_color(if active { theme.accent } else { theme.border })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if let Some(form) = this.modal.as_mut() {
                            form.set_driver(driver);
                        }
                        cx.notify();
                    }))
                    .child(super::dot(theme.driver_color(driver)))
                    .child(driver.label())
            })));

        let tls_choice = div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .w(metrics::scaled(72.))
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .child("TLS"),
            )
            .child(div().flex().gap_2().children(TlsMode::ALL.map(|mode| {
                let active = form.tls() == mode;
                div()
                    .id(("tls", mode as usize))
                    .flex()
                    .items_center()
                    .px_3()
                    .h(metrics::control_height())
                    .rounded_md()
                    .cursor_pointer()
                    .text_size(metrics::scaled(11.))
                    .bg(if active {
                        theme.elevated
                    } else {
                        theme.background
                    })
                    .text_color(if active { theme.text } else { theme.text_faint })
                    .border_1()
                    .border_color(if active { theme.accent } else { theme.border })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        if let Some(form) = this.modal.as_mut() {
                            form.set_tls(mode);
                        }
                        cx.notify();
                    }))
                    .child(mode.label())
            })));

        let current = form.environment();
        let environment_choice = labelled(
            "Tag",
            div()
                .flex()
                .flex_wrap()
                .gap_1()
                .children(Environment::ALL.map(|environment| {
                    let active = current == environment;
                    let color = theme.environment_color(environment);
                    div()
                        .id(("environment", environment as usize))
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .h(metrics::control_height())
                        .rounded_md()
                        .cursor_pointer()
                        .text_size(metrics::scaled(11.))
                        .bg(if active {
                            theme.elevated
                        } else {
                            theme.background
                        })
                        .text_color(if active { theme.text } else { theme.text_faint })
                        .border_1()
                        .border_color(match (active, color) {
                            (true, Some(color)) => color,
                            (true, None) => theme.accent,
                            (false, _) => theme.border,
                        })
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            if let Some(form) = this.modal.as_mut() {
                                form.set_environment(environment);
                            }
                            cx.notify();
                        }))
                        .children(color.map(super::dot))
                        .child(environment.label())
                })),
            theme,
        );

        let read_only = form.read_only();
        let read_only_choice = labelled(
            "Access",
            toggle_box(
                "read-only-toggle",
                read_only,
                theme.warning,
                "Read only — refuse every write",
                theme,
            )
            .on_click(cx.listener(|this, _, _window, cx| {
                if let Some(form) = this.modal.as_mut() {
                    form.toggle_read_only();
                }
                cx.notify();
            })),
            theme,
        );

        let ssh_choice = labelled(
            "Tunnel",
            toggle_box(
                "ssh-toggle",
                form.ssh_enabled(),
                theme.accent,
                "Connect through an SSH server",
                theme,
            )
            .on_click(cx.listener(|this, _, _window, cx| {
                if let Some(form) = this.modal.as_mut() {
                    form.toggle_ssh();
                }
                cx.notify();
            })),
            theme,
        );

        let message = form.message.as_ref().map(|(ok, text)| {
            div()
                .text_size(metrics::scaled(11.))
                .text_color(if *ok { theme.success } else { theme.danger })
                .child(SharedString::from(text.clone()))
        });

        let test_label = if form.testing { "Testing…" } else { "Test" };
        let cancel_focused = form.focused == form.cancel_index();
        let test_focused = form.focused == form.test_index();
        let save_focused = form.focused == form.save_index();

        // The scrim: a click outside the sheet dismisses it, the way every
        // other modal on the platform behaves.
        motion::dialog(
            "modal-in",
            div()
                .id("modal-scrim")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui::rgba(0x00000099))
                // Nothing behind the sheet hears the mouse. Without this a click
                // on a checkbox also landed on the grid row under it, selecting
                // it and opening its details behind the scrim.
                .occlude()
                .on_click(cx.listener(|this, _, _window, cx| this.close_modal(cx)))
                .child(
                    div()
                        .id("modal-sheet")
                        .w(metrics::scaled(420.))
                        // With the tunnel's fields open the sheet is taller than
                        // a small window. The fields scroll; the buttons stay.
                        .max_h(gpui::relative(0.92))
                        .flex()
                        .flex_col()
                        .rounded_lg()
                        .bg(theme.elevated)
                        .border_1()
                        .border_color(theme.border)
                        // Swallow clicks so hitting a field does not dismiss the
                        // sheet through the scrim behind it. An empty handler is
                        // not enough -- GPUI still bubbles unless stopped.
                        .on_click(|_, _window, cx| cx.stop_propagation())
                        .child(
                            div()
                                .id("modal-fields")
                                .flex_1()
                                .min_h(px(0.))
                                .overflow_y_scroll()
                                .flex()
                                .flex_col()
                                .gap_3()
                                .p_5()
                                .pb_3()
                                .child(div().text_size(metrics::scaled(15.)).child(title))
                                .child(driver_choice)
                                .children(rows)
                                .child(environment_choice)
                                // A local file has no transport to encrypt, and no
                                // server to tunnel to.
                                .when(!engine.is_file_based(), |sheet| {
                                    sheet.child(tls_choice).child(ssh_choice).children(ssh_rows)
                                })
                                .child(read_only_choice)
                                .child(caption(
                                    "Passwords are kept in the system keychain, never in the \
                                 connections file.",
                                    theme,
                                )),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .px_5()
                                .pb_5()
                                .children(message)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .pt_2()
                                        // Removal lives with the connection it removes,
                                        // and only appears when there is one to remove.
                                        .when(form.is_editing(), |actions| {
                                            let id = form.config.id;
                                            actions.child(
                                                div()
                                                    .id("form-remove")
                                                    .px_2()
                                                    .text_size(metrics::scaled(11.))
                                                    .text_color(theme.text_faint)
                                                    .cursor_pointer()
                                                    .hover(|label| label.text_color(theme.danger))
                                                    .on_click(cx.listener(
                                                        move |this, _, _window, cx| {
                                                            this.close_modal(cx);
                                                            this.remove_connection(id, cx);
                                                        },
                                                    ))
                                                    .child("Remove"),
                                            )
                                        })
                                        .child(div().flex_1())
                                        .child(
                                            button_with_focus(
                                                "form-cancel",
                                                "Cancel",
                                                theme,
                                                false,
                                                cancel_focused,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _window, cx| {
                                                    if let Some(form) = this.modal.as_mut() {
                                                        form.focus_action(FormAction::Cancel);
                                                    }
                                                    this.close_modal(cx);
                                                }),
                                            ),
                                        )
                                        .child(
                                            button_with_focus(
                                                "form-test",
                                                test_label,
                                                theme,
                                                false,
                                                test_focused,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _window, cx| {
                                                    if let Some(form) = this.modal.as_mut() {
                                                        form.focus_action(FormAction::Test);
                                                    }
                                                    this.test_connection(cx);
                                                }),
                                            ),
                                        )
                                        .child(
                                            button_with_focus(
                                                "form-save",
                                                "Save & Connect",
                                                theme,
                                                true,
                                                save_focused,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _window, cx| {
                                                    if let Some(form) = this.modal.as_mut() {
                                                        form.focus_action(FormAction::Save);
                                                    }
                                                    this.save_connection(cx);
                                                }),
                                            ),
                                        ),
                                ),
                        ),
                ),
            px(0.),
        )
        .into_any_element()
    }
}

/// A row with the sheet's left-hand label.
fn labelled(label: &'static str, control: impl IntoElement, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .w(metrics::scaled(72.))
                .flex_shrink_0()
                .text_color(theme.text_muted)
                .child(label),
        )
        .child(control)
}

/// A checkbox and its caption, as one click target.
fn toggle_box(
    id: &'static str,
    on: bool,
    on_color: gpui::Rgba,
    text: &'static str,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_2()
        .px_3()
        .h(metrics::control_height())
        .rounded_md()
        .cursor_pointer()
        .bg(if on { theme.elevated } else { theme.background })
        .border_1()
        .border_color(if on { on_color } else { theme.border })
        .child(
            div()
                .w(metrics::scaled(12.))
                .h(metrics::scaled(12.))
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .bg(if on { on_color } else { gpui::rgba(0x00000000) }),
        )
        .child(
            div()
                .text_size(metrics::scaled(11.))
                .text_color(if on { theme.text } else { theme.text_faint })
                .child(text),
        )
}

/// Draw a field's text with the caret and selection in it.
fn render_field_text(input: &TextInput, field: Field, focused: bool, theme: &Theme) -> AnyElement {
    let text = input.text();
    let selection = input.selection();
    let cursor = input.cursor();

    if let Some(placeholder) = field.placeholder().filter(|_| text.is_empty()) {
        let caret_color = if focused {
            theme.accent
        } else {
            gpui::rgba(0x00000000)
        };
        return div()
            .flex()
            .items_center()
            .w_full()
            .child(text_input::caret_element(caret_color, px(16.)))
            .child(div().text_color(theme.text_faint).child(placeholder))
            .into_any_element();
    }

    let mask = |part: &str| -> String {
        if field.is_secret() {
            "•".repeat(part.chars().count())
        } else {
            part.to_string()
        }
    };

    let caret_color = if focused {
        theme.accent
    } else {
        gpui::rgba(0x00000000)
    };

    // No caret in the flow while a range is selected -- the highlight is the
    // affordance, and a laid-out caret used to shove glyphs sideways.
    if selection.start != selection.end {
        let before = &text[..selection.start];
        let selected = &text[selection.clone()];
        let after = &text[selection.end..];
        return div()
            .flex()
            .items_center()
            .w_full()
            .text_color(theme.text)
            .child(mask(before))
            .child(
                div()
                    .bg(theme.selection)
                    .flex()
                    .items_center()
                    .child(mask(selected)),
            )
            .child(mask(after))
            .into_any_element();
    }

    let (before, after) = text.split_at(cursor);
    div()
        .flex()
        .items_center()
        .w_full()
        .text_color(theme.text)
        .child(mask(before))
        .child(text_input::caret_element(caret_color, px(16.)))
        .child(mask(after))
        .into_any_element()
}
