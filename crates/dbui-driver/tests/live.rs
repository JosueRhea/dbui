//! End-to-end tests against real PostgreSQL and MySQL servers.
//!
//! These are the only tests that prove the introspection SQL parses, the type
//! decoders match what the wire actually sends, and the generated statements
//! are accepted. None of that can be faked convincingly, so nothing here is
//! mocked.
//!
//! They are **opt-in**, because a checkout with no servers running should still
//! be `cargo test`-clean:
//!
//! ```sh
//! docker compose up -d          # from the repo root
//! DBUI_LIVE_TESTS=1 cargo test -p dbui-driver
//! ```
//!
//! Without `DBUI_LIVE_TESTS` each test prints why it did nothing and passes.
//! Connection details default to the compose file's and can be overridden with
//! `DBUI_PG_HOST`, `DBUI_MYSQL_PORT`, and so on.
//!
//! **Isolation:** cargo runs these in parallel, so every test builds its
//! fixtures in a schema named after itself and never touches another's. A
//! single shared fixture looks tidier right up until one test's `DELETE`
//! changes another's row count, and until two of them race to create it.

use dbui_domain::{
    ConnectionConfig, Driver, Page, QueryOutcome, TableRef, TlsMode, Value,
};
use dbui_driver::{DatabaseDriver, DriverError};
use std::sync::Arc;

// -- harness ---------------------------------------------------------------

fn env_or(driver: Driver, key: &str, fallback: &str) -> String {
    let prefix = match driver {
        Driver::Postgres => "DBUI_PG",
        Driver::MySql => "DBUI_MYSQL",
        // SQLite needs no server, so it has no environment to read; its tests
        // are at the bottom of this file and always run.
        Driver::Sqlite => "DBUI_SQLITE",
    };
    std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| fallback.to_string())
}

fn config(driver: Driver) -> ConnectionConfig {
    let mut config = ConnectionConfig::new(driver);
    config.host = env_or(driver, "HOST", "127.0.0.1");
    // The compose file's ports, deliberately not the engines' defaults, so
    // these tests never talk to a server the machine was already running.
    config.port = env_or(
        driver,
        "PORT",
        match driver {
            Driver::Postgres => "55432",
            Driver::MySql => "53306",
            Driver::Sqlite => "0",
        },
    )
    .parse()
    .expect("port must be a number");
    config.username = env_or(
        driver,
        "USER",
        match driver {
            Driver::Postgres => "postgres",
            Driver::MySql => "root",
            Driver::Sqlite => "",
        },
    );
    config.password = env_or(driver, "PASSWORD", "dbui");
    config.database = env_or(driver, "DATABASE", "dbui_test");
    // The compose servers are plaintext on loopback; insisting on TLS here
    // would test the certificate setup rather than the adapter.
    config.tls = TlsMode::Disable;
    config
}

/// A connection plus the private schema this test's fixtures live in.
pub struct Fixture {
    db: Arc<dyn DatabaseDriver>,
    schema: String,
}

impl Fixture {
    fn people(&self) -> TableRef {
        TableRef::new(&self.schema, "people")
    }

    fn table(&self, name: &str) -> TableRef {
        TableRef::new(&self.schema, name)
    }

    fn schema(&self) -> &str {
        &self.schema
    }
}

/// So a fixture can be used as the driver it wraps: `fx.execute(...)`.
impl std::ops::Deref for Fixture {
    type Target = dyn DatabaseDriver;

    fn deref(&self) -> &Self::Target {
        self.db.as_ref()
    }
}

/// Connect and lay down this test's fixtures, or explain why it is a no-op.
///
/// Returning `None` rather than failing keeps `cargo test` green on a machine
/// with no servers, which is the state most checkouts are in. If the flag *is*
/// set, an unreachable server is a failure -- silently skipping then would
/// defeat the point of asking for these to run.
async fn live(driver: Driver, test_name: &str) -> Option<Fixture> {
    if std::env::var("DBUI_LIVE_TESTS").is_err() {
        eprintln!("skipping {driver} live test: set DBUI_LIVE_TESTS=1 to run it");
        return None;
    }

    let config = config(driver);
    let db = match dbui_driver::connect(&config).await {
        Ok(db) => db,
        Err(error) => panic!(
            "DBUI_LIVE_TESTS is set but {driver} at {} is unreachable: {error}",
            config.summary()
        ),
    };

    // Identifiers cap at 63 bytes in Postgres and 64 in MySQL, and test names
    // are long. The prefix plus a truncated name stays inside both.
    let schema = format!("dbui_{}", &test_name[..test_name.len().min(48)]);

    let fixture = Fixture { db, schema };
    fixture.seed().await;
    Some(fixture)
}

impl Fixture {
    /// Build this test's schema from scratch.
    ///
    /// Dropped and recreated rather than reused, so a run always starts from
    /// the same five rows no matter what the last one left behind.
    async fn seed(&self) {
        let schema = &self.schema;
        let quoted = Driver::quote_identifier(self.driver(), schema);

        let statements: Vec<String> = match self.driver() {
            Driver::Sqlite => unreachable!("the file-based tests seed themselves"),
            Driver::Postgres => vec![
                format!("DROP SCHEMA IF EXISTS {quoted} CASCADE"),
                format!("CREATE SCHEMA {quoted}"),
                format!(
                    "CREATE TABLE {quoted}.people (
                         id         bigint PRIMARY KEY,
                         name       text NOT NULL,
                         nickname   text,
                         score      numeric(10,2),
                         active     boolean NOT NULL DEFAULT true,
                         tags       text[],
                         meta       jsonb,
                         created_at timestamptz
                     )"
                ),
                format!(
                    "INSERT INTO {quoted}.people
                         (id, name, nickname, score, active, tags, meta, created_at)
                     VALUES
                       (1, 'Ada',     NULL,      0.10,  true,  ARRAY['one','two'], '{{\"a\":1}}', '2024-01-01T00:00:00Z'),
                       (2, 'Grace',   'Amazing', 99.95, false, ARRAY['three'],     NULL,          '2024-01-02T00:00:00Z'),
                       (3, 'Alan',    NULL,      NULL,  true,  NULL,               NULL,          NULL),
                       (4, 'Edsger',  NULL,      -1.50, true,  NULL,               NULL,          NULL),
                       (5, 'Barbara', NULL,      3.00,  true,  NULL,               NULL,          NULL)"
                ),
                format!(
                    "CREATE VIEW {quoted}.people_view AS SELECT id, name FROM {quoted}.people"
                ),
            ],
            Driver::MySql => vec![
                // MySQL has no schema layer: a database *is* the namespace.
                format!("DROP DATABASE IF EXISTS {quoted}"),
                format!("CREATE DATABASE {quoted}"),
                format!(
                    "CREATE TABLE {quoted}.people (
                         id         BIGINT PRIMARY KEY,
                         name       VARCHAR(64) NOT NULL,
                         nickname   VARCHAR(64),
                         score      DECIMAL(10,2),
                         active     TINYINT(1) NOT NULL DEFAULT 1,
                         meta       JSON,
                         created_at DATETIME
                     )"
                ),
                format!(
                    "INSERT INTO {quoted}.people
                         (id, name, nickname, score, active, meta, created_at)
                     VALUES
                       (1, 'Ada',     NULL,      0.10,  1, '{{\"a\":1}}', '2024-01-01 00:00:00'),
                       (2, 'Grace',   'Amazing', 99.95, 0, NULL,          '2024-01-02 00:00:00'),
                       (3, 'Alan',    NULL,      NULL,  1, NULL,          NULL),
                       (4, 'Edsger',  NULL,      -1.50, 1, NULL,          NULL),
                       (5, 'Barbara', NULL,      3.00,  1, NULL,          NULL)"
                ),
                format!(
                    "CREATE VIEW {quoted}.people_view AS SELECT id, name FROM {quoted}.people"
                ),
            ],
        };

        for sql in statements {
            self.execute(&sql)
                .await
                .unwrap_or_else(|error| panic!("fixture failed: {error}\n{sql}"));
        }
    }
}

/// Define one test body and run it against both engines.
macro_rules! both_engines {
    ($name:ident, $body:expr) => {
        mod $name {
            use super::*;

            #[tokio::test]
            async fn postgres() {
                if let Some(fixture) = live(Driver::Postgres, stringify!($name)).await {
                    let body: fn(Fixture) -> _ = $body;
                    body(fixture).await;
                }
            }

            #[tokio::test]
            async fn mysql() {
                if let Some(fixture) = live(Driver::MySql, stringify!($name)).await {
                    let body: fn(Fixture) -> _ = $body;
                    body(fixture).await;
                }
            }
        }
    };
}

// -- the tests -------------------------------------------------------------

both_engines!(a_live_connection_answers, |fx: Fixture| async move {
    fx.ping().await.expect("ping");
    assert!(
        !fx.server_version().is_empty(),
        "the version is read at connect time and shown in the titlebar"
    );
});

both_engines!(the_catalog_lists_tables_and_views, |fx: Fixture| async move {
    let catalog = fx.catalog().await.expect("catalog");

    let schema = catalog
        .schemas
        .iter()
        .find(|schema| schema.name == fx.schema())
        .unwrap_or_else(|| panic!("no {} schema in {:?}", fx.schema(), names(&catalog)));

    let table = schema
        .tables
        .iter()
        .find(|table| table.name == "people")
        .expect("the fixture table");
    assert!(!table.kind.is_view());

    let view = schema
        .tables
        .iter()
        .find(|table| table.name == "people_view")
        .expect("the fixture view");
    assert!(view.kind.is_view(), "a view must not look like a table");

    // The server's own bookkeeping stays out of the tree.
    let hidden = [
        "pg_catalog",
        "information_schema",
        "performance_schema",
        "mysql",
        "sys",
        "_vt",
    ];
    for schema in &catalog.schemas {
        assert!(
            !hidden.contains(&schema.name.as_str()),
            "system schema {} leaked into the tree",
            schema.name
        );
    }
});

both_engines!(
    columns_carry_nullability_and_the_primary_key,
    |fx: Fixture| async move {
        let columns = fx.columns(&fx.people()).await.expect("columns");
        let find = |name: &str| {
            columns
                .iter()
                .find(|column| column.name == name)
                .unwrap_or_else(|| panic!("no column {name}"))
        };

        let id = find("id");
        assert!(id.is_primary_key, "id is the primary key");
        assert!(!id.nullable);

        assert!(!find("name").nullable, "name is NOT NULL");
        assert!(find("nickname").nullable, "nickname is nullable");
        assert!(!find("nickname").is_primary_key);

        // The declared type, not the bare type name: the structure pane shows
        // `varchar(64)` / `numeric(10,2)`, not `varchar` / `numeric`.
        let score = &find("score").data_type;
        assert!(
            score.contains("10,2") || score.contains("10, 2"),
            "score's type should carry its precision, got {score:?}"
        );

        assert_eq!(
            columns.first().map(|c| c.name.as_str()),
            Some("id"),
            "columns come back in declaration order"
        );
    }
);

both_engines!(
    a_page_of_rows_reports_when_there_is_more,
    |fx: Fixture| async move {
        let table = fx.people();

        let first = fx
            .table_rows(&table, Page { limit: 2, offset: 0 }, "", &[])
            .await
            .expect("first page");
        assert_eq!(first.rows.len(), 2, "the probe row must not be shown");
        assert!(first.truncated, "five rows exist, two were asked for");

        let second = fx
            .table_rows(&table, Page { limit: 2, offset: 2 }, "", &[])
            .await
            .expect("second page");
        assert_eq!(second.rows.len(), 2);
        assert_ne!(
            first.rows[0], second.rows[0],
            "offset must actually move the window"
        );

        let all = fx
            .table_rows(&table, Page { limit: 100, offset: 0 }, "", &[])
            .await
            .expect("everything");
        assert_eq!(all.rows.len(), 5);
        assert!(!all.truncated, "nothing was left behind");

        assert_eq!(fx.row_count(&table, "").await.expect("count"), 5);
    }
);

both_engines!(values_decode_to_the_right_variants, |fx: Fixture| async move {
    let rows = fx
        .table_rows(&fx.people(), Page { limit: 100, offset: 0 }, "", &[])
        .await
        .expect("rows");

    let at = |row: usize, column: &str| -> Value {
        let index = rows
            .column_index(column)
            .unwrap_or_else(|| panic!("no column {column}"));
        rows.rows[row].get(index).cloned().expect("cell")
    };

    assert_eq!(at(0, "id"), Value::Int(1));
    assert_eq!(at(0, "name"), Value::Text("Ada".into()));

    // A NULL must be Null, not an empty string -- the grid draws them apart.
    assert_eq!(at(0, "nickname"), Value::Null);
    assert_eq!(at(1, "nickname"), Value::Text("Amazing".into()));

    // Exact numerics stay exact. 0.10 through an f64 would not come back as
    // "0.10", which is the entire reason Decimal is a String.
    assert_eq!(at(0, "score"), Value::Decimal("0.10".into()));
    assert_eq!(at(1, "score"), Value::Decimal("99.95".into()));
    assert_eq!(at(3, "score"), Value::Decimal("-1.50".into()));

    // MySQL has no real boolean: TINYINT(1) is an integer on the wire, and
    // reporting it as one is the honest reading.
    match fx.driver() {
        Driver::Postgres => {
            assert_eq!(at(0, "active"), Value::Bool(true));
            assert_eq!(at(1, "active"), Value::Bool(false));
        }
        Driver::MySql => {
            assert_eq!(at(0, "active"), Value::Int(1));
            assert_eq!(at(1, "active"), Value::Int(0));
        }
        Driver::Sqlite => unreachable!("not part of the two-engine suite"),
    }

    let meta = at(0, "meta");
    assert!(
        matches!(&meta, Value::Json(text) if text.contains('a')),
        "jsonb/JSON should decode as Json, got {meta:?}"
    );
    assert_eq!(at(1, "meta"), Value::Null);

    let created = at(0, "created_at");
    assert!(
        matches!(&created, Value::Temporal(text) if text.starts_with("2024-01-01")),
        "timestamps should decode as Temporal, got {created:?}"
    );
    assert_eq!(at(2, "created_at"), Value::Null);

    if fx.driver() == Driver::Postgres {
        assert_eq!(
            at(0, "tags"),
            Value::Array(vec![Value::Text("one".into()), Value::Text("two".into())]),
            "text[] should decode element-wise"
        );
    }

    // No ordinary column should have fallen through to the unsupported marker.
    for row in &rows.rows {
        for value in &row.0 {
            assert!(
                !matches!(value, Value::Unsupported(_)),
                "an ordinary column decoded as {value:?}"
            );
        }
    }
});

both_engines!(
    a_select_returns_rows_and_a_write_returns_a_count,
    |fx: Fixture| async move {
        let quoted = fx.people().quoted(fx.driver());

        let selected = fx
            .execute(&format!("SELECT id, name FROM {quoted} ORDER BY id"))
            .await
            .expect("select");
        match &selected.outcome {
            QueryOutcome::Rows(set) => {
                assert_eq!(set.rows.len(), 5);
                assert_eq!(set.columns.len(), 2);
                assert_eq!(set.columns[0].name, "id");
                assert!(
                    !selected.summary().contains("affected"),
                    "a select reports rows, not an affected count"
                );
            }
            other => panic!("expected rows, got {other:?}"),
        }

        let updated = fx
            .execute(&format!(
                "UPDATE {quoted} SET nickname = 'x' WHERE id IN (1, 3)"
            ))
            .await
            .expect("update");
        match updated.outcome {
            QueryOutcome::Affected(count) => assert_eq!(count, 2),
            other => panic!("expected an affected count, got {other:?}"),
        }

        let deleted = fx
            .execute(&format!("DELETE FROM {quoted} WHERE id = 5"))
            .await
            .expect("delete");
        assert_eq!(deleted.outcome, QueryOutcome::Affected(1));
        assert_eq!(fx.row_count(&fx.people(), "").await.expect("count"), 4);
    }
);

both_engines!(an_empty_result_still_has_its_headers, |fx: Fixture| async move {
    // A query that matches nothing carries no column metadata with the rows,
    // so the adapter prepares the statement to recover it. Without that the
    // grid would show no headers and look broken rather than empty.
    let quoted = fx.people().quoted(fx.driver());
    let result = fx
        .execute(&format!("SELECT id, name FROM {quoted} WHERE id = -1"))
        .await
        .expect("query");

    match &result.outcome {
        QueryOutcome::Rows(set) => {
            assert!(set.rows.is_empty());
            assert_eq!(
                set.columns.len(),
                2,
                "an empty result must still name its columns"
            );
            assert_eq!(set.columns[0].name, "id");
            assert_eq!(set.columns[1].name, "name");
        }
        other => panic!("expected rows, got {other:?}"),
    }
});

both_engines!(a_hostile_table_name_survives_quoting, |fx: Fixture| async move {
    // The identifier that would end the quoted string early if it were pasted
    // in raw. Nothing binds identifiers as parameters, so this is the property
    // `TableRef::quoted` exists to hold.
    let nasty = match fx.driver() {
        Driver::Postgres | Driver::Sqlite => "we\"ird; DROP TABLE people; --",
        Driver::MySql => "we`ird; DROP TABLE people; --",
    };
    let table = fx.table(nasty);
    let quoted = table.quoted(fx.driver());

    fx.execute(&format!("CREATE TABLE {quoted} (id int)"))
        .await
        .expect("create");
    fx.execute(&format!("INSERT INTO {quoted} (id) VALUES (7)"))
        .await
        .expect("insert");

    let rows = fx
        .table_rows(&table, Page::first(), "", &[])
        .await
        .expect("the generated SELECT must quote the name too");
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get(0), Some(&Value::Int(7)));
    assert_eq!(fx.row_count(&table, "").await.expect("count"), 1);

    // The injected DROP did nothing: the fixture table is still there.
    assert_eq!(fx.row_count(&fx.people(), "").await.expect("count"), 5);

    let catalog = fx.catalog().await.expect("catalog");
    assert!(
        catalog.find(&table).is_some(),
        "the odd name should come back out of the catalog intact"
    );
});

both_engines!(
    a_broken_query_reports_the_servers_own_words,
    |fx: Fixture| async move {
        let error = fx
            .execute("SELECT * FROM definitely_not_a_table_xyz")
            .await
            .expect_err("this table does not exist");

        match error {
            DriverError::Query { message, .. } => {
                assert!(
                    message.to_lowercase().contains("definitely_not_a_table_xyz"),
                    "the message should name the table; got {message:?}"
                );
                assert!(
                    !message.contains("error returned from database"),
                    "sqlx's wrapper text should be stripped; got {message:?}"
                );
            }
            other => panic!("expected a query error, got {other:?}"),
        }

        // The connection is still usable afterwards.
        fx.ping()
            .await
            .expect("a failed query must not poison the pool");
    }
);

both_engines!(
    filters_and_updates_round_trip,
    |fx: Fixture| async move {
        let table = fx.people();
        let where_eq = "name = 'Ada'";
        let rows = fx
            .table_rows(&table, Page::first(), where_eq, &[])
            .await
            .expect("filtered rows");
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(fx.row_count(&table, where_eq).await.expect("count"), 1);

        let where_like = "name LIKE '%a%'";
        let matched = fx
            .row_count(&table, where_like)
            .await
            .expect("like count");
        assert!(matched >= 2, "several names contain 'a'");

        let affected = fx
            .update_row(
                &table,
                &[("id".into(), Value::Int(1))],
                &[("nickname".into(), Value::Text("Lovelace".into()))],
            )
            .await
            .expect("update");
        assert_eq!(affected, 1);

        let again = fx
            .table_rows(&table, Page::first(), where_eq, &[])
            .await
            .expect("reload");
        assert_eq!(
            again.rows[0].get(2).map(|v| v.to_text()),
            Some("Lovelace".into())
        );
    }
);

both_engines!(
    batch_update_rolls_back_on_failure,
    |fx: Fixture| async move {
        use dbui_driver::RowUpdate;

        let table = fx.people();
        let err = fx
            .update_rows(
                &table,
                &[
                    RowUpdate {
                        pk: vec![("id".into(), Value::Int(1))],
                        changes: vec![("nickname".into(), Value::Text("should-not-stick".into()))],
                    },
                    // name is NOT NULL — this second statement must abort the tx.
                    RowUpdate {
                        pk: vec![("id".into(), Value::Int(2))],
                        changes: vec![("name".into(), Value::Null)],
                    },
                ],
            )
            .await
            .expect_err("null name should fail");
        assert!(
            !err.to_string().is_empty(),
            "failure should surface a message"
        );

        let ada = fx
            .table_rows(&table, Page::first(), "name = 'Ada'", &[])
            .await
            .expect("reload ada");
        let nickname = ada.rows[0].get(2).expect("nickname column");
        assert!(
            nickname.is_null(),
            "first update must roll back with the failing statement; got {:?}",
            nickname.to_text()
        );
    }
);

// One commit, both kinds of change. This is what ⌘S sends: if the two
// travelled in separate transactions, a failing delete would leave the edits
// written, which is the state the staged batch exists to prevent.
both_engines!(
    a_batch_commits_edits_and_deletions_together,
    |fx: Fixture| async move {
        use dbui_driver::{RowBatch, RowDelete, RowUpdate};

        let table = fx.people();
        let affected = fx
            .apply_changes(
                &table,
                &RowBatch {
                    inserts: Vec::new(),
                    updates: vec![RowUpdate {
                        pk: vec![("id".into(), Value::Int(1))],
                        changes: vec![("nickname".into(), Value::Text("Lovelace".into()))],
                    }],
                    deletes: vec![
                        RowDelete {
                            pk: vec![("id".into(), Value::Int(4))],
                        },
                        RowDelete {
                            pk: vec![("id".into(), Value::Int(5))],
                        },
                    ],
                },
            )
            .await
            .expect("mixed batch");
        assert_eq!(affected, 3, "one update and two deletes");

        assert_eq!(
            fx.row_count(&table, "").await.expect("count"),
            3,
            "the seed has five rows and two were deleted"
        );
        let ada = fx
            .table_rows(&table, Page::first(), "name = 'Ada'", &[])
            .await
            .expect("reload ada");
        assert_eq!(
            ada.rows[0].get(2).map(|v| v.to_text()),
            Some("Lovelace".into()),
            "and the edit in the same batch stuck"
        );
    }
);

// A delete that the server refuses takes the whole batch with it, including
// the edits that had already been applied inside the transaction.
both_engines!(
    a_failing_delete_rolls_the_edits_back,
    |fx: Fixture| async move {
        use dbui_driver::{RowBatch, RowDelete, RowUpdate};

        let table = fx.people();
        // A child row referencing Grace, so deleting her is refused by the
        // server rather than by anything this crate checks first.
        let children = fx.table("memberships");
        let child_sql = children.quoted(fx.driver());
        let parent_sql = table.quoted(fx.driver());
        // A table-level FOREIGN KEY, not a column-level `REFERENCES`: MySQL
        // parses the inline form and then ignores it, so the constraint this
        // test depends on would silently not exist.
        fx.execute(&format!(
            "CREATE TABLE {child_sql} (
                 id        bigint PRIMARY KEY,
                 person_id bigint NOT NULL,
                 FOREIGN KEY (person_id) REFERENCES {parent_sql} (id)
             )"
        ))
        .await
        .expect("create child table");
        fx.execute(&format!(
            "INSERT INTO {child_sql} (id, person_id) VALUES (1, 2)"
        ))
        .await
        .expect("seed child row");

        let err = fx
            .apply_changes(
                &table,
                &RowBatch {
                    inserts: Vec::new(),
                    updates: vec![RowUpdate {
                        pk: vec![("id".into(), Value::Int(1))],
                        changes: vec![("nickname".into(), Value::Text("should-not-stick".into()))],
                    }],
                    // A child row references this one, so the FK aborts the tx.
                    deletes: vec![RowDelete {
                        pk: vec![("id".into(), Value::Int(2))],
                    }],
                },
            )
            .await
            .expect_err("a referenced row cannot be deleted");
        assert!(!err.to_string().is_empty(), "and it says why");

        assert_eq!(
            fx.row_count(&table, "").await.expect("count"),
            5,
            "nothing was deleted"
        );
        let ada = fx
            .table_rows(&table, Page::first(), "name = 'Ada'", &[])
            .await
            .expect("reload ada");
        assert!(
            ada.rows[0].get(2).expect("nickname").is_null(),
            "and the edit that ran before it rolled back too"
        );
    }
);

// A row named by a key that matches nothing is not an error -- it is a row
// somebody else already deleted. The count is what says so.
both_engines!(deleting_a_missing_row_affects_nothing, |fx: Fixture| async move {
    use dbui_driver::{RowBatch, RowDelete};

    let table = fx.people();
    let affected = fx
        .apply_changes(
            &table,
            &RowBatch {
                inserts: Vec::new(),
                updates: Vec::new(),
                deletes: vec![RowDelete {
                    pk: vec![("id".into(), Value::Int(9_999))],
                }],
            },
        )
        .await
        .expect("a no-op delete is not a failure");
    assert_eq!(affected, 0);
    assert_eq!(fx.row_count(&table, "").await.expect("count"), 5);
});

// The two statements the context menu offers have to be accepted as written.
both_engines!(generated_truncate_and_drop_are_accepted, |fx: Fixture| async move {
    use dbui_domain::TableKind;

    let table = fx.table("scratch");
    let quoted = table.quoted(fx.driver());
    fx.execute(&format!("CREATE TABLE {quoted} (id bigint PRIMARY KEY)"))
        .await
        .expect("create");
    fx.execute(&format!("INSERT INTO {quoted} (id) VALUES (1), (2)"))
        .await
        .expect("seed");

    fx.execute(&dbui_driver::truncate_sql(fx.driver(), &table))
        .await
        .expect("truncate");
    assert_eq!(fx.row_count(&table, "").await.expect("count"), 0);

    fx.execute(&dbui_driver::drop_sql(fx.driver(), &table, TableKind::Table))
        .await
        .expect("drop");
    assert!(
        fx.row_count(&table, "").await.is_err(),
        "the table is gone"
    );

    // A view needs DROP VIEW, which is why `drop_sql` takes the kind.
    let view = fx.table("people_view");
    fx.execute(&dbui_driver::drop_sql(fx.driver(), &view, TableKind::View))
        .await
        .expect("drop view");
});

// Paging without an order is not paging: `LIMIT`/`OFFSET` over an unordered
// read can hand back the same row on two pages and never show another. This
// walks a table one row at a time and checks it sees each exactly once.
both_engines!(paging_in_key_order_sees_every_row_once, |fx: Fixture| async move {
    use dbui_domain::order_for;

    let table = fx.people();
    let key = vec!["id".to_string()];

    let mut seen = Vec::new();
    for offset in 0..5u64 {
        let page = Page { limit: 1, offset };
        let rows = fx
            .table_rows(&table, page, "", &order_for(None, &key))
            .await
            .expect("one row");
        assert_eq!(rows.rows.len(), 1, "page {offset} should hold one row");
        seen.push(rows.rows[0].get(0).expect("id").to_text());
    }

    assert_eq!(
        seen,
        vec!["1", "2", "3", "4", "5"],
        "each row exactly once, in key order"
    );
});

// A descending sort is applied by the server, not by the page the UI happens
// to be holding.
both_engines!(a_sort_is_applied_across_the_whole_table, |fx: Fixture| async move {
    use dbui_domain::{order_for, SortKey};

    let table = fx.people();
    let key = vec!["id".to_string()];
    let order = order_for(Some(&SortKey::desc("name")), &key);

    let rows = fx
        .table_rows(&table, Page::first(), "", &order)
        .await
        .expect("sorted rows");
    let names: Vec<String> = rows
        .rows
        .iter()
        .map(|row| row.get(1).expect("name").to_text())
        .collect();

    let mut expected = names.clone();
    expected.sort();
    expected.reverse();
    assert_eq!(names, expected, "the server ordered them, not the client");

    // The first page of a descending sort is the *last* rows alphabetically.
    let first = fx
        .table_rows(&table, Page { limit: 1, offset: 0 }, "", &order)
        .await
        .expect("first row");
    assert_eq!(first.rows[0].get(1).map(|v| v.to_text()), Some("Grace".into()));
});

// A new row goes in the same transaction as everything else, and the columns
// left out of it are the ones the server fills in.
both_engines!(an_insert_commits_with_the_rest_of_the_batch, |fx: Fixture| async move {
    use dbui_driver::{RowBatch, RowInsert, RowUpdate};

    let table = fx.people();
    let affected = fx
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
                deletes: Vec::new(),
            },
        )
        .await
        .expect("insert + update");
    assert_eq!(affected, 2);

    let rows = fx
        .table_rows(&table, Page::first(), "id = 6", &[])
        .await
        .expect("the new row");
    assert_eq!(rows.rows.len(), 1);
    assert_eq!(rows.rows[0].get(1).map(|v| v.to_text()), Some("Katherine".into()));
    assert!(
        rows.rows[0].get(2).expect("nickname").is_null(),
        "a column left out of the INSERT keeps the table's own default"
    );
});

// An insert the server refuses takes the whole batch down with it.
both_engines!(a_failing_insert_rolls_the_batch_back, |fx: Fixture| async move {
    use dbui_driver::{RowBatch, RowInsert, RowUpdate};

    let table = fx.people();
    let err = fx
        .apply_changes(
            &table,
            &RowBatch {
                // id 1 already exists: the primary key rejects it.
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
        .expect_err("a duplicate key is refused");
    assert!(!err.to_string().is_empty());

    assert_eq!(fx.row_count(&table, "").await.expect("count"), 5);
    let grace = fx
        .table_rows(&table, Page::first(), "id = 2", &[])
        .await
        .expect("reload grace");
    assert_eq!(
        grace.rows[0].get(2).map(|v| v.to_text()),
        Some("Amazing".into()),
        "the update alongside it rolled back too"
    );
});

// The introspection SQL for foreign keys is the kind of thing only a real
// server can prove parses -- and the composite-key filter only a real one with
// a composite key can prove works.
both_engines!(foreign_keys_are_read_for_single_column_references, |fx: Fixture| async move {
    let child = fx.table("orders");
    let parent = fx.people();
    let child_sql = child.quoted(fx.driver());
    let parent_sql = parent.quoted(fx.driver());

    fx.execute(&format!(
        "CREATE TABLE {child_sql} (
             id        bigint PRIMARY KEY,
             person_id bigint NOT NULL,
             FOREIGN KEY (person_id) REFERENCES {parent_sql} (id)
         )"
    ))
    .await
    .expect("create child");

    let columns = fx.columns(&child).await.expect("columns");
    let person = columns
        .iter()
        .find(|column| column.name == "person_id")
        .expect("person_id");
    let key = person.references.as_ref().expect("it references people");
    assert_eq!(key.column, "person_id");
    assert_eq!(key.references.name, parent.name);
    assert_eq!(key.references_column, "id");

    let id = columns.iter().find(|column| column.name == "id").unwrap();
    assert!(id.references.is_none(), "a plain key points nowhere");
});

// A composite foreign key cannot be followed from one cell, so it is not
// reported at all -- the value on screen is only part of the key.
both_engines!(a_composite_foreign_key_is_not_reported, |fx: Fixture| async move {
    let parent = fx.table("pairs");
    let child = fx.table("pair_refs");
    let parent_sql = parent.quoted(fx.driver());
    let child_sql = child.quoted(fx.driver());

    fx.execute(&format!(
        "CREATE TABLE {parent_sql} (a bigint, b bigint, PRIMARY KEY (a, b))"
    ))
    .await
    .expect("create parent");
    fx.execute(&format!(
        "CREATE TABLE {child_sql} (
             id bigint PRIMARY KEY,
             a  bigint NOT NULL,
             b  bigint NOT NULL,
             FOREIGN KEY (a, b) REFERENCES {parent_sql} (a, b)
         )"
    ))
    .await
    .expect("create child");

    let columns = fx.columns(&child).await.expect("columns");
    assert!(
        columns.iter().all(|column| column.references.is_none()),
        "a two-column key is not followable from one cell"
    );
});

both_engines!(closing_is_idempotent, |fx: Fixture| async move {
    fx.close().await;
    fx.close().await;
    assert!(
        fx.ping().await.is_err(),
        "a closed pool cannot serve queries"
    );
});

fn names(catalog: &dbui_domain::Catalog) -> Vec<&str> {
    catalog.schemas.iter().map(|s| s.name.as_str()).collect()
}
