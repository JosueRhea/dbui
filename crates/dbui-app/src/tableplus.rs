//! Import saved connections from TablePlus on macOS.
//!
//! TablePlus keeps connection metadata in
//! `~/Library/Application Support/com.tinyapp.TablePlus/Data/Connections.plist`
//! (Setapp uses `com.tinyapp.TablePlus-setapp`). Passwords live in the OS
//! keychain under service `com.tableplus.TablePlus`, account `{uuid}_database`.
//!
//! Only PostgreSQL and MySQL are imported — those are the engines dbui speaks.
//! A connection TablePlus reaches over SSH comes across with its tunnel: the
//! bastion's address, port and user, and its password (account
//! `{uuid}_server`) when the keychain has one. One that names no bastion is
//! skipped, because the only thing left to import is a direct dial to a host
//! that was never meant to be reachable directly.

use dbui_domain::{ConnectionConfig, ConnectionId, Driver, Environment, SshConfig, TlsMode};
use keyring::Entry;
use plist::Value as PlistValue;
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "com.tableplus.TablePlus";

#[derive(Debug, thiserror::Error)]
pub enum TablePlusError {
    #[error("TablePlus connections file not found (looked in standard Application Support paths)")]
    NotFound,
    #[error("Could not read {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("{path} is not a TablePlus connections list: {message}")]
    Parse { path: PathBuf, message: String },
}

/// Result of scanning TablePlus and converting what we can.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Ready to add to the workspace (new ids, passwords hydrated when found).
    pub imported: Vec<ConnectionConfig>,
    /// Skipped because an equivalent connection already exists in dbui.
    pub skipped_existing: usize,
    /// Skipped because the driver is not PostgreSQL / MySQL.
    pub skipped_unsupported: usize,
    /// Skipped because the connection uses an SSH tunnel whose server is not
    /// in the file.
    pub skipped_ssh: usize,
    /// Imported without a password (keychain miss or empty).
    pub missing_password: usize,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.imported.is_empty() {
            parts.push(format!(
                "Imported {} connection{}",
                self.imported.len(),
                if self.imported.len() == 1 { "" } else { "s" }
            ));
        }
        if self.skipped_existing > 0 {
            parts.push(format!("{} already present", self.skipped_existing));
        }
        if self.skipped_unsupported > 0 {
            parts.push(format!(
                "{} unsupported driver{}",
                self.skipped_unsupported,
                if self.skipped_unsupported == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        if self.skipped_ssh > 0 {
            parts.push(format!(
                "{} over SSH with no server named (skipped)",
                self.skipped_ssh
            ));
        }
        if self.missing_password > 0 {
            parts.push(format!("{} without password", self.missing_password));
        }
        if parts.is_empty() {
            "No TablePlus connections to import".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// Candidate paths, standalone then Setapp.
pub fn connections_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(support) = dirs::home_dir().map(|h| h.join("Library/Application Support")) {
        out.push(
            support
                .join("com.tinyapp.TablePlus")
                .join("Data")
                .join("Connections.plist"),
        );
        out.push(
            support
                .join("com.tinyapp.TablePlus-setapp")
                .join("Data")
                .join("Connections.plist"),
        );
    }
    out
}

/// First existing Connections.plist among the usual locations.
pub fn find_connections_plist() -> Option<PathBuf> {
    connections_candidates().into_iter().find(|p| p.is_file())
}

/// Import PostgreSQL / MySQL connections from TablePlus.
///
/// `existing` is used to skip duplicates (same driver, host, port, user, database).
pub fn import_from_tableplus(
    existing: &[ConnectionConfig],
) -> Result<ImportReport, TablePlusError> {
    let path = find_connections_plist().ok_or(TablePlusError::NotFound)?;
    import_from_plist(&path, existing)
}

pub fn import_from_plist(
    path: &Path,
    existing: &[ConnectionConfig],
) -> Result<ImportReport, TablePlusError> {
    let root = PlistValue::from_file(path).map_err(|error| TablePlusError::Read {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    let array = root.as_array().ok_or_else(|| TablePlusError::Parse {
        path: path.to_path_buf(),
        message: "expected an array of connections".into(),
    })?;

    let mut report = ImportReport::default();

    for item in array {
        let Some(dict) = item.as_dictionary() else {
            continue;
        };

        let tp_id = plist_string(dict.get("ID")).unwrap_or_default();

        let ssh = if plist_bool(dict.get("isOverSSH")).unwrap_or(false) {
            match ssh_config(dict, &tp_id) {
                Some(ssh) => ssh,
                None => {
                    report.skipped_ssh += 1;
                    continue;
                }
            }
        } else {
            SshConfig::default()
        };

        let driver_label = plist_string(dict.get("Driver")).unwrap_or_default();
        let Some(driver) = map_driver(&driver_label) else {
            report.skipped_unsupported += 1;
            continue;
        };

        let host = plist_string(dict.get("DatabaseHost")).unwrap_or_default();
        if host.trim().is_empty() {
            report.skipped_unsupported += 1;
            continue;
        }

        let port = parse_port(plist_string(dict.get("DatabasePort")), driver);
        let username = plist_string(dict.get("DatabaseUser")).unwrap_or_default();
        let database = plist_string(dict.get("DatabaseName")).unwrap_or_default();
        let tls = map_tls(plist_int(dict.get("tLSMode")));

        if already_have(existing, driver, &host, port, &username, &database)
            || already_have(&report.imported, driver, &host, port, &username, &database)
        {
            report.skipped_existing += 1;
            continue;
        }

        let password = load_tableplus_password(&tp_id, "database").unwrap_or_default();
        if password.is_empty() {
            report.missing_password += 1;
        }

        let name = connection_name(
            &plist_string(dict.get("ConnectionName")).unwrap_or_default(),
            &database,
            &host,
            driver,
        );

        report.imported.push(ConnectionConfig {
            id: ConnectionId::next(),
            name,
            driver,
            host,
            port,
            username,
            password,
            database,
            tls,
            // An import brings whole servers over at once, production ones
            // among them. They arrive writable, the same as a hand-typed
            // connection -- the flag is the user's to set, not ours to guess.
            read_only: false,
            query_timeout_secs: 0,
            ssh,
            environment: map_environment(
                plist_string(dict.get("Enviroment"))
                    .or_else(|| plist_string(dict.get("Environment"))),
            ),
        });
    }

    Ok(report)
}

fn map_driver(label: &str) -> Option<Driver> {
    match label.trim().to_ascii_lowercase().as_str() {
        "postgresql" | "postgres" => Some(Driver::Postgres),
        "mysql" | "mariadb" => Some(Driver::MySql),
        _ => None,
    }
}

/// TablePlus tags each connection too, and the tag is the one thing about a
/// production server worth bringing over intact. (Its plist has spelled the
/// key both ways over the years.)
fn map_environment(label: Option<String>) -> Environment {
    match label
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "local" => Environment::Local,
        "testing" | "development" => Environment::Testing,
        "staging" => Environment::Staging,
        "production" => Environment::Production,
        _ => Environment::None,
    }
}

fn map_tls(mode: Option<i64>) -> TlsMode {
    // TablePlus: 0 prefer/auto, 1 require-ish in practice varies by build.
    // Prefer matches our default and works for PlanetScale-style hosts that
    // still list tlsMode 0 in the plist.
    match mode.unwrap_or(0) {
        2 => TlsMode::Require,
        1 => TlsMode::Prefer,
        _ => TlsMode::Prefer,
    }
}

fn parse_port(raw: Option<String>, driver: Driver) -> u16 {
    raw.and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            t.parse().ok()
        }
    })
    .unwrap_or_else(|| driver.default_port())
}

fn connection_name(raw: &str, database: &str, host: &str, driver: Driver) -> String {
    let trimmed = raw.trim();
    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("New Connection") {
        return trimmed.to_string();
    }
    if !database.trim().is_empty() {
        return database.trim().to_string();
    }
    if !host.trim().is_empty() {
        return host.trim().to_string();
    }
    format!("Imported {}", driver.label())
}

fn already_have(
    configs: &[ConnectionConfig],
    driver: Driver,
    host: &str,
    port: u16,
    username: &str,
    database: &str,
) -> bool {
    configs.iter().any(|c| {
        c.driver == driver
            && c.host.eq_ignore_ascii_case(host)
            && c.port == port
            && c.username == username
            && c.database == database
    })
}

/// The tunnel TablePlus used for a connection, or `None` when the file does
/// not say which server it goes through.
fn ssh_config(dict: &plist::Dictionary, tp_id: &str) -> Option<SshConfig> {
    let host = plist_string(dict.get("ServerAddress")).unwrap_or_default();
    if host.trim().is_empty() {
        return None;
    }
    let port = plist_string(dict.get("ServerPort"))
        .and_then(|port| port.trim().parse().ok())
        .filter(|port| *port != 0)
        .unwrap_or(SshConfig::DEFAULT_PORT);
    // TablePlus may keep a key as a bare name inside its own container, which
    // is no path `ssh` can open. Only a real path is carried over; without
    // one, ssh falls back to the agent and `~/.ssh/config`, which is where a
    // bastion key usually is anyway.
    let key_path = plist_string(dict.get("ServerPrivateKeyName"))
        .map(|key| key.trim().to_string())
        .filter(|key| key.starts_with('/') || key.starts_with("~/"))
        .unwrap_or_default();
    Some(SshConfig {
        enabled: true,
        host: host.trim().to_string(),
        port,
        username: plist_string(dict.get("ServerUser"))
            .unwrap_or_default()
            .trim()
            .to_string(),
        key_path,
        password: load_tableplus_password(tp_id, "server").unwrap_or_default(),
    })
}

/// A secret TablePlus keeps in the keychain: `kind` is `database` for the
/// database password and `server` for the SSH one.
fn load_tableplus_password(connection_id: &str, kind: &str) -> Option<String> {
    if connection_id.is_empty() {
        return None;
    }
    let account = format!("{connection_id}_{kind}");
    let entry = Entry::new(KEYCHAIN_SERVICE, &account).ok()?;
    match entry.get_password() {
        Ok(password) => Some(password),
        Err(keyring::Error::NoEntry) => None,
        Err(_) => None,
    }
}

fn plist_string(value: Option<&PlistValue>) -> Option<String> {
    match value? {
        PlistValue::String(s) => Some(s.clone()),
        PlistValue::Integer(i) => i
            .as_signed()
            .map(|n| n.to_string())
            .or_else(|| i.as_unsigned().map(|n| n.to_string())),
        _ => None,
    }
}

fn plist_int(value: Option<&PlistValue>) -> Option<i64> {
    match value? {
        PlistValue::Integer(i) => i.as_signed(),
        PlistValue::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn plist_bool(value: Option<&PlistValue>) -> Option<bool> {
    match value? {
        PlistValue::Boolean(b) => Some(*b),
        PlistValue::Integer(i) => i.as_signed().map(|n| n != 0),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_fixture(path: &Path, xml: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(xml.as_bytes()).unwrap();
    }

    #[test]
    fn imports_postgres_and_mysql_and_skips_the_rest() {
        let dir = std::env::temp_dir().join(format!(
            "dbui-tableplus-{}-{}",
            std::process::id(),
            "import"
        ));
        let path = dir.join("Connections.plist");
        write_fixture(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>ID</key><string>AAAA</string>
    <key>ConnectionName</key><string>Prod PG</string>
    <key>Driver</key><string>PostgreSQL</string>
    <key>DatabaseHost</key><string>db.example.com</string>
    <key>DatabasePort</key><string></string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>app</string>
    <key>tLSMode</key><integer>0</integer>
    <key>isOverSSH</key><false/>
    <key>Enviroment</key><string>production</string>
  </dict>
  <dict>
    <key>ID</key><string>BBBB</string>
    <key>ConnectionName</key><string></string>
    <key>Driver</key><string>MySQL</string>
    <key>DatabaseHost</key><string>mysql.example.com</string>
    <key>DatabasePort</key><string>3307</string>
    <key>DatabaseUser</key><string>root</string>
    <key>DatabaseName</key><string>shop</string>
    <key>tLSMode</key><integer>2</integer>
    <key>isOverSSH</key><false/>
  </dict>
  <dict>
    <key>ID</key><string>CCCC</string>
    <key>ConnectionName</key><string>Redis</string>
    <key>Driver</key><string>Redis</string>
    <key>DatabaseHost</key><string>redis.example.com</string>
    <key>isOverSSH</key><false/>
  </dict>
  <dict>
    <key>ID</key><string>DDDD</string>
    <key>ConnectionName</key><string>Via SSH</string>
    <key>Driver</key><string>PostgreSQL</string>
    <key>DatabaseHost</key><string>db.internal</string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>app</string>
    <key>isOverSSH</key><true/>
  </dict>
</array>
</plist>"#,
        );

        let report = import_from_plist(&path, &[]).unwrap();
        assert_eq!(report.imported.len(), 2);
        assert_eq!(report.skipped_unsupported, 1);
        assert_eq!(report.skipped_ssh, 1);

        let pg = &report.imported[0];
        assert_eq!(pg.name, "Prod PG");
        assert_eq!(pg.driver, Driver::Postgres);
        assert_eq!(pg.port, 5432);
        assert_eq!(pg.tls, TlsMode::Prefer);
        assert_eq!(pg.environment, Environment::Production);

        let mysql = &report.imported[1];
        assert_eq!(mysql.name, "shop");
        assert_eq!(mysql.driver, Driver::MySql);
        assert_eq!(mysql.port, 3307);
        assert_eq!(mysql.tls, TlsMode::Require);
        assert_eq!(mysql.environment, Environment::None);

        let _ = std::fs::remove_dir_all(dir);
    }

    /// A connection TablePlus reaches over SSH keeps its tunnel; one that
    /// does not name the SSH server is skipped rather than imported as a
    /// direct dial to a host that was never meant to be reachable.
    #[test]
    fn brings_ssh_connections_over_with_their_tunnel() {
        let dir =
            std::env::temp_dir().join(format!("dbui-tableplus-{}-{}", std::process::id(), "ssh"));
        let path = dir.join("Connections.plist");
        write_fixture(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>Driver</key><string>PostgreSQL</string>
    <key>ConnectionName</key><string>Behind a bastion</string>
    <key>DatabaseHost</key><string>10.0.0.5</string>
    <key>DatabasePort</key><string>5432</string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>prod</string>
    <key>isOverSSH</key><true/>
    <key>ServerAddress</key><string>bastion.example.com</string>
    <key>ServerPort</key><string>2222</string>
    <key>ServerUser</key><string>deploy</string>
    <key>ServerPrivateKeyName</key><string>~/.ssh/id_bastion</string>
  </dict>
  <dict>
    <key>Driver</key><string>PostgreSQL</string>
    <key>ConnectionName</key><string>No server named</string>
    <key>DatabaseHost</key><string>10.0.0.6</string>
    <key>DatabasePort</key><string>5432</string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>other</string>
    <key>isOverSSH</key><true/>
  </dict>
  <dict>
    <key>Driver</key><string>PostgreSQL</string>
    <key>ConnectionName</key><string>Direct</string>
    <key>DatabaseHost</key><string>127.0.0.1</string>
    <key>DatabasePort</key><string>5432</string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>dev</string>
  </dict>
</array>
</plist>
"#,
        );

        let report = import_from_plist(&path, &[]).expect("import");
        assert_eq!(report.skipped_ssh, 1);
        assert_eq!(report.imported.len(), 2);

        let tunneled = &report.imported[0];
        assert_eq!(tunneled.host, "10.0.0.5");
        assert!(tunneled.uses_ssh());
        assert_eq!(tunneled.ssh.host, "bastion.example.com");
        assert_eq!(tunneled.ssh.port, 2222);
        assert_eq!(tunneled.ssh.username, "deploy");
        assert_eq!(tunneled.ssh.key_path, "~/.ssh/id_bastion");

        let direct = &report.imported[1];
        assert_eq!(direct.host, "127.0.0.1");
        assert!(!direct.uses_ssh());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_duplicates_already_in_dbui() {
        let dir = std::env::temp_dir().join(format!(
            "dbui-tableplus-{}-{}",
            std::process::id(),
            "dedupe"
        ));
        let path = dir.join("Connections.plist");
        write_fixture(
            &path,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<array>
  <dict>
    <key>ID</key><string>AAAA</string>
    <key>ConnectionName</key><string>Prod</string>
    <key>Driver</key><string>PostgreSQL</string>
    <key>DatabaseHost</key><string>db.example.com</string>
    <key>DatabasePort</key><string>5432</string>
    <key>DatabaseUser</key><string>postgres</string>
    <key>DatabaseName</key><string>app</string>
    <key>isOverSSH</key><false/>
  </dict>
</array>
</plist>"#,
        );

        let mut existing = ConnectionConfig::new(Driver::Postgres);
        existing.host = "db.example.com".into();
        existing.port = 5432;
        existing.username = "postgres".into();
        existing.database = "app".into();

        let report = import_from_plist(&path, &[existing]).unwrap();
        assert!(report.imported.is_empty());
        assert_eq!(report.skipped_existing, 1);

        let _ = std::fs::remove_dir_all(dir);
    }
}
