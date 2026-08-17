//! Saved connections on disk, passwords in the OS keychain.
//!
//! [`ConnectionConfig::password`] is `skip_serializing`, so
//! `connections.json` holds hosts and usernames and nothing that grants
//! access on its own. Passwords are stored under the service name `dbui`,
//! keyed by connection id, and hydrated back into memory on load.

use dbui_domain::{ConnectionConfig, ConnectionId};
use keyring::Entry;
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "dbui";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("Could not locate a configuration directory for this user")]
    NoConfigDir,
    #[error("Could not read {path}: {message}")]
    Read { path: PathBuf, message: String },
    #[error("Could not write {path}: {message}")]
    Write { path: PathBuf, message: String },
    #[error("{path} is not valid connection JSON: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("The keychain refused the password: {message}")]
    Keychain { message: String },
}

fn keychain_error(error: keyring::Error) -> StoreError {
    StoreError::Keychain {
        message: error.to_string(),
    }
}

/// Environment variable pointing dbui at a different configuration directory.
pub const CONFIG_DIR_VAR: &str = "DBUI_CONFIG_DIR";

/// `~/.config/dbui` (or the platform equivalent), unless [`CONFIG_DIR_VAR`]
/// says otherwise.
///
/// The override is what lets a second profile exist side by side — and what
/// keeps the UI tests, which persist a session as they click around, out of
/// the developer's own configuration.
/// Write a file by renaming a sibling temp file over it.
///
/// A plain write truncates before it fills, so anything reading -- or a crash
/// -- during that window sees half a file. A rename on the same filesystem is
/// atomic, so a reader gets one whole version or the other and never a torn
/// one. Shared by the session and the history, both of which are rewritten
/// often enough for that window to matter.
pub fn write_atomic(path: &Path, text: &str) -> Result<(), StoreError> {
    let write_error = |path: &Path, error: std::io::Error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| write_error(parent, error))?;
    }

    // The pid keeps two processes from renaming each other's half-written file
    // into place.
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, text).map_err(|error| write_error(&temp, error))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        write_error(path, error)
    })
}

pub fn config_dir() -> Result<PathBuf, StoreError> {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_VAR) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let base = dirs::config_dir().ok_or(StoreError::NoConfigDir)?;
    Ok(base.join("dbui"))
}

/// `~/.config/dbui/connections.json` (or the platform equivalent).
pub fn connections_path() -> Result<PathBuf, StoreError> {
    Ok(config_dir()?.join("connections.json"))
}

/// `~/.config/dbui/prefs.json` — UI preferences like the active theme.
pub fn prefs_path() -> Result<PathBuf, StoreError> {
    Ok(config_dir()?.join("prefs.json"))
}

/// Window preferences persisted beside connections.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Prefs {
    /// Theme id (`wave`, `light`, `gruvbox-dark`, …).
    #[serde(default = "default_theme_id")]
    pub theme: String,
    /// UI zoom percentage (100 = default). Clamped to 50–200 on apply.
    #[serde(default = "default_zoom_pct")]
    pub zoom_pct: u32,
    /// SQL editor pane height in CSS pixels (unzoomed).
    #[serde(default = "default_sql_editor_height_px")]
    pub sql_editor_height_px: u32,
}

fn default_theme_id() -> String {
    "wave".into()
}

fn default_zoom_pct() -> u32 {
    100
}

fn default_sql_editor_height_px() -> u32 {
    150
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: default_theme_id(),
            zoom_pct: default_zoom_pct(),
            sql_editor_height_px: default_sql_editor_height_px(),
        }
    }
}

pub fn load_prefs(path: &Path) -> Result<Prefs, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Prefs::default())
        }
        Err(error) => {
            return Err(StoreError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
        }
    };
    serde_json::from_str(&text).map_err(|error| StoreError::Parse {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

pub fn save_prefs(path: &Path, prefs: &Prefs) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StoreError::Write {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }
    let text = serde_json::to_string_pretty(prefs).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;
    std::fs::write(path, text).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

/// Saved connections, and the reason any of their passwords are missing.
///
/// A keychain that will not answer is worth saying out loud -- the connection
/// will fail to authenticate later, and "wrong password" is a poor explanation
/// for a password that was never read -- but it is no reason to hide hosts and
/// usernames that read perfectly well.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Loaded {
    pub configs: Vec<ConnectionConfig>,
    /// Set when at least one password could not be read from the keychain.
    pub password_error: Option<String>,
}

/// Read saved connections, treating "no file yet" as "no connections yet".
///
/// Passwords are pulled from the OS keychain when present. A first launch has
/// no file, and that is not a failure worth showing anyone.
pub fn load(path: &Path) -> Result<Loaded, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::default()),
        Err(error) => {
            return Err(StoreError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            })
        }
    };

    let mut configs: Vec<ConnectionConfig> =
        serde_json::from_str(&text).map_err(|error| StoreError::Parse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;

    let mut password_error = None;
    for config in &mut configs {
        ConnectionId::observe(config.id);
        match load_password(config.id) {
            Ok(password) => config.password = password,
            Err(error) => {
                config.password.clear();
                password_error.get_or_insert_with(|| error.to_string());
            }
        }
    }

    Ok(Loaded {
        configs,
        password_error,
    })
}

/// Write saved connections, creating the directory if it is missing.
///
/// Each config's password is synced to the keychain; empty passwords remove
/// any existing secret for that id. A keychain that refuses is an error even
/// though the JSON landed: the connections are saved, but a password the user
/// just typed is not, and they will be asked for it again.
pub fn save(path: &Path, configs: &[ConnectionConfig]) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StoreError::Write {
            path: parent.to_path_buf(),
            message: error.to_string(),
        })?;
    }

    let text = serde_json::to_string_pretty(configs).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    std::fs::write(path, text).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    // Every password is attempted before the first failure is reported: the
    // JSON is already on disk, so stopping early would leave the keychain
    // agreeing with neither the old file nor the new one.
    let mut refused = None;
    for config in configs {
        if let Err(error) = store_password(config.id, &config.password) {
            refused.get_or_insert(error);
        }
    }

    match refused {
        Some(error) => Err(keychain_error(error)),
        None => Ok(()),
    }
}

/// Drop the keychain secret for a connection that is being deleted.
pub fn delete_password(id: ConnectionId) -> Result<(), StoreError> {
    let entry = password_entry(id).map_err(keychain_error)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(keychain_error(error)),
    }
}

fn password_entry(id: ConnectionId) -> keyring::Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, &format!("connection-{id}"))
}

fn store_password(id: ConnectionId, password: &str) -> keyring::Result<()> {
    let entry = password_entry(id)?;
    if password.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        entry.set_password(password)
    }
}

fn load_password(id: ConnectionId) -> keyring::Result<String> {
    match password_entry(id)?.get_password() {
        Ok(password) => Ok(password),
        Err(keyring::Error::NoEntry) => Ok(String::new()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_domain::Driver;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-store-test-{}-{name}", std::process::id()));
        path.push("connections.json");
        path
    }

    #[test]
    fn a_missing_file_reads_as_no_connections() {
        let path = temp_path("missing");
        assert_eq!(load(&path).unwrap(), Loaded::default());
    }

    #[test]
    fn configs_survive_a_round_trip_and_passwords_stay_out_of_json() {
        let path = temp_path("roundtrip");
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.name = "Staging".into();
        config.host = "db.internal".into();
        config.password = "hunter2".into();
        let id = config.id;

        // Keychain may be unavailable in some CI sandboxes; still verify JSON.
        let keychain_ok = store_password(id, &config.password).is_ok();
        let saved = save(&path, std::slice::from_ref(&config));
        assert_eq!(saved.is_ok(), keychain_ok, "the keychain is reported");

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("hunter2"), "passwords must not reach disk");

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.configs.len(), 1);
        assert_eq!(loaded.configs[0].name, "Staging");
        assert_eq!(loaded.configs[0].host, "db.internal");
        assert_eq!(loaded.configs[0].id, id);
        if keychain_ok {
            assert_eq!(loaded.configs[0].password, "hunter2");
            assert_eq!(loaded.password_error, None);
            delete_password(id).unwrap();
        }

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn removing_a_password_clears_the_keychain_entry() {
        let id = ConnectionId::next();
        if store_password(id, "secret").is_err() {
            return;
        }
        assert_eq!(load_password(id).unwrap(), "secret");
        delete_password(id).unwrap();
        assert_eq!(load_password(id).unwrap(), "");
    }

    #[test]
    fn a_connection_still_loads_when_its_password_does_not() {
        let path = temp_path("keychainless");
        let mut config = ConnectionConfig::new(Driver::Postgres);
        config.name = "Staging".into();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(&[&config]).unwrap()).unwrap();

        // No secret was ever stored for this id, so a working keychain answers
        // "no entry" -- an empty password and nothing to report. A keychain
        // that is not there at all fails instead, and *that* is reported: the
        // connection is still listed, but the password behind it is not.
        let readable = load_password(config.id).is_ok();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.configs.len(), 1, "the connection is still listed");
        assert_eq!(loaded.configs[0].password, "");
        assert_eq!(loaded.password_error.is_none(), readable);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn malformed_json_is_reported_not_swallowed() {
        let path = temp_path("malformed");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert!(matches!(load(&path), Err(StoreError::Parse { .. })));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
