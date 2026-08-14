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
use dbui_app::domain::{ConnectionConfig, Driver, TableRef};
use dbui_app::{DbRuntime, Workspace};
use gpui::{AppContext as _, Entity, TestAppContext, VisualContext as _, VisualTestContext};

/// Point the config directory at a scratch path for the whole test process.
///
/// Opening a tab persists the session, so without this a test run would
/// overwrite whatever the developer had open in the real app. Every test in
/// this file shares one directory, which is harmless: they all write and none
/// reads back.
fn redirect_config_dir() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let mut dir = std::env::temp_dir();
        dir.push(format!("dbui-e2e-{}", std::process::id()));
        std::env::set_var(dbui_app::store::CONFIG_DIR_VAR, dir);
    });
}

/// Open a window on an empty workspace.
fn open(cx: &mut TestAppContext) -> (Entity<DbUi>, &mut VisualTestContext) {
    open_with(cx, Workspace::new())
}

fn open_with(
    cx: &mut TestAppContext,
    workspace: Workspace,
) -> (Entity<DbUi>, &mut VisualTestContext) {
    redirect_config_dir();
    let runtime = DbRuntime::new().expect("runtime");
    cx.update(|cx| crate::load_bundled_fonts(cx));
    cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        window.focus(&focus);
        DbUi::new(runtime, workspace, focus)
    })
}

/// A workspace over `count` saved connections named `Conn 1`, `Conn 2`, ….
fn saved_connections(count: usize) -> Workspace {
    Workspace::from_configs((1..=count).map(|n| {
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.name = format!("Conn {n}");
        config
    }))
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
        assert_eq!(
            config.name, "wrapped",
            "tab should wrap past buttons back to Name"
        );
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
fn run_resolves_selection_then_statement_under_caret(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "SELECT 1; SELECT 2");
        // Caret in the second statement.
        if let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active_mut() {
            editor.move_to(12);
        }
        assert_eq!(view.resolve_run_sql().as_deref(), Some("SELECT 2"));

        // Selection wins over caret.
        if let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active_mut() {
            editor.move_to(0);
            editor.select_to(8); // "SELECT 1"
        }
        assert_eq!(view.resolve_run_sql().as_deref(), Some("SELECT 1"));
    });
}

#[gpui::test]
fn run_all_splits_the_buffer(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "SELECT 1; SELECT 2");
        assert_eq!(
            view.resolve_run_all_sql().as_deref(),
            Some(["SELECT 1".to_string(), "SELECT 2".to_string()].as_slice())
        );
    });

    cx.simulate_keystrokes("cmd-shift-enter");
    view.update(cx, |view, _| match &view.status {
        Status::Error(message) => assert_eq!(message.as_ref(), "Not connected"),
        other => panic!("expected not-connected error, got {}", describe(other)),
    });
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
            draft,
            selected_row,
            ..
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

    // Long enough to overflow the field and start panning horizontally.
    let mut seen: Vec<(f32, f32, f32)> = Vec::new();
    for c in "nnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnnn".chars() {
        cx.simulate_keystrokes(&format!("{c}->{c}"));
        cx.run_until_parked();
        let probe = view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            let input = &draft.as_ref().unwrap().field_search;
            let bounds = painted_bounds(input);
            (
                f32::from(bounds.origin.y),
                f32::from(bounds.size.height),
                f32::from(input.scroll_handle().offset().y),
            )
        });
        seen.push(probe);
    }

    let first = seen[0];
    let drift: Vec<_> = seen.iter().filter(|b| **b != first).collect();
    assert!(
        drift.is_empty(),
        "detail search moved vertically while typing: first {first:?}, later {drift:?}",
    );

    // …and the horizontal follow that panning is there for still happened.
    let panned = view.read_with(cx, |this, _| {
        let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
            unreachable!()
        };
        f32::from(
            draft
                .as_ref()
                .unwrap()
                .field_search
                .scroll_handle()
                .offset()
                .x,
        )
    });
    assert!(
        panned < 0.,
        "a long value must pan to keep the caret in view"
    );
}

/// The bounce, measured where it lives.
///
/// A detail field that fits its own text must have no vertical scroll range.
/// When the field's height left out the 1px border top and bottom, the
/// scrollport came out 2px shorter than the single line inside it, and
/// `ensure_caret_visible` flipped the offset 0 → -2 → 0 on every keystroke.
#[gpui::test]
fn detail_field_has_no_vertical_scroll_range(cx: &mut TestAppContext) {
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
        let probe = view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            let handle = draft.as_ref().unwrap().fields[1].1.scroll_handle();
            (
                f32::from(handle.max_offset().height),
                f32::from(handle.offset().y),
            )
        });
        seen.push(probe);
    }

    let moved: Vec<_> = seen
        .iter()
        .filter(|(max, y)| *max != 0. || *y != 0.)
        .collect();
    assert!(
        moved.is_empty(),
        "a one-line detail field scrolled vertically while typing \
         (max_offset.height, offset.y): {moved:?}",
    );
}

/// A field taller than its window still scrolls -- the fix above must not have
/// pinned the vertical offset for everyone.
#[gpui::test]
fn a_long_detail_field_still_scrolls_vertically(cx: &mut TestAppContext) {
    use crate::components::text_field::InputTarget;

    let long = (1..=20)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (view, cx) = open_detail_draft(cx, &[("id", "1", true), ("body", long.as_str(), false)]);

    view.update(cx, |this, cx| {
        this.focus_input(InputTarget::DetailField(1), cx);
    });
    cx.run_until_parked();

    let read = |cx: &mut VisualTestContext| {
        view.read_with(cx, |this, _| {
            let WorkspaceTab::Sql { draft, .. } = this.tabs.active().unwrap() else {
                unreachable!()
            };
            let handle = draft.as_ref().unwrap().fields[1].1.scroll_handle();
            (
                f32::from(handle.max_offset().height),
                f32::from(handle.offset().y),
            )
        })
    };

    // The caret sits at the end of the text, so the first keystroke scrolls the
    // field down to it; the ones after must leave it exactly where it is.
    cx.simulate_keystrokes("x->x");
    cx.run_until_parked();
    let (max_y, first) = read(cx);
    assert!(max_y > 0., "20 lines in an 8-line box must be scrollable");
    assert!(
        first < 0.,
        "the caret's line must be scrolled into view: {first}"
    );

    for _ in 0..8 {
        cx.simulate_keystrokes("x->x");
        cx.run_until_parked();
        let (_, y) = read(cx);
        assert_eq!(y, first, "typing on the last line must not re-scroll");
    }
}

// -- connection tabs ---------------------------------------------------------

/// A saved connection is not an open tab. The bar starts with one tab, and
/// the rest of the address book waits behind the `+`.
#[gpui::test]
fn only_the_first_saved_connection_starts_as_a_tab(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(3));

    view.update(cx, |view, _| {
        assert_eq!(view.workspace.entries().len(), 3);
        assert_eq!(view.workspace.open_count(), 1);
        assert_eq!(view.workspace.active().unwrap().config.name, "Conn 1");
    });
}

/// The point of the whole feature: each connection tab keeps its own tables,
/// and coming back finds them where they were left.
#[gpui::test]
fn each_connection_tab_keeps_its_own_tabs(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));

    let (first, second) = view.update(cx, |view, _| {
        let ids: Vec<_> = view.workspace.entries().iter().map(|e| e.id()).collect();
        (ids[0], ids[1])
    });

    // Two tables on the first connection...
    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.open_table_tab(TableRef::new("public", "orders"), cx);
        assert_eq!(view.tabs.items.len(), 2);
    });

    // ...a different one on the second.
    view.update(cx, |view, cx| {
        view.open_connection_tab(second, cx);
        assert!(
            view.tabs.items.is_empty(),
            "a freshly opened connection starts with no tabs"
        );
        view.open_table_tab(TableRef::new("shop", "products"), cx);
    });

    // Back to the first: its two tables are still there, and the second's
    // single table is not.
    view.update(cx, |view, cx| {
        view.open_connection_tab(first, cx);
        let labels: Vec<_> = view.tabs.items.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, vec!["users", "orders"]);
    });

    view.update(cx, |view, cx| {
        view.open_connection_tab(second, cx);
        let labels: Vec<_> = view.tabs.items.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, vec!["products"]);
    });
}

/// SQL survives the switch too -- the editor buffer is part of the tab, not
/// part of the window.
#[gpui::test]
fn sql_text_survives_a_connection_switch(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));
    let second = view.update(cx, |view, _| view.workspace.entries()[1].id());
    let first = view.update(cx, |view, _| view.workspace.entries()[0].id());

    cx.simulate_keystrokes("cmd-e");
    cx.simulate_keystrokes(&typing("select 1"));

    view.update(cx, |view, cx| view.open_connection_tab(second, cx));
    view.update(cx, |view, cx| view.open_connection_tab(first, cx));

    view.update(cx, |view, _| {
        let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active() else {
            panic!("the SQL tab should be back in front");
        };
        assert_eq!(editor.text(), "select 1");
    });
}

/// Closing a tab is not deleting a connection: it leaves the address book
/// alone and the `+` can open it again.
#[gpui::test]
fn closing_a_connection_tab_keeps_the_connection_saved(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));
    let (first, second) = view.update(cx, |view, _| {
        let ids: Vec<_> = view.workspace.entries().iter().map(|e| e.id()).collect();
        (ids[0], ids[1])
    });

    view.update(cx, |view, cx| view.open_connection_tab(second, cx));
    view.update(cx, |view, cx| view.close_connection_tab(second, cx));

    view.update(cx, |view, _| {
        assert_eq!(view.workspace.open_count(), 1);
        assert_eq!(view.workspace.active_id(), Some(first));
        assert_eq!(
            view.workspace.entries().len(),
            2,
            "closing a tab must not forget the connection"
        );
    });
}

/// A closed tab's tables are gone for good -- reopening the connection is a
/// fresh start, not a resurrection of what was discarded.
#[gpui::test]
fn reopening_a_closed_connection_starts_clean(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));
    let only = view.update(cx, |view, _| view.workspace.entries()[0].id());

    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.close_connection_tab(only, cx);
        assert_eq!(view.workspace.open_count(), 0);
        assert!(view.tabs.items.is_empty());
    });

    view.update(cx, |view, cx| {
        view.open_connection_tab(only, cx);
        assert!(view.tabs.items.is_empty());
    });
}

/// ⌘⌥] and ⌘⌥[ walk the connection bar, wrapping at either end.
#[gpui::test]
fn cmd_alt_bracket_cycles_connection_tabs(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(3));
    let ids = view.update(cx, |view, cx| {
        let ids: Vec<_> = view.workspace.entries().iter().map(|e| e.id()).collect();
        view.open_connection_tab(ids[1], cx);
        view.open_connection_tab(ids[2], cx);
        view.open_connection_tab(ids[0], cx);
        ids
    });

    cx.simulate_keystrokes("cmd-alt-]");
    view.update(cx, |view, _| {
        assert_eq!(view.workspace.active_id(), Some(ids[1]))
    });

    cx.simulate_keystrokes("cmd-alt-[");
    view.update(cx, |view, _| {
        assert_eq!(view.workspace.active_id(), Some(ids[0]))
    });

    cx.simulate_keystrokes("cmd-alt-[");
    view.update(cx, |view, _| {
        assert_eq!(
            view.workspace.active_id(),
            Some(ids[2]),
            "stepping back off the first tab wraps to the last"
        )
    });
}

/// ⌘⇧W closes the connection tab; plain ⌘W closes only a table tab. Getting
/// these the wrong way round would throw away a whole connection's worth of
/// tabs on a keystroke people use constantly.
#[gpui::test]
fn cmd_shift_w_closes_the_connection_not_the_table(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));
    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.open_table_tab(TableRef::new("public", "orders"), cx);
    });

    cx.simulate_keystrokes("cmd-w");
    view.update(cx, |view, _| {
        assert_eq!(view.tabs.items.len(), 1, "⌘W closes one table tab");
        assert_eq!(view.workspace.open_count(), 1);
    });

    cx.simulate_keystrokes("cmd-shift-w");
    view.update(cx, |view, _| {
        assert_eq!(view.workspace.open_count(), 0, "⌘⇧W closes the connection");
    });
}

/// Disconnecting invalidates the rows, not the arrangement. The tabs stay so
/// reconnecting puts the user back where they were.
#[gpui::test]
fn disconnecting_keeps_the_tabs_it_had_open(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));
    let only = view.update(cx, |view, _| view.workspace.entries()[0].id());

    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.disconnect(only, cx);

        assert_eq!(view.tabs.items.len(), 1, "the tab outlives the connection");
        assert!(view.tabs.active().unwrap().result().is_none());
    });
}

// -- the session -------------------------------------------------------------

/// The restart, end to end: what is open is snapshotted, and a fresh window
/// fed that snapshot comes up with the same tabs on the same connections.
#[gpui::test]
fn a_session_snapshot_restores_the_whole_tab_layout(cx: &mut TestAppContext) {
    let configs: Vec<_> = (1..=2)
        .map(|n| {
            let mut config = ConnectionConfig::new(Driver::Postgres);
            config.name = format!("Conn {n}");
            config
        })
        .collect();

    let (view, cx) = open_with(cx, Workspace::from_configs(configs.clone()));
    let (first, second) = (configs[0].id, configs[1].id);

    let session = view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.open_connection_tab(second, cx);
        view.open_table_tab(TableRef::new("shop", "products"), cx);
        view.open_sql_tab(cx);
        view.session_snapshot()
    });

    assert_eq!(session.tabs.len(), 2);
    assert_eq!(session.active_connection(), Some(second));

    // A brand new window, as if the app had been relaunched.
    let (restored, cx) = open_with(cx, Workspace::from_configs(configs));
    restored.update(cx, |view, _| {
        let reopen = view.restore_session(&session);
        assert_eq!(reopen, Some(second), "only the front tab reconnects");

        assert_eq!(view.workspace.open_ids(), &[first, second]);
        let labels: Vec<_> = view.tabs.items.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, vec!["products", "SQL Query"]);

        let stashed = view.stashed_tabs.get(&first).expect("the other tab's set");
        assert_eq!(
            stashed.items.iter().map(|t| t.label()).collect::<Vec<_>>(),
            vec!["users"]
        );
    });
}

/// The claim the feature is sold on, with the disk hop included: open some
/// tabs, quit, come back, find them.
///
/// The file goes to a path of this test's own rather than the shared one every
/// other test here writes as it clicks around — a session read back from that
/// would be whichever test wrote last.
#[gpui::test]
fn tabs_come_back_after_a_relaunch(cx: &mut TestAppContext) {
    let configs: Vec<_> = (1..=2)
        .map(|n| {
            let mut config = ConnectionConfig::new(Driver::Postgres);
            config.name = format!("Relaunch {n}");
            config
        })
        .collect();
    let (first, second) = (configs[0].id, configs[1].id);

    let mut path = std::env::temp_dir();
    path.push(format!("dbui-relaunch-{}", std::process::id()));
    path.push("session.json");

    {
        let (view, cx) = open_with(cx, Workspace::from_configs(configs.clone()));
        view.update(cx, |view, cx| {
            view.open_table_tab(TableRef::new("public", "users"), cx);
            view.open_connection_tab(second, cx);
            view.open_sql_tab(cx);
        });
        // What ⌘Q catches: the editor buffer no structural change saw.
        cx.simulate_keystrokes(&typing("select 42"));
        view.update(cx, |view, _| {
            dbui_app::session::save(&path, &view.session_snapshot()).expect("write the session");
        });
    }

    let mut session = dbui_app::session::load(&path);
    session.prune(&[first, second]);

    let (restored, cx) = open_with(cx, Workspace::from_configs(configs));
    restored.update(cx, |view, _| {
        assert_eq!(view.restore_session(&session), Some(second));
        assert_eq!(view.workspace.open_ids(), &[first, second]);

        let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active() else {
            panic!("the SQL tab was in front when we quit");
        };
        assert_eq!(editor.text(), "select 42", "typed SQL must survive a quit");

        let stashed = view.stashed_tabs.get(&first).expect("the other connection");
        assert_eq!(
            stashed.items.iter().map(|t| t.label()).collect::<Vec<_>>(),
            vec!["users"]
        );
    });

    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

/// `persist_session` has to write where the launch path reads. The content is
/// whatever test wrote last, so only the format is asserted here.
#[gpui::test]
fn persisting_writes_a_readable_file_at_the_session_path(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));
    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.persist_session();
    });

    let path = dbui_app::session::session_path().expect("a session path");
    assert!(path.exists(), "the launch path must be where saves land");
    assert!(
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .is_some(),
        "and what lands there must parse"
    );
}

/// Which folders were unfolded is part of where the user was, so it goes out
/// with the session and comes back with it -- held on the entry until the
/// catalog arrives and the tree can actually draw them.
#[gpui::test]
fn expanded_schemas_survive_a_relaunch(cx: &mut TestAppContext) {
    let configs: Vec<_> = (1..=2)
        .map(|n| {
            let mut config = ConnectionConfig::new(Driver::Postgres);
            config.name = format!("Tree {n}");
            config
        })
        .collect();
    let (first, second) = (configs[0].id, configs[1].id);

    let (view, cx) = open_with(cx, Workspace::from_configs(configs.clone()));
    let session = view.update(cx, |view, cx| {
        view.open_connection_tab(second, cx);
        // What clicking a folder open does.
        view.toggle_schema(first, "public", cx);
        view.toggle_schema(first, "drizzle", cx);
        view.toggle_schema(second, "shop", cx);
        view.session_snapshot()
    });

    assert_eq!(session.tabs[0].expanded, vec!["public", "drizzle"]);
    assert_eq!(session.tabs[1].expanded, vec!["shop"]);

    let (restored, cx) = open_with(cx, Workspace::from_configs(configs));
    restored.update(cx, |view, _| {
        view.restore_session(&session);
        assert_eq!(
            view.workspace.get(first).unwrap().expanded,
            vec!["public", "drizzle"]
        );
        assert_eq!(view.workspace.get(second).unwrap().expanded, vec!["shop"]);
    });
}

/// Disconnecting drops the catalog but not the shape of the tree, so
/// reconnecting does not collapse everything the user had opened.
#[gpui::test]
fn disconnecting_does_not_collapse_the_tree(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));
    let only = view.update(cx, |view, _| view.workspace.entries()[0].id());

    view.update(cx, |view, cx| {
        view.toggle_schema(only, "public", cx);
        view.disconnect(only, cx);
        assert_eq!(view.workspace.get(only).unwrap().expanded, vec!["public"]);
    });
}

/// Restoring must not dial anything but the front tab, and must not connect
/// on its own when there is nothing to restore.
#[gpui::test]
fn restoring_an_empty_session_changes_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));

    view.update(cx, |view, _| {
        let before = view.workspace.active_id();
        let reopen = view.restore_session(&dbui_app::Session::default());
        assert_eq!(reopen, before);
        assert_eq!(view.workspace.open_count(), 1);
    });
}
