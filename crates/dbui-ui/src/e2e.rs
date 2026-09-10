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

/// Serialises the tests that cannot run alongside a zoom change.
///
/// `metrics` keeps the zoom in a process-wide static, because it scales every
/// surface at once. Anything measuring painted pixels therefore has to be kept
/// away from the test that moves it, or it measures a layout from the middle
/// of someone else's zoom.
fn layout_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    cx.update(|cx| {
        crate::load_bundled_fonts(cx);
        // The same bindings `run()` installs. Without them a test only ever
        // reaches `on_key`, while the real window dispatches the action first
        // -- so a shortcut handled in both places would be tested in the one
        // place it does not run.
        cx.bind_keys(crate::key_bindings());
    });
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
                rows: vec![0],
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
    let _layout = layout_lock();
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
    let _layout = layout_lock();
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

/// A statement wider than the pane has to be reachable.
///
/// The SQL editor only ever scrolled vertically, so everything past the right
/// edge of the pane was simply gone: no scrollbar, no wheel, and typing kept
/// writing where the user could not see it.
#[gpui::test]
fn a_long_statement_scrolls_the_sql_editor_sideways(cx: &mut TestAppContext) {
    let _layout = layout_lock();
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
    });
    cx.run_until_parked();

    // A short statement fits, and must not pan.
    view.update(cx, |view, _| set_sql_editor_text(view, "select 1"));
    cx.simulate_keystrokes(&typing("0"));
    cx.run_until_parked();
    let short = editor_scroll(&view, cx);
    assert_eq!(
        short,
        (0., 0.),
        "a statement that fits gave the editor a scroll range (offset.x, max.width): {short:?}",
    );

    view.update(cx, |view, _| {
        set_sql_editor_text(
            view,
            &format!(
                "select * from orders where status = 5 and note = '{}'",
                "x".repeat(300)
            ),
        )
    });
    cx.run_until_parked();
    // One keystroke at the caret, which `set_text` left at the end of the line.
    cx.simulate_keystrokes(&typing("x"));
    cx.run_until_parked();

    let (offset_x, max_x) = editor_scroll(&view, cx);
    assert!(
        max_x > 0.,
        "a long statement must leave the editor something to scroll: max {max_x}",
    );
    assert!(
        offset_x < 0.,
        "typing past the right edge must pan the editor: offset {offset_x}",
    );
}

/// The SQL editor's horizontal offset and scroll range, in pixels.
fn editor_scroll(view: &Entity<DbUi>, cx: &mut VisualTestContext) -> (f32, f32) {
    view.read_with(cx, |view, _| {
        let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active() else {
            unreachable!("the SQL tab is the active one")
        };
        let handle = editor.scroll_handle();
        (
            f32::from(handle.offset().x),
            f32::from(handle.max_offset().width),
        )
    })
}

/// The bounce, measured where it lives.
///
/// A detail field that fits its own text must have no vertical scroll range.
/// When the field's height left out the 1px border top and bottom, the
/// scrollport came out 2px shorter than the single line inside it, and
/// `ensure_caret_visible` flipped the offset 0 → -2 → 0 on every keystroke.
#[gpui::test]
fn detail_field_has_no_vertical_scroll_range(cx: &mut TestAppContext) {
    let _layout = layout_lock();
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

/// A *folded* field taller than its window still scrolls -- the fix above must
/// not have pinned the vertical offset for everyone.
///
/// Folding is what puts a field back into a box smaller than its text: left
/// alone it draws every line it has, and the panel does the scrolling.
#[gpui::test]
fn a_long_folded_detail_field_still_scrolls_vertically(cx: &mut TestAppContext) {
    let _layout = layout_lock();
    use crate::components::text_field::InputTarget;

    let long = (1..=20)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (view, cx) = open_detail_draft(cx, &[("id", "1", true), ("body", long.as_str(), false)]);

    view.update(cx, |this, cx| {
        this.toggle_detail_collapsed("body", cx);
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

// -- selecting rows, and staging them for deletion ------------------------

/// A table tab holding `count` rows of `(id, name)`, with `id` as the key.
///
/// No server is involved: the point of these tests is what the keyboard and
/// the pointer do to rows that are already on screen.
fn open_table_with_rows<'a>(
    cx: &'a mut TestAppContext,
    count: usize,
) -> (Entity<DbUi>, &'a mut VisualTestContext) {
    use crate::root::{ResultSource, ResultView};
    use dbui_app::domain::{Column, ColumnInfo, Page, ResultSet, Row, Value};

    // A saved connection, so the engine is known for quoting even though
    // nothing here dials out.
    let (view, cx) = open_with(cx, saved_connections(1));
    let table = TableRef::new("public", "users");
    let columns = vec![
        ColumnInfo {
            name: "id".into(),
            type_name: "int8".into(),
        },
        ColumnInfo {
            name: "name".into(),
            type_name: "text".into(),
        },
    ];
    let structure = vec![
        Column {
            name: "id".into(),
            data_type: "bigint".into(),
            nullable: false,
            default: None,
            is_primary_key: true,
            ordinal: 1,
            references: None,
        },
        Column {
            name: "name".into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
            is_primary_key: false,
            ordinal: 2,
            // `name` doubles as the foreign key in these tests: the fixture
            // only has two columns, and what matters is that a column with a
            // reference behaves differently from one without.
            references: Some(dbui_app::domain::ForeignKey {
                column: "name".into(),
                references: TableRef::new("public", "teams"),
                references_column: "slug".into(),
            }),
        },
    ];
    let rows = (0..count)
        .map(|n| {
            Row(vec![
                Value::Int(n as i64 + 1),
                Value::Text(format!("row {}", n + 1)),
            ])
        })
        .collect();

    view.update(cx, |this, cx| {
        this.tabs.open_table(table.clone());
        if let Some(WorkspaceTab::Table { result, .. }) = this.tabs.active_mut() {
            *result = Some(ResultView::new(
                ResultSet {
                    columns,
                    rows,
                    truncated: false,
                },
                ResultSource::Table {
                    table,
                    page: Page::first(),
                    total_rows: Some(count as i64),
                    where_clause: String::new(),
                },
                format!("{count} rows"),
                structure,
            ));
        }
        this.focus = Focus::Grid;
        cx.notify();
    });
    cx.run_until_parked();
    (view, cx)
}

fn selected_rows(view: &DbUi) -> Vec<usize> {
    view.tabs.active().unwrap().selection().ordered()
}

#[gpui::test]
fn cmd_a_selects_every_row_in_the_grid(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, _| {
        assert_eq!(selected_rows(view), vec![0, 1, 2, 3, 4]);
    });
}

/// ⌘A belongs to whichever surface has the keyboard. In the SQL editor it is
/// still "select all text", which is the reason it is not a global action.
#[gpui::test]
fn cmd_a_in_the_editor_selects_text_not_rows(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select 1");
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, _| {
        let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active() else {
            panic!("sql tab");
        };
        assert_eq!(editor.selected_text(), Some("select 1"));
    });
}

/// Clicking one row, then shift-clicking another, takes everything between --
/// and dragging back the other way shrinks the range rather than dragging the
/// far end along with the pointer.
#[gpui::test]
fn shift_click_and_drag_take_a_range(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 6);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        assert_eq!(selected_rows(view), vec![1], "a plain click takes one row");

        view.grid_pointer_down(4, None, gpui::Modifiers::shift(), cx);
        assert_eq!(selected_rows(view), vec![1, 2, 3, 4]);

        // Still holding: dragging back up measures from the anchor, not from
        // the far end of the range it just built.
        view.grid_drag_over(2, cx);
        assert_eq!(selected_rows(view), vec![1, 2]);
        view.grid_drag_over(0, cx);
        assert_eq!(selected_rows(view), vec![0, 1]);
        view.end_row_drag(cx);
    });
}

#[gpui::test]
fn cmd_click_toggles_one_row_at_a_time(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(2, None, gpui::Modifiers::command(), cx);
        assert_eq!(selected_rows(view), vec![0, 2]);

        view.grid_pointer_down(2, None, gpui::Modifiers::command(), cx);
        assert_eq!(selected_rows(view), vec![0], "clicking again removes it");
    });
}

/// Releasing the button outside the window has to end the drag, or the next
/// pointer move over the grid extends a selection nobody is still holding.
#[gpui::test]
fn a_finished_drag_stops_extending_the_range(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_drag_over(2, cx);
        view.end_row_drag(cx);
        view.grid_drag_over(4, cx);
        assert_eq!(selected_rows(view), vec![0, 1, 2]);
    });
}

#[gpui::test]
fn cmd_delete_stages_the_selection_without_writing_anything(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");

    view.update(cx, |view, _| {
        let staged = view.collect_batch_deletes();
        assert_eq!(staged.len(), 4, "every selected row is staged");
        assert_eq!(staged[0].label, "id=1");
        assert!(
            view.tabs.active().unwrap().row_is_staged_for_delete(0),
            "and the grid can tell, so it can strike the row through"
        );
    });
}

/// Staging the same row twice is one deletion, not two.
#[gpui::test]
fn staging_a_row_twice_stages_it_once(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        view.delete_selected_rows(cx);
        view.delete_selected_rows(cx);
        assert_eq!(view.collect_batch_deletes().len(), 1);
    });
}

/// Nothing is selected: ⌘⌫ has to say so rather than quietly do nothing.
#[gpui::test]
fn deleting_with_no_selection_explains_itself(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.clear_row_selection(cx);
        view.delete_selected_rows(cx);
        assert!(
            describe(&view.status).contains("Select a row"),
            "got: {}",
            describe(&view.status)
        );
        assert!(view.collect_batch_deletes().is_empty());
    });
}

/// ⌘Z on the grid throws the staged batch away. There is no step-by-step undo
/// of a batch, and the batch is what the user is looking at.
#[gpui::test]
fn cmd_z_discards_the_staged_batch(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");
    view.update(cx, |view, _| {
        assert_eq!(view.collect_batch_deletes().len(), 4);
    });

    cx.simulate_keystrokes("cmd-z");
    view.update(cx, |view, _| {
        assert!(view.collect_batch_deletes().is_empty());
        assert!(view.collect_batch_edits().is_empty());
        assert_eq!(describe(&view.status), "info: Discarded 4 changes");
    });
}

/// It takes bulk edits with it, not just deletions.
#[gpui::test]
fn cmd_z_discards_a_bulk_edit(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        type_into_draft(view, 1, "renamed");
        view.clear_row_selection(cx);
        assert_eq!(view.collect_batch_edits().len(), 3);
    });

    cx.simulate_keystrokes("cmd-z");
    view.update(cx, |view, _| {
        assert!(view.collect_batch_edits().is_empty());
    });
}

/// ⌘Z belongs to whichever surface has the keyboard. In the SQL editor it is
/// still undo, which is why it is not bound as a global action.
#[gpui::test]
fn cmd_z_in_the_editor_is_still_text_undo(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        view.focus = Focus::Editor;
    });
    cx.run_until_parked();

    cx.simulate_keystrokes(&typing("select"));
    view.update(cx, |view, _| assert_eq!(sql_editor_text(view), "select"));

    cx.simulate_keystrokes("cmd-z");
    view.update(cx, |view, _| {
        assert_ne!(
            sql_editor_text(view),
            "select",
            "the editor's own undo has to still run"
        );
    });
}

/// Nothing staged: ⌘Z must not claim to have undone something.
#[gpui::test]
fn cmd_z_with_nothing_staged_reports_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-z");
    view.update(cx, |view, _| {
        assert_eq!(describe(&view.status), "idle");
    });
}

/// The batch belongs to the tab, not to the grid: Escape hands the keyboard
/// back to the tree, and ⌘Z there still has to discard the changes the bubble
/// is showing.
#[gpui::test]
fn cmd_z_discards_from_the_sidebar_too(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");
    view.update(cx, |view, cx| {
        view.focus = Focus::Sidebar;
        cx.notify();
    });

    cx.simulate_keystrokes("cmd-z");
    view.update(cx, |view, _| {
        assert!(view.collect_batch_deletes().is_empty());
    });
}

/// Every surface that owns an undo stack has to keep ⌘Z for it.
#[gpui::test]
fn text_surfaces_keep_cmd_z_for_their_own_undo(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, _| {
        for focus in [
            Focus::Editor,
            Focus::Filter,
            Focus::PageSize,
            Focus::SidebarSearch,
        ] {
            view.focus = focus;
            assert!(view.text_undo_has_focus(), "{focus:?} types text");
        }

        // The detail sidebar only owns it while a field is actually focused.
        view.focus = Focus::Detail;
        view.detail_input = Some(crate::components::DetailInput::Field(1));
        assert!(view.text_undo_has_focus());
        view.detail_input = None;
        assert!(
            !view.text_undo_has_focus(),
            "row chrome is not a text field"
        );

        view.focus = Focus::Grid;
        assert!(!view.text_undo_has_focus());
    });
}

/// Discard is the way out of a staged batch, and it has to take the deletions
/// with it -- otherwise ⌘S after a Discard still drops rows.
#[gpui::test]
fn discard_clears_staged_deletions_too(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");
    view.update(cx, |view, cx| {
        assert_eq!(view.collect_batch_deletes().len(), 3);
        view.discard_pending_edits(cx);
        assert!(view.collect_batch_deletes().is_empty());
    });
}

/// ⌘S with nothing staged must not reach for a connection: "not connected" on
/// a tab with no changes is a message about the wrong thing.
#[gpui::test]
fn commit_with_nothing_staged_says_so(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-s");
    view.update(cx, |view, _| {
        assert_eq!(
            describe(&view.status),
            "info: No changes to commit on this tab",
            "and with no other tab holding any, it does not send the user hunting"
        );
    });
}

/// With a batch staged and no connection, ⌘S reports the connection.
#[gpui::test]
fn committing_without_a_connection_says_so(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");
    cx.simulate_keystrokes("cmd-s");

    view.update(cx, |view, _| {
        assert_eq!(describe(&view.status), "error: Not connected");
        assert_eq!(
            view.collect_batch_deletes().len(),
            3,
            "and nothing is thrown away, so the user can connect and retry"
        );
    });
}

/// Escape is how every other mode in this window is left, and a range of rows
/// is a mode.
#[gpui::test]
fn escape_drops_a_multi_row_selection(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| {
        assert!(selected_rows(view).is_empty());
        assert_eq!(view.focus, Focus::Grid, "and the grid keeps the keyboard");
    });

    // A second Escape then does what it always did.
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert_eq!(view.focus, Focus::Sidebar));
}

/// Arrowing away from a range collapses it: leaving fifty rows lit behind the
/// caret is how a selection gets committed by accident.
#[gpui::test]
fn arrowing_collapses_the_range_and_shift_grows_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("down");
    view.update(cx, |view, _| assert_eq!(selected_rows(view).len(), 1));

    cx.simulate_keystrokes("shift-down shift-down");
    view.update(cx, |view, _| assert_eq!(selected_rows(view).len(), 3));
}

// -- editing a selection --------------------------------------------------

/// What the detail sidebar's editors currently read.
fn draft_texts(view: &DbUi) -> Vec<String> {
    let draft = match view.tabs.active() {
        Some(WorkspaceTab::Table { draft, .. }) | Some(WorkspaceTab::Sql { draft, .. }) => {
            draft.as_ref().expect("a draft is open")
        }
        None => panic!("no tab"),
    };
    draft
        .fields
        .iter()
        .map(|(_, input, _)| input.text().to_string())
        .collect()
}

fn type_into_field_search(view: &mut DbUi, text: &str) {
    let draft = match view.tabs.active_mut() {
        Some(WorkspaceTab::Table { draft, .. }) | Some(WorkspaceTab::Sql { draft, .. }) => {
            draft.as_mut().expect("a draft is open")
        }
        None => panic!("no tab"),
    };
    draft.field_search = crate::text_input::TextInput::with_text(text, false);
}

fn type_into_draft(view: &mut DbUi, field: usize, text: &str) {
    let draft = match view.tabs.active_mut() {
        Some(WorkspaceTab::Table { draft, .. }) | Some(WorkspaceTab::Sql { draft, .. }) => {
            draft.as_mut().expect("a draft is open")
        }
        None => panic!("no tab"),
    };
    draft.fields[field].1 = crate::text_input::TextInput::with_text(text, true);
}

/// Selecting a range has to re-point the sidebar at the whole selection, not
/// leave it describing the row that happened to be clicked first.
#[gpui::test]
fn selecting_a_range_points_the_sidebar_at_all_of_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        assert_eq!(draft_texts(view), vec!["1", "row 1"]);

        view.grid_pointer_down(2, None, gpui::Modifiers::shift(), cx);
        assert_eq!(
            draft_texts(view),
            vec![crate::tabs::MIXED, crate::tabs::MIXED],
            "three rows agreeing on nothing"
        );
        assert!(view.detail_open, "and the sidebar is open to show it");
    });
}

/// The headline behaviour: type into one field, every selected row gets it.
#[gpui::test]
fn editing_a_field_stages_it_for_every_selected_row(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        type_into_draft(view, 1, "renamed");
        // Leaving the draft is what folds it into the batch, the same as
        // clicking another row would.
        view.clear_row_selection(cx);

        let batch = view.collect_batch_edits();
        assert_eq!(batch.len(), 4, "one edit per selected row");
        for edit in &batch {
            assert_eq!(edit.changes.len(), 1);
            assert_eq!(edit.changes[0].column, "name");
            assert_eq!(edit.changes[0].new_text, "renamed");
        }
    });
}

/// A field left reading MIXED is the rows disagreeing, not an instruction --
/// committing must not write the word to any of them.
#[gpui::test]
fn a_selection_left_untouched_stages_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        assert_eq!(
            draft_texts(view),
            vec![crate::tabs::MIXED, crate::tabs::MIXED]
        );
        view.clear_row_selection(cx);
        assert!(view.collect_batch_edits().is_empty());
    });
}

/// Reopening a selection has to show the edit that was staged for it rather
/// than the values the server sent.
#[gpui::test]
fn a_staged_bulk_edit_comes_back_when_the_rows_are_reselected(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        type_into_draft(view, 1, "renamed");
        view.clear_row_selection(cx);
        assert_eq!(view.collect_batch_edits().len(), 3);
    });

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, _| {
        assert_eq!(
            draft_texts(view)[1],
            "renamed",
            "the staged value, not the stored one"
        );
    });
}

/// Rows on their way out have nothing left to update, so an edit staged over
/// a deletion must not be written alongside it.
#[gpui::test]
fn editing_rows_staged_for_deletion_writes_no_update(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-backspace");
    view.update(cx, |view, cx| {
        assert_eq!(view.collect_batch_deletes().len(), 3);
        type_into_draft(view, 1, "renamed");
        view.clear_row_selection(cx);

        assert!(
            view.collect_batch_edits().is_empty(),
            "a row being deleted is not also updated"
        );
        assert_eq!(view.collect_batch_deletes().len(), 3);
    });
}

// -- sorting, inserting, copying ------------------------------------------

/// Clicking a header cycles the sort and resets to the first page, because an
/// offset into the old order means nothing in the new one.
#[gpui::test]
fn sorting_cycles_and_returns_to_the_first_page(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table { page, .. }) = view.tabs.active_mut() {
            page.offset = 500;
        }

        view.toggle_sort("name", cx);
        let sort = view.active_sort().expect("sorted");
        assert_eq!(sort.column, "name");
        assert!(sort.ascending);
        let Some(WorkspaceTab::Table { page, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert_eq!(page.offset, 0, "a new order restarts the paging");

        view.toggle_sort("name", cx);
        assert!(!view.active_sort().expect("still sorted").ascending);

        view.toggle_sort("name", cx);
        assert!(view.active_sort().is_none(), "a third click clears it");
    });
}

/// The columns as the grid would draw them, in order.
fn drawn_columns(view: &DbUi) -> Vec<String> {
    view.tabs
        .active()
        .map(|tab| {
            tab.display_columns()
                .iter()
                .map(|(_, info)| info.name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// A header does two jobs: released where it was pressed it sorts, dragged
/// sideways it carries the column somewhere else. Which one it was is decided
/// by whether the pointer travelled, so both have to be checked against the
/// same press.
#[gpui::test]
fn dragging_a_header_moves_the_column(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        assert_eq!(drawn_columns(view), ["id", "name"]);

        view.begin_column_move(0, gpui::px(100.));
        view.drag_column_over(1, gpui::px(102.), cx);
        assert_eq!(
            drawn_columns(view),
            ["id", "name"],
            "two pixels of travel is still a click"
        );

        view.drag_column_over(1, gpui::px(160.), cx);
        assert_eq!(
            drawn_columns(view),
            ["name", "id"],
            "and past the slop the column follows the pointer"
        );

        // The cells have to follow the heading: rows are stored in the
        // result's order, so the column carries its index with it.
        let first = view.tabs.active().expect("a tab").display_columns()[0].0;
        assert_eq!(
            first, 1,
            "the first column drawn is still the result's `name`"
        );

        view.end_column_move(cx);
        assert!(view.active_sort().is_none(), "a drag is not a sort");
    });
}

/// The same thing again, but through the pointer -- the press, the crossing
/// and the release all dispatched into a real window.
///
/// The state machine above is only half of it: the other half is that the
/// header hands it the right column, that the press does not also reach the
/// sort, and that letting go anywhere ends the drag.
#[gpui::test]
fn a_header_dragged_with_the_pointer_moves_its_column(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 40);
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    cx.run_until_parked();

    // The header sits directly above the rows, along the top of the pane.
    let (pane, list, widths) = view.read_with(cx, |view, _| {
        (
            view.grid_h_scroll.bounds(),
            view.grid_scroll.0.borrow().base_handle.bounds(),
            view.tabs
                .active()
                .and_then(|tab| tab.result())
                .map(|result| result.widths.clone())
                .unwrap_or_default(),
        )
    });
    assert!(widths.len() >= 2, "the fixture has two columns");
    let y = list.top() - crate::theme::metrics::header_height() / 2.;
    let centre = |column: usize| {
        let before: f32 = widths[..column].iter().sum();
        pane.left()
            + crate::theme::metrics::row_number_width()
            + gpui::px(before + widths[column] / 2.)
    };

    cx.simulate_mouse_move(gpui::point(centre(0), y), None, gpui::Modifiers::default());
    cx.simulate_mouse_down(
        gpui::point(centre(0), y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        gpui::point(centre(1), y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        gpui::point(centre(1), y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    view.read_with(cx, |view, _| {
        assert_eq!(drawn_columns(view), ["name", "id"], "the pointer moved it");
        assert!(
            view.active_sort().is_none(),
            "and the press that moved it did not also sort"
        );
        assert!(view.column_move.is_none(), "the drag ended on release");
    });
}

/// A press that never travels is still a click on the heading.
#[gpui::test]
fn a_header_press_that_stays_put_sorts(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        view.begin_column_move(1, gpui::px(100.));
        view.end_column_move(cx);

        assert_eq!(view.active_sort().expect("sorted").column, "name");
        assert_eq!(drawn_columns(view), ["id", "name"], "and nothing moved");
    });
}

/// Where the columns were put is part of where the user was, so it comes back
/// with the session -- by name, like the widths.
#[gpui::test]
fn a_moved_column_survives_a_relaunch(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);
    let saved = view.update(cx, |view, cx| {
        view.begin_column_move(1, gpui::px(200.));
        view.drag_column_over(0, gpui::px(60.), cx);
        assert_eq!(drawn_columns(view), ["name", "id"]);
        view.end_column_move(cx);
        view.tabs.to_saved().0
    });

    let order = match &saved[0] {
        dbui_app::SavedTab::Table { column_order, .. } => column_order.clone(),
        _ => panic!("a table tab"),
    };
    assert_eq!(order, ["name", "id"]);

    // And a fresh window restores it onto the result it loads next.
    let restored = crate::tabs::Tabs::from_saved(&saved, 0);
    let mut tab = restored.items.into_iter().next().expect("the tab");
    let crate::tabs::WorkspaceTab::Table { column_order, .. } = &mut tab else {
        panic!("a table tab");
    };
    assert_eq!(column_order.as_slice(), ["name", "id"]);
}

/// A sort is part of where the user was, so it comes back with the session.
#[gpui::test]
fn a_sort_survives_a_relaunch(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);
    let saved = view.update(cx, |view, cx| {
        view.toggle_sort("name", cx);
        view.tabs.to_saved().0
    });

    let sort = match &saved[0] {
        dbui_app::SavedTab::Table { sort, .. } => sort.clone(),
        _ => panic!("a table tab"),
    };
    assert_eq!(sort.as_ref().expect("saved").column, "name");

    // And a tab rebuilt from that comes back sorted the same way.
    let restored = crate::tabs::Tabs::from_saved(&saved, 0);
    let Some(WorkspaceTab::Table { sort, .. }) = restored.active() else {
        panic!("a table tab");
    };
    assert_eq!(sort.as_ref().expect("restored").column, "name");
}

#[gpui::test]
fn add_row_stages_a_new_row_and_opens_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        let tab = view.tabs.active().expect("tab");
        assert_eq!(tab.pending_inserts().len(), 1);
        assert_eq!(tab.editing_insert(), Some(0));
        assert!(view.detail_open, "and it opens for filling in");

        // Every column starts as DEFAULT, so an untouched row writes nothing
        // the server has an opinion about.
        let row = &tab.pending_inserts()[0];
        assert_eq!(row.fields.len(), 2);
        assert!(row.to_values().expect("parses").is_empty());
    });
}

/// A column left reading DEFAULT is left out of the statement entirely: that
/// is what lets a sequence or a column default still fire.
#[gpui::test]
fn only_filled_in_columns_reach_the_insert(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        if let Some(WorkspaceTab::Table {
            pending_inserts, ..
        }) = view.tabs.active_mut()
        {
            pending_inserts[0].fields[1].1 =
                crate::text_input::TextInput::with_text("Katherine", true);
        }

        let staged = view.collect_batch_inserts().expect("parses");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].values.len(), 1, "only the column that was typed");
        assert_eq!(staged[0].values[0].0, "name");
    });
}

/// A new row has no stored value to take a type from, so the column's declared
/// type is used instead. Sending `6` as text is what made Postgres refuse the
/// INSERT with "column is of type bigint but expression is of type text".
#[gpui::test]
fn a_new_rows_values_take_their_type_from_the_column(cx: &mut TestAppContext) {
    use dbui_app::domain::Value;

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        if let Some(WorkspaceTab::Table {
            pending_inserts, ..
        }) = view.tabs.active_mut()
        {
            pending_inserts[0].fields[0].1 = crate::text_input::TextInput::with_text("6", true);
        }

        let staged = view.collect_batch_inserts().expect("parses");
        assert_eq!(
            staged[0].values[0],
            ("id".to_string(), Value::Int(6)),
            "a bigint column takes an integer, not the string \"6\""
        );
    });
}

/// ⌘⌫ on a new row takes it back off the list -- there is nothing on the
/// server to delete.
#[gpui::test]
fn deleting_a_staged_new_row_unstages_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        assert_eq!(view.staged_insert_count(), 1);
        view.delete_selected_rows(cx);
        assert_eq!(view.staged_insert_count(), 0);
        assert!(
            view.collect_batch_deletes().is_empty(),
            "and no DELETE is staged for a row that never existed"
        );
    });
}

#[gpui::test]
fn discard_clears_staged_new_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        view.add_row(cx);
        assert_eq!(view.staged_insert_count(), 2);
        view.discard_pending_edits(cx);
        assert_eq!(view.staged_insert_count(), 0);
    });
}

#[gpui::test]
fn cmd_c_copies_the_selected_rows_as_tsv(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(1, None, gpui::Modifiers::shift(), cx);
    });
    cx.simulate_keystrokes("cmd-c");

    cx.update(|_, cx| {
        let text = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .expect("clipboard");
        assert_eq!(
            text,
            "id	name
1	row 1
2	row 2
"
        );
    });
}

/// Copy with nothing selected takes the page rather than nothing: a shortcut
/// that silently does nothing reads as broken.
#[gpui::test]
fn copying_with_no_selection_takes_the_whole_page(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.clear_row_selection(cx);
        view.copy_selected_rows(crate::row_export::RowFormat::Tsv, cx);
        assert_eq!(describe(&view.status), "info: Copied 3 rows");
    });
}

/// Clicking a cell is asking about that cell, so ⌘C after it hands back that
/// cell rather than the whole row.
#[gpui::test]
fn cmd_c_copies_the_focused_cell(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, Some(1), gpui::Modifiers::default(), cx);
        assert_eq!(view.selected_cell, Some((1, 1)));
    });
    cx.simulate_keystrokes("cmd-c");

    view.update(cx, |view, _| {
        assert_eq!(describe(&view.status), "info: Copied name");
    });
    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("row 2".to_string()),
            "the value alone, with no header and no other column"
        );
    });
}

/// ⌘⇧C is the way back to the rows while a cell is focused.
#[gpui::test]
fn cmd_shift_c_copies_rows_even_with_a_cell_focused(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, Some(1), gpui::Modifiers::default(), cx);
    });
    cx.simulate_keystrokes("cmd-shift-c");

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("id\tname\n2\trow 2\n".to_string())
        );
    });
}

/// A range is a different question from a cell, so it still copies as rows
/// even though the cell cursor is sitting inside it.
#[gpui::test]
fn a_selected_range_still_copies_as_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, Some(1), gpui::Modifiers::default(), cx);
        view.grid_pointer_down(1, None, gpui::Modifiers::shift(), cx);
        assert_eq!(
            view.selected_cell,
            Some((0, 1)),
            "the cell cursor stays put"
        );
    });
    cx.simulate_keystrokes("cmd-c");

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("id\tname\n1\trow 1\n2\trow 2\n".to_string())
        );
    });
}

/// What is copied is what the grid shows: a staged edit wins over the value
/// the server last sent.
#[gpui::test]
fn copying_a_cell_takes_the_staged_edit(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("renamed");
        view.commit_cell_edit(cx);
        assert!(view.copy_focused_cell(cx));
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("renamed".to_string())
        );
    });
}

/// Right-clicking a cell carries the column through, so the menu entry about
/// *this cell* has one to act on.
#[gpui::test]
fn copying_a_cell_from_the_menu_uses_the_cell_under_the_pointer(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.focus_cell(2, 0, cx);
        assert!(view.copy_focused_cell(cx));
        assert_eq!(describe(&view.status), "info: Copied id");
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("3".to_string())
        );
    });
}

/// No cell, nothing copied -- and the caller falls through to the rows.
#[gpui::test]
fn copying_a_cell_with_none_focused_does_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        assert_eq!(view.selected_cell, None);
        assert!(!view.copy_focused_cell(cx));
    });
}

// -- per-statement results, history ---------------------------------------

/// Put a finished batch onto a SQL tab, the way a run does.
fn put_batch(view: &mut DbUi, statements: &[(&str, Option<usize>)], cx: &mut gpui::Context<DbUi>) {
    use dbui_app::domain::{
        ColumnInfo, QueryOutcome, QueryResult, QueryStats, ResultSet, Row, Value,
    };

    let results: Vec<QueryResult> = statements
        .iter()
        .map(|(sql, rows)| QueryResult {
            statement: (*sql).to_string(),
            outcome: match rows {
                Some(count) => QueryOutcome::Rows(ResultSet {
                    columns: vec![ColumnInfo {
                        name: "n".into(),
                        type_name: "int8".into(),
                    }],
                    rows: (0..*count)
                        .map(|n| Row(vec![Value::Int(n as i64)]))
                        .collect(),
                    truncated: false,
                }),
                None => QueryOutcome::Affected(3),
            },
            stats: QueryStats {
                elapsed: std::time::Duration::from_millis(1),
            },
        })
        .collect();

    let tab_id = view.tabs.active_id().expect("a tab");
    let sent: Vec<String> = results.iter().map(|r| r.statement.clone()).collect();
    let batch = dbui_app::BatchQueryResult {
        last_rows: results.iter().find(|r| r.rows().is_some()).cloned(),
        attempted: results.len(),
        results,
        total_elapsed: std::time::Duration::from_millis(3),
        failure: None,
    };
    view.absorb_batch_result_for_test(tab_id, batch, &sent, true);
    cx.notify();
}

/// The rows of a query, in the order they are on screen.
fn grid_column(view: &DbUi, column: usize) -> Vec<String> {
    view.tabs
        .active()
        .and_then(|tab| tab.result())
        .map(|result| {
            result
                .set
                .rows
                .iter()
                .map(|row| row.get(column).map(|v| v.to_text()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

/// A query's order is its own `ORDER BY`, so the grid reorders the page it
/// already has rather than re-running the statement with one bolted on.
#[gpui::test]
fn a_query_result_sorts_without_touching_the_sql(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        if let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active_mut() {
            *editor = crate::text_input::TextInput::with_text("SELECT n FROM series", true);
        }
        put_batch(view, &[("SELECT n FROM series", Some(3))], cx);
        assert_eq!(grid_column(view, 0), vec!["0", "1", "2"]);

        view.toggle_sort("n", cx);
        assert_eq!(grid_column(view, 0), vec!["0", "1", "2"], "ascending");
        assert!(view.active_sort().is_some_and(|key| key.ascending));

        view.toggle_sort("n", cx);
        assert_eq!(grid_column(view, 0), vec!["2", "1", "0"], "descending");
        assert_eq!(describe(&view.status), "info: Sorted by n ↓");

        // The statement is untouched throughout -- nothing was re-run.
        let sql = match view.tabs.active() {
            Some(WorkspaceTab::Sql { editor, .. }) => editor.text().to_string(),
            _ => panic!("a query tab"),
        };
        assert_eq!(sql, "SELECT n FROM series");
    });
}

/// A third click puts the server's own order back, which is the reason the
/// view remembers where each row started.
#[gpui::test]
fn a_third_click_restores_the_order_the_server_sent(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(view, &[("SELECT n FROM series", Some(4))], cx);

        view.toggle_sort("n", cx);
        view.toggle_sort("n", cx);
        assert_eq!(grid_column(view, 0), vec!["3", "2", "1", "0"]);

        view.toggle_sort("n", cx);
        assert!(view.active_sort().is_none());
        assert_eq!(grid_column(view, 0), vec!["0", "1", "2", "3"]);
        assert_eq!(describe(&view.status), "info: Sort cleared");
    });
}

/// Every row index the window holds is a position, and sorting moves them --
/// so the selection goes rather than pointing at whoever landed there.
#[gpui::test]
fn sorting_a_query_result_drops_the_selection(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(view, &[("SELECT n FROM series", Some(3))], cx);

        view.grid_pointer_down(2, Some(0), gpui::Modifiers::default(), cx);
        assert_eq!(selected_rows(view), vec![2]);
        assert_eq!(view.selected_cell, Some((2, 0)));

        view.toggle_sort("n", cx);
        assert!(selected_rows(view).is_empty());
        assert_eq!(view.selected_cell, None);
    });
}

/// Selecting another statement from the strip shows its rows as it returned
/// them, not under the sort the last one was wearing.
#[gpui::test]
fn picking_another_statement_starts_unsorted(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(
            view,
            &[("SELECT n FROM a", Some(3)), ("SELECT n FROM b", Some(3))],
            cx,
        );

        view.toggle_sort("n", cx);
        view.toggle_sort("n", cx);
        assert!(view.active_sort().is_some());

        view.select_statement_result(1, cx);
        assert!(view.active_sort().is_none());
        assert_eq!(grid_column(view, 0), vec!["0", "1", "2"]);
    });
}

/// A template lands in the editor at the caret, and does not eat what is
/// already there.
#[gpui::test]
fn a_template_is_inserted_rather_than_pasted_over(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        if let Some(WorkspaceTab::Sql { editor, .. }) = view.tabs.active_mut() {
            *editor = crate::text_input::TextInput::with_text("SELECT 1;", true);
            editor.move_end();
        }

        view.insert_sql_template("INSERT", "INSERT INTO t (a)\nVALUES ('v');\n", cx);

        let sql = match view.tabs.active() {
            Some(WorkspaceTab::Sql { editor, .. }) => editor.text().to_string(),
            _ => panic!("a query tab"),
        };
        assert!(
            sql.starts_with("SELECT 1;"),
            "what was typed survives: {sql}"
        );
        assert!(sql.contains("INSERT INTO t (a)"));
        assert!(
            sql.contains("SELECT 1;\nINSERT"),
            "and the template starts its own line: {sql}"
        );
        assert_eq!(view.focus, Focus::Editor);
    });
}

/// The templates that can empty a table carry their `WHERE` already written.
#[gpui::test]
fn the_destructive_templates_ship_with_a_where(_cx: &mut TestAppContext) {
    use dbui_app::domain::Driver;

    for driver in Driver::ALL {
        for template in crate::sql_scaffold::templates(driver, None) {
            let body = template.body.to_uppercase();
            if body.starts_with("DELETE") || body.starts_with("UPDATE") {
                assert!(
                    body.contains("WHERE"),
                    "{} on {driver:?} has no WHERE: {}",
                    template.name,
                    template.body
                );
            }
        }
    }
}

/// MySQL spells an upsert differently, and the template has to say so.
#[gpui::test]
fn the_upsert_template_follows_the_engine(_cx: &mut TestAppContext) {
    use dbui_app::domain::Driver;

    let upsert = |driver| {
        crate::sql_scaffold::templates(driver, None)
            .into_iter()
            .find(|template| template.name == "INSERT or update")
            .expect("an upsert template")
            .body
    };

    assert!(upsert(Driver::MySql).contains("ON DUPLICATE KEY UPDATE"));
    assert!(upsert(Driver::Postgres).contains("ON CONFLICT"));
    assert!(upsert(Driver::Sqlite).contains("ON CONFLICT"));
}

/// Put a batch that stopped on an error onto a SQL tab.
///
/// `ok` are the statements that ran; `failed` is the one that did not, and
/// `skipped` are the ones the run never reached.
fn put_failed_batch(
    view: &mut DbUi,
    ok: &[&str],
    failed: (&str, &str, Option<&str>),
    skipped: &[&str],
    cx: &mut gpui::Context<DbUi>,
) {
    use dbui_app::domain::{QueryOutcome, QueryResult, QueryStats};

    let (failed_sql, message, code) = failed;
    let results: Vec<QueryResult> = ok
        .iter()
        .map(|sql| QueryResult {
            statement: (*sql).to_string(),
            outcome: QueryOutcome::Affected(1),
            stats: QueryStats {
                elapsed: std::time::Duration::from_millis(1),
            },
        })
        .collect();

    let mut sent: Vec<String> = ok.iter().map(|sql| (*sql).to_string()).collect();
    sent.push(failed_sql.to_string());
    sent.extend(skipped.iter().map(|sql| (*sql).to_string()));

    let tab_id = view.tabs.active_id().expect("a tab");
    let batch = dbui_app::BatchQueryResult {
        last_rows: None,
        attempted: sent.len(),
        results,
        total_elapsed: std::time::Duration::from_millis(2),
        failure: Some(dbui_app::DriverError::Query {
            statement: failed_sql.to_string(),
            message: message.to_string(),
            code: code.map(str::to_string),
        }),
    };
    view.absorb_batch_result_for_test(tab_id, batch, &sent, true);
    cx.notify();
}

/// The bug this fixes: run-all kept only the last row-producing result and
/// threw the rest away, so a batch whose interesting statement was in the
/// middle was unreadable.
#[gpui::test]
fn every_statement_of_a_run_keeps_its_result(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(
            view,
            &[
                ("UPDATE t SET a = 1", None),
                ("SELECT * FROM t", Some(4)),
                ("DELETE FROM t", None),
            ],
            cx,
        );

        let Some(WorkspaceTab::Sql {
            results,
            active_result,
            ..
        }) = view.tabs.active()
        else {
            panic!("sql tab");
        };
        assert_eq!(results.len(), 3, "all three are kept");
        assert_eq!(
            *active_result, 1,
            "the first that produced rows is put in front"
        );
        assert_eq!(results[0].label(0), "1 UPDATE");
        assert_eq!(results[2].label(2), "3 DELETE");
    });
}

/// Selecting a statement swaps its rows into the grid and hands the old ones
/// back, so switching to and fro does not lose either.
#[gpui::test]
fn selecting_a_statement_swaps_its_rows_into_the_grid(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(view, &[("SELECT 1", Some(2)), ("SELECT 2", Some(5))], cx);

        let rows_now = |view: &DbUi| {
            view.tabs
                .active()
                .and_then(|tab| tab.result())
                .map(|v| v.set.rows.len())
        };
        assert_eq!(rows_now(view), Some(2), "the first is in front");

        view.select_statement_result(1, cx);
        assert_eq!(rows_now(view), Some(5));

        view.select_statement_result(0, cx);
        assert_eq!(rows_now(view), Some(2), "and going back finds them again");
    });
}

/// A statement that returned no rows is still selectable -- "3 rows affected"
/// is a result.
#[gpui::test]
fn a_statement_with_no_rows_is_still_in_the_strip(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(
            view,
            &[("SELECT 1", Some(1)), ("UPDATE t SET a = 1", None)],
            cx,
        );

        view.select_statement_result(1, cx);
        assert!(
            view.tabs.active().and_then(|tab| tab.result()).is_none(),
            "no grid for a statement that produced none"
        );
        assert!(
            describe(&view.status).contains("affected"),
            "but it says what happened: {}",
            describe(&view.status)
        );
    });
}

/// The bug this fixes: a batch that failed halfway threw away the results of
/// everything that had already run, so a failed run looked like a run that
/// never happened.
#[gpui::test]
fn a_run_that_fails_halfway_keeps_what_did_run(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_failed_batch(
            view,
            &["CREATE TABLE t (a int)", "INSERT INTO t VALUES (1)"],
            (
                "SELECT * FROM missing",
                "relation \"missing\" does not exist",
                Some("42P01"),
            ),
            &["DROP TABLE t"],
            cx,
        );

        let Some(WorkspaceTab::Sql { results, .. }) = view.tabs.active() else {
            panic!("sql tab");
        };
        assert_eq!(results.len(), 2, "the two that ran are still in the strip");

        let error = view
            .tabs
            .active()
            .and_then(|tab| tab.error())
            .expect("the failure is on the tab");
        assert_eq!(error.sql, "SELECT * FROM missing");
        assert_eq!(error.code.as_deref(), Some("42P01"));
        assert_eq!(error.succeeded, 2);
        assert_eq!(error.skipped, 1, "the one after it was never attempted");
        let progress = error.progress().expect("a run of four says how far it got");
        assert!(
            progress.contains("2 statements ran") && progress.contains("1 was not attempted"),
            "got: {progress}"
        );
        assert!(
            describe(&view.status).contains("42P01"),
            "and the footer carries the short version: {}",
            describe(&view.status)
        );
    });
}

/// The bug this fixes: the status bar is one line, and the next message
/// overwrote the reason the query failed. The error now belongs to the tab.
#[gpui::test]
fn a_sql_error_outlives_the_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_failed_batch(
            view,
            &[],
            ("SELECT bad", "syntax error at or near \"bad\"", None),
            &[],
            cx,
        );
        assert!(matches!(view.status, Status::Error(_)));

        // Anything at all can own the footer a moment later -- here, reading
        // the error out to the clipboard.
        view.copy_tab_error(cx);
        assert!(
            !matches!(view.status, Status::Error(_)),
            "the footer moved on, which is what footers do"
        );

        let error = view
            .tabs
            .active()
            .and_then(|tab| tab.error())
            .expect("but the error is still on the tab");
        assert_eq!(error.message, "syntax error at or near \"bad\"");
        assert_eq!(
            error.progress(),
            None,
            "a lone statement has no progress to report"
        );
    });
}

/// It goes away when the next run succeeds, and when the user dismisses it --
/// and not before. An unread error is not a handled one.
#[gpui::test]
fn an_error_clears_on_the_next_good_run_and_on_dismissal(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_failed_batch(view, &[], ("SELECT bad", "no", None), &[], cx);
        view.clear_tab_error(cx);
        assert!(view.tabs.active().and_then(|tab| tab.error()).is_none());
        assert!(
            matches!(view.status, Status::Idle),
            "the footer cleared with it"
        );

        put_failed_batch(view, &[], ("SELECT bad", "no", None), &[], cx);
        put_batch(view, &[("SELECT 1", Some(1))], cx);
        assert!(
            view.tabs.active().and_then(|tab| tab.error()).is_none(),
            "a clean run replaces the last failure"
        );
    });
}

/// The panel has to paint, at every window size, with and without a code.
#[gpui::test]
fn the_error_panel_draws(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_failed_batch(
            view,
            &["SELECT 1"],
            (
                "SELECT * FROM\n  a_very_long_table_name_that_wraps",
                "relation \"a_very_long_table_name_that_wraps\" does not exist, and this \
                 message is long enough to need more than one line of the panel",
                Some("42P01"),
            ),
            &["SELECT 2"],
            cx,
        );
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| {
        assert!(
            view.tabs.active().and_then(|tab| tab.error()).is_some(),
            "and it survived the round trip"
        );
    });
}

/// Running a statement records it, and picking it out of the history loads it
/// back into the editor rather than running it again.
#[gpui::test]
fn history_records_statements_and_loads_them_back(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        view.history.record(dbui_app::HistoryEntry {
            sql: "SELECT * FROM users".into(),
            connection: None,
            at: 1,
            ok: true,
        });

        view.put_sql_in_editor("SELECT * FROM users", cx);
        assert_eq!(sql_editor_text(view), "SELECT * FROM users");
        assert_eq!(view.focus, Focus::Editor, "ready to read before running");
        assert!(
            describe(&view.status).contains("history"),
            "got: {}",
            describe(&view.status)
        );
    });
}

// -- a real connection, with no server ------------------------------------

/// A window with a *live* connection, backed by a temporary SQLite file.
///
/// The surfaces that only exist once something is connected -- the schema
/// tree above all -- cannot be reached by faking a catalog, because they check
/// the connection status rather than the data. SQLite is what makes a real one
/// available in a unit test: the engine is linked in and the database is a
/// file this makes and deletes.
struct ConnectedDb {
    path: std::path::PathBuf,
}

impl Drop for ConnectedDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn open_connected<'a>(
    cx: &'a mut TestAppContext,
    name: &str,
) -> (Entity<DbUi>, &'a mut VisualTestContext, ConnectedDb) {
    let mut path = std::env::temp_dir();
    path.push(format!("dbui-e2e-{}-{name}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).expect("create the database file");

    // Seeded with the driver directly; the UI only has to open it.
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime to seed with");
        let file = path.to_string_lossy().to_string();
        runtime.block_on(async {
            let mut config = ConnectionConfig::new(Driver::Sqlite);
            config.database = file;
            let db = dbui_driver_connect(&config).await;
            for sql in [
                "CREATE TABLE teams (slug TEXT PRIMARY KEY, name TEXT)",
                "INSERT INTO teams VALUES ('core','Core'), ('ops','Operations')",
                "CREATE TABLE members (
                     id INTEGER PRIMARY KEY,
                     name TEXT NOT NULL,
                     team_slug TEXT REFERENCES teams (slug)
                 )",
                "INSERT INTO members (name, team_slug) VALUES ('Ada','core'), ('Grace','ops')",
                "CREATE VIEW active_members AS SELECT id, name FROM members",
            ] {
                db.execute(sql).await.expect("seed");
            }
            db.close().await;
        });
    }

    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.name = name.to_string();
    config.database = path.to_string_lossy().to_string();

    let (view, cx) = open_with(cx, Workspace::from_configs(vec![config]));
    view.update(cx, |view, cx| {
        let id = view.workspace.entries()[0].id();
        view.connect(id, cx);
    });

    // The connect crosses to tokio and back, so parking once is not enough.
    for _ in 0..200 {
        cx.run_until_parked();
        let connected = view.update(cx, |view, _| {
            view.workspace
                .active()
                .is_some_and(|entry| entry.status.is_connected())
        });
        if connected {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    view.update(cx, |view, _| {
        assert!(
            view.workspace
                .active()
                .is_some_and(|entry| entry.status.is_connected()),
            "the test database should have connected"
        );
    });

    (view, cx, ConnectedDb { path })
}

/// The driver crate is not a direct dependency of this one; the app layer
/// re-exports what is needed to open a connection.
async fn dbui_driver_connect(
    config: &ConnectionConfig,
) -> std::sync::Arc<dyn dbui_app::DatabaseDriver> {
    dbui_app::connect_driver(config).await.expect("connect")
}

/// The tree only draws once something is connected, which is why nothing had
/// ever drawn it.
#[gpui::test]
fn the_schema_tree_draws_against_a_real_connection(cx: &mut TestAppContext) {
    let (view, cx, _db) = open_connected(cx, "tree");

    view.update(cx, |view, _| {
        let items = view.sidebar_visible_items();
        assert!(!items.is_empty(), "the tree has rows to draw");
    });
    draw_at_every_size(&view, cx);

    // With a filter over it, and with one that matches nothing.
    view.update(cx, |view, cx| {
        view.sidebar_filter.set_text("mem");
        cx.notify();
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, cx| {
        view.sidebar_filter.set_text("zzzz");
        cx.notify();
    });
    draw_at_every_size(&view, cx);
}

/// End to end through the real UI: open a table, edit a cell, commit, and ask
/// the database whether it happened.
#[gpui::test]
fn a_committed_edit_reaches_the_database(cx: &mut TestAppContext) {
    let (view, cx, _db) = open_connected(cx, "commit");

    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("main", "members"), cx);
    });
    for _ in 0..200 {
        cx.run_until_parked();
        let loaded = view.update(cx, |view, _| {
            view.tabs.active().and_then(|tab| tab.result()).is_some()
        });
        if loaded {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    view.update(cx, |view, _| {
        let rows = view
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .map(|r| r.set.rows.len());
        assert_eq!(rows, Some(2), "the seeded rows arrived");
    });
    draw_at_every_size(&view, cx);
}

/// The bug this fixes: `CREATE TABLE` came back as "0 rows affected" -- which
/// reads as a statement that did nothing -- and the schema tree went on
/// showing the database as it was before, so nothing on screen said the table
/// existed.
#[gpui::test]
fn create_table_says_what_it_did_and_the_tree_catches_up(cx: &mut TestAppContext) {
    let (view, cx, _db) = open_connected(cx, "create-table");

    let tables_before = view.update(cx, |view, _| view.sidebar_visible_items().len());

    view.update(cx, |view, cx| {
        view.put_sql_in_editor("CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT)", cx);
        view.run_query(cx);
    });
    settle(&view, cx, |view| {
        !matches!(view.status, Status::Busy(_)) && view.workspace.active().is_some()
    });

    view.update(cx, |view, _| {
        let said = describe(&view.status);
        assert!(
            said.contains("CREATE TABLE") && said.contains("notes"),
            "the verdict names the statement and its table, got: {said}"
        );
        assert!(
            !said.contains("0 rows affected"),
            "and not a row count that means nothing: {said}"
        );
        assert!(
            view.tabs.active().and_then(|tab| tab.error()).is_none(),
            "nothing failed"
        );
    });

    // The refresh it triggers is a second trip to the database.
    settle(&view, cx, |view| {
        view.sidebar_visible_items().len() > tables_before
    });
    view.update(cx, |view, _| {
        assert!(
            view.sidebar_visible_items().len() > tables_before,
            "the new table reached the schema tree"
        );
        assert!(
            describe(&view.status).contains("CREATE TABLE"),
            "and the quiet refresh did not overwrite the verdict: {}",
            describe(&view.status)
        );
    });
}

/// A statement the engine refuses lands on the tab, not only in the footer.
#[gpui::test]
fn a_failing_statement_leaves_its_error_on_the_tab(cx: &mut TestAppContext) {
    let (view, cx, _db) = open_connected(cx, "sql-error");

    view.update(cx, |view, cx| {
        view.put_sql_in_editor("SELECT * FROM no_such_table", cx);
        view.run_query(cx);
    });
    settle(&view, cx, |view| {
        view.tabs.active().and_then(|tab| tab.error()).is_some()
    });

    view.update(cx, |view, _| {
        let error = view
            .tabs
            .active()
            .and_then(|tab| tab.error())
            .expect("the engine refused it");
        assert_eq!(error.sql, "SELECT * FROM no_such_table");
        assert!(
            error.message.contains("no_such_table"),
            "the engine's own words: {}",
            error.message
        );
        assert_eq!(
            view.focus,
            Focus::Editor,
            "the keyboard stays where the statement is"
        );
    });

    // The failed statement is on the clipboard in full when asked for.
    view.update(cx, |view, cx| {
        view.copy_tab_error(cx);
        assert!(
            describe(&view.status).contains("copied"),
            "{}",
            describe(&view.status)
        );
    });
    draw_at_every_size(&view, cx);
}

/// Pump the runtime until `done`, or give up. The tasks cross to tokio and
/// back, so parking once is never enough.
fn settle(view: &Entity<DbUi>, cx: &mut VisualTestContext, done: impl Fn(&DbUi) -> bool) {
    for _ in 0..200 {
        cx.run_until_parked();
        if view.update(cx, |view, _| done(view)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

// -- every surface actually draws -----------------------------------------
//
// The behaviour tests above prove what each command *does*. These prove the
// surfaces they open can be painted -- at a comfortable size, at a narrow one,
// and at a short one. A layout that divides by a zero width, or an element id
// that collides, only shows up when something actually draws it, and until now
// the only way to find that was to open the app and look.

/// Open the change bubble's detail area, whatever state it was in.
///
/// Staging a row or a deletion expands it already, so toggling can just as
/// easily close it -- which is how a test meant to draw the diff ended up
/// drawing nothing at all.
fn expand_change_bubble(view: &mut DbUi, cx: &mut gpui::Context<DbUi>) {
    if let Some(WorkspaceTab::Table {
        change_bubble_expanded,
        ..
    }) = view.tabs.active_mut()
    {
        *change_bubble_expanded = true;
    }
    cx.notify();
}

/// Sizes worth drawing at: the default, a narrow window, and a short one.
///
/// The sidebar, detail panel and change bubble all claim fixed widths or
/// heights, so a window smaller than the sum of them is where clamping either
/// works or panics.
const DRAW_SIZES: [(f32, f32); 3] = [(1200., 800.), (620., 700.), (900., 320.)];

/// Draw the window at each size. A panic in any layout fails the test; the
/// return is the state surviving the round trip, which is what proves a draw
/// happened rather than being skipped.
fn draw_at_every_size(view: &Entity<DbUi>, cx: &mut VisualTestContext) {
    for (width, height) in DRAW_SIZES {
        cx.simulate_resize(gpui::size(gpui::px(width), gpui::px(height)));
        view.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
    }
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    cx.run_until_parked();
}

/// The grid's scrollbar is a control, not a decoration: pressing its thumb
/// and dragging moves the rows.
///
/// It is also the only part of the window whose geometry is worked out during
/// paint rather than in `render`, so a test that only drew it would prove
/// nothing about where it ended up.
#[gpui::test]
fn dragging_the_grid_scrollbar_scrolls_the_rows(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 400);
    cx.simulate_resize(gpui::size(gpui::px(1200.), gpui::px(800.)));
    cx.run_until_parked();

    // The list's measured bounds say how tall the track is; the pane's say
    // where its right-hand edge is. They are not the same box -- with two
    // narrow columns the rows stop well short of the pane.
    let list = view.read_with(cx, |view, _| {
        view.grid_scroll.0.borrow().base_handle.bounds()
    });
    assert!(list.size.height > gpui::px(0.), "the grid was laid out");
    let pane = view.read_with(cx, |view, _| view.grid_h_scroll.bounds());
    let x = pane.right() - gpui::px(6.);
    let top = list.top() + gpui::px(10.);

    let offset = |cx: &mut VisualTestContext| {
        view.read_with(cx, |view, _| {
            view.grid_scroll.0.borrow().base_handle.offset().y
        })
    };
    assert_eq!(offset(cx), gpui::px(0.), "starts at the top");

    cx.simulate_mouse_move(gpui::point(x, top), None, gpui::Modifiers::default());
    cx.simulate_mouse_down(
        gpui::point(x, top),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_move(
        gpui::point(x, list.center().y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.simulate_mouse_up(
        gpui::point(x, list.center().y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    let scrolled = offset(cx);
    assert!(
        scrolled < gpui::px(0.),
        "dragging the thumb down scrolled the rows, got {scrolled:?}"
    );

    // And the press was the scrollbar's alone -- a row under it must not have
    // taken the click as a selection.
    view.read_with(cx, |view, _| {
        assert_eq!(view.selected_cell, None, "no cell was selected by the drag");
    });
}

#[gpui::test]
fn the_grid_and_detail_sidebar_draw(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 40);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(3, Some(1), gpui::Modifiers::default(), cx);
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| {
        assert_eq!(view.selected_cell, Some((3, 1)), "and the state survived");
    });
}

/// The change bubble, expanded, with all three kinds of staged change in it --
/// the diff renderer is the most intricate thing in the window.
#[gpui::test]
fn the_change_bubble_draws_with_every_kind_of_change(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 6);

    view.update(cx, |view, cx| {
        // An edit.
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("renamed");
        view.commit_cell_edit(cx);
        // A deletion.
        view.grid_pointer_down(2, None, gpui::Modifiers::default(), cx);
        view.delete_selected_rows(cx);
        // And a new row.
        view.add_row(cx);
        expand_change_bubble(view, cx);
    });

    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| {
        assert_eq!(view.collect_batch_edits().len(), 1);
        assert_eq!(view.collect_batch_deletes().len(), 1);
        assert_eq!(view.staged_insert_count(), 1);
    });
}

/// A multi-line JSON edit, which is what puts the line-diff renderer to work.
#[gpui::test]
fn the_bubble_draws_a_multi_line_diff(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        type_into_draft(view, 1, "{\n  \"a\": 1,\n  \"b\": 2\n}");
        expand_change_bubble(view, cx);
    });
    draw_at_every_size(&view, cx);
}

#[gpui::test]
fn the_statement_strip_draws(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        put_batch(
            view,
            &[
                ("UPDATE t SET a = 1", None),
                ("SELECT * FROM t", Some(12)),
                ("DELETE FROM t WHERE id = 1", None),
                ("SELECT count(*) FROM t", Some(1)),
            ],
            cx,
        );
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, cx| {
        view.select_statement_result(3, cx);
    });
    draw_at_every_size(&view, cx);
}

#[gpui::test]
fn each_palette_draws(cx: &mut TestAppContext) {
    use crate::components::palette::PaletteKind;

    let (view, cx) = with_catalog(cx, &[("users", &["id"]), ("orders", &["id"])]);

    for kind in [
        PaletteKind::GoToTable,
        PaletteKind::Actions,
        PaletteKind::Themes,
        PaletteKind::History,
    ] {
        view.update(cx, |view, cx| {
            view.history.record(dbui_app::HistoryEntry {
                sql: "SELECT * FROM users WHERE id = 1".into(),
                connection: None,
                at: 1,
                ok: true,
            });
            view.open_palette(kind, cx);
        });
        draw_at_every_size(&view, cx);
        view.update(cx, |view, cx| view.close_palette(cx));
    }
}

/// Every theme, drawn -- a colour that does not exist is a compile error, but a
/// theme whose stripe or selection is missing only shows up when painted.
#[gpui::test]
fn every_theme_draws(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 10);

    for theme in crate::theme::all_themes() {
        view.update(cx, |view, cx| {
            view.apply_theme_id(theme.id);
            view.grid_pointer_down(1, Some(0), gpui::Modifiers::default(), cx);
            cx.notify();
        });
        cx.run_until_parked();
    }
}

#[gpui::test]
fn the_context_menu_and_confirmation_draw(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};
    use dbui_app::domain::TableKind;

    let (view, cx) = open_table_with_rows(cx, 5);

    // Against the sidebar, near a corner, so the flip-and-clamp runs.
    for position in [(20., 40.), (1180., 780.), (0., 0.)] {
        view.update(cx, |view, cx| {
            view.open_context_menu(
                ContextTarget::Table {
                    table: TableRef::new("public", "users"),
                    kind: TableKind::Table,
                },
                gpui::point(gpui::px(position.0), gpui::px(position.1)),
                cx,
            );
        });
        draw_at_every_size(&view, cx);
        view.update(cx, |view, cx| view.close_context_menu(cx));
    }

    // The row menu, and then the typed confirmation over the top of it.
    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Rows,
            gpui::point(gpui::px(500.), gpui::px(300.)),
            cx,
        );
    });
    draw_at_every_size(&view, cx);

    // The tab bar's menu, hanging off a tab near the top-left corner.
    view.update(cx, |view, cx| {
        view.close_context_menu(cx);
        let id = view.tabs.items[0].id();
        view.open_context_menu(
            ContextTarget::Tab { id },
            gpui::point(gpui::px(60.), gpui::px(30.)),
            cx,
        );
    });
    draw_at_every_size(&view, cx);
    view.update(cx, |view, cx| view.close_context_menu(cx));

    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Table {
                table: TableRef::new("public", "users"),
                kind: TableKind::Table,
            },
            gpui::point(gpui::px(40.), gpui::px(40.)),
            cx,
        );
        view.run_context_action(MenuAction::Drop, cx);
        assert!(view.confirm.is_some());
    });
    draw_at_every_size(&view, cx);
}

/// Every surface, drawn at both ends of the zoom range.
///
/// The chrome used to be half-scaled: `metrics` grew with ⌘+ but the captions,
/// buttons and panel widths were written as raw pixels, so at 200% the text
/// outgrew the boxes holding it. Nothing here can assert on how it *looks*, but
/// drawing the lot at each end is what catches a surface that was measured for
/// one zoom and painted at another.
#[gpui::test]
fn every_surface_draws_at_every_zoom(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 40);

    for pct in [50, 100, 200] {
        crate::theme::metrics::set_zoom_pct(pct);

        view.update(cx, |view, cx| {
            view.grid_pointer_down(3, Some(1), gpui::Modifiers::default(), cx);
            view.detail_open = true;
            view.toggle_filters_open(cx);
            view.toggle_columns_open(cx);
        });
        draw_at_every_size(&view, cx);

        view.update(cx, |view, cx| {
            view.toggle_filters_open(cx);
            view.toggle_columns_open(cx);
            view.open_sql_tab(cx);
            view.toggle_connection_picker(cx);
        });
        draw_at_every_size(&view, cx);
        view.update(cx, |view, cx| {
            view.close_connection_picker(cx);
            view.close_tab(view.tabs.active, cx);
        });
    }

    crate::theme::metrics::zoom_reset();
}

/// The "discard staged changes?" guard, at every window size.
#[gpui::test]
fn the_close_guard_draws(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_tab(0, cx);
        assert!(view.close_guard.is_some(), "the guard has to be up to draw");
    });
    draw_at_every_size(&view, cx);

    // And the dirty dot beside the tab that put it there.
    view.update(cx, |view, cx| view.cancel_close(cx));
    draw_at_every_size(&view, cx);
}

/// The connection sheet, the picker, and the read-only badge beside them.
#[gpui::test]
fn the_connection_surfaces_draw(cx: &mut TestAppContext) {
    let mut config = ConnectionConfig::new(Driver::Postgres);
    config.name = "Prod".into();
    config.read_only = true;
    let mut sqlite = ConnectionConfig::new(Driver::Sqlite);
    sqlite.name = "Local file".into();
    sqlite.database = "/tmp/x.db".into();

    let (view, cx) = open_with(cx, Workspace::from_configs(vec![config, sqlite]));

    // The read-only badge in the titlebar.
    draw_at_every_size(&view, cx);

    view.update(cx, |view, cx| view.toggle_connection_picker(cx));
    draw_at_every_size(&view, cx);
    view.update(cx, |view, cx| view.close_connection_picker(cx));

    // The sheet, for both a networked engine and a file-based one -- the
    // second hides host, port, user and password.
    view.update(cx, |view, cx| view.open_new_connection(cx));
    draw_at_every_size(&view, cx);
    view.update(cx, |view, cx| {
        if let Some(form) = view.modal.as_mut() {
            form.set_driver(Driver::Sqlite);
        }
        cx.notify();
    });
    draw_at_every_size(&view, cx);
}

/// The tree with a filter over it, the structure pane, the columns panel and
/// the filter strip -- everything the centre column can show.
#[gpui::test]
fn the_side_and_centre_panels_draw(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id"]), ("user_sessions", &["id"])]);

    view.update(cx, |view, cx| {
        view.focus_sidebar_search(cx);
        view.sidebar_filter.set_text("user");
        cx.notify();
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, cx| {
        view.sidebar_filter.set_text("nothing matches this");
        cx.notify();
    });
    draw_at_every_size(&view, cx);
}

#[gpui::test]
fn the_table_panes_draw(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 20);

    view.update(cx, |view, cx| {
        view.toggle_filters_open(cx);
        view.toggle_columns_open(cx);
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, cx| {
        view.set_table_pane(crate::tabs::TablePane::Structure, cx);
    });
    draw_at_every_size(&view, cx);

    // An empty result, which is a different path from rows.
    view.update(cx, |view, cx| {
        view.set_table_pane(crate::tabs::TablePane::Data, cx);
        if let Some(WorkspaceTab::Table { result, .. }) = view.tabs.active_mut() {
            if let Some(view) = result.as_mut() {
                view.set.rows.clear();
            }
        }
        cx.notify();
    });
    draw_at_every_size(&view, cx);
}

/// The editor with highlighting, a completion popup over it, and a cell editor
/// open in the grid underneath.
#[gpui::test]
fn the_editors_draw(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id", "email"])]);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(
            view,
            "-- a comment\nselect id, email\nfrom us\nwhere id = 1;\n",
        );
        view.trigger_completion(cx);
        view.focus = Focus::Editor;
        cx.notify();
    });
    draw_at_every_size(&view, cx);
}

#[gpui::test]
fn the_inline_cell_editor_draws(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 8);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(2, 1, cx);
        assert!(view.editing_cell.is_some());
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| {
        assert!(view.editing_cell.is_some(), "still open after the redraws");
    });
}

/// A bulk selection, which is what puts MIXED and the banner on screen.
#[gpui::test]
fn the_bulk_edit_sidebar_draws(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 30);

    cx.simulate_keystrokes("cmd-a");
    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| {
        assert_eq!(draft_texts(view)[1], crate::tabs::MIXED);
    });
}

/// Staged new rows are drawn under the stored ones, so the grid renders two
/// kinds of row from one list.
#[gpui::test]
fn staged_new_rows_draw_in_the_grid(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        view.add_row(cx);
        view.edit_insert(0, cx);
    });
    draw_at_every_size(&view, cx);

    view.update(cx, |view, _| assert_eq!(view.staged_insert_count(), 2));
}

// -- moving the cell cursor -----------------------------------------------

/// Without ← / → the grid is only half keyboard-drivable: everything keyed off
/// the selected cell -- editing in place, following a foreign key -- could
/// only be reached with the pointer.
#[gpui::test]
fn arrows_walk_the_cell_cursor_along_a_row(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.select_row(1, cx);
        view.focus = Focus::Grid;
        assert_eq!(view.selected_cell, None);

        // Arriving from a row selection lands on the first column.
        view.move_selected_cell(1, cx);
        assert_eq!(view.selected_cell, Some((1, 0)));

        view.move_selected_cell(1, cx);
        assert_eq!(view.selected_cell, Some((1, 1)));

        // Two columns in the fixture, so it wraps.
        view.move_selected_cell(1, cx);
        assert_eq!(view.selected_cell, Some((1, 0)));
        view.move_selected_cell(-1, cx);
        assert_eq!(view.selected_cell, Some((1, 1)), "and wraps the other way");
    });
}

/// Going left from no cell at all lands on the last column, so ← from a fresh
/// row selection reaches the far end without walking the whole row.
#[gpui::test]
fn going_left_from_a_row_selection_lands_at_the_end(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.select_row(0, cx);
        view.focus = Focus::Grid;
        view.move_selected_cell(-1, cx);
        assert_eq!(view.selected_cell, Some((0, 1)));
    });
}

/// The whole point: a foreign key is now reachable without the pointer.
#[gpui::test]
fn a_foreign_key_can_be_followed_from_the_keyboard(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.select_row(1, cx);
        view.focus = Focus::Grid;
        // Walk to the column that references another table.
        view.move_selected_cell(1, cx);
        view.move_selected_cell(1, cx);
        assert_eq!(view.selected_cell, Some((1, 1)));
        assert!(view.foreign_key_at(1, 1).is_some());
    });

    cx.simulate_keystrokes("cmd-enter");
    view.update(cx, |view, _| {
        assert_eq!(
            view.tabs
                .active()
                .and_then(|tab| tab.table_ref())
                .map(|t| t.qualified()),
            Some("public.teams".to_string())
        );
    });
}

// -- connections, tabs, menus ---------------------------------------------

#[gpui::test]
fn the_connection_picker_opens_and_closes(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));

    view.update(cx, |view, cx| {
        assert!(!view.connection_picker_open);
        view.toggle_connection_picker(cx);
        assert!(view.connection_picker_open);
        view.toggle_connection_picker(cx);
        assert!(!view.connection_picker_open);
    });

    view.update(cx, |view, cx| view.toggle_connection_picker(cx));
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| {
        assert!(!view.connection_picker_open, "escape closes it");
    });
}

/// Picking a connection from the picker opens it as a tab and closes the
/// picker behind it.
#[gpui::test]
fn picking_a_connection_opens_it_as_a_tab(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));

    view.update(cx, |view, cx| {
        let second = view.workspace.entries()[1].id();
        view.toggle_connection_picker(cx);
        view.pick_connection(second, cx);

        assert!(!view.connection_picker_open);
        assert_eq!(view.workspace.active_id(), Some(second));
        assert_eq!(view.workspace.open_count(), 2);
    });
}

/// Removing a connection takes its tabs with it but leaves the others alone.
#[gpui::test]
fn removing_a_connection_takes_its_tabs_and_no_others(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(2));

    view.update(cx, |view, cx| {
        let (first, second) = (
            view.workspace.entries()[0].id(),
            view.workspace.entries()[1].id(),
        );
        view.open_connection_tab(second, cx);
        view.open_table_tab(TableRef::new("public", "orders"), cx);
        view.open_connection_tab(first, cx);
        view.open_table_tab(TableRef::new("public", "users"), cx);

        view.remove_connection(second, cx);

        assert!(view.workspace.get(second).is_none(), "it is gone");
        assert!(view.workspace.get(first).is_some(), "the other is not");
        assert!(!view.stashed_tabs.contains_key(&second));
        let labels: Vec<_> = view.tabs.items.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, vec!["users"], "the surviving tab is untouched");
    });
}

/// Dropping a relation closes the tabs pointing at it -- one onto a table that
/// is gone only fails on its next load.
#[gpui::test]
fn dropping_a_table_closes_the_tabs_onto_it(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("public", "users"), cx);
        view.open_table_tab(TableRef::new("public", "orders"), cx);
        assert_eq!(view.tabs.items.len(), 2);

        view.close_tabs_for_table(&TableRef::new("public", "users"), cx);
        let labels: Vec<_> = view.tabs.items.iter().map(|tab| tab.label()).collect();
        assert_eq!(labels, vec!["orders"]);
    });
}

/// ⌘1…⌘9 jump to a tab by number, and ⌘9 is the last one.
#[gpui::test]
fn tab_numbers_select_and_nine_is_the_last(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        for name in ["a", "b", "c"] {
            view.open_table_tab(TableRef::new("public", name), cx);
        }
        assert_eq!(view.tabs.items.len(), 3);

        view.select_tab_number(1, cx);
        assert_eq!(view.tabs.active, 0);
        view.select_tab_number(2, cx);
        assert_eq!(view.tabs.active, 1);
        view.select_tab_number(9, cx);
        assert_eq!(view.tabs.active, 2, "9 is the last, browser-style");

        // A number past the end is ignored rather than clamped.
        view.select_tab_number(1, cx);
        view.select_tab_number(8, cx);
        assert_eq!(view.tabs.active, 0);
    });
}

#[gpui::test]
fn next_and_previous_tab_wrap(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        for name in ["a", "b", "c"] {
            view.open_table_tab(TableRef::new("public", name), cx);
        }
        view.select_tab_number(1, cx);

        view.prev_tab(cx);
        assert_eq!(view.tabs.active, 2, "back from the first wraps to the last");
        view.next_tab(cx);
        assert_eq!(view.tabs.active, 0, "and forward wraps round again");
    });
}

/// The context menu is walkable from the keyboard, and Enter runs what the
/// cursor is on -- separators are skipped because they are not selectable.
#[gpui::test]
fn the_context_menu_is_walkable_from_the_keyboard(cx: &mut TestAppContext) {
    use crate::components::context_menu::ContextTarget;
    use dbui_app::domain::TableKind;

    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Table {
                table: TableRef::new("public", "users"),
                kind: TableKind::Table,
            },
            gpui::point(gpui::px(0.), gpui::px(0.)),
            cx,
        );
        assert_eq!(view.context_menu.as_ref().unwrap().selected, 0);

        view.handle_context_menu_key("down", cx);
        assert_eq!(view.context_menu.as_ref().unwrap().selected, 1);
        view.handle_context_menu_key("up", cx);
        view.handle_context_menu_key("up", cx);
        assert!(
            view.context_menu.as_ref().unwrap().selected > 0,
            "up from the first wraps to the last"
        );

        // Enter on "Open" (the first entry) opens the table.
        view.handle_context_menu_key("down", cx);
        while view.context_menu.as_ref().unwrap().selected != 0 {
            view.handle_context_menu_key("down", cx);
        }
        view.handle_context_menu_key("enter", cx);
        assert!(view.context_menu.is_none(), "and the menu closes");
        assert_eq!(
            view.tabs.active().map(|tab| tab.label()),
            Some("users".to_string())
        );
    });
}

/// Refreshing with nothing open falls through to the catalog rather than
/// erroring about a result that does not exist.
#[gpui::test]
fn refresh_with_no_tab_refreshes_the_catalog(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        assert!(view.tabs.items.is_empty());
        view.refresh_result(cx);
        // Nothing is connected, so it is a no-op -- the property being pinned
        // is that it does not panic or report the wrong thing.
        assert!(!matches!(view.status, Status::Error(_)));
    });
}

// -- view toggles, paging, panes ------------------------------------------

#[gpui::test]
fn the_view_toggles_flip_and_flip_back(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        let open = |view: &DbUi| match view.tabs.active() {
            Some(WorkspaceTab::Table {
                filters_open,
                columns_open,
                ..
            }) => (*filters_open, *columns_open),
            _ => panic!("table tab"),
        };
        assert_eq!(open(view), (false, false));

        view.toggle_filters_open(cx);
        assert_eq!(open(view).0, true);
        // Opening the filter strip hands it the keyboard, or it is a box the
        // user has to click before typing in.
        assert_eq!(view.focus, Focus::Filter);
        view.toggle_filters_open(cx);
        assert_eq!(open(view).0, false);

        view.toggle_columns_open(cx);
        assert_eq!(open(view).1, true);
        view.toggle_columns_open(cx);
        assert_eq!(open(view).1, false);
    });
}

#[gpui::test]
fn hiding_a_column_keeps_it_out_of_the_grid_and_the_session(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.toggle_column_hidden("name", cx);
        let Some(WorkspaceTab::Table { hidden_columns, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert!(hidden_columns.contains("name"));

        // And it survives the round trip to disk.
        let saved = view.tabs.to_saved().0;
        let dbui_app::SavedTab::Table { hidden_columns, .. } = &saved[0] else {
            panic!("a table tab");
        };
        assert_eq!(hidden_columns, &vec!["name".to_string()]);

        view.toggle_column_hidden("name", cx);
        let Some(WorkspaceTab::Table { hidden_columns, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert!(hidden_columns.is_empty());
    });
}

#[gpui::test]
fn the_pane_switcher_moves_between_data_and_structure(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.set_table_pane(crate::tabs::TablePane::Structure, cx);
        let Some(WorkspaceTab::Table { pane, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert_eq!(*pane, crate::tabs::TablePane::Structure);

        view.set_table_pane(crate::tabs::TablePane::Data, cx);
        let Some(WorkspaceTab::Table { pane, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert_eq!(*pane, crate::tabs::TablePane::Data);
    });
}

/// Paging is anchored to the rows on screen, so a burst of clicks cannot run
/// the offset ahead of what has actually loaded.
#[gpui::test]
fn paging_does_not_run_ahead_of_the_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);
    let offset = |view: &DbUi| match view.tabs.active() {
        Some(WorkspaceTab::Table { page, .. }) => page.offset,
        _ => panic!("table tab"),
    };

    view.update(cx, |view, cx| {
        let limit = match view.tabs.active() {
            Some(WorkspaceTab::Table { page, .. }) => page.limit as u64,
            _ => panic!("table tab"),
        };

        // Nothing is connected here, so the reload never lands and the result
        // stays on page 0 -- which is exactly the state this guards.
        view.page(true, cx);
        assert_eq!(offset(view), limit);
        view.page(true, cx);
        assert_eq!(offset(view), limit, "the second click does not double it");
    });
}

/// The bug this fixes: the early return compared against the *result's* page,
/// so after a load that never landed, paging back did nothing at all and the
/// tab stayed pointing at a page it had never reached.
#[gpui::test]
fn paging_back_works_after_a_load_that_never_landed(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);
    let offset = |view: &DbUi| match view.tabs.active() {
        Some(WorkspaceTab::Table { page, .. }) => page.offset,
        _ => panic!("table tab"),
    };

    view.update(cx, |view, cx| {
        view.page(true, cx);
        assert!(offset(view) > 0);

        view.page(false, cx);
        assert_eq!(offset(view), 0, "and back it goes");
        view.page(false, cx);
        assert_eq!(offset(view), 0, "never past the first page");
    });
}

#[gpui::test]
fn the_detail_sidebar_toggles(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let was = view.detail_open;
        view.toggle_detail(cx);
        assert_eq!(view.detail_open, !was);
        view.toggle_detail(cx);
        assert_eq!(view.detail_open, was);
    });
}

#[gpui::test]
fn zooming_moves_in_steps_and_resets(cx: &mut TestAppContext) {
    let _layout = layout_lock();
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        view.zoom_delta(0, cx);
        let base = crate::theme::metrics::zoom_pct();

        view.zoom_delta(1, cx);
        assert!(crate::theme::metrics::zoom_pct() > base);
        view.zoom_delta(-1, cx);
        assert_eq!(crate::theme::metrics::zoom_pct(), base);

        view.zoom_delta(1, cx);
        view.zoom_delta(0, cx);
        assert_eq!(crate::theme::metrics::zoom_pct(), base, "0 is actual size");
    });
}

/// The change bubble is resized by dragging its top edge -- upward makes it
/// taller, which is the direction that is not obvious from the delta.
#[gpui::test]
fn dragging_the_bubble_edge_upward_makes_it_taller(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update_in(cx, |view, window, cx| {
        let before = view.change_bubble_height;
        view.begin_change_bubble_drag(gpui::px(400.), cx);
        view.drag_change_bubble(gpui::px(340.), window, cx);
        assert!(view.change_bubble_height > before, "up is bigger");

        view.drag_change_bubble(gpui::px(460.), window, cx);
        assert!(view.change_bubble_height < before, "and down is smaller");

        // Past the stop it clamps rather than collapsing to nothing.
        view.drag_change_bubble(gpui::px(9_000.), window, cx);
        assert!(view.change_bubble_height > gpui::px(0.));
        view.end_change_bubble_drag(cx);
        assert!(view.change_bubble_drag.is_none());
    });
}

/// The editor's handle is its *bottom* edge, so the delta runs the other way.
#[gpui::test]
fn dragging_the_editor_edge_downward_makes_it_taller(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update_in(cx, |view, window, cx| {
        let before = view.editor_height;
        view.begin_editor_drag(gpui::px(300.), cx);
        view.drag_editor(gpui::px(360.), window, cx);
        assert!(view.editor_height > before, "down is bigger");

        view.drag_editor(gpui::px(0.), window, cx);
        assert!(view.editor_height > gpui::px(0.), "and it clamps");
        view.end_editor_drag(cx);
        assert!(view.editor_drag.is_none());
    });
}

/// A drag that never moves changes nothing.
#[gpui::test]
fn a_drag_that_goes_nowhere_leaves_the_size_alone(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        let bubble = view.change_bubble_height;
        let editor = view.editor_height;
        view.begin_change_bubble_drag(gpui::px(400.), cx);
        view.end_change_bubble_drag(cx);
        view.begin_editor_drag(gpui::px(300.), cx);
        view.end_editor_drag(cx);
        assert_eq!(view.change_bubble_height, bubble);
        assert_eq!(view.editor_height, editor);
    });
}

/// The left rail's handle is its right edge, so rightward is wider.
#[gpui::test]
fn dragging_the_left_rail_rightward_widens_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        let before = view.sidebar_width;
        view.begin_sidebar_drag(gpui::px(258.), cx);
        view.drag_sidebar(gpui::px(330.), cx);
        assert!(view.sidebar_width > before, "right is wider");

        view.drag_sidebar(gpui::px(180.), cx);
        assert!(view.sidebar_width < before, "and left is narrower");

        // Dragging it into the window edge leaves a rail, not nothing.
        view.drag_sidebar(gpui::px(-9_000.), cx);
        assert!(view.sidebar_width > 0., "it clamps rather than closing");
        view.end_sidebar_drag(cx);
        assert!(view.sidebar_drag.is_none());
    });
}

/// The detail panel is dragged by its *left* edge, so the delta runs the
/// other way -- the half of this that is not obvious from the pointer.
#[gpui::test]
fn dragging_the_detail_panel_leftward_widens_it(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        let before = view.detail_width;
        view.begin_detail_drag(gpui::px(900.), cx);
        view.drag_detail(gpui::px(820.), cx);
        assert!(view.detail_width > before, "left is wider");

        view.drag_detail(gpui::px(960.), cx);
        assert!(view.detail_width < before, "and right is narrower");

        view.drag_detail(gpui::px(9_000.), cx);
        assert!(view.detail_width > 0., "it clamps rather than closing");
        view.end_detail_drag(cx);
        assert!(view.detail_drag.is_none());
    });
}

/// A rail grabbed and released without moving keeps the width it had.
#[gpui::test]
fn a_rail_drag_that_goes_nowhere_leaves_the_width_alone(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    view.update(cx, |view, cx| {
        let sidebar = view.sidebar_width;
        let detail = view.detail_width;
        view.begin_sidebar_drag(gpui::px(258.), cx);
        view.end_sidebar_drag(cx);
        view.begin_detail_drag(gpui::px(900.), cx);
        view.end_detail_drag(cx);
        assert_eq!(view.sidebar_width, sidebar);
        assert_eq!(view.detail_width, detail);
    });
}

/// A field folded down to a scrolling box stays folded as the selection moves.
///
/// Nothing is in the set until the user puts it there -- a field is full height
/// by default. The set is keyed by column name for exactly this: selecting the
/// next row rebuilds every editor, and a column that sprang back open on each
/// arrow key would be a control that does not hold.
#[gpui::test]
fn a_folded_field_survives_moving_to_the_next_row(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("down");
    view.update(cx, |view, cx| {
        assert!(
            view.detail_collapsed.is_empty(),
            "a field is full height until it is folded"
        );
        view.toggle_detail_collapsed("name", cx);
        assert!(view.detail_collapsed.contains("name"));
    });

    cx.simulate_keystrokes("down");
    view.update(cx, |view, cx| {
        assert!(
            view.detail_collapsed.contains("name"),
            "the next row shows the same column at the same height"
        );
        view.toggle_detail_collapsed("name", cx);
        assert!(
            !view.detail_collapsed.contains("name"),
            "and it opens back up"
        );
    });
}

#[gpui::test]
fn the_change_bubble_expands_and_collapses(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let expanded = |view: &DbUi| match view.tabs.active() {
            Some(WorkspaceTab::Table {
                change_bubble_expanded,
                ..
            }) => *change_bubble_expanded,
            _ => panic!("table tab"),
        };
        assert!(!expanded(view));
        view.toggle_change_bubble(cx);
        assert!(expanded(view));
        view.toggle_change_bubble(cx);
        assert!(!expanded(view));
    });
}

/// The write-token dropdown opens on the field it was clicked on, and a second
/// click on the same one closes it rather than reopening it.
#[gpui::test]
fn the_value_menu_opens_and_closes_on_the_same_field(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);

        view.toggle_detail_value_menu(1, cx);
        assert_eq!(view.detail_value_menu, Some(1));
        view.toggle_detail_value_menu(1, cx);
        assert_eq!(view.detail_value_menu, None, "the same field closes it");

        view.toggle_detail_value_menu(1, cx);
        view.close_detail_value_menu(cx);
        assert_eq!(view.detail_value_menu, None);
    });

    // And escape closes it before it closes anything else.
    view.update(cx, |view, cx| view.toggle_detail_value_menu(1, cx));
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert_eq!(view.detail_value_menu, None));
}

/// The Test button validates before it dials, so a half-filled sheet says what
/// is missing instead of timing out against nothing.
#[gpui::test]
fn testing_an_invalid_connection_reports_without_dialling(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    // A fresh sheet is pre-filled and would be valid, so the name is emptied
    // first -- which is also what keeps this test from dialling anything.
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes(&clear_field());

    view.update(cx, |view, cx| {
        view.test_connection(cx);

        let form = view.modal.as_ref().expect("still open");
        assert!(!form.testing, "it never started dialling");
        assert!(form.has_problem(), "and it says what is missing instead");
    });
}

/// Refreshing the catalog with nothing connected is a no-op, not an error
/// about a driver that was never there.
#[gpui::test]
fn refreshing_the_catalog_unconnected_is_a_no_op(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        view.refresh_catalog(cx);
        assert!(!matches!(view.status, Status::Busy(_)));
        assert!(!matches!(view.status, Status::Error(_)));
    });
}

// -- SQL autocomplete -----------------------------------------------------

/// Give a connection a catalog without dialling anything, so the surfaces that
/// read one -- autocomplete, the tree, the go-to palette -- can be tested.
fn with_catalog<'a>(
    cx: &'a mut TestAppContext,
    tables: &[(&str, &[&str])],
) -> (Entity<DbUi>, &'a mut VisualTestContext) {
    use dbui_app::domain::{Catalog, Schema, Table, TableKind};

    let (view, cx) = open_with(cx, saved_connections(1));
    let catalog = Catalog {
        schemas: vec![Schema {
            name: "public".into(),
            tables: tables
                .iter()
                .map(|(name, _)| Table {
                    schema: "public".into(),
                    name: (*name).to_string(),
                    kind: TableKind::Table,
                })
                .collect(),
        }],
    };
    view.update(cx, |view, cx| {
        let id = view.workspace.entries()[0].id();
        if let Some(entry) = view.workspace.get_mut(id) {
            entry.catalog = Some(catalog);
        }
        // The column cache is what autocomplete reads for column names; it is
        // normally filled by a fetch, which needs no connection to fake.
        for (table, columns) in tables {
            view.column_cache.insert(
                ("public".to_string(), (*table).to_string()),
                columns
                    .iter()
                    .map(|name| dbui_app::domain::Column {
                        name: (*name).to_string(),
                        data_type: "text".into(),
                        nullable: true,
                        default: None,
                        is_primary_key: false,
                        ordinal: 0,
                        references: None,
                    })
                    .collect(),
            );
        }
        cx.notify();
    });
    (view, cx)
}

#[gpui::test]
fn autocomplete_offers_tables_after_from(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id", "email"]), ("orders", &["id"])]);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select * from us");
        view.trigger_completion(cx);

        let popup = view.completion.as_ref().expect("a popup");
        assert!(
            popup.items.iter().any(|item| item.label.contains("users")),
            "got: {:?}",
            popup.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    });
}

/// Accepting replaces the partial word rather than appending to it.
#[gpui::test]
fn accepting_a_completion_replaces_what_was_typed(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id"])]);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select * from us");
        view.trigger_completion(cx);
        assert!(view.completion.is_some());

        view.accept_completion(cx);
        assert!(view.completion.is_none(), "and the popup closes");
        let text = sql_editor_text(view);
        assert!(text.starts_with("select * from "), "got: {text}");
        assert!(
            !text.contains("from us "),
            "the partial word is gone: {text}"
        );
    });
}

#[gpui::test]
fn escape_dismisses_the_completion_popup(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id"])]);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select * from us");
        view.trigger_completion(cx);
        view.focus = Focus::Editor;
        cx.notify();
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert!(view.completion.is_none()));
}

/// A popup with nothing in it is not shown at all.
#[gpui::test]
fn a_completion_with_no_matches_does_not_open(cx: &mut TestAppContext) {
    let (view, cx) = with_catalog(cx, &[("users", &["id"])]);

    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select * from zzzz");
        view.trigger_completion(cx);
        assert!(view.completion.is_none());
    });
}

// -- the filter strip and page size ---------------------------------------

/// Applying a filter puts the draft into the applied clause and restarts the
/// paging: an offset into the unfiltered rows means nothing once they change.
#[gpui::test]
fn applying_a_filter_restarts_the_paging(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 5);

    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table {
            where_draft, page, ..
        }) = view.tabs.active_mut()
        {
            *where_draft = crate::text_input::TextInput::with_text("id > 2", false);
            page.offset = 500;
        }
        view.apply_filters(cx);

        let Some(WorkspaceTab::Table {
            where_clause, page, ..
        }) = view.tabs.active()
        else {
            panic!("table tab");
        };
        assert_eq!(where_clause, "id > 2");
        assert_eq!(page.offset, 0);
    });
}

#[gpui::test]
fn clearing_a_filter_empties_both_the_draft_and_the_clause(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table { where_draft, .. }) = view.tabs.active_mut() {
            *where_draft = crate::text_input::TextInput::with_text("id > 2", false);
        }
        view.apply_filters(cx);
        view.clear_filters(cx);

        let Some(WorkspaceTab::Table {
            where_clause,
            where_draft,
            ..
        }) = view.tabs.active()
        else {
            panic!("table tab");
        };
        assert!(where_clause.is_empty());
        assert!(where_draft.text().is_empty(), "the box is emptied too");
    });
}

/// A page size that is not a number is refused with a message, and the box is
/// put back to the size actually in use.
#[gpui::test]
fn a_bad_page_size_is_refused_and_reset(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table {
            page_size_draft, ..
        }) = view.tabs.active_mut()
        {
            *page_size_draft = crate::text_input::TextInput::with_text("lots", false);
        }
        view.apply_page_size(cx);

        assert!(
            describe(&view.status).contains("must be a number"),
            "got: {}",
            describe(&view.status)
        );
        let Some(WorkspaceTab::Table {
            page_size_draft,
            page,
            ..
        }) = view.tabs.active()
        else {
            panic!("table tab");
        };
        assert_eq!(page_size_draft.text(), page.limit.to_string());
    });
}

/// An absurd page size is clamped rather than sent to the server.
#[gpui::test]
fn a_huge_page_size_is_clamped(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table {
            page_size_draft, ..
        }) = view.tabs.active_mut()
        {
            *page_size_draft = crate::text_input::TextInput::with_text("999999", false);
        }
        view.apply_page_size(cx);

        let Some(WorkspaceTab::Table { page, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert_eq!(page.limit, 5_000);
        assert_eq!(page.offset, 0, "and a new size restarts the paging");
    });
}

// -- gaps found by auditing which commands no test drove ------------------

/// The palette's "Clear Sort" is a different path from clicking the header a
/// third time, and had no test of its own.
#[gpui::test]
fn clearing_the_sort_returns_to_key_order(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.toggle_sort("name", cx);
        assert!(view.active_sort().is_some());

        if let Some(WorkspaceTab::Table { page, .. }) = view.tabs.active_mut() {
            page.offset = 500;
        }
        view.clear_sort(cx);

        assert!(view.active_sort().is_none());
        let Some(WorkspaceTab::Table { page, .. }) = view.tabs.active() else {
            panic!("table tab");
        };
        assert_eq!(page.offset, 0, "and the paging restarts");
    });
}

/// Clicking a staged row in the grid reopens it for editing rather than
/// selecting a stored row that is not there.
#[gpui::test]
fn clicking_a_staged_row_reopens_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        view.add_row(cx);
        assert_eq!(view.tabs.active().unwrap().editing_insert(), Some(1));

        view.edit_insert(0, cx);
        assert_eq!(view.tabs.active().unwrap().editing_insert(), Some(0));
        assert!(
            view.tabs.active().unwrap().selection().is_empty(),
            "a new row is not one of the stored ones"
        );
    });
}

/// Removing a staged row renumbers the one the sidebar is editing, or the
/// sidebar ends up showing a different row than the one that was open.
#[gpui::test]
fn removing_a_staged_row_keeps_the_open_one_open(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.add_row(cx);
        view.add_row(cx);
        view.add_row(cx);
        view.edit_insert(2, cx);

        // Drop the first: the one being edited slides down to index 1.
        view.remove_insert(0, cx);
        assert_eq!(view.staged_insert_count(), 2);
        assert_eq!(view.tabs.active().unwrap().editing_insert(), Some(1));

        // Dropping the open one closes the sidebar on it.
        view.remove_insert(1, cx);
        assert_eq!(view.tabs.active().unwrap().editing_insert(), None);
    });
}

/// ↓ from the filter box steps into the tree; ↵ opens the first match.
#[gpui::test]
fn the_filter_box_hands_off_to_the_tree(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        // Nothing is connected, so the tree is empty and both are no-ops
        // rather than panics -- which is the property worth pinning.
        view.focus_sidebar_search(cx);
        view.enter_filtered_tree(cx);
        assert!(view.sidebar_cursor.is_none());

        let before = view.tabs.items.len();
        view.open_first_filtered_table(cx);
        assert_eq!(view.tabs.items.len(), before);
    });
}

/// The write tokens, including the MIXED one that only a bulk edit offers.
#[gpui::test]
fn the_value_menu_writes_its_token_into_the_field(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        view.set_detail_special_value(1, "NULL", cx);
        assert_eq!(draft_texts(view)[1], "NULL");

        // Every selected row is set to NULL, not just the lead one.
        view.clear_row_selection(cx);
        let batch = view.collect_batch_edits();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].changes[0].new_value, dbui_app::domain::Value::Null);
    });

    // And MIXED puts it back to leaving each row alone.
    cx.simulate_keystrokes("cmd-a");
    view.update(cx, |view, cx| {
        view.set_detail_special_value(1, crate::tabs::MIXED, cx);
        assert_eq!(draft_texts(view)[1], crate::tabs::MIXED);
        view.clear_row_selection(cx);
        assert!(view.collect_batch_edits().is_empty());
    });
}

/// The two MIXEDs mean different things, and the difference matters: the one
/// a draft *shows* because rows disagree has no opinion and keeps what is
/// staged; the one picked from the menu was asked for, and reverts.
#[gpui::test]
fn a_shown_mixed_keeps_staged_edits_but_a_chosen_one_reverts(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    // Stage a different name on two rows, so the column reads MIXED on its own.
    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        type_into_draft(view, 1, "first");
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        type_into_draft(view, 1, "second");
        view.clear_row_selection(cx);
        assert_eq!(view.collect_batch_edits().len(), 2);
    });

    // Selecting both shows MIXED -- and that must not drop either edit.
    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(1, None, gpui::Modifiers::shift(), cx);
        assert_eq!(draft_texts(view)[1], crate::tabs::MIXED);
        view.clear_row_selection(cx);
        assert_eq!(
            view.collect_batch_edits().len(),
            2,
            "merely looking at them changes nothing"
        );
    });

    // Choosing MIXED from the menu is the way back out.
    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(1, None, gpui::Modifiers::shift(), cx);
        view.set_detail_special_value(1, crate::tabs::MIXED, cx);
        view.clear_row_selection(cx);
        assert!(
            view.collect_batch_edits().is_empty(),
            "picking it undoes what was staged"
        );
    });
}

/// A primary key is not editable through the value menu either.
#[gpui::test]
fn the_value_menu_refuses_a_key_column(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        let before = draft_texts(view)[0].clone();
        view.set_detail_special_value(0, "NULL", cx);
        assert_eq!(draft_texts(view)[0], before, "the key is untouched");
    });
}

// -- duplicate, copy, paste -----------------------------------------------

/// ⌘D stages a copy of each selected row. The key is left for the table to
/// generate, so the copy gets its own identity rather than clashing.
#[gpui::test]
fn cmd_d_duplicates_the_selected_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(2, None, gpui::Modifiers::shift(), cx);
    });
    cx.simulate_keystrokes("cmd-d");

    view.update(cx, |view, _| {
        assert_eq!(view.staged_insert_count(), 2, "one copy per selected row");

        let staged = view.collect_batch_inserts().expect("parses");
        // `id` is a lone integer key: left out so the sequence fires.
        for row in &staged {
            assert!(
                !row.values.iter().any(|(column, _)| column == "id"),
                "the generated key is left for the table"
            );
        }
        assert_eq!(
            staged[0].values[0],
            (
                "name".to_string(),
                dbui_app::domain::Value::Text("row 2".into())
            ),
            "and the rest of the row is copied"
        );
    });
}

/// A key the table cannot generate is copied instead, so the clash is visible
/// in the sidebar rather than only at commit time.
#[gpui::test]
fn duplicating_copies_a_key_the_table_cannot_generate(cx: &mut TestAppContext) {
    use dbui_app::domain::{Column, ColumnInfo, Value};

    let structure = vec![Column {
        name: "slug".into(),
        data_type: "text".into(),
        nullable: false,
        default: None,
        is_primary_key: true,
        ordinal: 1,
        references: None,
    }];
    let columns = vec![ColumnInfo {
        name: "slug".into(),
        type_name: "text".into(),
    }];

    let row = crate::tabs::PendingRowInsert::duplicating(
        &columns,
        &structure,
        &[Value::Text("alpha".into())],
    );
    assert_eq!(
        row.to_values().expect("parses"),
        vec![("slug".to_string(), Value::Text("alpha".into()))],
        "a natural key is copied, not dropped"
    );
    let _ = cx;
}

/// ⌘V is the exact inverse of ⌘C: rows copied out come back in as new ones.
#[gpui::test]
fn copied_rows_paste_back_as_new_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        view.grid_pointer_down(1, None, gpui::Modifiers::shift(), cx);
    });
    cx.simulate_keystrokes("cmd-c");
    cx.simulate_keystrokes("cmd-v");

    view.update(cx, |view, _| {
        assert_eq!(view.staged_insert_count(), 2);
        let staged = view.collect_batch_inserts().expect("parses");
        // Paste is faithful: it writes back exactly what was copied, key and
        // all. ⌘D is the one that knows to leave the key out.
        let names: Vec<String> = staged
            .iter()
            .flat_map(|row| row.values.iter())
            .filter(|(column, _)| column == "name")
            .map(|(_, value)| value.to_text())
            .collect();
        assert_eq!(names, vec!["row 1", "row 2"]);
    });
}

/// Text that is not a table is refused rather than staged as nonsense.
#[gpui::test]
fn pasting_something_that_is_not_a_table_says_so(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("just some notes".into()));
    });
    view.update(cx, |view, cx| {
        view.paste_rows(cx);
        assert_eq!(view.staged_insert_count(), 0);
        assert!(
            describe(&view.status).contains("does not hold rows"),
            "got: {}",
            describe(&view.status)
        );
    });
}

/// Columns the table does not have are ignored rather than refused: pasting
/// three of five columns is a reasonable thing to want.
#[gpui::test]
fn pasting_ignores_columns_this_table_does_not_have(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            "name	elsewhere
Katherine	ignored
"
            .into(),
        ));
    });
    view.update(cx, |view, cx| {
        view.paste_rows(cx);
        let staged = view.collect_batch_inserts().expect("parses");
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0].values.len(), 1, "only the column that matched");
        assert_eq!(staged[0].values[0].0, "name");
        assert!(
            describe(&view.status).contains("ignored"),
            "and it says one was dropped: {}",
            describe(&view.status)
        );
    });
}

/// None of the names matching is a paste into the wrong table, and worth
/// refusing outright.
#[gpui::test]
fn pasting_rows_from_an_unrelated_table_is_refused(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    cx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
            "alpha	beta
1	2
"
            .into(),
        ));
    });
    view.update(cx, |view, cx| {
        view.paste_rows(cx);
        assert_eq!(view.staged_insert_count(), 0);
        assert!(
            describe(&view.status).contains("column names"),
            "got: {}",
            describe(&view.status)
        );
    });
}

/// ⌘C and ⌘V inside an editor are still the text operations they always were.
#[gpui::test]
fn copy_and_paste_in_the_editor_are_still_text(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);
    view.update(cx, |view, cx| {
        open_sql_editor(view, cx);
        set_sql_editor_text(view, "select 1");
        view.focus = Focus::Editor;
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-c");
    cx.simulate_keystrokes("cmd-v");

    view.update(cx, |view, _| {
        assert_eq!(
            sql_editor_text(view),
            "select 1",
            "the editor's own copy and paste ran, not the grid's"
        );
        assert_eq!(view.staged_insert_count(), 0);
    });
}

// -- foreign keys ---------------------------------------------------------

/// A cell on a foreign key knows where it points; one that is not does not.
#[gpui::test]
fn a_foreign_key_cell_knows_its_target(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, _| {
        let (key, value) = view.foreign_key_at(1, 1).expect("name references teams");
        assert_eq!(key.references.qualified(), "public.teams");
        assert_eq!(key.references_column, "slug");
        assert_eq!(value.to_text(), "row 2");

        assert!(
            view.foreign_key_at(1, 0).is_none(),
            "the key column points nowhere"
        );
    });
}

/// Following opens the referenced table filtered to the one row, because the
/// only thing known about it is its key -- not where it sits in the table.
#[gpui::test]
fn following_a_key_opens_the_target_filtered_to_that_row(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.follow_foreign_key(1, 1, cx);

        let Some(WorkspaceTab::Table {
            table,
            where_clause,
            filters_open,
            ..
        }) = view.tabs.active()
        else {
            panic!("a table tab");
        };
        assert_eq!(table.qualified(), "public.teams");
        assert_eq!(where_clause, "\"slug\" = 'row 2'");
        assert!(
            *filters_open,
            "and the filter is shown, not applied invisibly"
        );
    });
}

/// A null reference points at nothing, so there is no row to open.
#[gpui::test]
fn a_null_foreign_key_is_not_followed(cx: &mut TestAppContext) {
    use dbui_app::domain::{Row, Value};

    let (view, cx) = open_table_with_rows(cx, 2);
    view.update(cx, |view, cx| {
        if let Some(WorkspaceTab::Table {
            result: Some(view), ..
        }) = view.tabs.active_mut()
        {
            view.set.rows[0] = Row(vec![Value::Int(1), Value::Null]);
        }
        assert!(view.foreign_key_at(0, 1).is_none());

        let before = view.tabs.items.len();
        view.follow_foreign_key(0, 1, cx);
        assert_eq!(view.tabs.items.len(), before, "nothing was opened");
    });
}

// -- editing in the grid, resizing, read-only -----------------------------

/// Enter on a selected cell opens it in place, and what is typed there is
/// staged by the same path a sidebar edit takes.
#[gpui::test]
fn editing_a_cell_in_place_stages_the_change(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, Some(1), gpui::Modifiers::default(), cx);
        view.begin_cell_edit(1, 1, cx);
        assert_eq!(view.editing_cell, Some((1, 1)));
        assert_eq!(view.cell_editor.text(), "row 2", "seeded from the cell");

        view.cell_editor.set_text("renamed");
        view.commit_cell_edit(cx);
        assert!(view.editing_cell.is_none());

        let batch = view.collect_batch_edits();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].changes[0].column, "name");
        assert_eq!(batch[0].changes[0].new_text, "renamed");
    });
}

/// A cell the user has edited shows what it will become, not what the server
/// last said -- otherwise typing into a cell looks like it did nothing.
#[gpui::test]
fn a_staged_edit_is_what_the_grid_shows(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("renamed");
        view.commit_cell_edit(cx);

        let batch = view.collect_batch_edits();
        let tab = view.tabs.active().expect("tab");
        let staged = tab
            .staged_edit_for_row(1, &batch)
            .expect("row 1 has a staged edit");
        assert_eq!(staged.changes[0].new_text, "renamed");

        assert!(
            tab.staged_edit_for_row(0, &batch).is_none(),
            "and the rows around it are untouched"
        );
    });
}

/// Clicking another cell commits the one being edited. Leaving the box open
/// over a row the user has moved on from is how an edit gets lost.
#[gpui::test]
fn clicking_away_commits_the_open_cell_editor(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("renamed");

        // A click on a different row.
        view.grid_pointer_down(3, Some(1), gpui::Modifiers::default(), cx);
        assert!(view.editing_cell.is_none(), "the editor closed");

        let batch = view.collect_batch_edits();
        assert_eq!(batch.len(), 1, "and what was typed was kept");
        assert_eq!(batch[0].changes[0].new_text, "renamed");
    });
}

/// A press anywhere in the window closes it -- not only on another cell. The
/// handlers on the other surfaces cover the ones that *have* handlers; this is
/// what covers the tab bar, the empty space beside the grid, and everything
/// else that does not.
#[gpui::test]
fn a_press_anywhere_else_commits_the_cell_editor(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("renamed");
    });
    cx.run_until_parked();

    // Far from the grid: the tab bar at the top of the window.
    cx.simulate_click(
        gpui::point(gpui::px(400.), gpui::px(30.)),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert!(view.editing_cell.is_none(), "the editor closed");
        assert_eq!(
            view.collect_batch_edits()[0].changes[0].new_text,
            "renamed",
            "and kept what was typed"
        );
    });
}

/// Clicking the *same* cell keeps editing it, rather than closing and
/// reopening under the pointer.
#[gpui::test]
fn clicking_the_cell_being_edited_keeps_it_open(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.grid_pointer_down(0, Some(1), gpui::Modifiers::default(), cx);
        assert_eq!(view.editing_cell, Some((0, 1)));
    });
}

/// Moving the keyboard into any other box closes it too, or there are two
/// editors on screen at once.
#[gpui::test]
fn focusing_another_field_commits_the_cell_editor(cx: &mut TestAppContext) {
    use crate::components::text_field::InputTarget;

    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("via the sidebar");
        view.focus_input(InputTarget::DetailField(1), cx);

        assert!(view.editing_cell.is_none());
        assert_eq!(view.collect_batch_edits().len(), 1);
    });
}

/// And so does clicking a table in the tree.
#[gpui::test]
fn clicking_the_tree_commits_the_cell_editor(cx: &mut TestAppContext) {
    use crate::root::SidebarItem;

    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("typed");
        let connection = view.workspace.entries()[0].id();
        view.set_sidebar_cursor(
            SidebarItem::Table {
                connection,
                table: TableRef::new("public", "teams"),
            },
            cx,
        );
        assert!(view.editing_cell.is_none());
    });
}

/// ⌥-click opens what a cell points at. A plain click cannot: a foreign-key
/// column is still an editable column.
#[gpui::test]
fn alt_click_follows_a_foreign_key_and_a_plain_click_does_not(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        let before = view.tabs.items.len();
        view.grid_pointer_down(1, Some(1), gpui::Modifiers::default(), cx);
        assert_eq!(view.tabs.items.len(), before, "a plain click selects");
        assert_eq!(view.selected_cell, Some((1, 1)));

        view.grid_pointer_down(1, Some(1), gpui::Modifiers::alt(), cx);
        assert_eq!(
            view.tabs
                .active()
                .and_then(|tab| tab.table_ref())
                .map(|t| t.qualified()),
            Some("public.teams".to_string()),
            "and ⌥ opens it"
        );
    });
}

/// ⌥-click on a column that points nowhere is just a click.
#[gpui::test]
fn alt_click_on_an_ordinary_cell_selects_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        let before = view.tabs.items.len();
        view.grid_pointer_down(2, Some(0), gpui::Modifiers::alt(), cx);
        assert_eq!(view.tabs.items.len(), before);
        assert_eq!(view.selected_cell, Some((2, 0)));
    });
}

/// Escape throws the cell edit away without staging anything.
#[gpui::test]
fn cancelling_a_cell_edit_stages_nothing(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("discarded");
        view.cancel_cell_edit(cx);
        assert!(view.collect_batch_edits().is_empty());
    });
}

/// The primary key is the row's identity; editing it in place would be
/// rewriting which row is being talked about.
#[gpui::test]
fn a_key_cell_cannot_be_edited_in_place(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 0, cx);
        assert!(view.editing_cell.is_none());
        assert!(
            describe(&view.status).contains("primary key"),
            "got: {}",
            describe(&view.status)
        );
    });
}

/// A key cell cannot be opened, but the gesture still answers: the value is
/// on the clipboard.
#[gpui::test]
fn double_clicking_a_key_cell_copies_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 0, cx);
        assert!(view.editing_cell.is_none());
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("1".to_string())
        );
    });
}

/// A JSON document is too tall for a one-line box, so double-clicking one
/// copies it and hands the sidebar the whole value, selected and ready for a
/// second ⌘C -- rather than saying "not here" and leaving it unreachable.
#[gpui::test]
fn double_clicking_a_multi_line_cell_copies_it_and_opens_the_sidebar(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);
    let document = "{\n  \"a\": 1,\n  \"b\": 2\n}";

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        type_into_draft(view, 1, document);

        view.begin_cell_edit(0, 1, cx);
        assert!(view.editing_cell.is_none(), "no one-line box over it");
        assert_eq!(
            view.detail_input,
            Some(crate::components::DetailInput::Field(1))
        );
        assert!(
            describe(&view.status).contains("multi-line"),
            "got: {}",
            describe(&view.status)
        );
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(document.to_string()),
            "the whole document, newlines and all"
        );
    });

    view.update(cx, |view, cx| {
        assert_eq!(
            view.copied_cell,
            Some((0, 1)),
            "and the cell that was clicked says so, since the status bar is at \
             the far bottom of the window"
        );

        // The mark belongs to the row it was taken from.
        view.grid_pointer_down(1, None, gpui::Modifiers::default(), cx);
        assert_eq!(view.copied_cell, None);
    });
}

/// Sending someone to "the sidebar" is only an instruction if the sidebar
/// then shows the field. The position handed to the scroll handle counts the
/// search box above the fields, and the search filter can hide some of them.
#[gpui::test]
fn revealing_a_detail_field_counts_the_chrome_above_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        // Nothing here can be asserted about pixels -- the point is that both
        // fields resolve to a position rather than being dropped, including
        // while a filter is hiding the one above.
        view.reveal_detail_field(0);
        view.reveal_detail_field(1);

        type_into_field_search(view, "name");
        view.reveal_detail_field(1);
        view.reveal_detail_field(0);
    });
}

/// ⌘C over a key cell copies it like any other. A key cannot be *edited*,
/// which is a different thing from cannot be read.
#[gpui::test]
fn cmd_c_copies_a_key_cell(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(1, Some(0), gpui::Modifiers::default(), cx);
    });
    cx.simulate_keystrokes("cmd-c");

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("2".to_string())
        );
    });
}

/// The key's sidebar field is drawn, not edited, so no caret can be put in
/// it -- clicking it copies instead, or the key is the one value on the row
/// nobody can take anywhere.
#[gpui::test]
fn the_key_field_in_the_sidebar_copies_itself(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.grid_pointer_down(2, None, gpui::Modifiers::default(), cx);
        view.copy_detail_field(0, cx);
        assert_eq!(describe(&view.status), "info: Copied id");
        assert_eq!(
            view.copied_field,
            Some(0),
            "and the button that was pressed says so, since the status bar may \
             be far below a scrolled sidebar"
        );

        // Moving to another row drops the mark: it belongs to the row it was
        // taken from.
        view.grid_pointer_down(0, None, gpui::Modifiers::default(), cx);
        assert_eq!(view.copied_field, None);
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("3".to_string())
        );
    });
}

/// Tab commits and steps to the next editable column, skipping the key.
#[gpui::test]
fn tab_moves_to_the_next_editable_cell(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("first");
        view.commit_cell_and_advance(false, cx);

        // `id` is the key and `name` is where we started, so it wraps back.
        assert_eq!(view.editing_cell, Some((0, 1)));
        assert_eq!(view.collect_batch_edits()[0].changes[0].new_text, "first");
    });
}

// -- closing something that is holding staged work --------------------------

/// Stage one edit on the open table tab.
fn stage_one_edit(view: &mut DbUi, cx: &mut gpui::Context<DbUi>) {
    view.begin_cell_edit(0, 1, cx);
    view.cell_editor.set_text("edited");
    view.finish_cell_edit(cx);
    assert_eq!(
        view.collect_batch_edits().len(),
        1,
        "the fixture has to actually stage something"
    );
}

/// The bug: the × and ⌘W threw a staged batch away without a word.
#[gpui::test]
fn closing_a_tab_holding_changes_asks_first(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        let before = view.tabs.items.len();

        view.close_tab(0, cx);

        assert_eq!(view.tabs.items.len(), before, "the tab is still open");
        let guard = view.close_guard.as_ref().expect("it asked");
        assert_eq!(guard.changes, 1, "and said how much is at stake");
    });
}

/// Answering "keep open" leaves the tab and the batch exactly as they were.
#[gpui::test]
fn keeping_a_tab_open_keeps_its_batch(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_tab(0, cx);
        view.cancel_close(cx);

        assert!(view.close_guard.is_none());
        assert_eq!(view.tabs.items.len(), 1);
        assert_eq!(view.collect_batch_edits().len(), 1, "nothing was discarded");
    });
}

/// Answering "discard" goes through with the close it was guarding.
#[gpui::test]
fn discarding_closes_the_tab_it_was_asked_about(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_tab(0, cx);
        view.confirm_close(cx);

        assert!(view.close_guard.is_none());
        assert!(view.tabs.items.is_empty(), "the tab is gone");
    });
}

/// A tab with nothing staged still closes on the first press: the guard must
/// not become a speed bump in front of every ⌘W.
#[gpui::test]
fn closing_a_clean_tab_does_not_ask(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.close_tab(0, cx);
        assert!(view.close_guard.is_none());
        assert!(view.tabs.items.is_empty());
    });
}

/// A value typed into the detail sidebar and never committed is work too --
/// it reaches the staged batch before the count is taken.
#[gpui::test]
fn an_uncommitted_draft_still_counts_as_a_change(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.select_row(0, cx);
        let Some(WorkspaceTab::Table {
            draft: Some(draft), ..
        }) = view.tabs.active_mut()
        else {
            panic!("selecting a row opens a draft");
        };
        draft.fields[1].1.set_text("typed, never committed");

        view.close_tab(0, cx);
        assert!(
            view.close_guard.is_some(),
            "it must not be closed out from under the draft"
        );
    });
}

/// Dropping the table takes its tabs with it and does not offer to keep a
/// batch that has nothing left to be committed against.
#[gpui::test]
fn dropping_a_table_closes_its_tabs_without_asking(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_tabs_for_table(&TableRef::new("public", "users"), cx);

        assert!(view.close_guard.is_none());
        assert!(view.tabs.items.is_empty());
    });
}

/// ⌘⇧W closes every tab under the connection at once, so it counts them all.
#[gpui::test]
fn closing_a_connection_holding_changes_asks_first(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);
    let id = view.update(cx, |view, _| view.workspace.active_id().expect("connected"));

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_connection_tab(id, cx);

        assert_eq!(view.workspace.open_count(), 1, "still open");
        let guard = view.close_guard.as_ref().expect("it asked");
        assert_eq!(guard.changes, 1);

        view.confirm_close(cx);
        assert_eq!(view.workspace.open_count(), 0);
    });
}

// -- dragging a tab along the strip -----------------------------------------

/// Dragging one tab across another puts it in that tab's slot, and the tab in
/// hand is the one left in front.
#[gpui::test]
fn dragging_a_tab_moves_it_to_the_slot_it_was_dropped_on(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        // Grab the first tab and pull it to the right, across both others.
        view.begin_tab_drag(ids[0], gpui::px(40.));
        view.drag_tab_over(1, gpui::px(140.), cx);
        view.drag_tab_over(2, gpui::px(260.), cx);
        view.end_tab_drag(cx);

        let order: Vec<_> = view.tabs.items.iter().map(|tab| tab.id()).collect();
        assert_eq!(order, vec![ids[1], ids[2], ids[0]]);
        assert_eq!(view.tabs.active, 2, "the tab that was dragged is in front");
        assert!(view.tab_drag.is_none(), "and the drag is over");
    });
}

/// The active tab is whichever tab was active, not whichever slot it was in.
#[gpui::test]
fn reordering_around_the_active_tab_does_not_move_the_front(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);
        view.activate_tab(1, cx);

        // Drag the last tab to the front, past the active one.
        view.begin_tab_drag(ids[2], gpui::px(260.));
        view.drag_tab_over(1, gpui::px(140.), cx);
        view.drag_tab_over(0, gpui::px(40.), cx);

        assert_eq!(
            view.tabs.active_id(),
            Some(ids[1]),
            "the tab that was in front is still in front"
        );
    });
}

/// A press that never travels is a click. It must not shuffle the strip on the
/// way to activating the tab.
#[gpui::test]
fn a_click_on_a_tab_does_not_reorder_it(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        view.begin_tab_drag(ids[0], gpui::px(40.));
        // A pointer that wobbles a pixel inside the tab it was pressed on.
        view.drag_tab_over(1, gpui::px(42.), cx);
        view.end_tab_drag(cx);

        let order: Vec<_> = view.tabs.items.iter().map(|tab| tab.id()).collect();
        assert_eq!(order, ids, "nothing moved");
    });
}

/// The bug this direction gate is for: a tab dropped into a neighbour's slot
/// leaves the neighbour under the pointer, and without it the two trade places
/// on every mouse-move for as long as the button is held.
#[gpui::test]
fn a_held_pointer_does_not_shuttle_two_tabs_back_and_forth(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        view.begin_tab_drag(ids[0], gpui::px(40.));
        view.drag_tab_over(1, gpui::px(140.), cx);

        // The pointer has not moved since; the neighbour is now at slot 0 and
        // keeps reporting the pointer over it.
        for _ in 0..5 {
            view.drag_tab_over(0, gpui::px(140.), cx);
        }

        let order: Vec<_> = view.tabs.items.iter().map(|tab| tab.id()).collect();
        assert_eq!(
            order,
            vec![ids[1], ids[0], ids[2]],
            "it settled where it was put"
        );
    });
}

/// Dragging a tab onto a slot that is no longer there -- the tab closed under
/// the drag -- ends it rather than moving whatever took its place.
#[gpui::test]
fn a_drag_whose_tab_closed_under_it_stops(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        view.begin_tab_drag(ids[0], gpui::px(40.));
        view.close_tab_now(0, cx);
        view.drag_tab_over(1, gpui::px(200.), cx);

        assert!(view.tab_drag.is_none());
        let order: Vec<_> = view.tabs.items.iter().map(|tab| tab.id()).collect();
        assert_eq!(
            order,
            vec![ids[1], ids[2]],
            "the survivors kept their order"
        );
    });
}

/// A reordered strip is the strip that comes back on the next launch.
#[gpui::test]
fn a_dragged_order_survives_a_restart(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    let order = view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);
        view.begin_tab_drag(ids[2], gpui::px(260.));
        view.drag_tab_over(0, gpui::px(40.), cx);
        view.end_tab_drag(cx);
        view.tabs
            .items
            .iter()
            .map(|tab| tab.label())
            .collect::<Vec<_>>()
    });

    let restored = view.update(cx, |view, _| {
        let (saved, active) = view.tabs.to_saved();
        (saved.len(), active)
    });
    assert_eq!(restored.0, 3);
    assert_eq!(
        order[0], "SQL Query",
        "the SQL tab was dragged to the front: {order:?}"
    );
}

// -- the tab bar's own right-click menu -------------------------------------

/// Three tabs, and the id of each, left to right.
fn open_three_tabs(view: &mut DbUi, cx: &mut gpui::Context<DbUi>) -> Vec<crate::tabs::TabId> {
    view.open_table_tab(TableRef::new("public", "teams"), cx);
    view.open_sql_tab(cx);
    assert_eq!(view.tabs.items.len(), 3, "users, teams and the SQL tab");
    view.tabs.items.iter().map(|tab| tab.id()).collect()
}

/// Right-clicking a tab and picking "Close Others" keeps the tab under the
/// pointer -- not the one that happened to be in front.
#[gpui::test]
fn closing_the_other_tabs_keeps_the_one_the_menu_was_opened_on(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);
        assert_eq!(view.tabs.active, 2, "the SQL tab is in front");

        view.open_context_menu(
            ContextTarget::Tab { id: ids[1] },
            gpui::point(gpui::px(200.), gpui::px(10.)),
            cx,
        );
        view.run_context_action(MenuAction::CloseOtherTabs, cx);

        assert_eq!(view.tabs.items.len(), 1);
        assert_eq!(view.tabs.items[0].id(), ids[1], "the middle tab survived");
        assert_eq!(view.tabs.active, 0, "and it is what the user is looking at");
    });
}

/// "Close to the Right" takes the tail and leaves everything before it.
#[gpui::test]
fn closing_to_the_right_leaves_the_tabs_before_it(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        view.open_context_menu(
            ContextTarget::Tab { id: ids[0] },
            gpui::point(gpui::px(20.), gpui::px(10.)),
            cx,
        );
        view.run_context_action(MenuAction::CloseTabsToRight, cx);

        let left: Vec<_> = view.tabs.items.iter().map(|tab| tab.id()).collect();
        assert_eq!(left, vec![ids[0]]);
    });
}

/// "Close All" empties the strip, whichever tab it was asked from.
#[gpui::test]
fn closing_all_tabs_from_the_menu_empties_the_bar(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);

        view.open_context_menu(
            ContextTarget::Tab { id: ids[1] },
            gpui::point(gpui::px(200.), gpui::px(10.)),
            cx,
        );
        view.run_context_action(MenuAction::CloseAllTabs, cx);

        assert!(view.tabs.items.is_empty());
        assert!(view.workspace.open_table.is_none(), "and nothing is open");
    });
}

/// A bulk close asks once for the whole run, counting the work on every tab
/// it is about to take -- not once per tab, and not silently.
#[gpui::test]
fn a_bulk_close_over_staged_work_asks_once(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        let ids = open_three_tabs(view, cx);

        view.open_context_menu(
            ContextTarget::Tab { id: ids[2] },
            gpui::point(gpui::px(400.), gpui::px(10.)),
            cx,
        );
        view.run_context_action(MenuAction::CloseAllTabs, cx);

        assert_eq!(view.tabs.items.len(), 3, "nothing has closed yet");
        let guard = view.close_guard.as_ref().expect("it asked");
        assert_eq!(guard.changes, 1, "the batch on the first tab is at stake");
        assert_eq!(guard.label, "3 tabs", "and it says how many are going");

        view.confirm_close(cx);
        assert!(view.tabs.items.is_empty());
    });
}

/// Work on the tab being *kept* is not work at risk, so "Close Others" over a
/// clean run does not stop to ask about it.
#[gpui::test]
fn closing_the_others_ignores_the_batch_on_the_tab_it_keeps(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        let ids = open_three_tabs(view, cx);

        view.open_context_menu(
            ContextTarget::Tab { id: ids[0] },
            gpui::point(gpui::px(20.), gpui::px(10.)),
            cx,
        );
        view.run_context_action(MenuAction::CloseOtherTabs, cx);

        assert!(view.close_guard.is_none(), "nothing staged was in the way");
        assert_eq!(view.tabs.items.len(), 1);
        assert_eq!(
            view.collect_batch_edits().len(),
            1,
            "and the batch is still on the tab that was kept"
        );
    });
}

/// The menu names its tab by identity, so a close it can no longer aim is a
/// close that does nothing -- rather than one that empties the bar.
#[gpui::test]
fn a_tab_menu_outliving_its_tab_closes_nothing(cx: &mut TestAppContext) {
    use crate::components::close_guard::TabScope;

    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        let ids = open_three_tabs(view, cx);
        view.close_tab_now(1, cx);

        view.close_tab_scope(TabScope::Others(ids[1]), cx);
        assert_eq!(view.tabs.items.len(), 2, "the survivors are untouched");
    });
}

/// The hole the close guard had: opening a new tab left the typed value in the
/// old tab's draft, where nothing counted it -- so the dot stayed dark and
/// closing that tab discarded the value without asking.
#[gpui::test]
fn opening_a_tab_folds_the_draft_away_first(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("typed, then moved on");
        view.finish_cell_edit(cx);

        // Opening a SQL tab pushes the table tab to the back.
        view.open_sql_tab(cx);

        assert_eq!(
            view.tabs.items[0].pending_change_count(),
            1,
            "the value has to be staged where it can be seen and counted"
        );
    });

    // And closing the tab it belongs to now asks, rather than dropping it.
    view.update(cx, |view, cx| {
        view.close_tab(0, cx);
        assert!(view.close_guard.is_some());
    });
}

/// Same for opening a table tab.
#[gpui::test]
fn opening_a_table_tab_folds_the_draft_away_first(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("typed, then moved on");
        view.finish_cell_edit(cx);
        view.open_table_tab(TableRef::new("public", "teams"), cx);

        assert_eq!(view.tabs.items[0].pending_change_count(), 1);
    });
}

/// ⌘S commits the tab it is pressed on, so pressed on the wrong one it has to
/// say where the work is -- otherwise it reads as the key having failed.
#[gpui::test]
fn commit_on_the_wrong_tab_says_where_the_changes_are(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.open_sql_tab(cx);
        view.save_pending_edits(cx);

        let said = describe(&view.status);
        assert!(said.contains("users"), "it names the tab: {said}");
        assert!(said.contains('1'), "and how much is on it: {said}");
    });
}

/// The guard's own caption points at ⌘S, so ⌘S has to answer it -- and take
/// the panel, with its now-stale count, down with it.
///
/// Against a real database, because a commit that cannot be sent leaves the
/// question standing, which is the other half of the behaviour.
#[gpui::test]
fn committing_from_under_the_guard_dismisses_it(cx: &mut TestAppContext) {
    let (view, cx, _db) = open_connected(cx, "guard-commit");
    view.update(cx, |view, cx| {
        view.open_table_tab(TableRef::new("main", "members"), cx);
    });
    for _ in 0..200 {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }

    view.update(cx, |view, cx| {
        let rows = view
            .tabs
            .active()
            .and_then(|t| t.result())
            .map(|v| v.set.rows.len());
        assert_eq!(rows, Some(2), "the table has to have loaded first");
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("Ada Lovelace");
        view.finish_cell_edit(cx);
        assert_eq!(view.collect_batch_edits().len(), 1, "the edit is staged");
        view.close_tab(view.tabs.active, cx);
        assert!(view.close_guard.is_some(), "it asked");
    });

    cx.simulate_keystrokes("cmd-s");

    view.update(cx, |view, _| {
        assert!(view.close_guard.is_none(), "the question has been answered");
        assert_eq!(view.tabs.items.len(), 1, "and the tab stayed open");
    });
}

/// A commit that never leaves the app leaves the question standing: nothing
/// has been saved, so there is still something to lose.
#[gpui::test]
fn a_commit_that_cannot_be_sent_keeps_the_guard_up(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 2);

    view.update(cx, |view, cx| {
        stage_one_edit(view, cx);
        view.close_tab(0, cx);
    });

    cx.simulate_keystrokes("cmd-s");

    view.update(cx, |view, _| {
        assert!(
            view.close_guard.is_some(),
            "the batch is still staged, so the guard still has a job"
        );
    });
}

/// A dragged width outlives the reload that paging or sorting causes.
#[gpui::test]
fn a_dragged_column_width_survives_a_reload(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| {
        view.begin_column_drag(1, gpui::px(100.), cx);
        view.drag_column(gpui::px(180.), cx);
        view.end_column_drag(cx);

        let widened = view
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .and_then(|v| v.widths.get(1).copied())
            .expect("a width");

        // Rebuild the result the way a reload does, then re-apply.
        let id = view.tabs.active_id().expect("tab id");
        view.reapply_column_widths_for_test(id);
        let after = view
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .and_then(|v| v.widths.get(1).copied())
            .expect("a width");
        assert_eq!(after, widened, "the drag is remembered by column name");
    });
}

/// A read-only connection refuses to commit, and says why.
#[gpui::test]
fn a_read_only_connection_refuses_to_commit(cx: &mut TestAppContext) {
    let mut config = ConnectionConfig::new(Driver::Postgres);
    config.name = "Prod".into();
    config.read_only = true;
    let (view, cx) = open_with(cx, Workspace::from_configs(vec![config]));

    view.update(cx, |view, cx| {
        assert!(view.is_read_only());
        view.save_pending_edits(cx);
        assert!(
            describe(&view.status).contains("read only"),
            "got: {}",
            describe(&view.status)
        );
    });
}

// -- searching the tree ---------------------------------------------------

#[gpui::test]
fn cmd_shift_f_focuses_the_table_filter(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-shift-f");
    view.update(cx, |view, _| assert_eq!(view.focus, Focus::SidebarSearch));

    cx.simulate_keystrokes(&typing("user"));
    view.update(cx, |view, _| {
        assert_eq!(view.sidebar_filter.text(), "user");
        assert_eq!(view.sidebar_query(), "user");
    });
}

/// The first Escape empties a filter the user is still reading the results of;
/// the second gives the keyboard back to the tree.
#[gpui::test]
fn escape_empties_the_filter_then_leaves_it(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-shift-f");
    cx.simulate_keystrokes(&typing("abc"));
    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| {
        assert!(view.sidebar_filter.is_empty());
        assert_eq!(view.focus, Focus::SidebarSearch, "still in the box");
    });

    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert_eq!(view.focus, Focus::Sidebar));
}

/// Both spellings of a name have to find it, because both are what people
/// type: the bare one from the tree and the qualified one from an error.
#[gpui::test]
fn the_filter_matches_bare_and_qualified_names(cx: &mut TestAppContext) {
    use dbui_app::domain::{Table, TableKind};

    let (view, cx) = open(cx);
    let table = Table {
        schema: "public".into(),
        name: "user_sessions".into(),
        kind: TableKind::Table,
    };

    view.update(cx, |view, _| {
        assert!(view.table_matches_filter(&table, ""), "no filter keeps all");
        assert!(view.table_matches_filter(&table, "sessions"));
        assert!(view.table_matches_filter(&table, "public.user"));
        assert!(!view.table_matches_filter(&table, "orders"));
    });
}

// -- the context menu -----------------------------------------------------

#[gpui::test]
fn right_clicking_a_table_opens_a_menu_that_escape_closes(cx: &mut TestAppContext) {
    use crate::components::context_menu::ContextTarget;
    use dbui_app::domain::TableKind;

    let (view, cx) = open_with(cx, saved_connections(1));

    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Table {
                table: TableRef::new("public", "users"),
                kind: TableKind::Table,
            },
            gpui::point(gpui::px(40.), gpui::px(120.)),
            cx,
        );
        assert!(view.context_menu.is_some());
    });

    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert!(view.context_menu.is_none()));
}

/// Copying a name is the one menu entry that must work with no connection at
/// all -- it does not need the server to answer anything.
#[gpui::test]
fn copying_a_table_name_needs_no_connection(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};
    use dbui_app::domain::TableKind;

    let (view, cx) = open(cx);
    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Table {
                table: TableRef::new("public", "users"),
                kind: TableKind::Table,
            },
            gpui::point(gpui::px(0.), gpui::px(0.)),
            cx,
        );
        view.run_context_action(MenuAction::CopyQualifiedName, cx);
    });

    cx.update(|_, cx| {
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("public.users".to_string())
        );
    });
    view.update(cx, |view, _| {
        assert!(view.context_menu.is_none(), "picking closes the menu");
    });
}

/// A destructive pick opens the confirmation instead of running, and the
/// confirmation owns the keyboard until it is answered.
#[gpui::test]
fn dropping_a_table_asks_before_it_does_anything(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};
    use dbui_app::domain::TableKind;

    let (view, cx) = open_with(cx, saved_connections(1));
    view.update(cx, |view, cx| {
        view.open_context_menu(
            ContextTarget::Table {
                table: TableRef::new("public", "users"),
                kind: TableKind::Table,
            },
            gpui::point(gpui::px(0.), gpui::px(0.)),
            cx,
        );
        view.run_context_action(MenuAction::Drop, cx);
        let prompt = view.confirm.as_ref().expect("a confirmation is up");
        assert_eq!(prompt.expected(), "users");
        assert!(!prompt.armed(), "an empty box cannot fire it");
    });

    // A wrong name leaves it disarmed, and Enter is refused with a reason.
    cx.simulate_keystrokes(&typing("order"));
    cx.simulate_keystrokes("enter");
    view.update(cx, |view, _| {
        let prompt = view.confirm.as_ref().expect("still up");
        assert!(!prompt.armed());
        assert!(prompt.error.is_some(), "and it says what it wanted");
    });

    cx.simulate_keystrokes("escape");
    view.update(cx, |view, _| assert!(view.confirm.is_none()));
}

/// The bug this fixes: the confirmation blocked the keyboard but not the
/// pointer, so a right-click still opened the tree's menu behind an unanswered
/// "drop this table" -- offering a second destructive statement over the top
/// of the first.
#[gpui::test]
fn a_confirmation_blocks_the_context_menu_behind_it(cx: &mut TestAppContext) {
    use crate::components::context_menu::{ContextTarget, MenuAction};
    use dbui_app::domain::TableKind;

    let (view, cx) = open_with(cx, saved_connections(1));
    let target = || ContextTarget::Table {
        table: TableRef::new("public", "users"),
        kind: TableKind::Table,
    };

    view.update(cx, |view, cx| {
        view.open_context_menu(target(), gpui::point(gpui::px(0.), gpui::px(0.)), cx);
        view.run_context_action(MenuAction::Drop, cx);
        assert!(view.confirm.is_some());

        view.open_context_menu(target(), gpui::point(gpui::px(0.), gpui::px(0.)), cx);
        assert!(
            view.context_menu.is_none(),
            "no menu may open while a confirmation is unanswered"
        );
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

// -- caps lock ------------------------------------------------------------
//
// macOS never sets the alpha-lock bit on the character it hands GPUI, so
// every field in the app typed lowercase with caps lock on. The fix lives in
// one place -- `on_key`, which every keystroke passes through -- and these
// pin it there and at the surfaces a person would notice it on.

/// The connection sheet: the field that holds focus when it opens.
#[gpui::test]
fn caps_lock_types_capitals_into_a_form_field(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("p r o d");

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().expect("sheet open").to_config();
        assert_eq!(config.name, "PROD");
    });
}

/// Shift does not invert caps lock on macOS the way it does on Windows: both
/// down is still uppercase. The keyboard layout is the authority on that, and
/// it was asked directly rather than guessed at.
#[gpui::test]
fn shift_and_caps_lock_are_still_uppercase(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes(&clear_field());
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("shift-p shift-r shift-o shift-d");

    view.update(cx, |view, _| {
        let config = view.modal.as_ref().expect("sheet open").to_config();
        assert_eq!(config.name, "PROD");
    });
}

/// The SQL editor.
#[gpui::test]
fn caps_lock_types_capitals_into_the_sql_editor(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-e");
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("s e l space 1");

    view.update(cx, |view, _| {
        assert_eq!(sql_editor_text(view), "SEL 1");
    });
}

/// The cell editor, which writes straight into a row.
#[gpui::test]
fn caps_lock_types_capitals_into_a_cell(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 3);

    view.update(cx, |view, cx| view.begin_cell_edit(0, 1, cx));
    cx.simulate_keystrokes("cmd-a");
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("a b c");

    view.update(cx, |view, _| assert_eq!(view.cell_editor.text(), "ABC"));
}

/// The schema filter, which is the one box a person is most likely to be
/// typing in with caps lock already down.
#[gpui::test]
fn caps_lock_types_capitals_into_the_tree_filter(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-shift-f");
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("u s r");

    view.update(cx, |view, _| assert_eq!(view.sidebar_filter.text(), "USR"));
}

/// Caps lock must not reach the shortcuts: `key` is what a binding matches on,
/// and ⌘N has to open the sheet with caps lock down like it does without.
#[gpui::test]
fn caps_lock_leaves_shortcuts_alone(cx: &mut TestAppContext) {
    let (view, cx) = open(cx);

    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("cmd-n");

    view.update(cx, |view, _| assert!(view.modal.is_some()));
}

/// And it must not reach anything without a case. A digit is a digit.
#[gpui::test]
fn caps_lock_leaves_digits_and_punctuation_alone(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-e");
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes(&typing("1-2.3_4"));

    view.update(cx, |view, _| {
        assert_eq!(sql_editor_text(view), "1-2.3_4");
    });
}

/// Releasing caps lock puts it back: the state is read per keystroke, not
/// latched at the time a field opened.
#[gpui::test]
fn releasing_caps_lock_types_lowercase_again(cx: &mut TestAppContext) {
    let (view, cx) = open_with(cx, saved_connections(1));

    cx.simulate_keystrokes("cmd-e");
    cx.simulate_capslock_change(true);
    cx.simulate_keystrokes("o n");
    cx.simulate_capslock_change(false);
    cx.simulate_keystrokes("o f f");

    view.update(cx, |view, _| {
        assert_eq!(sql_editor_text(view), "ONoff");
    });
}

/// Clicking *inside* the open editor is not clicking away from it.
///
/// The press has to reach the field -- that is how a caret is placed, a word
/// double-clicked, a range dragged -- without also reaching the row under it,
/// which reads a press with no column as "the user moved on" and commits.
#[gpui::test]
fn clicking_inside_the_open_cell_editor_keeps_it_open(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("abcdef");
    });
    cx.run_until_parked();

    // The field records where it was painted, so this is a press on the real
    // editor rather than a guess at where the grid put it.
    let inside = view
        .update(cx, |view, _| view.cell_editor.hit_bounds_slot().get())
        .expect("the editor was painted")
        .center();
    cx.simulate_click(inside, gpui::Modifiers::default());
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert_eq!(
            view.editing_cell,
            Some((1, 1)),
            "the editor stayed open under the pointer"
        );
        assert_eq!(view.cell_editor.text(), "abcdef", "and kept what was typed");
    });
}

/// The editor's own painted bounds, for tests that press inside it.
fn cell_editor_bounds(
    view: &Entity<DbUi>,
    cx: &mut VisualTestContext,
) -> gpui::Bounds<gpui::Pixels> {
    view.update(cx, |view, _| view.cell_editor.hit_bounds_slot().get())
        .expect("the editor was painted")
}

/// And the press has to actually land in the text: clicking near the start of
/// the value puts the caret there, rather than leaving it wherever it was.
#[gpui::test]
fn clicking_in_the_open_cell_editor_moves_the_caret(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("abcdef");
        view.cell_editor.move_to(6);
    });
    cx.run_until_parked();

    let bounds = cell_editor_bounds(&view, cx);
    cx.simulate_click(
        gpui::point(bounds.left() + gpui::px(1.), bounds.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert_eq!(view.editing_cell, Some((1, 1)), "still open");
        // The caret went where it was put.
        assert_eq!(view.cell_editor.cursor(), 0);
    });
}

/// Double-clicking a word selects it. The grid reads a second click on a cell
/// as "open the editor"; the cell already being open is what makes this the
/// field's press rather than the grid's.
#[gpui::test]
fn double_clicking_in_the_open_cell_editor_selects_a_word(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("alpha beta");
    });
    cx.run_until_parked();

    let bounds = cell_editor_bounds(&view, cx);
    let inside = gpui::point(bounds.left() + gpui::px(2.), bounds.center().y);
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Left,
        position: inside,
        modifiers: gpui::Modifiers::default(),
        click_count: 2,
        first_mouse: false,
    });
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert_eq!(view.editing_cell, Some((1, 1)), "still open");
        // The word under the pointer, not the whole value.
        assert_eq!(view.cell_editor.selection(), 0..5);
    });
}

/// Dragging out a selection inside the field is a text drag, not a row drag.
/// The rows underneath must not follow the pointer.
#[gpui::test]
fn dragging_inside_the_open_cell_editor_leaves_the_rows_alone(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("abcdef");
    });
    cx.run_until_parked();
    let before = view.update(cx, |view, _| selected_rows(view));

    let bounds = cell_editor_bounds(&view, cx);
    cx.simulate_mouse_down(
        gpui::point(bounds.left() + gpui::px(1.), bounds.center().y),
        gpui::MouseButton::Left,
        gpui::Modifiers::default(),
    );
    // Down the grid, across the rows under the field.
    let below = bounds.bottom() + gpui::px(60.);
    let away = gpui::point(bounds.right() - gpui::px(1.), below);
    cx.simulate_mouse_move(away, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.simulate_mouse_up(away, gpui::MouseButton::Left, gpui::Modifiers::default());
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert_eq!(view.editing_cell, Some((1, 1)), "still open");
        // The row selection did not follow the pointer.
        assert_eq!(selected_rows(view), before);
    });
}

/// Right-clicking inside it is the field's too: the row menu would close the
/// editor out from under a person reaching for Copy.
#[gpui::test]
fn right_clicking_inside_the_open_cell_editor_opens_no_row_menu(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx) = open_table_with_rows(cx, 4);

    view.update(cx, |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text("abcdef");
    });
    cx.run_until_parked();

    let bounds = cell_editor_bounds(&view, cx);
    cx.simulate_event(gpui::MouseDownEvent {
        button: gpui::MouseButton::Right,
        position: bounds.center(),
        modifiers: gpui::Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    });
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert!(view.context_menu.is_none(), "no row menu over the editor");
        assert_eq!(view.editing_cell, Some((1, 1)), "and it stayed open");
    });
}

// -- the inline editor is a text box, and holds the text keys ---------------
//
// While it is open the grid's own ⌘ shortcuts have to stand down: the box is
// on top, and every one of these means something different inside a field.
// ⌘Z was the loud one -- it threw away the whole staged batch instead of
// undoing a keystroke -- but the same guard covers all of them.

/// An open cell editor with `count` rows behind it, holding `text`.
fn open_cell_editor<'a>(
    cx: &'a mut TestAppContext,
    count: usize,
    text: &str,
) -> (Entity<DbUi>, &'a mut VisualTestContext) {
    let (view, cx) = open_table_with_rows(cx, count);
    let typed = text.to_string();
    view.update(cx, move |view, cx| {
        view.begin_cell_edit(1, 1, cx);
        view.cell_editor.set_text(&typed);
    });
    cx.run_until_parked();
    (view, cx)
}

/// The report: ⌘Z did not undo, it discarded the staged batch.
#[gpui::test]
fn cmd_z_in_the_cell_editor_undoes_the_typing(cx: &mut TestAppContext) {
    let (view, cx) = open_table_with_rows(cx, 4);

    // A staged edit on another row, so a discard would be visible.
    view.update(cx, |view, cx| {
        view.begin_cell_edit(0, 1, cx);
        view.cell_editor.set_text("first");
        view.commit_cell_edit(cx);
        view.begin_cell_edit(1, 1, cx);
    });
    cx.run_until_parked();
    cx.simulate_keystrokes(&typing("abc"));
    cx.simulate_keystrokes("cmd-z");

    view.update(cx, |view, _| {
        assert_eq!(view.editing_cell, Some((1, 1)), "still open");
        assert_ne!(view.cell_editor.text(), "abc", "the typing was undone");
        assert_eq!(
            view.collect_batch_edits().len(),
            1,
            "and the staged batch was left alone"
        );
    });
}

/// ⌘⇧Z redoes it again, which is the half that already worked -- pinned so
/// the pair stays a pair.
#[gpui::test]
fn cmd_shift_z_in_the_cell_editor_redoes(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "");

    cx.simulate_keystrokes(&typing("abc"));
    cx.simulate_keystrokes("cmd-z");
    cx.simulate_keystrokes("cmd-shift-z");

    view.update(cx, |view, _| assert_eq!(view.cell_editor.text(), "abc"));
}

/// ⌘A selects the value, not every row in the table.
#[gpui::test]
fn cmd_a_in_the_cell_editor_selects_its_text(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "abcdef");

    cx.simulate_keystrokes("cmd-a");

    view.update(cx, |view, _| {
        assert_eq!(view.cell_editor.selection(), 0..6, "the value is selected");
        assert_ne!(selected_rows(view).len(), 4, "not every row in the table");
    });
}

/// ⌘C copies what is selected in the box, not the rows behind it.
#[gpui::test]
fn cmd_c_in_the_cell_editor_copies_its_text(cx: &mut TestAppContext) {
    let (_view, cx) = open_cell_editor(cx, 4, "copy me");

    cx.simulate_keystrokes("cmd-a");
    cx.simulate_keystrokes("cmd-c");

    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("copy me".to_string())
    );
}

/// ⌘V pastes text into the box rather than staging rows from the clipboard.
#[gpui::test]
fn cmd_v_in_the_cell_editor_pastes_text(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "");

    cx.write_to_clipboard(gpui::ClipboardItem::new_string("pasted".into()));
    cx.simulate_keystrokes("cmd-v");

    view.update(cx, |view, _| {
        assert_eq!(view.cell_editor.text(), "pasted");
        assert_eq!(
            view.tabs.active().unwrap().pending_inserts().len(),
            0,
            "and staged no rows"
        );
    });
}

/// ⌘⌫ deletes to the start of the line. Over the grid it stages the selected
/// rows for deletion, which is not what a person typing into a box wants.
#[gpui::test]
fn cmd_backspace_in_the_cell_editor_deletes_text(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "abcdef");

    cx.simulate_keystrokes("cmd-backspace");

    view.update(cx, |view, _| {
        assert_eq!(view.editing_cell, Some((1, 1)), "still open");
        // Nothing was staged for deletion.
        assert!(view.collect_batch_deletes().is_empty());
    });
}

/// ⌘D duplicates rows over the grid. While a box is open it must not fire.
#[gpui::test]
fn cmd_d_in_the_cell_editor_stages_no_rows(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "abcdef");

    cx.simulate_keystrokes("cmd-d");

    view.update(cx, |view, _| {
        assert_eq!(view.tabs.active().unwrap().pending_inserts().len(), 0);
    });
}

/// ⌘S commits the batch -- so it has to fold in the box that is still open,
/// or it saves everything except the value the user just typed.
#[gpui::test]
fn cmd_s_folds_in_the_open_cell_editor(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "typed just now");

    view.update(cx, |view, cx| view.save_pending_edits(cx));

    view.update(cx, |view, _| {
        assert!(view.editing_cell.is_none(), "the box closed");
        let staged = view.collect_batch_edits();
        assert!(
            staged.iter().any(|edit| edit
                .changes
                .iter()
                .any(|change| change.new_text == "typed just now")),
            "the typed value went into the batch"
        );
    });
}

/// The rest of what a text box answers to, in one sweep.
///
/// None of these were ever intercepted -- ⌘X, the word and line jumps and the
/// shifted arrows are absent from the grid's shortcut list, so they always
/// reached the field. They are pinned here anyway: the bug was a *global*
/// shortcut quietly claiming a key the box needed, and the way that stays
/// fixed is for the whole contract to be written down.
#[gpui::test]
fn the_cell_editor_answers_the_rest_of_the_text_keys(cx: &mut TestAppContext) {
    let (view, cx) = open_cell_editor(cx, 4, "alpha beta gamma");

    // ⌘→ / ⌘← are the ends of the line.
    cx.simulate_keystrokes("cmd-right");
    view.update(cx, |view, _| assert_eq!(view.cell_editor.cursor(), 16));
    cx.simulate_keystrokes("cmd-left");
    view.update(cx, |view, _| assert_eq!(view.cell_editor.cursor(), 0));

    // ⌥→ walks a word at a time, landing after each one.
    cx.simulate_keystrokes("alt-right alt-right");
    view.update(cx, |view, _| assert_eq!(view.cell_editor.cursor(), 10));

    // ⇧← grows a selection back over "ta".
    cx.simulate_keystrokes("shift-left shift-left");
    let cut = view.update(cx, |view, _| view.cell_editor.selection());
    assert_eq!(cut, 8..10);

    // ⌘X takes it away, and ⌘V puts it back.
    cx.simulate_keystrokes("cmd-x");
    view.update(cx, |view, _| {
        assert_eq!(view.cell_editor.text(), "alpha be gamma");
    });
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("ta".to_string())
    );
    cx.simulate_keystrokes("cmd-v");
    view.update(cx, |view, _| {
        assert_eq!(view.cell_editor.text(), "alpha beta gamma");
        assert_eq!(view.editing_cell, Some((1, 1)), "and it never closed");
    });
}

// -- every text box, one battery ------------------------------------------
//
// Three bugs in a row here had the same shape: something upstream of the
// field ate a key or a press, and only the one surface somebody happened to
// try showed it. Caps lock was every box at once; ⌘Z was the box over a grid
// cell; a press inside that box went on to the row behind it. Each was found
// by hand, one at a time.
//
// So the contract is asserted for all of them together. Adding a text box to
// the app means adding a row here, and the battery says whether the box is a
// real one.

use crate::components::text_field::{DetailInput, InputTarget};
use crate::text_input::TextInput;

/// One editable box, and how to put the keyboard into it.
struct Box2 {
    name: &'static str,
    /// Reach the state the box lives in. Called on a fresh window.
    open: fn(&Entity<DbUi>, &mut VisualTestContext),
    /// The buffer behind it, for seeding and reading back.
    text: fn(&mut DbUi) -> Option<&mut TextInput>,
    /// Whether the box still has the keyboard.
    ///
    /// `text` cannot answer this: the buffer behind a closed box is still
    /// readable, so a press that shut the box would otherwise look like a
    /// pass. That is exactly how the cell editor closing under the pointer
    /// went unnoticed.
    holds_keys: fn(&DbUi) -> bool,
}

fn by_target(target: InputTarget) -> impl Fn(&mut DbUi) -> Option<&mut TextInput> {
    move |view: &mut DbUi| view.input_mut(target)
}

/// Every box the app draws.
fn every_box() -> Vec<Box2> {
    fn detail(view: &Entity<DbUi>, cx: &mut VisualTestContext) {
        view.update(cx, |view, cx| {
            view.select_row(0, cx);
            view.focus_input(InputTarget::DetailSearch, cx);
        });
    }
    vec![
        Box2 {
            name: "SQL editor",
            open: |_, cx| cx.simulate_keystrokes("cmd-e"),
            text: |view| match view.tabs.active_mut() {
                Some(WorkspaceTab::Sql { editor, .. }) => Some(editor),
                _ => None,
            },
            holds_keys: |view| view.focus == Focus::Editor,
        },
        Box2 {
            name: "connection sheet",
            open: |_, cx| cx.simulate_keystrokes("cmd-n"),
            text: |view| view.modal.as_mut().and_then(|form| form.field_mut(0)),
            holds_keys: |view| view.modal.is_some(),
        },
        Box2 {
            name: "row filter (⌘F)",
            open: |_, cx| cx.simulate_keystrokes("cmd-f"),
            text: |view| view.input_mut(InputTarget::WhereDraft),
            holds_keys: |view| view.focus == Focus::Filter,
        },
        Box2 {
            name: "page size",
            open: |view, cx| {
                view.update(cx, |view, cx| view.focus_input(InputTarget::PageSize, cx));
            },
            text: |view| view.input_mut(InputTarget::PageSize),
            holds_keys: |view| view.focus == Focus::PageSize,
        },
        Box2 {
            name: "detail search",
            open: detail,
            text: |view| view.input_mut(InputTarget::DetailSearch),
            holds_keys: |view| view.detail_input == Some(DetailInput::Search),
        },
        Box2 {
            name: "detail field",
            open: |view, cx| {
                view.update(cx, |view, cx| {
                    view.select_row(0, cx);
                    // Field 0 is the primary key, which is deliberately not
                    // editable in the sidebar -- so this asks for field 1.
                    view.focus_input(InputTarget::DetailField(1), cx);
                });
            },
            text: |view| view.input_mut(InputTarget::DetailField(1)),
            holds_keys: |view| view.detail_input == Some(DetailInput::Field(1)),
        },
        Box2 {
            name: "staged insert field",
            open: |view, cx| {
                view.update(cx, |view, cx| {
                    view.add_row(cx);
                    view.edit_insert(0, cx);
                    view.focus_input(InputTarget::InsertField(1), cx);
                });
            },
            text: |view| view.input_mut(InputTarget::InsertField(1)),
            holds_keys: |view| view.detail_input == Some(DetailInput::Field(1)),
        },
        Box2 {
            name: "cell editor",
            open: |view, cx| {
                view.update(cx, |view, cx| {
                    view.begin_cell_edit(1, 1, cx);
                    view.focus_input(InputTarget::CellEditor, cx);
                });
            },
            text: |view| view.input_mut(InputTarget::CellEditor),
            holds_keys: |view| view.editing_cell.is_some(),
        },
        Box2 {
            name: "palette query",
            open: |_, cx| cx.simulate_keystrokes("cmd-p"),
            text: |view| view.input_mut(InputTarget::PaletteQuery),
            holds_keys: |view| view.palette.is_some(),
        },
        Box2 {
            name: "drop confirmation",
            open: |view, cx| {
                use crate::components::context_menu::{ContextTarget, MenuAction};
                view.update(cx, |view, cx| {
                    view.open_context_menu(
                        ContextTarget::Table {
                            table: TableRef::new("public", "users"),
                            kind: dbui_app::domain::TableKind::Table,
                        },
                        gpui::point(gpui::px(0.), gpui::px(0.)),
                        cx,
                    );
                    view.run_context_action(MenuAction::Drop, cx);
                });
            },
            text: |view| view.input_mut(InputTarget::ConfirmName),
            holds_keys: |view| view.confirm.is_some(),
        },
    ]
}

/// Seed the box with a known value and hand back a reader.
fn seed(view: &Entity<DbUi>, cx: &mut VisualTestContext, boxed: &Box2) {
    (boxed.open)(view, cx);
    cx.run_until_parked();
    let found = view.update(cx, |view, _| {
        (boxed.text)(view).map(|input| {
            input.set_text("alpha beta");
            input.move_to(10);
        })
    });
    assert!(found.is_some(), "{}: never reached the box", boxed.name);
    cx.run_until_parked();
}

fn read(view: &Entity<DbUi>, cx: &mut VisualTestContext, boxed: &Box2) -> String {
    view.update(cx, |view, _| {
        (boxed.text)(view)
            .map(|input| input.text().to_string())
            .unwrap_or_default()
    })
}

/// Caps lock reaches every box, not only the one it was found in.
#[gpui::test]
fn every_input_types_capitals_with_caps_lock(cx: &mut TestAppContext) {
    for boxed in every_box() {
        let (view, cx) = open_table_with_rows(cx, 4);
        seed(&view, cx, &boxed);

        cx.simulate_capslock_change(true);
        cx.simulate_keystrokes("x");
        cx.simulate_capslock_change(false);

        assert_eq!(
            read(&view, cx, &boxed),
            "alpha betaX",
            "{}: caps lock",
            boxed.name
        );
    }
}

/// ⌘Z undoes a keystroke in every box. This is the one that discarded a whole
/// staged batch instead, over a grid cell.
#[gpui::test]
fn every_input_undoes_with_cmd_z(cx: &mut TestAppContext) {
    for boxed in every_box() {
        let (view, cx) = open_table_with_rows(cx, 4);
        seed(&view, cx, &boxed);

        cx.simulate_keystrokes(&typing("XY"));
        assert_eq!(read(&view, cx, &boxed), "alpha betaXY", "{}", boxed.name);
        cx.simulate_keystrokes("cmd-z");

        assert_eq!(
            read(&view, cx, &boxed),
            "alpha betaX",
            "{}: ⌘Z undoes one keystroke",
            boxed.name
        );
    }
}

/// ⌘A selects the value, ⌘C copies it, ⌘X takes it, ⌘V puts it back.
#[gpui::test]
fn every_input_does_select_all_copy_cut_and_paste(cx: &mut TestAppContext) {
    for boxed in every_box() {
        let (view, cx) = open_table_with_rows(cx, 4);
        seed(&view, cx, &boxed);

        cx.simulate_keystrokes("cmd-a");
        let selected = view.update(cx, |view, _| (boxed.text)(view).map(|i| i.selection()));
        assert_eq!(selected, Some(0..10), "{}: ⌘A", boxed.name);

        // A sentinel on the board, so a copy that never happened is caught
        // rather than read as a pass.
        cx.write_to_clipboard(gpui::ClipboardItem::new_string("SENTINEL".into()));
        cx.simulate_keystrokes("cmd-c");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("alpha beta".to_string()),
            "{}: ⌘C",
            boxed.name
        );

        cx.simulate_keystrokes("cmd-a");
        cx.simulate_keystrokes("cmd-x");
        assert!(read(&view, cx, &boxed).is_empty(), "{}: ⌘X", boxed.name);

        cx.simulate_keystrokes("cmd-v");
        assert_eq!(read(&view, cx, &boxed), "alpha beta", "{}: ⌘V", boxed.name);
    }
}

/// ⌘⌫ clears to the start of the line rather than staging rows for deletion.
#[gpui::test]
fn every_input_deletes_to_the_line_start(cx: &mut TestAppContext) {
    for boxed in every_box() {
        let (view, cx) = open_table_with_rows(cx, 4);
        seed(&view, cx, &boxed);

        cx.simulate_keystrokes("cmd-backspace");

        assert!(read(&view, cx, &boxed).is_empty(), "{}: ⌘⌫", boxed.name);
        assert!(
            view.update(cx, |view, _| view.collect_batch_deletes().is_empty()),
            "{}: ⌘⌫ staged rows for deletion",
            boxed.name
        );
    }
}

/// A press inside a box places the caret there and leaves the value alone --
/// the failure that closed the cell editor out from under the pointer.
///
/// Every box in the list has to be on screen for this: a box that stopped
/// drawing would otherwise quietly drop out of the audit, which is the exact
/// shape of the bug the audit exists to catch. The tree filter is the one
/// box not in the list, because it needs a live connection to draw at all --
/// it has [`the_tree_filter_takes_a_press`] to itself.
#[gpui::test]
fn a_press_inside_any_input_places_the_caret(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    for boxed in every_box() {
        let (view, cx) = open_table_with_rows(cx, 4);
        seed(&view, cx, &boxed);

        let bounds = view
            .update(cx, |view, _| {
                (boxed.text)(view).and_then(|input| input.hit_bounds_slot().get())
            })
            .unwrap_or_else(|| panic!("{}: never drew, so no press could reach it", boxed.name));

        cx.simulate_click(
            gpui::point(bounds.left() + gpui::px(1.), bounds.center().y),
            gpui::Modifiers::default(),
        );
        cx.run_until_parked();

        assert_eq!(
            read(&view, cx, &boxed),
            "alpha beta",
            "{}: a press ate the value",
            boxed.name
        );
        assert_eq!(
            view.update(cx, |view, _| (boxed.text)(view).map(|i| i.cursor())),
            Some(0),
            "{}: the caret did not follow the press",
            boxed.name
        );
        assert!(
            view.update(cx, |view, _| (boxed.holds_keys)(view)),
            "{}: the press closed the box it landed in",
            boxed.name
        );
    }
}

/// The tree filter, which is hidden until there is a catalog to filter -- so
/// it needs a real connection rather than the fixture the battery uses.
#[gpui::test]
fn the_tree_filter_takes_a_press(cx: &mut TestAppContext) {
    let _lock = layout_lock();
    let (view, cx, _db) = open_connected(cx, "input-audit");

    cx.simulate_keystrokes("cmd-shift-f");
    view.update(cx, |view, _| view.sidebar_filter.set_text("alpha beta"));
    cx.run_until_parked();

    let bounds = view
        .update(cx, |view, _| view.sidebar_filter.hit_bounds_slot().get())
        .expect("the tree filter draws once there is a catalog");
    cx.simulate_click(
        gpui::point(bounds.left() + gpui::px(1.), bounds.center().y),
        gpui::Modifiers::default(),
    );
    cx.run_until_parked();

    view.update(cx, |view, _| {
        assert_eq!(view.sidebar_filter.text(), "alpha beta");
        assert_eq!(view.sidebar_filter.cursor(), 0);
        assert_eq!(view.focus, Focus::SidebarSearch, "and kept the keyboard");
    });
}
