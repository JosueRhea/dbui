//! What it takes to reach a server, and how to name one.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// The database engines this build speaks.
///
/// Adding a third means adding a variant here and an adapter in `dbui-driver`;
/// the compiler then walks you through every place that has to care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Driver {
    Postgres,
    MySql,
}

impl Driver {
    pub const ALL: [Driver; 2] = [Driver::Postgres, Driver::MySql];

    pub fn label(self) -> &'static str {
        match self {
            Driver::Postgres => "PostgreSQL",
            Driver::MySql => "MySQL",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Driver::Postgres => 5432,
            Driver::MySql => 3306,
        }
    }

    /// The default database to talk to when the user leaves the field blank.
    ///
    /// Postgres refuses a connection without one and conventionally has a
    /// `postgres` database; MySQL is happy to connect with no database
    /// selected, which is what an empty string gets you.
    pub fn default_database(self) -> &'static str {
        match self {
            Driver::Postgres => "postgres",
            Driver::MySql => "",
        }
    }

    /// Wrap an identifier the way this engine expects it.
    ///
    /// Every generated statement goes through here. Doubling the quote
    /// character is what both engines define as the escape, so a table called
    /// `we"ird` survives the round trip instead of ending the identifier early.
    pub fn quote_identifier(self, ident: &str) -> String {
        match self {
            Driver::Postgres => format!("\"{}\"", ident.replace('"', "\"\"")),
            Driver::MySql => format!("`{}`", ident.replace('`', "``")),
        }
    }
}

impl fmt::Display for Driver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// How hard to insist on TLS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    Disable,
    #[default]
    Prefer,
    Require,
}

impl TlsMode {
    pub const ALL: [TlsMode; 3] = [TlsMode::Disable, TlsMode::Prefer, TlsMode::Require];

    pub fn label(self) -> &'static str {
        match self {
            TlsMode::Disable => "Disabled",
            TlsMode::Prefer => "Preferred",
            TlsMode::Require => "Required",
        }
    }
}

/// Identifies one saved connection.
///
/// Stable across launches: the id is written to `connections.json` and used as
/// the keychain account for that connection's password. New ids are minted by
/// [`ConnectionId::next`]; after loading from disk, call [`ConnectionId::observe`]
/// so the counter stays above every id already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConnectionId(pub u64);

impl ConnectionId {
    fn counter() -> &'static AtomicU64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        &NEXT
    }

    pub fn next() -> Self {
        ConnectionId(Self::counter().fetch_add(1, Ordering::Relaxed))
    }

    /// Keep the id counter above every id already reserved (e.g. loaded from disk).
    pub fn observe(self) {
        let counter = Self::counter();
        let mut current = counter.load(Ordering::Relaxed);
        while self.0 >= current {
            match counter.compare_exchange(
                current,
                self.0 + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Everything needed to open a connection, plus the name shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    #[serde(default = "ConnectionId::next")]
    pub id: ConnectionId,
    pub name: String,
    pub driver: Driver,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Kept in memory as typed. Persistence writes every other field to JSON
    /// and stores this in the OS keychain (see `dbui_app::store`) rather than
    /// putting a password in a plaintext file.
    #[serde(default, skip_serializing)]
    pub password: String,
    pub database: String,
    #[serde(default)]
    pub tls: TlsMode,
}

impl ConnectionConfig {
    /// A blank connection pre-filled with the engine's conventional defaults.
    pub fn new(driver: Driver) -> Self {
        Self {
            id: ConnectionId::next(),
            name: format!("New {}", driver.label()),
            driver,
            host: "localhost".into(),
            port: driver.default_port(),
            username: match driver {
                Driver::Postgres => "postgres".into(),
                Driver::MySql => "root".into(),
            },
            password: String::new(),
            database: driver.default_database().into(),
            tls: TlsMode::default(),
        }
    }

    /// `user@host:port/database`, for the sidebar subtitle and the status bar.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != self.driver.default_port() {
            s.push(':');
            s.push_str(&self.port.to_string());
        }
        if !self.database.is_empty() {
            s.push('/');
            s.push_str(&self.database);
        }
        s
    }

    /// Reasons this config cannot be dialed, in the order a form should show
    /// them. Empty means good to go.
    pub fn validate(&self) -> Vec<&'static str> {
        let mut problems = Vec::new();
        if self.name.trim().is_empty() {
            problems.push("Name is required");
        }
        if self.host.trim().is_empty() {
            problems.push("Host is required");
        }
        if self.port == 0 {
            problems.push("Port must be greater than zero");
        }
        if self.username.trim().is_empty() {
            problems.push("Username is required");
        }
        if self.driver == Driver::Postgres && self.database.trim().is_empty() {
            problems.push("PostgreSQL requires a database name");
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_escape_their_own_quote() {
        assert_eq!(Driver::Postgres.quote_identifier("we\"ird"), "\"we\"\"ird\"");
        assert_eq!(Driver::MySql.quote_identifier("we`ird"), "`we``ird`");
    }

    #[test]
    fn summary_hides_the_default_port() {
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.host = "db.internal".into();
        config.database = "shop".into();
        assert_eq!(config.summary(), "postgres@db.internal/shop");

        config.port = 6543;
        assert_eq!(config.summary(), "postgres@db.internal:6543/shop");
    }

    #[test]
    fn ids_are_unique() {
        assert_ne!(ConnectionId::next(), ConnectionId::next());
    }

    #[test]
    fn observing_a_loaded_id_keeps_next_above_it() {
        ConnectionId::observe(ConnectionId(1_000));
        assert!(ConnectionId::next().0 > 1_000);
    }

    #[test]
    fn a_fresh_config_is_valid() {
        assert!(ConnectionConfig::new(Driver::Postgres).validate().is_empty());
        assert!(ConnectionConfig::new(Driver::MySql).validate().is_empty());
    }
}
