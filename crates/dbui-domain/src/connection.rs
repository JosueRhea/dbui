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
    Sqlite,
}

impl Driver {
    pub const ALL: [Driver; 3] = [Driver::Postgres, Driver::MySql, Driver::Sqlite];

    pub fn label(self) -> &'static str {
        match self {
            Driver::Postgres => "PostgreSQL",
            Driver::MySql => "MySQL",
            Driver::Sqlite => "SQLite",
        }
    }

    /// Whether this engine is reached over the network.
    ///
    /// SQLite is a file. Host, port, user and password mean nothing to it, and
    /// the connection form hides them rather than asking for values that are
    /// then ignored.
    pub fn is_file_based(self) -> bool {
        matches!(self, Driver::Sqlite)
    }

    pub fn default_port(self) -> u16 {
        match self {
            Driver::Postgres => 5432,
            Driver::MySql => 3306,
            // Not a port at all; kept so the field has something in it.
            Driver::Sqlite => 0,
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
            // For SQLite the "database" is the path to the file, and there is
            // no sensible default for that.
            Driver::Sqlite => "",
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
            // SQLite accepts both spellings; double quotes are the standard
            // one and match what the Postgres branch emits.
            Driver::Sqlite => format!("\"{}\"", ident.replace('"', "\"\"")),
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

    /// The id carried by a config that arrived without one, until the loader
    /// has seen which numbers the rest of the file already claims.
    ///
    /// Minting the id while deserializing instead -- the obvious `default` --
    /// reads a counter that nothing has primed yet, so `[{"id": 1}, {}]`
    /// parses into two connections both numbered 1, sharing one keychain
    /// entry; and a parse that then fails has moved the counter anyway.
    /// Zero is safe as the marker because [`ConnectionId::next`] starts at one
    /// and so has never handed it out.
    pub(crate) fn unassigned() -> Self {
        ConnectionId(0)
    }

    /// Whether this id still has to be minted (see `unassigned`).
    pub fn is_unassigned(self) -> bool {
        self.0 == 0
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

/// A hop through an SSH server to reach a database that only it can see.
///
/// The usual shape of a production database: it listens on a private network,
/// and the one way in is a bastion host. The tunnel forwards a local port to
/// the database's `host:port` *as the bastion resolves them*, so the
/// connection's own host is typically a private name or address.
///
/// Kept whole while switched off, so unticking the box to try a direct
/// connection does not throw away what was typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default = "SshConfig::default_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    /// A private key file. Empty means whatever `ssh` would use on its own:
    /// the agent, then `~/.ssh/config`, then the default key names.
    #[serde(default)]
    pub key_path: String,
    /// The SSH password, or the key's passphrase -- `ssh` asks for whichever
    /// it needs, and this is the answer to either. Never written to the
    /// connections file; the keychain holds it, like the database password.
    #[serde(default, skip_serializing)]
    pub password: String,
}

impl SshConfig {
    pub const DEFAULT_PORT: u16 = 22;

    fn default_port() -> u16 {
        Self::DEFAULT_PORT
    }

    /// `user@host:port`, for the sidebar subtitle.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        if !self.username.is_empty() {
            s.push_str(&self.username);
            s.push('@');
        }
        s.push_str(&self.host);
        if self.port != Self::DEFAULT_PORT {
            s.push(':');
            s.push_str(&self.port.to_string());
        }
        s
    }

    fn validate(&self, problems: &mut Vec<&'static str>) {
        if self.host.trim().is_empty() {
            problems.push("SSH host is required");
        }
        if self.port == 0 {
            problems.push("SSH port must be greater than zero");
        }
    }
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: Self::DEFAULT_PORT,
            username: String::new(),
            key_path: String::new(),
            password: String::new(),
        }
    }
}

/// Everything needed to open a connection, plus the name shown in the sidebar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// `default` so a hand-written or hand-merged file that leaves the id out
    /// still loads; the loader mints the real one once it knows what is taken.
    #[serde(default = "ConnectionId::unassigned")]
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
    /// Refuse every write through this connection.
    ///
    /// The app can `DROP`, `TRUNCATE` and commit batch deletes, and the
    /// TablePlus import brings connections over wholesale -- production ones
    /// included. This is the switch that makes a server safe to browse.
    /// `default` so a connections file written before it existed still loads.
    #[serde(default)]
    pub read_only: bool,
    /// Stop a statement that runs longer than this many seconds, on the
    /// server as well as in the app. Zero -- the default, and what a file
    /// written before this existed loads as -- waits for as long as it takes.
    #[serde(default)]
    pub query_timeout_secs: u32,
    /// Reach the server through an SSH tunnel. Ignored by file-based engines.
    /// `default` so a connections file written before tunnels existed loads.
    #[serde(default)]
    pub ssh: SshConfig,
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
                Driver::Sqlite => String::new(),
            },
            password: String::new(),
            database: driver.default_database().into(),
            tls: TlsMode::default(),
            read_only: false,
            query_timeout_secs: 0,
            ssh: SshConfig::default(),
        }
    }

    /// Whether opening this connection means dialing an SSH server first.
    pub fn uses_ssh(&self) -> bool {
        self.ssh.enabled && !self.driver.is_file_based()
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
        if self.uses_ssh() && !self.ssh.host.is_empty() {
            s.push_str(" via ");
            s.push_str(&self.ssh.host);
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

        // A file-based engine has no host, port or user to check -- demanding
        // them would be asking for values that are then ignored.
        if self.driver.is_file_based() {
            if self.database.trim().is_empty() {
                problems.push("A database file path is required");
            }
            return problems;
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
        if self.uses_ssh() {
            self.ssh.validate(&mut problems);
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_escape_their_own_quote() {
        assert_eq!(
            Driver::Postgres.quote_identifier("we\"ird"),
            "\"we\"\"ird\""
        );
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

        config.ssh.enabled = true;
        config.ssh.host = "bastion".into();
        assert_eq!(
            config.summary(),
            "postgres@db.internal:6543/shop via bastion"
        );
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
        assert!(ConnectionConfig::new(Driver::Postgres)
            .validate()
            .is_empty());
        assert!(ConnectionConfig::new(Driver::MySql).validate().is_empty());
    }

    #[test]
    fn a_tunnel_needs_a_host_only_when_it_is_switched_on() {
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.ssh.host = String::new();
        assert!(config.validate().is_empty());

        config.ssh.enabled = true;
        assert_eq!(config.validate(), vec!["SSH host is required"]);

        config.ssh.host = "bastion.example.com".into();
        assert!(config.validate().is_empty());
    }

    #[test]
    fn a_file_never_goes_through_a_tunnel() {
        let mut config = ConnectionConfig::new(Driver::Sqlite);
        config.ssh.enabled = true;
        assert!(!config.uses_ssh());
    }

    #[test]
    fn a_file_written_before_tunnels_loads_without_one() {
        let json = r#"{"id":1,"name":"a","driver":"postgres","host":"h","port":5432,
            "username":"u","database":"d"}"#;
        let config: ConnectionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.ssh, SshConfig::default());
    }

    #[test]
    fn the_ssh_password_never_reaches_the_file() {
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.ssh.password = "hunter2".into();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("hunter2"));
    }
}
