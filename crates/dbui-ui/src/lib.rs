//! The GPUI front-end.
//!
//! This crate is the delivery mechanism: it opens a window, draws the state
//! `dbui-app` holds, and turns clicks and keys back into use cases. GPUI stops
//! here -- nothing underneath it knows a window exists.

mod components;
mod highlight;
mod json_format;
mod root;
mod sql_complete;
mod sql_format;
mod sql_scaffold;
mod tabs;
mod text_diff;
mod text_input;
mod theme;
mod update;

#[cfg(target_os = "macos")]
mod mac_window;

#[cfg(test)]
mod e2e;

pub use root::{DbUi, Focus, ResultSource, ResultView, Status};

use dbui_app::{session, store, DbRuntime, Workspace};
use gpui::{
    point, px, size, App, AppContext, Application, Bounds, KeyBinding, Menu, MenuItem,
    TitlebarOptions, WindowBounds, WindowOptions,
};

/// Open the window and run until it closes.
pub fn run() {
    let runtime = match DbRuntime::new() {
        Ok(runtime) => runtime,
        // Nothing can work without it, and there is no window yet to say so in.
        Err(error) => {
            eprintln!("dbui: could not start the database runtime: {error}");
            std::process::exit(1);
        }
    };

    // A store that cannot be read is not fatal: the app opens with no saved
    // connections, and says why on the status bar once there is one.
    let (saved, load_error) = match store::connections_path().and_then(|path| store::load(&path)) {
        Ok(configs) => (configs, None),
        Err(error) => (Vec::new(), Some(error.to_string())),
    };

    let prefs = store::prefs_path()
        .and_then(|path| store::load_prefs(&path))
        .unwrap_or_default();

    // What was open last time. Pruned against the connections that actually
    // loaded, so a session naming a deleted -- or unreadable -- connection
    // opens one tab fewer rather than a tab onto nothing.
    let mut last_session = session::session_path()
        .map(|path| session::load(&path))
        .unwrap_or_default();
    last_session.prune(&saved.iter().map(|config| config.id).collect::<Vec<_>>());

    Application::new().run(move |cx: &mut App| {
        cx.activate(true);
        load_bundled_fonts(cx);
        // Bind before set_menus so the menu bar can show the key equivalents.
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-n", NewConnection, Some("DbUi")),
            KeyBinding::new("cmd-p", GoToTable, Some("DbUi")),
            KeyBinding::new("cmd-shift-p", CommandPalette, Some("DbUi")),
            KeyBinding::new("cmd-shift-t", ChooseTheme, Some("DbUi")),
            KeyBinding::new("cmd-f", Find, Some("DbUi")),
            KeyBinding::new("cmd-shift-f", SearchTables, Some("DbUi")),
            KeyBinding::new("cmd-s", CommitChanges, Some("DbUi")),
            // ⌘A and ⌘⌫ are handled in `DbUi::on_key` rather than bound here:
            // both mean something different depending on which surface has the
            // keyboard, and an action would claim them everywhere.
            KeyBinding::new("cmd-e", OpenSql, Some("DbUi")),
            KeyBinding::new("cmd-r", Refresh, Some("DbUi")),
            KeyBinding::new("cmd-enter", RunQuery, Some("DbUi")),
            KeyBinding::new("cmd-shift-enter", RunAllQueries, Some("DbUi")),
            // ⌘W is handled in `DbUi::on_key` so it isn't stolen / double-fired.
            KeyBinding::new("cmd-shift-]", NextTab, Some("DbUi")),
            KeyBinding::new("cmd-shift-[", PrevTab, Some("DbUi")),
            // Connection tabs sit one level above table tabs, and their
            // shortcuts are the table-tab ones plus ⌥.
            KeyBinding::new("cmd-alt-]", NextConnection, Some("DbUi")),
            KeyBinding::new("cmd-alt-[", PrevConnection, Some("DbUi")),
            KeyBinding::new("cmd-shift-w", CloseConnection, Some("DbUi")),
            KeyBinding::new("cmd-1", SelectTab1, Some("DbUi")),
            KeyBinding::new("cmd-2", SelectTab2, Some("DbUi")),
            KeyBinding::new("cmd-3", SelectTab3, Some("DbUi")),
            KeyBinding::new("cmd-4", SelectTab4, Some("DbUi")),
            KeyBinding::new("cmd-5", SelectTab5, Some("DbUi")),
            KeyBinding::new("cmd-6", SelectTab6, Some("DbUi")),
            KeyBinding::new("cmd-7", SelectTab7, Some("DbUi")),
            KeyBinding::new("cmd-8", SelectTab8, Some("DbUi")),
            KeyBinding::new("cmd-9", SelectTab9, Some("DbUi")),
            KeyBinding::new("cmd-=", ZoomIn, Some("DbUi")),
            KeyBinding::new("cmd-+", ZoomIn, Some("DbUi")),
            KeyBinding::new("cmd--", ZoomOut, Some("DbUi")),
            KeyBinding::new("cmd-0", ZoomReset, Some("DbUi")),
        ]);
        cx.set_menus(menus());

        let bounds = Bounds::centered(None, size(px(1200.), px(780.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("dbui".into()),
                // We draw our own titlebar; see `components::titlebar`.
                appears_transparent: true,
                traffic_light_position: Some(point(px(14.), px(12.))),
            }),
            ..Default::default()
        };

        let workspace = Workspace::from_configs(saved.clone());
        let load_error = load_error.clone();
        let runtime = runtime.clone();
        let theme_id = prefs.theme.clone();
        let zoom_pct = prefs.zoom_pct;
        let editor_height_px = prefs.sql_editor_height_px;
        let last_session = last_session.clone();

        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus);
                window.activate_window();

                let mut view = DbUi::new(runtime, workspace, focus);
                view.apply_theme_id(&theme_id);
                view.apply_zoom_pct(zoom_pct);
                view.apply_editor_height_px(editor_height_px);
                let reopen = view.restore_session(&last_session);
                if let Some(message) = load_error {
                    view.report_startup_error(message);
                }
                // Only the tab that was in front dials out. The rest wait to
                // be clicked -- restoring is not a reason to reach for every
                // server the user has ever had open.
                if let Some(id) = reopen {
                    view.connect(id, cx);
                }
                // SQL typed since the last structural change is only in the
                // editor buffer; this is what gets it to disk.
                cx.on_app_quit(|view: &mut DbUi, _cx| {
                    view.persist_session();
                    async {}
                })
                .detach();
                // Ask GitHub whether there is anything newer. Fire-and-forget:
                // it resolves into the status-bar chip, and a machine that is
                // offline never hears about it.
                view.check_for_update(cx);
                view
            })
        })
        .expect("failed to open window");
    });
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "dbui".into(),
            items: vec![MenuItem::action("Quit dbui", Quit)],
        },
        Menu {
            name: "Connection".into(),
            items: vec![
                MenuItem::action("New Connection", NewConnection),
                MenuItem::separator(),
                MenuItem::action("Next Connection", NextConnection),
                MenuItem::action("Previous Connection", PrevConnection),
                MenuItem::action("Close Connection", CloseConnection),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Change Theme…", ChooseTheme),
                MenuItem::separator(),
                MenuItem::action("Next Tab", NextTab),
                MenuItem::action("Previous Tab", PrevTab),
                MenuItem::action("Close Tab", CloseTab),
                MenuItem::separator(),
                MenuItem::action("Zoom In", ZoomIn),
                MenuItem::action("Zoom Out", ZoomOut),
                MenuItem::action("Actual Size", ZoomReset),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Select All Rows", SelectAllRows),
                MenuItem::action("Delete Selected Rows", DeleteRows),
                MenuItem::separator(),
                MenuItem::action("Commit Changes", CommitChanges),
            ],
        },
        Menu {
            name: "Go".into(),
            items: vec![
                MenuItem::action("Go to Table…", GoToTable),
                MenuItem::action("Command Palette…", CommandPalette),
                MenuItem::action("Search Tables", SearchTables),
                MenuItem::action("Find…", Find),
            ],
        },
        Menu {
            name: "Query".into(),
            items: vec![
                MenuItem::action("New SQL Tab", OpenSql),
                MenuItem::action("Run Query", RunQuery),
                MenuItem::action("Run All Queries", RunAllQueries),
                MenuItem::action("Refresh", Refresh),
            ],
        },
    ]
}

gpui::actions!(
    dbui,
    [
        Quit,
        NewConnection,
        GoToTable,
        CommandPalette,
        ChooseTheme,
        Find,
        SearchTables,
        CommitChanges,
        SelectAllRows,
        DeleteRows,
        OpenSql,
        Refresh,
        RunQuery,
        RunAllQueries,
        CloseTab,
        NextTab,
        PrevTab,
        CloseConnection,
        NextConnection,
        PrevConnection,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectTab9,
        ZoomIn,
        ZoomOut,
        ZoomReset
    ]
);

/// Geist Mono, compiled into the binary (same approach as edui). SIL OFL;
/// see `assets/fonts/OFL.txt`.
const BUNDLED_FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/GeistMono-Regular.otf"),
    include_bytes!("../assets/fonts/GeistMono-Italic.otf"),
    include_bytes!("../assets/fonts/GeistMono-Bold.otf"),
    include_bytes!("../assets/fonts/GeistMono-BoldItalic.otf"),
];

pub(crate) fn load_bundled_fonts(cx: &App) {
    let blobs = BUNDLED_FONTS
        .iter()
        .map(|bytes| std::borrow::Cow::Borrowed(*bytes))
        .collect();
    if let Err(error) = cx.text_system().add_fonts(blobs) {
        eprintln!("dbui: could not register bundled fonts: {error}");
    }
}
