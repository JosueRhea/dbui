//! End-to-end tests for the SQLite adapter, against a real database file.
//!
//! Unlike `live.rs` these are **not** opt-in and need nothing installed: the
//! engine is linked in and the database is a temp file this test makes and
//! deletes. That is the whole appeal of the third adapter -- everything the
//! other two can only prove against a running server, this proves on every
//! `cargo test`.

use dbui_domain::{
    ConnectionConfig, Driver, ObjectKind, Page, QueryOutcome, SortKey, TableRef, Value,
};
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

/// A query typed in the editor keeps at most QUERY_ROW_CAP rows and says there
/// were more, and the connection is fine for the next statement.
#[tokio::test]
async fn an_editor_query_stops_at_the_row_cap() {
    let db = open("row-cap").await;
    let cap = dbui_domain::ResultSet::QUERY_ROW_CAP;
    let token = dbui_driver::QueryToken::new();
    let sql = format!(
        "WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {}) \
         SELECT i FROM n",
        cap * 5
    );
    let result = db.execute_tracked(&sql, &token).await.expect("the query");
    let set = result.rows().expect("rows");
    assert_eq!(set.rows.len(), cap);
    assert!(set.truncated);

    let after = db.execute_tracked("SELECT 42", &token).await.expect("next");
    assert_eq!(after.rows().unwrap().rows[0].0[0].to_text(), "42");
}

/// The editor's session says when a typed `BEGIN` is still open.
#[tokio::test]
async fn the_editor_session_reports_its_transaction() {
    use dbui_domain::TransactionState;
    let db = open("transaction-state").await;
    let token = dbui_driver::QueryToken::new();
    db.execute_tracked("SELECT 1", &token).await.unwrap();
    assert_eq!(db.editor_transaction(), TransactionState::Idle);
    db.execute_tracked("BEGIN", &token).await.unwrap();
    assert_eq!(db.editor_transaction(), TransactionState::Open);
    db.execute_tracked("SELECT 1", &token).await.unwrap();
    assert_eq!(db.editor_transaction(), TransactionState::Open);
    db.execute_tracked("ROLLBACK", &token).await.unwrap();
    assert_eq!(db.editor_transaction(), TransactionState::Idle);
}

/// A table read, then given a column, then read again. The cached statement
/// kept the old column list while SQLite quietly re-prepared it with the new
/// one, so the second read panicked inside sqlx and came back with no rows.
#[tokio::test]
async fn a_table_given_a_column_still_reads() {
    let db = open("reshaped").await;
    let token = dbui_driver::QueryToken::new();
    let first = db
        .execute_tracked("SELECT * FROM people", &token)
        .await
        .unwrap();
    let (width, rows) = {
        let set = first.rows().unwrap();
        (set.columns.len(), set.rows.len())
    };
    assert!(rows > 0);
    db.table_rows(&TableRef::new("main", "people"), Page::first(), "", &[])
        .await
        .unwrap();

    db.execute("ALTER TABLE people ADD COLUMN extra TEXT")
        .await
        .unwrap();

    let again = db
        .execute_tracked("SELECT * FROM people", &token)
        .await
        .unwrap();
    let set = again.rows().unwrap();
    assert_eq!(set.columns.len(), width + 1);
    assert_eq!(set.rows.len(), rows, "the same rows, not none");
    let page = db
        .table_rows(&TableRef::new("main", "people"), Page::first(), "", &[])
        .await
        .unwrap();
    assert_eq!(page.columns.len(), width + 1);
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

/// The two statements the context menu offers have to be accepted as written.
/// SQLite has no `TRUNCATE`, so `truncate_sql` emits an unqualified `DELETE`
/// -- which its own docs point at and which optimises into the same thing.
#[tokio::test]
async fn generated_truncate_and_drop_are_accepted() {
    let db = open("ddl").await;
    let table = TableRef::new("main", "scratch");
    let quoted = table.quoted(Driver::Sqlite);

    db.execute(&format!("CREATE TABLE {quoted} (id INTEGER PRIMARY KEY)"))
        .await
        .expect("create");
    db.execute(&format!("INSERT INTO {quoted} (id) VALUES (1), (2)"))
        .await
        .expect("seed");

    db.execute(&dbui_driver::truncate_sql(Driver::Sqlite, &table))
        .await
        .expect("truncate");
    assert_eq!(db.row_count(&table, "").await.expect("count"), 0);

    db.execute(&dbui_driver::drop_sql(
        Driver::Sqlite,
        &table,
        dbui_domain::TableKind::Table,
    ))
    .await
    .expect("drop");
    assert!(db.row_count(&table, "").await.is_err(), "the table is gone");

    // A view needs DROP VIEW, which is why `drop_sql` takes the kind.
    db.execute(&dbui_driver::drop_sql(
        Driver::Sqlite,
        &TableRef::new("main", "people_view"),
        dbui_domain::TableKind::View,
    ))
    .await
    .expect("drop view");
}

/// A new row's values are typed from the column, not sent as text -- the same
/// bug that made Postgres refuse an INSERT against a bigint key.
#[tokio::test]
async fn an_all_defaults_insert_lets_the_key_fire() {
    let db = open("defaults").await;
    let table = db.people();

    // `id` is INTEGER PRIMARY KEY, which is SQLite's rowid alias: leaving it
    // out of the statement is what lets it assign one.
    db.apply_changes(
        &table,
        &RowBatch {
            inserts: vec![RowInsert {
                values: vec![("name".into(), Value::Text("Katherine".into()))],
            }],
            updates: Vec::new(),
            deletes: Vec::new(),
        },
    )
    .await
    .expect("insert");

    let rows = db
        .table_rows(&table, Page::first(), "name = 'Katherine'", &[])
        .await
        .expect("the new row");
    assert_eq!(rows.rows.len(), 1);
    assert!(
        matches!(rows.rows[0].get(0), Some(Value::Int(n)) if *n > 5),
        "the key was assigned, not written as text"
    );
}

/// SQLite's grammar has no `DEFAULT` in a value list and no `SET c = DEFAULT`
/// at all, so both spellings used to reach the parser as `near "DEFAULT":
/// syntax error`. The insert now leaves the column out, and the update is
/// refused with a reason instead of being sent.
#[tokio::test]
async fn a_default_valued_column_is_omitted_by_the_insert_and_refused_by_the_update() {
    let db = open("defaulted").await;
    db.execute(
        "CREATE TABLE notes (
             id   INTEGER PRIMARY KEY,
             body TEXT NOT NULL DEFAULT 'unwritten'
         )",
    )
    .await
    .expect("create");
    let table = TableRef::new("main", "notes");

    db.apply_changes(
        &table,
        &RowBatch {
            inserts: vec![
                RowInsert {
                    values: vec![
                        ("id".into(), Value::Int(1)),
                        ("body".into(), Value::Default),
                    ],
                },
                // Nothing but defaults: the all-defaults spelling, which
                // SQLite does accept.
                RowInsert {
                    values: vec![("body".into(), Value::Default)],
                },
            ],
            updates: Vec::new(),
            deletes: Vec::new(),
        },
    )
    .await
    .expect("the column's default fires");

    let rows = db
        .table_rows(&table, Page::first(), "body = 'unwritten'", &[])
        .await
        .expect("the new rows");
    assert_eq!(rows.rows.len(), 2, "both rows took the column's default");

    let error = db
        .apply_changes(
            &table,
            &RowBatch::of_updates(vec![RowUpdate {
                pk: vec![("id".into(), Value::Int(1))],
                changes: vec![("body".into(), Value::Default)],
            }]),
        )
        .await
        .expect_err("SQLite cannot put a column's default back");
    assert!(
        error.to_string().contains("default"),
        "the refusal says why, rather than quoting the parser: {error}"
    );
    assert!(
        !error.to_string().contains("syntax error"),
        "and it never reached the parser: {error}"
    );
}

#[tokio::test]
async fn closing_is_idempotent() {
    let db = open("closing").await;
    db.close().await;
    db.close().await;
    assert!(db.ping().await.is_err());
}

// -- stopping a statement mid-run -----------------------------------------

/// Counts to a number far past anything that finishes in a test's lifetime,
/// in one step's worth of result: exactly the shape of query that dropping a
/// future cannot stop, because the work is all inside one `sqlite3_step`.
const RUNAWAY: &str = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n) \
                       SELECT count(*) FROM (SELECT i FROM n LIMIT 5000000000)";

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_statement_stops_and_frees_the_connection() {
    let db = open("cancel").await;
    let token = dbui_driver::QueryToken::new();

    let started = std::time::Instant::now();
    let run = db.execute_tracked(RUNAWAY, &token);
    let stop = async {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        db.cancel(&token).await
    };
    let (result, told) = tokio::join!(run, stop);

    assert!(told.expect("cancel"), "a running statement was told");
    assert!(
        result.is_err(),
        "the statement ended with an error, not rows"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "and ended promptly, not when the count ran out: {:?}",
        started.elapsed()
    );

    // The one connection is free again, and the stale handler left on it
    // does not stop the next statement.
    let after = db.execute("SELECT 1").await.expect("the next query runs");
    assert!(matches!(after.outcome, QueryOutcome::Rows(_)));
}

#[tokio::test(flavor = "multi_thread")]
async fn cancelling_a_finished_statement_is_a_no_op() {
    let db = open("cancel-late").await;
    let token = dbui_driver::QueryToken::new();
    db.execute_tracked("SELECT 1", &token).await.expect("runs");
    assert!(!db.cancel(&token).await.expect("cancel"));
    db.execute("SELECT 2")
        .await
        .expect("and nothing after it is stopped");
}

/// The structure editor's statements on SQLite: what it can do in place --
/// add, rename, drop, index -- runs; what it cannot is refused before it runs.
#[tokio::test]
async fn the_structure_editor_statements_run_on_sqlite() {
    use dbui_domain::ddl::{self, ColumnSpec};
    let db = open("structure-editor").await;
    let people = TableRef::new("main", "people");
    let run = |statements: Vec<String>| async {
        for sql in statements {
            db.execute(&sql)
                .await
                .unwrap_or_else(|error| panic!("{sql}\n{error}"));
        }
    };

    let motto = ColumnSpec {
        name: "motto".into(),
        data_type: "TEXT".into(),
        nullable: true,
        default: Some("'hi'".into()),
    };
    run(ddl::add_column(Driver::Sqlite, &people, &motto).unwrap()).await;
    let columns = db.columns(&people).await.unwrap();
    let added = columns.iter().find(|c| c.name == "motto").expect("added");
    assert_eq!(added.default.as_deref(), Some("'hi'"));

    let mut renamed = ColumnSpec::of(added);
    renamed.name = "slogan".into();
    run(ddl::alter_column(Driver::Sqlite, &people, added, &renamed).unwrap()).await;
    let columns = db.columns(&people).await.unwrap();
    let slogan = columns
        .iter()
        .find(|c| c.name == "slogan")
        .expect("renamed");

    let mut retyped = ColumnSpec::of(slogan);
    retyped.data_type = "INTEGER".into();
    assert!(ddl::alter_column(Driver::Sqlite, &people, slogan, &retyped).is_err());

    run(ddl::create_index(
        Driver::Sqlite,
        &people,
        "by_slogan",
        &["slogan".into()],
        false,
    )
    .unwrap())
    .await;
    let indexes = db.indexes(&people).await.unwrap();
    let made = indexes
        .iter()
        .find(|i| i.name == "by_slogan")
        .expect("listed");
    assert_eq!(made.columns, ["slogan"]);
    run(ddl::drop_index(Driver::Sqlite, &people, "by_slogan")).await;
    assert!(db
        .indexes(&people)
        .await
        .unwrap()
        .iter()
        .all(|i| i.name != "by_slogan"));

    run(ddl::drop_column(Driver::Sqlite, &people, "slogan")).await;
    assert!(db
        .columns(&people)
        .await
        .unwrap()
        .iter()
        .all(|c| c.name != "slogan"));
}

/// A trigger is listed under the table it is on, and its definition is the
/// statement that created it.
#[tokio::test]
async fn triggers_are_listed_with_their_definitions() {
    let db = open("triggers").await;
    db.execute(
        "CREATE TRIGGER people_trim AFTER INSERT ON people
         BEGIN UPDATE people SET name = trim(name) WHERE id = NEW.id; END",
    )
    .await
    .expect("create trigger");

    let catalog = db.catalog().await.expect("catalog");
    let trigger = catalog
        .objects_of("main", ObjectKind::Trigger)
        .find(|object| object.name == "people_trim")
        .cloned()
        .expect("the trigger is listed");
    assert_eq!(trigger.detail.as_deref(), Some("people"));

    let body = db.definition(&trigger).await.expect("definition");
    assert!(body.starts_with("CREATE TRIGGER people_trim"), "{body}");
    assert!(body.trim_end().ends_with("END;"), "{body}");
}
