//! End-to-end tests for the SQLite adapter, against a real database file.
//!
//! Unlike `live.rs` these are **not** opt-in and need nothing installed: the
//! engine is linked in and the database is a temp file this test makes and
//! deletes. That is the whole appeal of the third adapter -- everything the
//! other two can only prove against a running server, this proves on every
//! `cargo test`.

use dbui_domain::{ConnectionConfig, Driver, Page, QueryOutcome, SortKey, TableRef, Value};
use dbui_driver::{DatabaseDriver, RowBatch, RowDelete, RowInsert, RowUpdate};
use std::path::PathBuf;
use std::sync::Arc;

/// A database file of its own per test, so they can run in parallel.
struct TempDb {
    path: PathBuf,
    db: Arc<dyn DatabaseDriver>,
}

impl std::ops::Deref for TempDb {
    type Target = dyn DatabaseDriver;
    fn deref(&self) -> &Self::Target {
        self.db.as_ref()
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

async fn open(name: &str) -> TempDb {
    let mut path = std::env::temp_dir();
    path.push(format!("dbui-sqlite-{}-{name}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // `create_if_missing` is off in the adapter, so the file has to exist
    // before it will open -- which is the behaviour a typo'd path relies on.
    std::fs::File::create(&path).expect("create the database file");

    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.name = name.to_string();
    config.database = path.to_string_lossy().to_string();

    let db = dbui_driver::connect(&config).await.expect("connect");
    let fixture = TempDb { path, db };
    fixture.seed().await;
    fixture
}

impl TempDb {
    async fn seed(&self) {
        for sql in [
            "CREATE TABLE people (
                 id       INTEGER PRIMARY KEY,
                 name     TEXT NOT NULL,
                 nickname TEXT,
                 score    NUMERIC,
                 active   BOOLEAN,
                 meta     JSON,
                 seen_at  DATETIME,
                 blob     BLOB
             )",
            "INSERT INTO people (id, name, nickname, score, active, meta, seen_at) VALUES
                 (1, 'Ada',     NULL,      0.10,  1, '{\"a\":1}', '2024-01-01 00:00:00'),
                 (2, 'Grace',   'Amazing', 99.95, 0, NULL,        '2024-01-02 00:00:00'),
                 (3, 'Alan',    NULL,      NULL,  1, NULL,        NULL),
                 (4, 'Edsger',  NULL,      -1.50, 1, NULL,        NULL),
                 (5, 'Barbara', NULL,      3.00,  1, NULL,        NULL)",
            "CREATE VIEW people_view AS SELECT id, name FROM people",
        ] {
            self.execute(sql).await.expect("seed");
        }
    }

    fn people(&self) -> TableRef {
        TableRef::new("main", "people")
    }
}

#[tokio::test]
async fn a_file_database_connects_and_reports_its_version() {
    let db = open("connect").await;
    db.ping().await.expect("ping");
    assert!(db.server_version().starts_with("SQLite "));
    assert_eq!(db.driver(), Driver::Sqlite);
}

/// A path that is not there is a typo worth reporting, not a reason to make an
/// empty database and look like it worked.
#[tokio::test]
async fn a_missing_file_is_an_error_not_a_new_database() {
    let mut path = std::env::temp_dir();
    path.push(format!("dbui-sqlite-absent-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.name = "absent".into();
    config.database = path.to_string_lossy().to_string();

    assert!(dbui_driver::connect(&config).await.is_err());
    assert!(!path.exists(), "and nothing was created");
}

#[tokio::test]
async fn an_empty_path_says_what_is_missing() {
    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.name = "blank".into();
    let Err(error) = dbui_driver::connect(&config).await else {
        panic!("a blank path should be refused");
    };
    assert!(error.to_string().contains("path"), "got: {error}");
}

#[tokio::test]
async fn the_catalog_lists_tables_and_views_under_one_schema() {
    let db = open("catalog").await;
    let catalog = db.catalog().await.expect("catalog");

    assert_eq!(catalog.schemas.len(), 1, "SQLite has one schema");
    let schema = &catalog.schemas[0];
    assert_eq!(schema.name, "main");

    let people = schema
        .tables
        .iter()
        .find(|table| table.name == "people")
        .expect("people");
    assert_eq!(people.kind, dbui_domain::TableKind::Table);

    let view = schema
        .tables
        .iter()
        .find(|table| table.name == "people_view")
        .expect("the view");
    assert!(view.kind.is_view());

    assert!(
        !schema.tables.iter().any(|t| t.name.starts_with("sqlite_")),
        "the engine's own bookkeeping tables are not the user's"
    );
}

#[tokio::test]
async fn columns_carry_nullability_and_the_primary_key() {
    let db = open("columns").await;
    let columns = db.columns(&db.people()).await.expect("columns");

    let id = columns.iter().find(|c| c.name == "id").expect("id");
    assert!(id.is_primary_key);
    let name = columns.iter().find(|c| c.name == "name").expect("name");
    assert!(!name.nullable, "declared NOT NULL");
    let nickname = columns
        .iter()
        .find(|c| c.name == "nickname")
        .expect("nickname");
    assert!(nickname.nullable);
    assert!(!nickname.is_primary_key);
}

/// SQLite stores five classes and a *declared* type that is only an affinity,
/// so the declared type is what decides how a value is presented.
#[tokio::test]
async fn values_decode_by_storage_class_and_declared_type() {
    let db = open("values").await;
    let rows = db
        .table_rows(&db.people(), Page::first(), "", &[])
        .await
        .expect("rows");

    let index = |name: &str| rows.column_index(name).expect("column");
    let at = |row: usize, name: &str| rows.rows[row].get(index(name)).expect("cell").clone();

    assert_eq!(at(0, "id"), Value::Int(1));
    assert_eq!(at(0, "name"), Value::Text("Ada".into()));
    assert_eq!(at(0, "nickname"), Value::Null);
    // A BOOLEAN column holds 0/1; the declared type is the only thing that
    // says it meant true/false.
    assert_eq!(at(0, "active"), Value::Bool(true));
    assert_eq!(at(1, "active"), Value::Bool(false));
    // SQLite has no exact numeric type: a NUMERIC column stores an IEEE
    // double, and reporting it as a decimal would claim an exactness the file
    // does not have.
    assert_eq!(at(1, "score"), Value::Float(99.95));
    // sqlx reports the declared type only for spellings it knows. DATETIME is
    // one; JSON is not, so that column decodes as the text it is stored as.
    assert!(matches!(at(0, "seen_at"), Value::Temporal(_)));
    assert_eq!(at(0, "meta"), Value::Text(r#"{"a":1}"#.into()));
    assert_eq!(at(2, "score"), Value::Null);
}

#[tokio::test]
async fn paging_in_key_order_sees_every_row_once() {
    let db = open("paging").await;
    let key = vec!["id".to_string()];
    let order = dbui_domain::order_for(None, &key);

    let mut seen = Vec::new();
    for offset in 0..5u64 {
        let rows = db
            .table_rows(&db.people(), Page { limit: 1, offset }, "", &order)
            .await
            .expect("one row");
        assert_eq!(rows.rows.len(), 1);
        seen.push(rows.rows[0].get(0).expect("id").to_text());
    }
    assert_eq!(seen, vec!["1", "2", "3", "4", "5"]);
}

#[tokio::test]
async fn a_sort_is_applied_by_the_engine() {
    let db = open("sorting").await;
    let order = dbui_domain::order_for(Some(&SortKey::desc("name")), &["id".to_string()]);
    let rows = db
        .table_rows(&db.people(), Page::first(), "", &order)
        .await
        .expect("sorted");

    let names: Vec<String> = rows
        .rows
        .iter()
        .map(|row| row.get(1).expect("name").to_text())
        .collect();
    let mut expected = names.clone();
    expected.sort();
    expected.reverse();
    assert_eq!(names, expected);
}

#[tokio::test]
async fn a_batch_commits_inserts_edits_and_deletions_together() {
    let db = open("batch").await;
    let table = db.people();

    let affected = db
        .apply_changes(
            &table,
            &RowBatch {
                inserts: vec![RowInsert {
                    values: vec![
                        ("id".into(), Value::Int(6)),
                        ("name".into(), Value::Text("Katherine".into())),
                    ],
                }],
                updates: vec![RowUpdate {
                    pk: vec![("id".into(), Value::Int(1))],
                    changes: vec![("nickname".into(), Value::Text("Lovelace".into()))],
                }],
                deletes: vec![RowDelete {
                    pk: vec![("id".into(), Value::Int(5))],
                }],
            },
        )
        .await
        .expect("mixed batch");
    assert_eq!(affected, 3);

    assert_eq!(db.row_count(&table, "").await.expect("count"), 5);
    let ada = db
        .table_rows(&table, Page::first(), "id = 1", &[])
        .await
        .expect("ada");
    assert_eq!(
        ada.rows[0].get(2).map(|v| v.to_text()),
        Some("Lovelace".into())
    );
}

/// One failing statement takes the whole batch with it.
#[tokio::test]
async fn a_failing_statement_rolls_the_batch_back() {
    let db = open("rollback").await;
    let table = db.people();

    let err = db
        .apply_changes(
            &table,
            &RowBatch {
                // id 1 is taken.
                inserts: vec![RowInsert {
                    values: vec![
                        ("id".into(), Value::Int(1)),
                        ("name".into(), Value::Text("Clash".into())),
                    ],
                }],
                updates: vec![RowUpdate {
                    pk: vec![("id".into(), Value::Int(2))],
                    changes: vec![("nickname".into(), Value::Text("should-not-stick".into()))],
                }],
                deletes: Vec::new(),
            },
        )
        .await
        .expect_err("duplicate key");
    assert!(!err.to_string().is_empty());

    assert_eq!(db.row_count(&table, "").await.expect("count"), 5);
    let grace = db
        .table_rows(&table, Page::first(), "id = 2", &[])
        .await
        .expect("grace");
    assert_eq!(
        grace.rows[0].get(2).map(|v| v.to_text()),
        Some("Amazing".into()),
        "the update alongside it rolled back too"
    );
}

#[tokio::test]
async fn foreign_keys_are_read_for_single_column_references() {
    let db = open("fks").await;
    db.execute(
        "CREATE TABLE orders (
             id        INTEGER PRIMARY KEY,
             person_id INTEGER NOT NULL REFERENCES people (id)
         )",
    )
    .await
    .expect("create");

    let columns = db
        .columns(&TableRef::new("main", "orders"))
        .await
        .expect("columns");
    let person = columns
        .iter()
        .find(|c| c.name == "person_id")
        .expect("person_id");
    let key = person.references.as_ref().expect("references people");
    assert_eq!(key.references.name, "people");
    assert_eq!(key.references_column, "id");

    let id = columns.iter().find(|c| c.name == "id").expect("id");
    assert!(id.references.is_none());
}

/// A composite key cannot be followed from one cell, so it is not reported.
#[tokio::test]
async fn a_composite_foreign_key_is_not_reported() {
    let db = open("composite").await;
    db.execute("CREATE TABLE pairs (a INTEGER, b INTEGER, PRIMARY KEY (a, b))")
        .await
        .expect("parent");
    db.execute(
        "CREATE TABLE pair_refs (
             id INTEGER PRIMARY KEY,
             a  INTEGER NOT NULL,
             b  INTEGER NOT NULL,
             FOREIGN KEY (a, b) REFERENCES pairs (a, b)
         )",
    )
    .await
    .expect("child");

    let columns = db
        .columns(&TableRef::new("main", "pair_refs"))
        .await
        .expect("columns");
    assert!(columns.iter().all(|c| c.references.is_none()));
}

/// Nothing binds identifiers as parameters, so a hostile table name has to
/// survive quoting rather than end the identifier early.
#[tokio::test]
async fn a_hostile_table_name_survives_quoting() {
    let db = open("hostile").await;
    let table = TableRef::new("main", "we\"ird; DROP TABLE people; --");
    let quoted = table.quoted(Driver::Sqlite);

    db.execute(&format!("CREATE TABLE {quoted} (id INTEGER)"))
        .await
        .expect("create");
    db.execute(&format!("INSERT INTO {quoted} (id) VALUES (1)"))
        .await
        .expect("insert");

    assert_eq!(db.row_count(&table, "").await.expect("count"), 1);
    assert_eq!(
        db.row_count(&db.people(), "").await.expect("people"),
        5,
        "and the table it tried to drop is still there"
    );
}

#[tokio::test]
async fn a_select_returns_rows_and_a_write_returns_a_count() {
    let db = open("execute").await;

    let selected = db.execute("SELECT * FROM people").await.expect("select");
    assert!(matches!(selected.outcome, QueryOutcome::Rows(_)));

    let written = db
        .execute("UPDATE people SET nickname = 'x' WHERE id = 1")
        .await
        .expect("update");
    assert_eq!(written.outcome, QueryOutcome::Affected(1));
}

/// A query that matched nothing still has to carry its headers, or the grid
/// looks broken rather than empty.
#[tokio::test]
async fn an_empty_result_still_has_its_headers() {
    let db = open("headers").await;
    let result = db
        .execute("SELECT id, name FROM people WHERE 1 = 0")
        .await
        .expect("select");
    let QueryOutcome::Rows(set) = result.outcome else {
        panic!("rows");
    };
    assert!(set.rows.is_empty());
    assert_eq!(set.columns.len(), 2);
}

/// A read-only connection is enforced by the engine, not just by the UI.
#[tokio::test]
async fn a_read_only_connection_refuses_writes_at_the_engine() {
    let db = open("readonly-seed").await;
    let path = db.path.to_string_lossy().to_string();
    db.close().await;

    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.name = "read only".into();
    config.database = path;
    config.read_only = true;

    let ro = dbui_driver::connect(&config).await.expect("connect");
    assert!(ro.execute("SELECT * FROM people").await.is_ok());
    assert!(
        ro.execute("DELETE FROM people").await.is_err(),
        "the file is opened read-only, so the engine refuses it too"
    );
}

#[tokio::test]
async fn closing_is_idempotent() {
    let db = open("closing").await;
    db.close().await;
    db.close().await;
    assert!(db.ping().await.is_err());
}
