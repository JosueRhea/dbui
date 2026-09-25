//! Connections through a real SSH tunnel.
//!
//! Opt-in behind `DBUI_SSH_TESTS=1`, like `live.rs`: they need an `sshd` that
//! allows TCP forwarding and a PostgreSQL the SSH server can reach. With the
//! flag set, anything unreachable *fails* rather than skipping.
//!
//! | variable              | default       |                                   |
//! |-----------------------|---------------|-----------------------------------|
//! | `DBUI_SSH_HOST`       | `127.0.0.1`   | the SSH server                    |
//! | `DBUI_SSH_PORT`       | `22`          |                                   |
//! | `DBUI_SSH_USER`       | (ssh config)  |                                   |
//! | `DBUI_SSH_KEY`        | (agent)       | a key with no passphrase          |
//! | `DBUI_SSH_PASSWORD`   | --            | that user's password; test skipped without it |
//! | `DBUI_SSH_PG_HOST`    | `127.0.0.1`   | Postgres, as the SSH server sees it |
//! | `DBUI_SSH_PG_PORT`    | `5432`        |                                   |
//! | `DBUI_SSH_PG_USER`    | `postgres`    |                                   |
//! | `DBUI_SSH_PG_PASSWORD`| (none)        |                                   |

use dbui_domain::{ConnectionConfig, Driver, QueryOutcome, TlsMode, Value};

fn enabled() -> bool {
    std::env::var("DBUI_SSH_TESTS").is_ok_and(|v| v == "1")
}

fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| fallback.to_string())
}

fn config() -> ConnectionConfig {
    let mut config = ConnectionConfig::new(Driver::Postgres);
    config.host = env_or("DBUI_SSH_PG_HOST", "127.0.0.1");
    config.port = env_or("DBUI_SSH_PG_PORT", "5432").parse().unwrap();
    config.username = env_or("DBUI_SSH_PG_USER", "postgres");
    config.password = env_or("DBUI_SSH_PG_PASSWORD", "");
    config.database = "postgres".into();
    config.tls = TlsMode::Disable;
    config.ssh.enabled = true;
    config.ssh.host = env_or("DBUI_SSH_HOST", "127.0.0.1");
    config.ssh.port = env_or("DBUI_SSH_PORT", "22").parse().unwrap();
    config.ssh.username = env_or("DBUI_SSH_USER", "");
    config.ssh.key_path = env_or("DBUI_SSH_KEY", "");
    config
}

async fn select_one(config: &ConnectionConfig) {
    let driver = dbui_driver::connect(config)
        .await
        .expect("connect through the tunnel");
    let result = driver.execute("SELECT 1 + 1 AS two").await.expect("query");
    let QueryOutcome::Rows(set) = result.outcome else {
        panic!("expected rows");
    };
    assert_eq!(set.rows[0].0[0], Value::Int(2));
    driver.close().await;
}

#[tokio::test]
async fn a_key_opens_the_tunnel() {
    if !enabled() {
        return;
    }
    select_one(&config()).await;
}

#[tokio::test]
async fn a_password_opens_the_tunnel() {
    if !enabled() {
        return;
    }
    let Ok(password) = std::env::var("DBUI_SSH_PASSWORD") else {
        return;
    };
    let mut config = config();
    // Only the password: no key to fall back on.
    config.ssh.key_path = "/nonexistent/key".into();
    config.ssh.password = password;
    select_one(&config).await;
}

#[tokio::test]
async fn a_wrong_password_says_so() {
    if !enabled() || std::env::var("DBUI_SSH_PASSWORD").is_err() {
        return;
    }
    let mut config = config();
    config.ssh.key_path = "/nonexistent/key".into();
    config.ssh.password = "not the password".into();
    let error = match dbui_driver::connect(&config).await {
        Ok(_) => panic!("a wrong password must not connect"),
        Err(error) => error.to_string(),
    };
    assert!(error.starts_with("Could not connect to SSH "), "{error}");
    assert!(error.contains("Permission denied"), "{error}");
}

#[tokio::test]
async fn a_database_the_bastion_cannot_reach_is_named_in_the_error() {
    if !enabled() {
        return;
    }
    let mut config = config();
    // Nothing listens on port 1; the tunnel comes up, the dial through it
    // does not, and the error names the database -- not the local port.
    config.port = 1;
    let error = match dbui_driver::connect(&config).await {
        Ok(_) => panic!("nothing listens on port 1"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains(":1 via SSH "), "{error}");
}
