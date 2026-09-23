//! Changing a table's shape: columns, indexes, and new tables.
//!
//! Every change goes through one sheet, and the sheet's centrepiece is the
//! SQL it is going to run, rebuilt as the fields change. Nothing here reaches
//! the server until that SQL has been on screen and Run pressed -- the same
//! stage-then-commit the grid's edits follow, for statements with rather more
//! reach. What an engine cannot do in place (SQLite retyping a column) is
//! said in the preview's place, and Run stays off.
//!
//! On success the table's rows, structure and indexes are read again and the
//! tree catches up; on failure the sheet stays open with the engine's own
//! words, so the fix is one edit away rather than a whole form retyped.

use super::text_field::{text_field, InputTarget};
use super::{button, caption, motion};
use crate::root::{DbUi, Status};
use crate::tabs::{TablePane, WorkspaceTab};
use crate::text_input::TextInput;
use crate::theme::metrics;
use dbui_app::commands;
use dbui_app::domain::{ddl, Column, ColumnSpec, Driver, TableRef};
use gpui::{div, prelude::*, px, AnyElement, Context, Keystroke, MouseButton, SharedString};

pub(crate) enum SheetKind {
    AddColumn,
    EditColumn(Column),
    DropColumn(String),
    /// With the table's columns, to pick the indexed ones from.
    AddIndex(Vec<String>),
    DropIndex(String),
    CreateTable,
}

/// One column of a table being created.
pub(crate) struct NewColumn {
    pub name: TextInput,
    pub data_type: TextInput,
    pub nullable: bool,
    pub key: bool,
}

impl NewColumn {
    fn blank() -> Self {
        Self {
            name: TextInput::new(false),
            data_type: TextInput::new(false),
            nullable: true,
            key: false,
        }
    }
}

pub(crate) struct SchemaSheet {
    pub table: TableRef,
    pub kind: SheetKind,
    /// Column sheets: name, type, default. Index sheet: name. Create-table
    /// sheet: the table's name; its columns' fields are in `new_columns`.
    pub fields: Vec<TextInput>,
    pub nullable: bool,
    pub unique: bool,
    /// Index columns, in the order they were picked -- which is index order.
    pub picked: Vec<String>,
    pub new_columns: Vec<NewColumn>,
    /// Which text field has the keyboard, counting every field in order.
    pub focused: usize,
    pub running: bool,
    pub error: Option<String>,
}

impl SchemaSheet {
    fn new(table: TableRef, kind: SheetKind) -> Self {
        let (fields, nullable, new_columns) = match &kind {
            SheetKind::AddColumn => (
                vec![
                    TextInput::new(false),
                    TextInput::new(false),
                    TextInput::new(false),
                ],
                true,
                Vec::new(),
            ),
            SheetKind::EditColumn(column) => (
                vec![
                    TextInput::with_text(column.name.clone(), false),
                    TextInput::with_text(column.data_type.clone(), false),
                    TextInput::with_text(column.default.clone().unwrap_or_default(), false),
                ],
                column.nullable,
                Vec::new(),
            ),
            SheetKind::AddIndex(_) => (vec![TextInput::new(false)], true, Vec::new()),
            SheetKind::CreateTable => (
                vec![TextInput::with_text(table.name.clone(), false)],
                true,
                vec![NewColumn::blank()],
            ),
            SheetKind::DropColumn(_) | SheetKind::DropIndex(_) => (Vec::new(), true, Vec::new()),
        };
        Self {
            table,
            kind,
            fields,
            nullable,
            unique: false,
            picked: Vec::new(),
            new_columns,
            focused: 0,
            running: false,
            error: None,
        }
    }

    fn field_count(&self) -> usize {
        self.fields.len() + self.new_columns.len() * 2
    }

    /// The text field at focus position `index`.
    pub(crate) fn input_mut(&mut self, index: usize) -> Option<&mut TextInput> {
        if index < self.fields.len() {
            return self.fields.get_mut(index);
        }
        let at = index - self.fields.len();
        let column = self.new_columns.get_mut(at / 2)?;
        Some(if at % 2 == 0 {
            &mut column.name
        } else {
            &mut column.data_type
        })
    }

    fn text(&self, index: usize) -> String {
        self.fields
            .get(index)
            .map(|field| field.text().trim().to_string())
            .unwrap_or_default()
    }

    fn column_spec(&self) -> ColumnSpec {
        let default = self.text(2);
        ColumnSpec {
            name: self.text(0),
            data_type: self.text(1),
            nullable: self.nullable,
            default: (!default.is_empty()).then_some(default),
        }
    }

    /// What Run will send, or why it cannot.
    pub(crate) fn statements(&self, driver: Driver) -> Result<Vec<String>, String> {
        let table = &self.table;
        match &self.kind {
            SheetKind::AddColumn => ddl::add_column(driver, table, &self.column_spec()),
            SheetKind::EditColumn(before) => {
                let statements = ddl::alter_column(driver, table, before, &self.column_spec())?;
                if statements.is_empty() {
                    return Err("Nothing changed yet".into());
                }
                Ok(statements)
            }
            SheetKind::DropColumn(name) => Ok(ddl::drop_column(driver, table, name)),
            SheetKind::AddIndex(_) => {
                ddl::create_index(driver, table, &self.text(0), &self.picked, self.unique)
            }
            SheetKind::DropIndex(name) => Ok(ddl::drop_index(driver, table, name)),
            SheetKind::CreateTable => {
                let named = TableRef::new(table.schema.clone(), self.text(0));
                let columns: Vec<ColumnSpec> = self
                    .new_columns
                    .iter()
                    .filter(|column| {
                        !column.name.text().trim().is_empty()
                            || !column.data_type.text().trim().is_empty()
                    })
                    .map(|column| ColumnSpec {
                        name: column.name.text().trim().to_string(),
                        data_type: column.data_type.text().trim().to_string(),
                        // A key column is never null: say so, rather than
                        // leave it to each engine to decide what that means.
                        nullable: column.nullable && !column.key,
                        default: None,
                    })
                    .collect();
                let key: Vec<String> = self
                    .new_columns
                    .iter()
                    .filter(|column| column.key && !column.name.text().trim().is_empty())
                    .map(|column| column.name.text().trim().to_string())
                    .collect();
                ddl::create_table(driver, &named, &columns, &key)
            }
        }
    }

    fn title(&self) -> String {
        let table = &self.table.name;
        match &self.kind {
            SheetKind::AddColumn => format!("Add a column to {table}"),
            SheetKind::EditColumn(column) => format!("Edit {table}.{}", column.name),
            SheetKind::DropColumn(name) => format!("Drop {table}.{name}"),
            SheetKind::AddIndex(_) => format!("Add an index to {table}"),
            SheetKind::DropIndex(name) => format!("Drop index {name}"),
            SheetKind::CreateTable => format!("New table in {}", self.table.schema),
        }
    }

    fn is_destructive(&self) -> bool {
        matches!(
            self.kind,
            SheetKind::DropColumn(_) | SheetKind::DropIndex(_)
        )
    }
}

impl DbUi {
    fn open_schema_sheet(&mut self, table: TableRef, kind: SheetKind, cx: &mut Context<Self>) {
        if self.refuse_if_read_only("Changing a table", cx) {
            return;
        }
        self.schema_sheet = Some(SchemaSheet::new(table, kind));
        cx.notify();
    }

    fn structure_table(&self) -> Option<TableRef> {
        self.tabs.active().and_then(|tab| tab.table_ref().cloned())
    }

    pub(crate) fn add_column_sheet(&mut self, cx: &mut Context<Self>) {
        if let Some(table) = self.structure_table() {
            self.open_schema_sheet(table, SheetKind::AddColumn, cx);
        }
    }

    pub(crate) fn edit_column_sheet(&mut self, column: Column, cx: &mut Context<Self>) {
        if let Some(table) = self.structure_table() {
            self.open_schema_sheet(table, SheetKind::EditColumn(column), cx);
        }
    }

    pub(crate) fn drop_column_sheet(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(table) = self.structure_table() {
            self.open_schema_sheet(table, SheetKind::DropColumn(name), cx);
        }
    }

    pub(crate) fn add_index_sheet(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.structure_table() else {
            return;
        };
        let columns = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                result: Some(view), ..
            }) => view.structure.iter().map(|c| c.name.clone()).collect(),
            _ => Vec::new(),
        };
        self.open_schema_sheet(table, SheetKind::AddIndex(columns), cx);
    }

    pub(crate) fn drop_index_sheet(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(table) = self.structure_table() {
            self.open_schema_sheet(table, SheetKind::DropIndex(name), cx);
        }
    }

    /// "New Table…", in `schema` -- or the schema of whatever is in front,
    /// or the connection's first.
    pub(crate) fn create_table_sheet(&mut self, schema: Option<String>, cx: &mut Context<Self>) {
        let schema = schema
            .or_else(|| self.structure_table().map(|table| table.schema))
            .or_else(|| {
                self.workspace
                    .active()
                    .and_then(|entry| entry.catalog.as_ref())
                    .and_then(|catalog| catalog.schemas.first())
                    .map(|schema| schema.name.clone())
            });
        let Some(schema) = schema else {
            self.status = Status::info("Connect first — a table needs a schema to live in");
            cx.notify();
            return;
        };
        self.open_schema_sheet(TableRef::new(schema, ""), SheetKind::CreateTable, cx);
    }

    pub(crate) fn close_schema_sheet(&mut self, cx: &mut Context<Self>) {
        self.schema_sheet = None;
        cx.notify();
    }

    /// Read the front table's indexes, for the structure pane.
    pub(crate) fn load_indexes(&mut self, cx: &mut Context<Self>) {
        let (Some(tab_id), Some(table), Some(driver)) = (
            self.tabs.active_id(),
            self.structure_table(),
            self.workspace.active_driver(),
        ) else {
            return;
        };
        let task = commands::fetch_indexes(&self.runtime, driver, table);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                if let Some(WorkspaceTab::Table { indexes, .. }) = this.tabs.get_mut(tab_id) {
                    // A table the engine will not list indexes for still
                    // shows its columns; the index list just says none.
                    *indexes = Some(match landed {
                        Some(Ok(found)) => found,
                        _ => Vec::new(),
                    });
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run: send the previewed statements, then bring everything that
    /// describes the table back in line with it.
    pub(crate) fn run_schema_sheet(&mut self, cx: &mut Context<Self>) {
        if self.refuse_if_read_only("Changing a table", cx) {
            self.schema_sheet = None;
            return;
        }
        let Some(driver) = self.workspace.active_driver() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };
        let Some(sheet) = self.schema_sheet.as_mut() else {
            return;
        };
        if sheet.running {
            return;
        }
        let statements = match sheet.statements(driver.driver()) {
            Ok(statements) => statements,
            Err(problem) => {
                sheet.error = Some(problem);
                cx.notify();
                return;
            }
        };
        sheet.running = true;
        sheet.error = None;
        let created = matches!(sheet.kind, SheetKind::CreateTable)
            .then(|| TableRef::new(sheet.table.schema.clone(), sheet.fields[0].text().trim()));
        let title = sheet.title();
        self.status = Status::busy("Changing the table…");
        cx.notify();

        let task = commands::run_ddl(&self.runtime, driver, statements);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| match landed {
                Some(Ok(_)) => {
                    this.schema_sheet = None;
                    this.status = Status::info(format!("Done: {}", title.to_lowercase()));
                    this.refresh_catalog_quietly(cx);
                    match created {
                        // A new table opens, on its structure.
                        Some(table) => {
                            this.open_table_tab(table, cx);
                            this.set_table_pane(TablePane::Structure, cx);
                        }
                        None => {
                            this.refresh_result(cx);
                            this.load_indexes(cx);
                        }
                    }
                }
                Some(Err(error)) => {
                    if let Some(sheet) = this.schema_sheet.as_mut() {
                        sheet.running = false;
                        sheet.error = Some(error.to_string());
                    }
                    this.status = Status::error(error.to_string());
                    cx.notify();
                }
                None => {
                    if let Some(sheet) = this.schema_sheet.as_mut() {
                        sheet.running = false;
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Keys while the sheet is up: it is modal, so everything stops here.
    pub(crate) fn handle_schema_sheet_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) {
        let key = keystroke.key.as_str();
        let Some(sheet) = self.schema_sheet.as_mut() else {
            return;
        };
        match key {
            "escape" => {
                self.close_schema_sheet(cx);
                return;
            }
            "enter" if !keystroke.modifiers.platform => {
                self.run_schema_sheet(cx);
                return;
            }
            "tab" => {
                let count = sheet.field_count();
                if count > 0 {
                    sheet.focused = if keystroke.modifiers.shift {
                        (sheet.focused + count - 1) % count
                    } else {
                        (sheet.focused + 1) % count
                    };
                }
                cx.notify();
                return;
            }
            _ => {}
        }
        if sheet.running {
            return;
        }
        let focused = sheet.focused;
        if let Some(input) = sheet.input_mut(focused) {
            if input.handle_key(keystroke, cx) {
                sheet.error = None;
                cx.notify();
            }
        }
    }

    pub(crate) fn render_schema_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.schema_sheet.as_ref()?;
        let theme = &self.theme;
        let driver = self.active_driver_kind().unwrap_or(Driver::Postgres);
        let preview = sheet.statements(driver);
        let destructive = sheet.is_destructive();
        let can_run = preview.is_ok() && !sheet.running;

        let labelled = |label: &'static str, field: AnyElement| {
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
                .child(div().flex_1().min_w(px(0.)).child(field))
                .into_any_element()
        };
        let field = |index: usize, hint: &'static str, cx: &mut Context<Self>| {
            let input = sheet.fields.get(index).expect("the sheet's field");
            text_field(
                ("sheet-field", index),
                input,
                InputTarget::SheetField(index),
                sheet.focused == index,
                Some(hint),
                theme,
                cx,
            )
        };
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .cursor_pointer()
                .text_color(if on { theme.text } else { theme.text_muted })
                .child(
                    div()
                        .w(px(14.))
                        .h(px(14.))
                        .rounded(px(3.))
                        .border_1()
                        .border_color(if on { theme.accent } else { theme.border })
                        .when(on, |check| check.bg(theme.accent)),
                )
                .child(label)
        };

        let mut body: Vec<AnyElement> = Vec::new();
        match &sheet.kind {
            SheetKind::AddColumn | SheetKind::EditColumn(_) => {
                body.push(labelled("Name", field(0, "column_name", cx)));
                body.push(labelled(
                    "Type",
                    field(1, "text, integer, numeric(10,2)…", cx),
                ));
                body.push(labelled(
                    "Default",
                    field(2, "none — or an expression: 0, now()", cx),
                ));
                body.push(
                    toggle("sheet-nullable", "Allows NULL", sheet.nullable)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(sheet) = this.schema_sheet.as_mut() {
                                sheet.nullable = !sheet.nullable;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            SheetKind::AddIndex(columns) => {
                body.push(labelled("Name", field(0, "index_name", cx)));
                body.push(
                    caption("Columns, in index order — click to add or remove", theme)
                        .into_any_element(),
                );
                let chips: Vec<AnyElement> = columns
                    .iter()
                    .enumerate()
                    .map(|(at, name)| {
                        let position = sheet.picked.iter().position(|picked| picked == name);
                        let target = name.clone();
                        div()
                            .id(("sheet-index-column", at))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .border_1()
                            .cursor_pointer()
                            .font_family(metrics::MONO_FONT)
                            .text_size(metrics::text_size_small())
                            .border_color(if position.is_some() {
                                theme.accent
                            } else {
                                theme.border
                            })
                            .text_color(if position.is_some() {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .child(SharedString::from(match position {
                                Some(position) => format!("{} {name}", position + 1),
                                None => name.clone(),
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(sheet) = this.schema_sheet.as_mut() {
                                    match sheet.picked.iter().position(|p| *p == target) {
                                        Some(at) => {
                                            sheet.picked.remove(at);
                                        }
                                        None => sheet.picked.push(target.clone()),
                                    }
                                    sheet.error = None;
                                }
                                cx.notify();
                            }))
                            .into_any_element()
                    })
                    .collect();
                body.push(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(chips)
                        .into_any_element(),
                );
                body.push(
                    toggle("sheet-unique", "Unique", sheet.unique)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(sheet) = this.schema_sheet.as_mut() {
                                sheet.unique = !sheet.unique;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            SheetKind::CreateTable => {
                body.push(labelled("Name", field(0, "table_name", cx)));
                let base = sheet.fields.len();
                for (at, column) in sheet.new_columns.iter().enumerate() {
                    let name_index = base + at * 2;
                    let key_on = column.key;
                    let null_on = column.nullable && !column.key;
                    body.push(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().w(metrics::scaled(140.)).child(text_field(
                                ("sheet-new-name", at),
                                &column.name,
                                InputTarget::SheetField(name_index),
                                sheet.focused == name_index,
                                Some("column"),
                                theme,
                                cx,
                            )))
                            .child(div().flex_1().min_w(px(0.)).child(text_field(
                                ("sheet-new-type", at),
                                &column.data_type,
                                InputTarget::SheetField(name_index + 1),
                                sheet.focused == name_index + 1,
                                Some("type"),
                                theme,
                                cx,
                            )))
                            .child(
                                toggle_owned(("sheet-new-key", at), "Key", key_on, theme).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        if let Some(column) = this
                                            .schema_sheet
                                            .as_mut()
                                            .and_then(|sheet| sheet.new_columns.get_mut(at))
                                        {
                                            column.key = !column.key;
                                        }
                                        cx.notify();
                                    }),
                                ),
                            )
                            .child(
                                toggle_owned(("sheet-new-null", at), "Null", null_on, theme)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(column) = this
                                            .schema_sheet
                                            .as_mut()
                                            .and_then(|sheet| sheet.new_columns.get_mut(at))
                                        {
                                            column.nullable = !column.nullable;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .into_any_element(),
                    );
                }
                body.push(
                    button("sheet-add-new-column", "+ Column", theme, false)
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(sheet) = this.schema_sheet.as_mut() {
                                sheet.new_columns.push(NewColumn::blank());
                                sheet.focused = sheet.field_count() - 2;
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
            SheetKind::DropColumn(_) | SheetKind::DropIndex(_) => {
                body.push(
                    div()
                        .text_color(theme.text_muted)
                        .child("This cannot be undone from here.")
                        .into_any_element(),
                );
            }
        }

        let (preview_text, preview_ok) = match &preview {
            Ok(statements) => (
                statements
                    .iter()
                    .map(|sql| format!("{sql};"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                true,
            ),
            Err(problem) => (problem.clone(), false),
        };

        let run_label = if sheet.running {
            "Running…"
        } else if destructive {
            "Drop"
        } else {
            "Run"
        };
        let run = button("sheet-run", run_label, theme, true)
            .when(destructive, |run| {
                run.bg(theme.danger).border_color(theme.danger)
            })
            .when(!can_run, |run| run.opacity(0.5).cursor_default())
            .on_click(cx.listener(|this, _, _, cx| this.run_schema_sheet(cx)));

        let scrim = if theme.is_light {
            gpui::rgba(0x00000044)
        } else {
            gpui::rgba(0x00000088)
        };

        Some(
            motion::dialog(
                "schema-sheet-in",
                div()
                    .id("schema-sheet-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .bg(scrim)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("schema-sheet")
                            .w(metrics::scaled(520.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .p_4()
                            .rounded_lg()
                            .bg(theme.elevated)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_lg()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(sheet.title())),
                            )
                            .children(body)
                            .child(caption("SQL", theme))
                            .child(
                                div()
                                    .p_2()
                                    .rounded_md()
                                    .bg(theme.background)
                                    .border_1()
                                    .border_color(theme.border)
                                    .font_family(metrics::MONO_FONT)
                                    .text_size(metrics::text_size_small())
                                    .text_color(if preview_ok {
                                        theme.text
                                    } else {
                                        theme.text_faint
                                    })
                                    .whitespace_normal()
                                    .child(SharedString::from(preview_text)),
                            )
                            .children(sheet.error.clone().map(|error| {
                                div()
                                    .text_color(theme.danger)
                                    .text_size(metrics::text_size_small())
                                    .child(SharedString::from(error))
                            }))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_2()
                                    .child(button("sheet-cancel", "Cancel", theme, false).on_click(
                                        cx.listener(|this, _, _, cx| this.close_schema_sheet(cx)),
                                    ))
                                    .child(run),
                            ),
                    ),
                metrics::scaled(96.),
            )
            .into_any_element(),
        )
    }
}

/// A checkbox-and-label, for toggles that live in a loop.
fn toggle_owned(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    on: bool,
    theme: &crate::theme::Theme,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .cursor_pointer()
        .text_size(metrics::text_size_small())
        .text_color(if on { theme.text } else { theme.text_muted })
        .child(
            div()
                .w(px(12.))
                .h(px(12.))
                .rounded(px(3.))
                .border_1()
                .border_color(if on { theme.accent } else { theme.border })
                .when(on, |check| check.bg(theme.accent)),
        )
        .child(label)
}
