//! Real `EXPLAIN` output, read as a plan.
//!
//! The parser's unit tests use output copied from documentation; these feed
//! it what the engines actually send today. SQLite always runs. Postgres and
//! MySQL are opt-in behind `DBUI_LIVE_TESTS=1`, with the same
//! `DBUI_PG_*` / `DBUI_MYSQL_*` overrides as the driver's live tests.

use dbui_app::plan::{explain_sql, Plan};
use dbui_domain::{ConnectionConfig, Driver, QueryOutcome, TlsMode};

fn env_or(prefix: &str, key: &str, fallback: &str) -> String {
    std::env::var(format!("{prefix}_{key}")).unwrap_or_else(|_| fallback.to_string())
}

fn server(driver: Driver) -> Option<ConnectionConfig> {
    if std::env::var("DBUI_LIVE_TESTS").is_err() {
        return None;
    }
    let (prefix, port) = match driver {
        Driver::Postgres => ("DBUI_PG", "55432"),
        _ => ("DBUI_MYSQL", "53306"),
    };
    let mut config = ConnectionConfig::new(driver);
    config.host = env_or(prefix, "HOST", "127.0.0.1");
    config.port = env_or(prefix, "PORT", port).parse().unwrap();
    config.username = env_or(
        prefix,
        "USER",
        if driver == Driver::Postgres {
            "postgres"
        } else {
            "root"
        },
    );
    config.password = env_or(prefix, "PASSWORD", "dbui");
    config.database = env_or(prefix, "DATABASE", "dbui_test");
    config.tls = TlsMode::Disable;
    Some(config)
}

async fn plan_of(config: &ConnectionConfig, setup: &[&str], sql: &str) -> Plan {
    let db = dbui_driver_connect(config).await;
    for statement in setup {
        db.execute(statement)
            .await
            .unwrap_or_else(|error| panic!("{error}\n{statement}"));
    }
    let explained = explain_sql(config.driver, sql);
    let result = db
        .execute(&explained)
        .await
        .unwrap_or_else(|error| panic!("{error}\n{explained}"));
    let QueryOutcome::Rows(set) = result.outcome else {
        panic!("EXPLAIN returned no rows");
    };
    db.close().await;
    Plan::from_result(&set).unwrap_or_else(|| panic!("not read as a plan: {set:?}"))
}

async fn dbui_driver_connect(
    config: &ConnectionConfig,
) -> std::sync::Arc<dyn dbui_app::DatabaseDriver> {
    dbui_app::connect_driver(config).await.expect("connect")
}

#[tokio::test]
async fn sqlite_explains_a_join() {
    let path = std::env::temp_dir().join(format!("dbui-plans-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    std::fs::File::create(&path).unwrap();
    let mut config = ConnectionConfig::new(Driver::Sqlite);
    config.database = path.to_string_lossy().into();

    let plan = plan_of(
        &config,
        &[
            "CREATE TABLE a (id INTEGER PRIMARY KEY, b_id INTEGER)",
            "CREATE TABLE b (id INTEGER PRIMARY KEY, name TEXT)",
        ],
        "SELECT * FROM a JOIN b ON b.id = a.b_id WHERE b.name = 'x'",
    )
    .await;
    let _ = std::fs::remove_file(&path);
    assert!(plan.steps.len() >= 2, "{plan:?}");
    assert!(
        plan.steps
            .iter()
            .any(|step| step.title.contains("SCAN") || step.title.contains("SEARCH")),
        "{plan:?}"
    );
}

#[tokio::test]
async fn postgres_explains_a_join_with_costs() {
    let Some(config) = server(Driver::Postgres) else {
        return;
    };
    let plan = plan_of(
        &config,
        &[
            "DROP SCHEMA IF EXISTS dbui_plans CASCADE",
            "CREATE SCHEMA dbui_plans",
            "CREATE TABLE dbui_plans.a (id int PRIMARY KEY, b_id int)",
            "CREATE TABLE dbui_plans.b (id int PRIMARY KEY, name text)",
        ],
        "SELECT * FROM dbui_plans.a JOIN dbui_plans.b ON b.id = a.b_id WHERE b.name = 'x'",
    )
    .await;
    assert!(plan.steps.len() >= 2, "{plan:?}");
    assert!(plan.steps[0].cost.is_some(), "the root has a cost");
    assert!(
        plan.steps.iter().any(|step| step.title.contains(" on ")),
        "{plan:?}"
    );
    let total: f64 = plan.self_shares().iter().sum();
    assert!((total - 1.0).abs() < 1e-6, "shares add up: {total}");
}

#[tokio::test]
async fn mysql_explains_a_join_with_costs() {
    let Some(config) = server(Driver::MySql) else {
        return;
    };
    let plan = plan_of(
        &config,
        &[
            "DROP DATABASE IF EXISTS dbui_plans",
            "CREATE DATABASE dbui_plans",
            "CREATE TABLE dbui_plans.a (id INT PRIMARY KEY, b_id INT)",
            "CREATE TABLE dbui_plans.b (id INT PRIMARY KEY, name VARCHAR(20))",
            "INSERT INTO dbui_plans.a VALUES (1, 1), (2, 2)",
            "INSERT INTO dbui_plans.b VALUES (1, 'x'), (2, 'y')",
        ],
        "SELECT * FROM dbui_plans.a JOIN dbui_plans.b ON b.id = a.b_id WHERE b.name = 'x'",
    )
    .await;
    assert_eq!(plan.steps[0].title, "Query block #1", "{plan:?}");
    assert!(plan.steps[0].cost.is_some(), "{plan:?}");
    assert!(
        plan.steps
            .iter()
            .any(|step| step.title.starts_with("a (") || step.title.starts_with("b (")),
        "{plan:?}"
    );
}
