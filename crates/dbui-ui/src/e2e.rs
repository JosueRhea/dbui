//! End-to-end tests: a real window, real keystrokes, real state.
//!
//! These open an actual GPUI window through the test harness and dispatch
//! keystrokes into it, so they cover the whole path -- window focus, the key
//! handler, the modal, the editor -- rather than calling methods directly.
//!
//! The first test exists because that path once broke in a way no unit test
//! could have caught: the app drew perfectly and every shortcut was dead,
//! because focus set at construction was lost before the first key arrived.
//!
//! Nothing here touches a database. Anything that would need a server asserts
//! the refusal instead, which is the behaviour worth pinning anyway. For tests
//! against real servers see `crates/dbui-driver/tests/live.rs`.

use crate::root::{DbUi, Focus, Status};
use crate::tabs::WorkspaceTab;
use dbui_app::domain::{ConnectionConfig, Driver};
use dbui_app::{DbRuntime, Workspace};
use gpui::{AppContext as _, Entity, TestAppContext, VisualContext as _, VisualTestContext};

/// Open a window on an empty workspace.
fn open(cx: &mut TestAppContext) -> (Entity<DbUi>, &mut VisualTestContext) {
    open_with(cx, Workspace::new())
}

fn open_with(
    cx: &mut TestAppContext,
    workspace: Workspace,
) -> (Entity<DbUi>, &mut VisualTestContext) {
    let runtime = DbRuntime::new().expect("runtime");
    cx.update(|cx| crate::load_bundled_fonts(cx));
    cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        window.focus(&focus);
        DbUi::new(runtime, workspace, focus)
    })
}

/// Spell text as keystrokes the harness will deliver with a `key_char`.
///
/// `Keystroke::parse("a")` sets `key` but leaves `key_char` empty, and typed
/// text is read from `key_char` -- the only field that is right for shifted
/// keys and non-US layouts. `a->a` is the harness's way of saying "the
/// platform delivered the character a".
///
/// Space is the exception: a literal space cannot appear inside a keystroke
/// (the list is space-separated), so it goes through as the named key, which
/// [`TextInput::handle_key`] handles on its own.
///
/// [`TextInput::handle_key`]: crate::text_input::TextInput::handle_key
fn typing(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == ' ' {
                "space".to_string()
            } else {
                format!("{c}->{c}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Enough backspaces to empty any field this app pre-fills.
fn clear_field() -> String {
    vec!["backspace"; 32].join(" ")
}

#[gpui::test]
fn cmd_n_opens_the_connection_sheet(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |view, _| assert!(view.modal.is_none()));

    cx.simulate_keystrokes("cmd-n");

    view.update(cx, |view, _| assert!(view.modal.is_some()));
}

/// The regression test for the bug described at the top of this file.
///
/// `window.focus()` at construction is lost if the window was not key yet, and
/// then `on_key_down` never fires: the app draws perfectly and every shortcut
/// is dead. `Render::render` re-asserts focus when nothing holds it, and this
/// reproduces the condition by blurring the window before typing.
///
/// Without that re-assertion this test fails and the rest of the suite still
/// passes, because the test platform's window *is* key at construction -- so
/// no other test here covers it.
#[gpui::test]
fn shortcuts_survive_the_window_losing_focus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    let handle = cx.window_handle();
    cx.update_window(handle, |_, window, _| window.blur())
        .expect("window is open");

    cx.simulate_keystrokes("cmd-n");

    view.update(cx, |view, _| {
        assert!(
            view.modal.is_some(),
            "a blurred root must take focus back on the next frame"
        );
    });
}

#[gpui::test]
fn escape_closes_the_sheet(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-n");
    view.update(cx, |view, _| assert!(view.modal.is_some()));

    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert!(view.modal.is_none()));
}

#[gpui::test]
fn typing_reaches_the_focused_field(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-n");
    // The Name field holds focus when the sheet opens.
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes(&typing("Prod"));

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().expect("sheet open").to_config();
        assert_eq!(config.name, "Prod");
    });
}

#[gpui::test]
fn sheet_field_supports_select_all_copy_and_paste(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes(&typing("Prod"));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-c");

    let copied = cx
        .read_from_clipboard()
        .and_then(|item| item.text())
        .expect("clipboard after copy");
    assert_eq!(copied, "Prod");

    cx.simulate_keystrokes("cmd-v");
    view.update(cx, |view, _| {
        let config = view.modal.as_ref().expect("sheet open").to_config();
        // Select-all then paste replaces with the same text.
        assert_eq!(config.name, "Prod");
    });

    cx.simulate_keystrokes("right");
    cx.simulate_keystrokes("cmd-v");
    view.update(cx, |view, _| {
        let config = view.modal.as_ref().expect("sheet open").to_config();
        assert_eq!(config.name, "ProdProd");
    });
}

#[gpui::test]
fn tab_walks_the_fields_and_wraps(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-n");

    // Name -> Host. Typing after one tab must change the host, not the name.
    cx.simulate_keystrokes("tab");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes(&typing("db.internal"));

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().unwrap().to_config();
        assert_eq!(config.host, "db.internal");
        assert_eq!(config.name, "New PostgreSQL", "the name was left alone");
    });

    // Port, User, Password, Database, then Cancel / Test / Save, then wrap to Name.
    cx.simulate_keystrokes("tab tab tab tab tab tab tab tab");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes(&typing("wrapped"));

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().unwrap().to_config();
        assert_eq!(config.name, "wrapped", "tab should wrap past buttons back to Name");
        assert_eq!(config.host, "db.internal", "host stays put");
    });
}

#[gpui::test]
fn shift_tab_walks_backwards(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-n");

    // From Name: Save → Test → Cancel → Database.
    cx.simulate_keystrokes("shift-tab shift-tab shift-tab shift-tab");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes(&typing("shop"));

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().unwrap().to_config();
        assert_eq!(config.database, "shop");
    });
}

#[gpui::test]
fn a_password_is_typed_but_never_shown_in_the_config_summary(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-n");

    // Name, Host, Port, User, then Password.
    cx.simulate_keystrokes("tab tab tab tab");
    cx.simulate_keystrokes(&typing("hunter2"));

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().unwrap().to_config();
        assert_eq!(config.password, "hunter2", "the field holds it");
        assert!(
            !config.summary().contains("hunter2"),
            "but the summary shown in the UI must not"
        );
    });
}

#[gpui::test]
fn an_unnamed_connection_is_refused_and_the_sheet_stays_open(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    cx.simulate_keystrokes("cmd-n");

    // Empty the Name field, then try to save with Enter.
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_keystrokes("enter");

    view.update(cx, |view, _| {
        assert!(
            view.modal.is_some(),
            "an invalid config must not close the sheet"
        );
        assert!(view.workspace.is_empty(), "and must not be saved");
    });
}

#[gpui::test]
fn running_a_query_without_a_connection_says_so(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
        set_sql_editor_text(view, "SELECT 1");
    });
    cx.simulate_keystrokes("cmd-enter");

    view.update(cx, |view, _| match &view.status {
        Status::Error(message) => assert_eq!(message.as_ref(), "Not connected"),
        other => panic!("expected an error, got {}", describe(other)),
    });
}

#[gpui::test]
fn an_empty_query_does_nothing_at_all(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
    });
    cx.simulate_keystrokes("cmd-enter");

    view.update(cx, |view, _| {
        assert!(
            matches!(view.status, Status::Idle),
            "got {}",
            describe(&view.status)
        );
    });
}

#[gpui::test]
fn the_editor_takes_typed_text_only_when_it_has_focus(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| open_sql_editor(view, cx));
    view.update(cx, |view, _| view.focus = Focus::Sidebar);

    cx.simulate_keystrokes(&typing("nope"));
    view.update(cx, |view, _| assert_eq!(sql_editor_text(view), ""));

    view.update(cx, |view, _| view.focus = Focus::Editor);
    cx.simulate_keystrokes(&typing("select 1"));
    view.update(cx, |view, _| assert_eq!(sql_editor_text(view), "select 1"));
}

#[gpui::test]
fn enter_in_the_editor_is_a_newline_not_a_run(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
    });
    cx.simulate_keystrokes(&typing("a"));
    cx.simulate_keystrokes("enter");
    cx.simulate_keystrokes(&typing("b"));

    view.update(cx, |view, _| assert_eq!(sql_editor_text(view), "a\nb"));
}

#[gpui::test]
fn cmd_e_opens_the_sql_tab_and_cmd_k_clears_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-e");
    view.update(cx, |view, _| {
        assert!(matches!(view.tabs.active(), Some(WorkspaceTab::Sql { .. })));
        assert_eq!(view.focus, Focus::Editor);
    });

    view.update(cx, |view, _| set_sql_editor_text(view, "SELECT 1"));
    cx.simulate_keystrokes("cmd-k");
    view.update(cx, |view, _| assert!(sql_editor_text(view).is_empty()));
}

#[gpui::test]
fn escape_hands_the_keyboard_back_to_the_sidebar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
    });
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert_eq!(view.focus, Focus::Sidebar));
}

#[gpui::test]
fn paging_shortcuts_are_inert_without_an_open_table(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-] cmd-[");

    view.update(cx, |view, _| {
        assert!(view.tabs.items.is_empty());
        assert!(matches!(view.status, Status::Idle));
    });
}

#[gpui::test]
fn a_saved_connection_is_listed_and_starts_disconnected(cx: &mut TestAppContext) {
    let mut config = ConnectionConfig::new(Driver::Postgres);
    config.name = "Staging".into();
    let workspace = Workspace::from_configs([config]);

    let (view, cx) = open_with(cx, workspace);

    view.update(cx, |view, _| {
        assert_eq!(view.workspace.entries().len(), 1);
        let entry = view.workspace.active().expect("one is active");
        assert_eq!(entry.config.name, "Staging");
        assert!(!entry.status.is_connected());
        assert!(entry.catalog.is_none());
    });
}

#[gpui::test]
fn editing_a_connection_opens_the_sheet_with_its_values(cx: &mut TestAppContext) {
    let mut config = ConnectionConfig::new(Driver::MySql);
    config.name = "Reports".into();
    config.host = "mysql.internal".into();
    let id = config.id;
    let workspace = Workspace::from_configs([config]);

    let (view, cx) = open_with(cx, workspace);

    view.update(cx, |view, cx| view.edit_connection(id, cx));
    view.update(cx, |view, _| {
        let form = view.modal.as_ref().expect("sheet open");
        let config = form.to_config();
        assert_eq!(config.name, "Reports");
        assert_eq!(config.host, "mysql.internal");
        assert_eq!(config.driver, Driver::MySql);
        assert!(form.is_editing());
    });
}

fn sql_editor_text(view: &DbUi) -> String {
    match view.tabs.active() {
        Some(WorkspaceTab::Sql { editor, .. }) => editor.text().to_string(),
        _ => String::new(),
    }
}

fn open_sql_editor(view: &mut DbUi, cx: &mut gpui::Context<DbUi>) {
    view.open_sql_tab(cx);
}

fn set_sql_editor_text(view: &mut DbUi, text: &str) {
    if let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active_mut() {
        editor.set_text(text);
    }
}

fn describe(status: &Status) -> String {
    match status {
        Status::Idle => "idle".into(),
        Status::Busy(text) => format!("busy: {text}"),
        Status::Info(text) => format!("info: {text}"),
        Status::Error(text) => format!("error: {text}"),
    }
}

// -- layout stability -----------------------------------------------------

/// Where a field actually landed on the last painted frame.
///
/// The paint pass writes the field's bounds into the input's hit-bounds slot,
/// so reading it back after a frame is the closest thing to measuring the
/// pixels a user sees.
fn painted_bounds(input: &crate::text_input::TextInput) -> gpui::Bounds<gpui::Pixels> {
    input
        .hit_bounds_slot()
        .get()
        .expect("field was painted at least once")
}

/// Put a row draft in the detail sidebar and open it.
fn open_detail_draft<'a>(
    cx: &'a mut TestAppContext,
    values: &[(&str, &str, bool)],
) -> (Entity<DbUi>, &'a mut VisualTestContext) {
    use crate::tabs::RowDraft;
    use crate::text_input::TextInput;

    let (view, cx) = open(cx);
    let fields = values
        .iter()
        .map(|(name, text, is_pk)| {
            (
                name.to_string(),
                TextInput::with_text(*text, !*is_pk),
                *is_pk,
            )
        })
        .collect();
    view.update(cx, |this, cx| {
        this.open_sql_tab(cx);
        if let Some(WorkspaceTab::Sql {
            draft, selected_row, ..
        }) = this.tabs.active_mut()
        {
            *selected_row = Some(0);
            *draft = Some(RowDraft {
                row_index: 0,
                fields,
                message: None,
                field_search: TextInput::new(false),
            });
        }
        this.detail_open = true;
        cx.notify();
    });
    cx.run_until_parked();
    (view, cx)
}

#[gpui::test]
fn detail_field_does_not_move_vertically_while_typing(cx: &mut TestAppContext) {
    use crate::components::text_field::InputTarget;

    let (view, cx) = open_detail_draft(cx, &[("id", "1", true), ("name", "alpha", false)]);

    view.update(cx, |this, cx| {
        this.focus_input(InputTarget::DetailField(1), cx);
    });
    cx.run_until_parked();

    let mut seen: Vec<(f32, f32)> = Vec::new();
    for c in "abcdefghijklmnopqrstuvwxyz0123456789".chars() {
        cx.simulate_keystrokes(&format!("{c}->{c}"));
        cx.run_until_parked();
        let bounds = view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            painted_bounds(&draft.as_ref().unwrap().fields[1].1)
        });
        seen.push((bounds.origin.y.into(), bounds.size.height.into()));
    }

    let first = seen[0];
    let drift: Vec<_> = seen.iter().filter(|b| **b != first).collect();
    assert!(
        drift.is_empty(),
        "detail field moved vertically while typing: first {first:?}, later {drift:?}",
    );
}

#[gpui::test]
fn detail_search_does_not_move_vertically_while_typing(cx: &mut TestAppContext) {
    use crate::components::text_field::InputTarget;

    let (view, cx) = open_detail_draft(cx, &[("id", "1", true), ("name", "alpha", false)]);

    view.update(cx, |this, cx| {
        this.focus_input(InputTarget::DetailSearch, cx);
    });
    cx.run_until_parked();

    let mut seen: Vec<(f32, f32)> = Vec::new();
    for c in "nnnnnnnnnn".chars() {
        cx.simulate_keystrokes(&format!("{c}->{c}"));
        cx.run_until_parked();
        let bounds = view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            painted_bounds(&draft.as_ref().unwrap().field_search)
        });
        seen.push((bounds.origin.y.into(), bounds.size.height.into()));
    }

    let first = seen[0];
    let drift: Vec<_> = seen.iter().filter(|b| **b != first).collect();
    assert!(
        drift.is_empty(),
        "detail search moved vertically while typing: first {first:?}, later {drift:?}",
    );
}

#[gpui::test]
fn detail_field_has_no_vertical_scroll_range(cx: &mut TestAppContext) {
    use crate::components::text_field::InputTarget;

    let (view, cx) = open_detail_draft(cx, &[("id", "1", true), ("name", "alpha", false)]);

    view.update(cx, |this, cx| {
        this.focus_input(InputTarget::DetailField(1), cx);
    });
    cx.run_until_parked();

    let mut seen: Vec<(f32, f32, f32)> = Vec::new();
    for c in "abcdefghijklmnopqrstuvwxyz0123456789".chars() {
        cx.simulate_keystrokes(&format!("{c}->{c}"));
        cx.run_until_parked();
        let probe = view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            let input = &draft.as_ref().unwrap().fields[1].1;
            let handle = input.scroll_handle();
            (
                f32::from(handle.max_offset().height),
                f32::from(handle.offset().y),
                f32::from(painted_bounds(input).size.height),
            )
        });
        seen.push(probe);
    }
    panic!("max_y / offset_y / hit_h per keystroke: {seen:?}");
}
