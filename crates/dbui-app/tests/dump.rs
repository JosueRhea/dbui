//! A dump restores to the same database.
//!
//! Each test builds a database with the awkward things in it -- quotes and
//! backslashes in text, binary, NULLs, arrays, identity and serial keys,
//! foreign keys (composite too), views, functions, triggers -- dumps it,
//! restores the file into an empty database through the same `run_script`
//! the app uses, and compares every row and every object. SQLite always
//! runs; Postgres and MySQL are opt-in behind `DBUI_LIVE_TESTS=1`, with the
//! driver tests' `DBUI_PG_*` / `DBUI_MYSQL_*` overrides.

use dbui_app::commands::{self, stop_signal};
use dbui_app::domain::{ConnectionConfig, Driver, ObjectKind, QueryOutcome, TlsMode};
use dbui_app::{DatabaseDriver, DbRuntime};
use std::future::Future;
use std::sync::{Arc, Mutex};

/// Wait for a `Task` without a runtime of our own: the work runs on the
/// `DbRuntime`, which is where the app runs it too.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::park();
    }
}

struct Db {
    runtime: Arc<DbRuntime>,
    driver: Arc<dyn DatabaseDriver>,
}

impl Db {
    fn connect(runtime: &Arc<DbRuntime>, config: ConnectionConfig) -> Self {
        let driver =
            block_on(runtime.spawn(async move { dbui_app::connect_driver(&config).await }))
                .expect("runtime")
                .expect("connect");
        Self {
            runtime: runtime.clone(),
            driver,
        }
    }

    fn run(&self, sql: &str) {
        let driver = self.driver.clone();
        let owned = sql.to_string();
        block_on(
            self.runtime
                .spawn(async move { driver.execute(&owned).await }),
        )
        .expect("runtime")
        .unwrap_or_else(|error| panic!("{error}\n{sql}"));
    }

    /// Every row of a query, as text, for comparing two databases.
    fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        let driver = self.driver.clone();
        let owned = sql.to_string();
        let result = block_on(
            self.runtime
                .spawn(async move { driver.execute(&owned).await }),
        )
        .expect("runtime")
        .unwrap_or_else(|error| panic!("{error}\n{sql}"));
        let QueryOutcome::Rows(set) = result.outcome else {
            panic!("no rows from {sql}");
        };
        set.rows
            .iter()
            .map(|row| row.0.iter().map(|value| format!("{value:?}")).collect())
            .collect()
    }

    fn object_names(&self, schema: &str) -> Vec<(ObjectKind, String)> {
        let driver = self.driver.clone();
        let catalog = block_on(self.runtime.spawn(async move { driver.catalog().await }))
            .expect("runtime")
            .expect("catalog");
        let mut names: Vec<_> = catalog
            .objects
            .iter()
            .filter(|object| object.schema == schema)
            .map(|object| (object.kind, object.name.clone()))
            .chain(
                catalog
                    .schemas
                    .iter()
                    .filter(|s| s.name == schema)
                    .flat_map(|s| &s.tables)
                    .map(|table| (ObjectKind::Type, format!("relation {}", table.name))),
            )
            .collect();
        names.sort();
        names
    }

    /// Dump `schemas` to a file and return its text.
    fn dump(&self, schemas: &[&str], name: &str) -> (String, dbui_app::dump::DumpReport) {
        let path =
            std::env::temp_dir().join(format!("dbui-dump-{}-{name}.sql", std::process::id()));
        let report = block_on(commands::dump_database(
            &self.runtime,
            self.driver.clone(),
            schemas.iter().map(|s| s.to_string()).collect(),
            path.clone(),
            Arc::new(Mutex::new(String::new())),
        ))
        .expect("runtime")
        .expect("dump");
        let text = std::fs::read_to_string(&path).expect("the dump file");
        let _ = std::fs::remove_file(&path);
        (text, report)
    }

    /// Run a dump's text the way Run SQL File does.
    fn restore(&self, text: &str) {
        let engine = self.driver.driver();
        let statements: Vec<String> = dbui_app::domain::split_statements_for(engine, text)
            .into_iter()
            .map(|range| text[range].trim().to_string())
            .filter(|sql| !sql.is_empty())
            .collect();
        let (_handle, stop) = stop_signal(None);
        let outcome = block_on(commands::run_script(
            &self.runtime,
            self.driver.clone(),
            statements,
            stop,
            Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        ))
        .expect("runtime");
        if let Some((index, sql, error)) = outcome.failure {
            panic!("restore stopped at statement {index}: {error}\n{sql}");
        }
        assert_eq!(outcome.ran, outcome.total);
    }
}

fn server(driver: Driver, database: &str) -> Option<ConnectionConfig> {
    if std::env::var("DBUI_LIVE_TESTS").is_err() {
        return None;
    }
    let (prefix, port) = match driver {
        Driver::Postgres => ("DBUI_PG", "55432"),
        _ => ("DBUI_MYSQL", "53306"),
    };
    let env = |key: &str, fallback: &str| {
        std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| fallback.to_string())
    };
    let mut config = ConnectionConfig::new(driver);
    config.host = env("HOST", "127.0.0.1");
    config.port = env("PORT", port).parse().unwrap();
    config.username = env(
        "USER",
        if driver == Driver::Postgres {
            "postgres"
        } else {
            "root"
        },
    );
    config.password = env("PASSWORD", "dbui");
    config.database = database.to_string();
    config.tls = TlsMode::Disable;
    Some(config)
}

#[test]
fn a_sqlite_file_survives_a_dump_and_restore() {
    let runtime = Arc::new(DbRuntime::new().unwrap());
    let file = |name: &str| {
        let path = std::env::temp_dir().join(format!("dbui-dump-{}-{name}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::File::create(&path).unwrap();
        let mut config = ConnectionConfig::new(Driver::Sqlite);
        config.database = path.to_string_lossy().into();
        (path, config)
    };
    let (source_path, source_config) = file("source");
    let (target_path, target_config) = file("target");
    let source = Db::connect(&runtime, source_config);
    for sql in [
        "CREATE TABLE teams (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)",
        "CREATE TABLE people (id INTEGER PRIMARY KEY AUTOINCREMENT, team_id INTEGER REFERENCES teams (id),
             name TEXT NOT NULL, note TEXT, score REAL, photo BLOB)",
        "CREATE INDEX people_team ON people (team_id)",
        "INSERT INTO teams VALUES (1, 'core'), (2, 'it''s ops')",
        "INSERT INTO people (team_id, name, note, score, photo) VALUES
             (1, 'Ada', 'C:\\temp\\new', 0.1, X'00ff10'),
             (2, 'Grace', NULL, -3.5e10, NULL),
             (NULL, 'O''Brien', 'line one
line two', NULL, X'')",
        "CREATE VIEW named AS SELECT p.name, t.name AS team FROM people p LEFT JOIN teams t ON t.id = p.team_id",
        "CREATE TRIGGER people_upper AFTER INSERT ON people BEGIN
             UPDATE people SET name = upper(name) WHERE id = NEW.id AND name = 'shout'; END",
    ] {
        source.run(sql);
    }

    let (text, report) = source.dump(&["main"], "sqlite");
    assert_eq!(report.tables, 2, "{report:?}\n{text}");
    assert_eq!(report.rows, 5);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);

    let target = Db::connect(&runtime, target_config);
    target.restore(&text);

    for table in ["teams", "people"] {
        let sql = format!("SELECT * FROM {table} ORDER BY id");
        assert_eq!(source.rows(&sql), target.rows(&sql), "{table}");
    }
    assert_eq!(source.object_names("main"), target.object_names("main"));
    // The trigger came back working, and AUTOINCREMENT carries on past the
    // ids that were loaded.
    target.run("INSERT INTO people (name) VALUES ('shout')");
    assert_eq!(
        target.rows("SELECT id, name FROM people WHERE name = 'SHOUT'"),
        vec![vec!["Int(4)".to_string(), "Text(\"SHOUT\")".to_string()]]
    );

    let _ = std::fs::remove_file(source_path);
    let _ = std::fs::remove_file(target_path);
}

#[test]
fn a_postgres_schema_survives_a_dump_and_restore() {
    let Some(admin) = server(Driver::Postgres, "postgres") else {
        return;
    };
    let runtime = Arc::new(DbRuntime::new().unwrap());
    let db = Db::connect(&runtime, admin.clone());
    for name in ["dbui_dump_source", "dbui_dump_target"] {
        db.run(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"));
        db.run(&format!("CREATE DATABASE {name}"));
    }

    let mut config = admin.clone();
    config.database = "dbui_dump_source".into();
    let source = Db::connect(&runtime, config);
    for sql in [
        "CREATE EXTENSION IF NOT EXISTS pgcrypto",
        "CREATE SCHEMA shop",
        "CREATE TYPE shop.status AS ENUM ('new', 'paid', 'it''s done')",
        "CREATE DOMAIN shop.cents AS bigint CHECK (VALUE >= 0)",
        "CREATE TABLE shop.customers (
             id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
             email text NOT NULL UNIQUE,
             tags text[],
             meta jsonb,
             avatar bytea,
             joined timestamptz DEFAULT now()
         )",
        "CREATE TABLE shop.orders (
             id serial PRIMARY KEY,
             customer_id bigint NOT NULL REFERENCES shop.customers (id),
             status shop.status NOT NULL DEFAULT 'new',
             total shop.cents NOT NULL,
             note text,
             CHECK (total < 100000000)
         )",
        "CREATE TABLE shop.lines (
             order_id int, line int, sku text, PRIMARY KEY (order_id, line)
         )",
        "CREATE TABLE shop.line_notes (
             order_id int, line int, body text,
             FOREIGN KEY (order_id, line) REFERENCES shop.lines (order_id, line)
         )",
        "CREATE INDEX orders_by_status ON shop.orders (status)",
        "INSERT INTO shop.customers (email, tags, meta, avatar) VALUES
             ('a@x.io', ARRAY['vip', 'has \"quotes\"', NULL], '{\"n\": 1, \"s\": \"a\\\\b\"}', '\\xdeadbeef'),
             ('b@x.io', NULL, NULL, NULL)",
        "INSERT INTO shop.orders (customer_id, status, total, note) VALUES
             (1, 'paid', 1250, 'C:\\temp'), (1, 'it''s done', 0, E'two\\nlines'), (2, 'new', 99, NULL)",
        "INSERT INTO shop.lines VALUES (1, 1, 'A'), (1, 2, 'B')",
        "INSERT INTO shop.line_notes VALUES (1, 2, 'fragile')",
        "CREATE FUNCTION shop.order_total(p_order int) RETURNS bigint LANGUAGE sql STABLE
             AS $$ SELECT total FROM shop.orders WHERE id = p_order $$",
        "CREATE FUNCTION shop.stamp() RETURNS trigger LANGUAGE plpgsql AS $$
             BEGIN NEW.note := coalesce(NEW.note, 'stamped'); RETURN NEW; END $$",
        "CREATE TRIGGER orders_stamp BEFORE INSERT ON shop.orders
             FOR EACH ROW EXECUTE FUNCTION shop.stamp()",
        "CREATE VIEW shop.paid AS SELECT id, total FROM shop.orders WHERE status = 'paid'",
        "CREATE MATERIALIZED VIEW shop.totals AS SELECT customer_id, sum(total) AS sum FROM shop.orders GROUP BY 1",
    ] {
        source.run(sql);
    }

    let (text, report) = source.dump(&["shop"], "postgres");
    assert_eq!(report.tables, 4, "{report:?}\n{text}");
    assert_eq!(report.rows, 8);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert_eq!(report.unreadable_values, 0);

    let mut config = admin.clone();
    config.database = "dbui_dump_target".into();
    let target = Db::connect(&runtime, config);
    // The extension lives in `public`, outside the dumped schema, so a real
    // restore target would have it already.
    target.run("CREATE EXTENSION IF NOT EXISTS pgcrypto");
    target.restore(&text);

    for (table, key) in [
        ("customers", "id"),
        ("orders", "id"),
        ("lines", "order_id, line"),
        ("line_notes", "order_id"),
        ("paid", "id"),
        ("totals", "customer_id"),
    ] {
        let sql = format!("SELECT * FROM shop.{table} ORDER BY {key}");
        assert_eq!(source.rows(&sql), target.rows(&sql), "{table}");
    }
    assert_eq!(source.object_names("shop"), target.object_names("shop"));

    // Keys, sequences and code all came back working.
    target.run("INSERT INTO shop.customers (email) VALUES ('c@x.io')");
    target.run("INSERT INTO shop.orders (customer_id, total) VALUES (3, 5)");
    assert_eq!(
        target.rows("SELECT id, note FROM shop.orders WHERE customer_id = 3"),
        vec![vec!["Int(4)".to_string(), "Text(\"stamped\")".to_string()]],
        "serial carries on past the loaded ids, and the trigger fires"
    );
    assert_eq!(
        target.rows("SELECT id FROM shop.customers WHERE email = 'c@x.io'"),
        vec![vec!["Int(3)".to_string()]],
        "the identity carries on too"
    );
    assert_eq!(
        target.rows("SELECT shop.order_total(1)"),
        vec![vec!["Int(1250)".to_string()]]
    );
    let refused = {
        let driver = target.driver.clone();
        block_on(runtime.spawn(async move {
            driver
                .execute("INSERT INTO shop.line_notes VALUES (9, 9, 'orphan')")
                .await
        }))
        .unwrap()
    };
    assert!(refused.is_err(), "the composite foreign key is enforced");
}

#[test]
fn a_mysql_database_survives_a_dump_and_restore() {
    let Some(admin) = server(Driver::MySql, "") else {
        return;
    };
    let runtime = Arc::new(DbRuntime::new().unwrap());
    let db = Db::connect(&runtime, admin.clone());
    db.run("DROP DATABASE IF EXISTS dbui_dump_mysql");
    db.run("CREATE DATABASE dbui_dump_mysql");
    for sql in [
        "CREATE TABLE dbui_dump_mysql.teams (
             id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(40) NOT NULL UNIQUE)",
        "CREATE TABLE dbui_dump_mysql.people (
             id INT AUTO_INCREMENT PRIMARY KEY,
             team_id INT, name VARCHAR(40) NOT NULL, note TEXT, score DECIMAL(10,2),
             photo BLOB, meta JSON, seen DATETIME,
             KEY by_name (name),
             FOREIGN KEY (team_id) REFERENCES dbui_dump_mysql.teams (id))",
        "INSERT INTO dbui_dump_mysql.teams (name) VALUES ('core'), ('it''s ops')",
        "INSERT INTO dbui_dump_mysql.people (team_id, name, note, score, photo, meta, seen) VALUES
             (1, 'Ada', 'C:\\\\temp\\\\new', 12.50, X'00ff10', '{\"a\": \"b\\\\\\\\c\"}', '2024-01-02 03:04:05'),
             (2, 'O''Brien', 'two\\nlines', NULL, NULL, NULL, NULL)",
        "CREATE VIEW dbui_dump_mysql.named AS SELECT name FROM dbui_dump_mysql.people",
        "CREATE FUNCTION dbui_dump_mysql.double_it(x INT) RETURNS INT DETERMINISTIC RETURN x * 2",
        "CREATE TRIGGER dbui_dump_mysql.people_trim BEFORE INSERT ON dbui_dump_mysql.people
             FOR EACH ROW SET NEW.name = TRIM(NEW.name)",
    ] {
        db.run(sql);
    }
    let people = "SELECT * FROM dbui_dump_mysql.people ORDER BY id";
    let teams = "SELECT * FROM dbui_dump_mysql.teams ORDER BY id";
    let before = (
        db.rows(people),
        db.rows(teams),
        db.object_names("dbui_dump_mysql"),
    );

    let (text, report) = db.dump(&["dbui_dump_mysql"], "mysql");
    assert_eq!(report.tables, 2, "{report:?}\n{text}");
    assert_eq!(report.rows, 4);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    assert!(!text.contains("DEFINER="), "restorable by anyone:\n{text}");

    db.run("DROP DATABASE dbui_dump_mysql");
    db.restore(&text);

    assert_eq!(before.0, db.rows(people), "people");
    assert_eq!(before.1, db.rows(teams), "teams");
    assert_eq!(before.2, db.object_names("dbui_dump_mysql"));
    db.run("INSERT INTO dbui_dump_mysql.people (team_id, name) VALUES (1, '  padded  ')");
    assert_eq!(
        db.rows("SELECT id, name FROM dbui_dump_mysql.people WHERE id > 2"),
        vec![vec!["Int(3)".to_string(), "Text(\"padded\")".to_string()]],
        "AUTO_INCREMENT carries on, and the trigger fires"
    );
    db.run("DROP DATABASE dbui_dump_mysql");
}
