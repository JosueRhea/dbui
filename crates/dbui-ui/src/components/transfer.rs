//! Rows in and out of files: export to CSV / JSON / SQL, import from CSV.
//!
//! Export is the clipboard's formats written to disk, with one difference
//! that matters: from a table tab it writes the *table*, not the page on
//! screen -- every row the filter matches, in the grid's order and with the
//! grid's columns, read a page at a time so a large table never has to fit
//! in memory. From a query tab it writes the result.
//!
//! Import stages rather than writes. A CSV's rows arrive as pending inserts,
//! exactly as pasted rows do, so they can be looked over in the change
//! bubble -- and ⌘S commits them in one transaction, or ⌘Z throws them away.

use crate::root::{DbUi, Status};
use crate::row_export::{self, ExportWriter, RowFormat};
use crate::tabs::WorkspaceTab;
use dbui_app::commands;
use dbui_app::domain::{ColumnInfo, Value};
use gpui::{Context, PathPromptOptions};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

/// An export on its way to a file.
struct FileSink {
    writer: ExportWriter<BufWriter<File>>,
    path: PathBuf,
}

impl dbui_app::PageSink for FileSink {
    fn page(&mut self, columns: &[ColumnInfo], rows: Vec<Vec<Value>>) -> Result<(), String> {
        self.writer
            .page(columns, &rows)
            .map_err(|error| format!("Could not write {}: {error}", self.path.display()))
    }

    fn finish(&mut self) -> Result<(), String> {
        self.writer
            .finish()
            .map_err(|error| format!("Could not write {}: {error}", self.path.display()))
    }
}

/// Where the save and open sheets start: Downloads, where a file someone is
/// about to hand to another program usually goes, else home.
fn start_directory() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let downloads = home.join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        home
    }
}

impl DbUi {
    /// "Export as …": ask where, then write.
    pub(crate) fn export_rows(&mut self, format: RowFormat, cx: &mut Context<Self>) {
        let name = match self.tabs.active() {
            Some(WorkspaceTab::Table { table, .. }) => table.name.clone(),
            Some(WorkspaceTab::Sql { .. })
                if self.tabs.active().and_then(|t| t.result()).is_some() =>
            {
                "query".to_string()
            }
            _ => {
                self.status = Status::info("Open a table or run a query to export it");
                cx.notify();
                return;
            }
        };
        let suggested = format!("{name}.{}", format.extension());
        let chosen = cx.prompt_for_new_path(&start_directory(), Some(&suggested));
        cx.spawn(async move |this, cx| {
            // Cancelled, or the sheet could not open: either way, nothing to do.
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            this.update(cx, |this, cx| this.export_to_path(path, format, cx))
                .ok();
        })
        .detach();
    }

    /// Write the front tab's rows to `path` -- the half of an export after
    /// the save sheet, and what the tests drive.
    pub(crate) fn export_to_path(
        &mut self,
        path: PathBuf,
        format: RowFormat,
        cx: &mut Context<Self>,
    ) {
        let file = match File::create(&path) {
            Ok(file) => file,
            Err(error) => {
                self.status =
                    Status::error(format!("Could not create {}: {error}", path.display()));
                cx.notify();
                return;
            }
        };
        let driver_kind = self
            .active_driver_kind()
            .unwrap_or(dbui_app::domain::Driver::Postgres);
        let Some(tab) = self.tabs.active() else {
            return;
        };
        // The grid's columns, in the grid's order: an export that brings back
        // the columns the user hid, or shuffles the ones they dragged into
        // place, is not what they were looking at.
        let shown: Option<Vec<String>> = tab.result().map(|_| {
            tab.display_columns()
                .into_iter()
                .map(|(_, info)| info.name.clone())
                .collect()
        });
        let writer = ExportWriter::new(
            BufWriter::new(file),
            format,
            driver_kind,
            tab.table_ref().cloned(),
            shown,
        );
        let sink = FileSink {
            writer,
            path: path.clone(),
        };

        let task = match tab {
            WorkspaceTab::Table {
                table,
                where_clause,
                sort,
                ..
            } => {
                let Some(driver) = self.workspace.active_driver() else {
                    self.status = Status::error("Not connected");
                    cx.notify();
                    return;
                };
                commands::export_table(
                    &self.runtime,
                    driver,
                    table.clone(),
                    where_clause.clone(),
                    sort.clone(),
                    sink,
                )
            }
            WorkspaceTab::Sql { .. } => {
                let Some(view) = tab.result() else {
                    return;
                };
                commands::export_rows(
                    &self.runtime,
                    view.set.columns.clone(),
                    view.set.rows.iter().map(|row| row.0.clone()).collect(),
                    sink,
                )
            }
        };

        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        self.status = Status::busy(format!("Exporting to {file_name}…"));
        cx.notify();
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.status = match landed {
                    Some(Ok(rows)) => {
                        let plural = if rows == 1 { "row" } else { "rows" };
                        Status::info(format!("Exported {rows} {plural} to {file_name}"))
                    }
                    Some(Err(message)) => Status::error(message),
                    None => Status::error("The export stopped before it finished"),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// "Import CSV…": ask which file, then stage its rows.
    pub(crate) fn import_csv(&mut self, cx: &mut Context<Self>) {
        if !matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Table {
                result: Some(_),
                ..
            })
        ) {
            self.status = Status::info("Open the table to import into first");
            cx.notify();
            return;
        }
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |this, cx| this.import_csv_from_path(&path, cx))
                .ok();
        })
        .detach();
    }

    /// Stage a CSV file's rows as inserts into the front table.
    pub(crate) fn import_csv_from_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.status = Status::error(format!("Could not read {}: {error}", path.display()));
                cx.notify();
                return;
            }
        };
        // A `.tsv` is the clipboard's own shape; anything else is CSV.
        let is_tsv = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("tsv"));
        let parsed = if is_tsv {
            row_export::parse_tsv(&text)
        } else {
            row_export::parse_csv(&text)
        };
        let Some(parsed) = parsed else {
            self.status = Status::error(
                "That file is not a table: it needs a header row and at least one row under it",
            );
            cx.notify();
            return;
        };
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        self.stage_named_rows(parsed, &format!("Imported from {file_name}"), cx);
    }
}
