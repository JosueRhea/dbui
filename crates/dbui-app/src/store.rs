//! Saved connections on disk, passwords in the OS keychain.
//!
//! [`ConnectionConfig::password`] is `skip_serializing`, so
//! `connections.json` holds hosts and usernames and nothing that grants
//! access on its own. Passwords are stored under the service name `dbui`,
//! keyed by connection id, and hydrated back into memory on load. An SSH
//! tunnel's password is a second secret beside the first, under its own
//! account, and goes through exactly the same rules.

use dbui_domain::{ConnectionConfig, ConnectionId};
use keyring::Entry;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
    /// Schema tree width in CSS pixels (unzoomed).
    #[serde(default = "default_sidebar_width_px")]
    pub sidebar_width_px: u32,
    /// Row detail panel width in CSS pixels (unzoomed).
    #[serde(default = "default_detail_width_px")]
    pub detail_width_px: u32,
    /// Blur the desktop through the titlebar and sidebar, with the content
    /// in a rounded card on top. On unless the user turns it off.
    #[serde(default = "default_translucent")]
    pub translucent: bool,
    /// How strongly the theme tints the glass, 0–100.
    #[serde(default = "default_glass_opacity_pct")]
    pub glass_opacity_pct: u32,
    /// How far the glass blurs the desktop behind it, in points. macOS's own
    /// sidebar material uses 30; a little more keeps busy windows behind
    /// from showing through as shapes.
    #[serde(default = "default_glass_blur")]
    pub glass_blur: u32,
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

fn default_sidebar_width_px() -> u32 {
    258
}

fn default_detail_width_px() -> u32 {
    280
}

pub fn default_translucent() -> bool {
    true
}

/// Heavy enough that the theme, not the desktop, sets the chrome's colour --
/// which is what keeps its text readable over a bright window.
pub fn default_glass_opacity_pct() -> u32 {
    80
}

pub fn default_glass_blur() -> u32 {
    40
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: default_theme_id(),
            zoom_pct: default_zoom_pct(),
            sql_editor_height_px: default_sql_editor_height_px(),
            sidebar_width_px: default_sidebar_width_px(),
            detail_width_px: default_detail_width_px(),
            translucent: default_translucent(),
            glass_opacity_pct: default_glass_opacity_pct(),
            glass_blur: default_glass_blur(),
        }
    }
}

pub fn load_prefs(path: &Path) -> Result<Prefs, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Prefs::default()),
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

/// Read saved connections, treating "no file yet" as "no connections yet".
///
/// Passwords are pulled from the OS keychain when present. A first launch has
/// no file, and that is not a failure worth showing anyone.
pub fn load(path: &Path) -> Result<Vec<ConnectionConfig>, StoreError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        // No file is a first launch, not a failure: there are no ids to miss
        // and nothing on disk for a later save to destroy.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            set_unloaded(path, false);
            return Ok(Vec::new());
        }
        Err(error) => {
            set_unloaded(path, true);
            return Err(StoreError::Read {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };

    let mut configs: Vec<ConnectionConfig> = match serde_json::from_str(&text) {
        Ok(configs) => configs,
        Err(error) => {
            set_unloaded(path, true);
            return Err(StoreError::Parse {
                path: path.to_path_buf(),
                message: error.to_string(),
            });
        }
    };

    assign_ids(&mut configs);
    set_unloaded(path, false);

    for config in &mut configs {
        for secret in Secret::ALL {
            let value = match load_secret(config.id, secret) {
                Ok(password) => {
                    set_secret_unread(config.id, secret, false);
                    password
                }
                // A locked keychain, or a user who pressed Deny on the prompt,
                // leaves us with no password for a connection that may well
                // have one. The field has to be empty because there is nothing
                // to put in it, so remember that the emptiness is ours and not
                // the user's before `save` reads it as an instruction to
                // delete.
                Err(_) => {
                    set_secret_unread(config.id, secret, true);
                    String::new()
                }
            };
            *secret.field_mut(config) = value;
        }
    }

    Ok(configs)
}

/// Write saved connections, creating the directory if it is missing.
///
/// Each config's password is synced to the keychain; an empty password
/// removes the secret for that id, unless the password is empty only because
/// reading it failed (see [`password_sync`]).
pub fn save(path: &Path, configs: &[ConnectionConfig]) -> Result<(), StoreError> {
    // The connections in a file we could not read are ones whose ids were
    // never observed, so the counter hands the next new connection an id that
    // is already taken on disk -- and the first save of that duplicate deletes
    // the older connection's keychain password. Refusing before the write and
    // before the password loop leaves both the file and the keychain as they
    // were, which is the only state anyone can recover from.
    if is_unloaded(path) {
        return Err(StoreError::Write {
            path: path.to_path_buf(),
            message: "it could not be read when dbui started, and saving would \
                      discard the connections it holds -- repair or move the \
                      file, then restart dbui"
                .into(),
        });
    }

    let text = serde_json::to_string_pretty(configs).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    // A half-written connections file does not parse, and the app answers an
    // unparseable one with an empty connection list that the next save then
    // makes permanent.
    write_atomic(path, &text)?;

    for config in configs {
        for secret in Secret::ALL {
            let value = secret.field(config);
            match password_sync(value, secret_unread(config.id, secret)) {
                PasswordSync::Keep => {}
                PasswordSync::Delete => {
                    let _ = store_secret(config.id, secret, "");
                }
                PasswordSync::Set => {
                    // Having written it, we now know it, so a later empty
                    // field for this id is the user clearing it rather than
                    // our own gap.
                    if store_secret(config.id, secret, value).is_ok() {
                        set_secret_unread(config.id, secret, false);
                    }
                }
            }
        }
    }

    Ok(())
}

/// What [`save`] should do with the keychain secret for one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasswordSync {
    Keep,
    Delete,
    Set,
}

/// Decide between keeping, deleting and writing a connection's secret.
///
/// An empty password means two opposite things depending on where it came
/// from: the user emptied the field, or we never managed to read the field in
/// the first place. Only the first is an instruction to delete, and a delete
/// cannot be taken back, so the unread case keeps whatever is already there.
fn password_sync(password: &str, unread: bool) -> PasswordSync {
    if !password.is_empty() {
        PasswordSync::Set
    } else if unread {
        PasswordSync::Keep
    } else {
        PasswordSync::Delete
    }
}

/// The secrets one connection can keep in the keychain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Secret {
    /// The database password.
    Database,
    /// The SSH tunnel's password or key passphrase.
    Ssh,
}

impl Secret {
    const ALL: [Secret; 2] = [Secret::Database, Secret::Ssh];

    /// The keychain account. The database one keeps the name it had before
    /// there was a second, so upgrading loses nobody's password.
    fn account(self, id: ConnectionId) -> String {
        match self {
            Secret::Database => format!("connection-{id}"),
            Secret::Ssh => format!("connection-{id}-ssh"),
        }
    }

    fn field(self, config: &ConnectionConfig) -> &str {
        match self {
            Secret::Database => &config.password,
            Secret::Ssh => &config.ssh.password,
        }
    }

    fn field_mut(self, config: &mut ConnectionConfig) -> &mut String {
        match self {
            Secret::Database => &mut config.password,
            Secret::Ssh => &mut config.ssh.password,
        }
    }
}

/// Connections whose password this process failed to read.
///
/// Kept here rather than on `ConnectionConfig` because the flag is about this
/// process's luck with the keychain, not about the connection: it must never
/// reach disk, and it has to survive the config being cloned through the
/// workspace and rebuilt by the connection form.
static UNREAD_PASSWORDS: Mutex<BTreeSet<(ConnectionId, Secret)>> = Mutex::new(BTreeSet::new());

fn set_secret_unread(id: ConnectionId, secret: Secret, unread: bool) {
    let mut ids = UNREAD_PASSWORDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if unread {
        ids.insert((id, secret));
    } else {
        ids.remove(&(id, secret));
    }
}

fn secret_unread(id: ConnectionId, secret: Secret) -> bool {
    UNREAD_PASSWORDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(&(id, secret))
}

#[cfg(test)]
fn set_password_unread(id: ConnectionId, unread: bool) {
    set_secret_unread(id, Secret::Database, unread);
}

/// Files whose last [`load`] failed, and which [`save`] must therefore leave
/// alone.
///
/// Keyed by path rather than remembered by the caller because the refusal has
/// to outlive every config the UI is holding: the danger is exactly that the
/// in-memory list is empty while the file is not.
static UNLOADED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn set_unloaded(path: &Path, unloaded: bool) {
    let mut paths = UNLOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if unloaded {
        paths.insert(path.to_path_buf());
    } else {
        paths.remove(path);
    }
}

fn is_unloaded(path: &Path) -> bool {
    UNLOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(path)
}

/// Keep the id counter clear of the file, and give an id to anything that
/// arrived without one.
///
/// The two passes cannot be merged: an id has to be minted knowing every
/// number the file claims, including the ones further down it, or the new
/// connection lands on an existing one and inherits its keychain entry.
fn assign_ids(configs: &mut [ConnectionConfig]) {
    for config in configs.iter() {
        if !config.id.is_unassigned() {
            ConnectionId::observe(config.id);
        }
    }
    for config in configs.iter_mut() {
        if config.id.is_unassigned() {
            config.id = ConnectionId::next();
        }
    }
}

/// Drop the keychain secret for a connection that is being deleted.
///
/// Refused when the connections file failed to load this session, and when we
/// cannot even name that file. Deleting the connection dbui has just said it
/// will not save is a very plausible next click, and the id being deleted was
/// minted against a counter the unread file never primed -- so it may name
/// somebody else's connection, whose password this would take with it.
pub fn delete_password(id: ConnectionId) {
    let Ok(connections) = connections_path() else {
        return;
    };
    delete_password_at(&connections, id);
}

/// The body of [`delete_password`], with the file it judges by passed in.
/// `true` means the keychain was reached for.
fn delete_password_at(connections: &Path, id: ConnectionId) -> bool {
    if is_unloaded(connections) {
        return false;
    }
    let mut reached = false;
    for secret in Secret::ALL {
        if let Ok(entry) = secret_entry(id, secret) {
            let _ = entry.delete_credential();
            reached = true;
        }
    }
    reached
}

fn secret_entry(id: ConnectionId, secret: Secret) -> keyring::Result<Entry> {
    Entry::new(KEYCHAIN_SERVICE, &secret.account(id))
}

#[cfg(test)]
fn store_password(id: ConnectionId, password: &str) -> keyring::Result<()> {
    store_secret(id, Secret::Database, password)
}

#[cfg(test)]
fn load_password(id: ConnectionId) -> keyring::Result<String> {
    load_secret(id, Secret::Database)
}

fn store_secret(id: ConnectionId, secret: Secret, password: &str) -> keyring::Result<()> {
    let entry = secret_entry(id, secret)?;
    if password.is_empty() {
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        entry.set_password(password)
    }
}

fn load_secret(id: ConnectionId, secret: Secret) -> keyring::Result<String> {
    match secret_entry(id, secret)?.get_password() {
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
        assert_eq!(load(&path).unwrap(), Vec::new());
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
        save(&path, std::slice::from_ref(&config)).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("hunter2"), "passwords must not reach disk");

        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "Staging");
        assert_eq!(loaded[0].host, "db.internal");
        assert_eq!(loaded[0].id, id);
        if keychain_ok {
            assert_eq!(loaded[0].password, "hunter2");
        }

        delete_password(id);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn removing_a_password_clears_the_keychain_entry() {
        let id = ConnectionId::next();
        if store_password(id, "secret").is_err() {
            return;
        }
        assert_eq!(load_password(id).unwrap(), "secret");
        delete_password(id);
        assert_eq!(load_password(id).unwrap(), "");
    }

    #[test]
    fn an_unread_password_is_kept_while_a_cleared_one_is_deleted() {
        assert_eq!(password_sync("", true), PasswordSync::Keep);
        assert_eq!(password_sync("", false), PasswordSync::Delete);
        assert_eq!(password_sync("hunter2", false), PasswordSync::Set);
        assert_eq!(password_sync("hunter2", true), PasswordSync::Set);
    }

    /// A read-only file is the one case where the two implementations differ
    /// observably without a crash: `fs::write` needs to open the file itself,
    /// while a rename only needs a writable directory.
    #[cfg(unix)]
    #[test]
    fn saving_over_an_unwritable_file_still_replaces_it_whole() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path("atomic");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let first = ConnectionConfig::new(Driver::Postgres);
        let mut second = ConnectionConfig::new(Driver::MySql);
        second.name = "Survivor".into();
        let before = vec![first, second.clone()];
        std::fs::write(&path, serde_json::to_string_pretty(&before).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();

        // Root ignores the mode bits, so there the plain write would pass this
        // test for the wrong reason.
        if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
            return;
        }

        // Nothing read this password, so the save must leave the keychain
        // alone -- which is also what keeps this test off the real one.
        set_password_unread(second.id, true);
        save(&path, std::slice::from_ref(&second)).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let after: Vec<ConnectionConfig> = serde_json::from_str(&raw).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].name, "Survivor");

        let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        assert!(!temp.exists(), "the temp file must not outlive the rename");

        set_password_unread(second.id, false);
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

    #[test]
    fn a_file_that_failed_to_load_is_not_saved_over() {
        let path = temp_path("unloaded");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let on_disk = "[{\"id\": 7, \"name\": \"Prod\"";
        std::fs::write(&path, on_disk).unwrap();

        assert!(matches!(load(&path), Err(StoreError::Parse { .. })));

        let config = ConnectionConfig::new(Driver::Sqlite);
        // Nothing read this password, so even a save that got through would
        // leave the keychain alone -- which is what keeps this test off the
        // real one.
        set_password_unread(config.id, true);
        assert!(matches!(
            save(&path, std::slice::from_ref(&config)),
            Err(StoreError::Write { .. })
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), on_disk);

        set_password_unread(config.id, false);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_first_launch_with_no_file_can_still_save() {
        let path = temp_path("first-launch");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());

        assert_eq!(load(&path).unwrap(), Vec::new());

        let mut config = ConnectionConfig::new(Driver::Sqlite);
        config.name = "Notes".into();
        set_password_unread(config.id, true);
        save(&path, std::slice::from_ref(&config)).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("Notes"));

        set_password_unread(config.id, false);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The good file holds no connections on purpose: hydrating a password
    /// would reach for the real keychain, and what is under test is the path,
    /// not the secrets.
    #[test]
    fn a_repaired_file_can_be_saved_over_again() {
        let path = temp_path("repaired");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert!(load(&path).is_err());
        assert!(save(&path, &[]).is_err());

        std::fs::write(&path, "[]").unwrap();
        assert_eq!(load(&path).unwrap(), Vec::new());
        save(&path, &[]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_config_that_omits_its_id_does_not_reuse_one_from_the_same_file() {
        let raw = r#"[
            {"id": 1, "name": "Prod", "driver": "postgres", "host": "db",
             "port": 5432, "username": "u", "database": "shop"},
            {"name": "Notes", "driver": "sqlite", "host": "", "port": 0,
             "username": "", "database": "/tmp/notes.db"}
        ]"#;

        let mut configs: Vec<ConnectionConfig> = serde_json::from_str(raw).unwrap();
        assign_ids(&mut configs);

        assert_eq!(configs[0].id, ConnectionId(1));
        assert!(!configs[1].id.is_unassigned());
        assert_ne!(configs[1].id, configs[0].id);
    }

    #[test]
    fn a_file_that_failed_to_load_holds_back_the_keychain_delete() {
        let path = temp_path("delete-guard");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert!(load(&path).is_err());

        // False is "never reached for the secret", which is both the point of
        // the guard and what keeps this test off the real keychain.
        assert!(!delete_password_at(&path, ConnectionId(1)));

        set_unloaded(&path, false);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The input that tells the two passes apart from one interleaved pass: a
    /// config with no id standing *before* the ids the file goes on to claim.
    /// Minting as we walk would hand it one of them.
    #[test]
    fn an_id_is_minted_only_after_the_whole_file_has_been_read() {
        // Above every id the rest of the suite mints, so the numbers claimed
        // below are exactly the ones a single pass would mint into.
        let base = 9_000_000_000;
        ConnectionId::observe(ConnectionId(base));

        let mut entries = vec![r#"{"name": "Notes", "driver": "sqlite", "host": "",
             "port": 0, "username": "", "database": "/tmp/notes.db"}"#
            .to_string()];
        // A block of ids rather than one: connections minted in parallel by
        // other tests shift the counter, and every landing spot is taken.
        for claimed in base + 1..=base + 32 {
            entries.push(format!(
                r#"{{"id": {claimed}, "name": "Prod", "driver": "postgres",
                     "host": "db", "port": 5432, "username": "u",
                     "database": "shop"}}"#
            ));
        }

        let mut configs: Vec<ConnectionConfig> =
            serde_json::from_str(&format!("[{}]", entries.join(","))).unwrap();
        assign_ids(&mut configs);

        let ids: BTreeSet<ConnectionId> = configs.iter().map(|config| config.id).collect();
        assert_eq!(ids.len(), configs.len(), "an id was handed out twice");
    }
}
