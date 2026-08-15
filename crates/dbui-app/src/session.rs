//! What was open when the app last closed.
//!
//! `connections.json` says which servers exist; this says which of them were on
//! screen and what each had open. The two are separate files on purpose: losing
//! a session is a cosmetic annoyance, losing the connection list is not, and a
//! session that fails to parse must never take the connections down with it.
//!
//! Nothing here is authoritative. Every id is checked against the saved
//! connections on load ([`Session::prune`]), so deleting a connection cannot
//! leave a tab pointing at a server that is gone.

use crate::store::StoreError;
use dbui_domain::{ConnectionId, SortKey};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// `~/.config/dbui/session.json` (or the platform equivalent).
pub fn session_path() -> Result<PathBuf, StoreError> {
    Ok(crate::store::config_dir()?.join("session.json"))
}

/// One table or SQL tab as it survives a restart.
///
/// Rows are deliberately absent: they are a cache of what the server held at
/// the time, and restoring them would show yesterday's data under today's
/// heading. What is kept is the question, not the answer — which table, which
/// filter, which columns were hidden, what SQL was typed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SavedTab {
    Table {
        schema: String,
        name: String,
        /// Applied WHERE body (empty = no filter).
        #[serde(default)]
        where_clause: String,
        #[serde(default)]
        hidden_columns: Vec<String>,
        /// Which column the grid was sorted by. `default` so a session
        /// written before sorting existed still loads.
        #[serde(default)]
        sort: Option<SortKey>,
    },
    Sql {
        #[serde(default)]
        text: String,
    },
}

/// One connection tab: which connection it points at, and what it had open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedConnectionTab {
    pub connection: ConnectionId,
    #[serde(default)]
    pub tabs: Vec<SavedTab>,
    #[serde(default)]
    pub active_tab: usize,
    /// Schemas left open in the tree. Which folders were unfolded is part of
    /// where the user was, the same as which tables were open.
    #[serde(default)]
    pub expanded: Vec<String>,
}

impl SavedConnectionTab {
    pub fn new(connection: ConnectionId) -> Self {
        Self {
            connection,
            tabs: Vec::new(),
            active_tab: 0,
            expanded: Vec::new(),
        }
    }
}

/// The connection tabs that were open, and which was in front.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub tabs: Vec<SavedConnectionTab>,
    /// Index into `tabs`. Out of range is treated as "the first one".
    #[serde(default)]
    pub active: usize,
}

impl Session {
    /// Drop anything that no longer refers to a saved connection.
    ///
    /// Runs on load and again before save. A connection deleted in another
    /// window — or by hand-editing `connections.json` — otherwise leaves a tab
    /// that draws a name for a server nothing can open.
    ///
    /// Duplicates go too: two tabs onto one connection would both restore into
    /// the same tab set, and the second would silently win.
    pub fn prune(&mut self, known: &[ConnectionId]) {
        let active_id = self.active_connection();

        let mut seen = Vec::new();
        self.tabs.retain(|tab| {
            let keep = known.contains(&tab.connection) && !seen.contains(&tab.connection);
            if keep {
                seen.push(tab.connection);
            }
            keep
        });

        // Follow the connection that was active rather than its old index:
        // pruning a tab to its left would otherwise shift the selection.
        self.active = active_id
            .and_then(|id| self.tabs.iter().position(|tab| tab.connection == id))
            .unwrap_or(0);
    }

    /// The connection in front, if there is one.
    pub fn active_connection(&self) -> Option<ConnectionId> {
        self.tabs.get(self.active).map(|tab| tab.connection)
    }

    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }
}

/// Read the last session, treating "no file yet" and "unreadable" alike.
///
/// A first launch has no file. A corrupt one is not worth a dialog either: the
/// worst case is an empty tab bar, and refusing to start over a cache would be
/// a far worse trade than losing it.
pub fn load(path: &Path) -> Session {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Session::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write the session, creating the directory if it is missing.
///
/// Written to a sibling temp file and renamed into place. A plain write
/// truncates first, so anything reading — or a crash — during it sees half a
/// file; this runs on every tab click, which makes that window one worth
/// closing. A rename on the same filesystem is atomic, so a reader gets either
/// the old session or the new one and never a torn one.
pub fn save(path: &Path, session: &Session) -> Result<(), StoreError> {
    let write_error = |path: &Path, error: std::io::Error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| write_error(parent, error))?;
    }

    let text = serde_json::to_string_pretty(session).map_err(|error| StoreError::Write {
        path: path.to_path_buf(),
        message: error.to_string(),
    })?;

    // The pid keeps two processes from renaming each other's half-written file
    // into place.
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temp, text).map_err(|error| write_error(&temp, error))?;
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        write_error(path, error)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-session-test-{}-{name}", std::process::id()));
        path.push("session.json");
        path
    }

    fn tab_with(connection: u64, tabs: Vec<SavedTab>) -> SavedConnectionTab {
        SavedConnectionTab {
            connection: ConnectionId(connection),
            tabs,
            active_tab: 0,
            expanded: Vec::new(),
        }
    }

    fn table_tab(name: &str) -> SavedTab {
        SavedTab::Table {
            schema: "public".into(),
            name: name.into(),
            where_clause: String::new(),
            hidden_columns: Vec::new(),
            sort: None,
        }
    }

    #[test]
    fn a_missing_file_reads_as_no_session() {
        assert_eq!(load(&temp_path("missing")), Session::default());
    }

    /// The whole point: what was open comes back, down to the SQL text.
    #[test]
    fn tabs_survive_a_round_trip() {
        let path = temp_path("roundtrip");
        let mut first = tab_with(
            1,
            vec![
                SavedTab::Table {
                    schema: "public".into(),
                    name: "users".into(),
                    where_clause: "id > 10".into(),
                    hidden_columns: vec!["secret".into()],
                    sort: None,
                },
                SavedTab::Sql {
                    text: "select 1".into(),
                },
            ],
        );
        first.expanded = vec!["public".into(), "drizzle".into()];

        let session = Session {
            tabs: vec![first, tab_with(2, vec![table_tab("orders")])],
            active: 1,
        };

        save(&path, &session).unwrap();
        assert_eq!(load(&path), session);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A session written by an earlier build has no `expanded` key. It has to
    /// keep loading -- the alternative is everyone who upgrades losing the
    /// tabs they had open.
    #[test]
    fn a_session_from_before_expanded_existed_still_loads() {
        let path = temp_path("older");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"tabs":[{"connection":3,"tabs":[
                 {"kind":"table","schema":"iobot","name":"activities",
                  "where_clause":"","hidden_columns":[]}],
               "active_tab":0}],"active":0}"#,
        )
        .unwrap();

        let session = load(&path);
        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.tabs[0].tabs.len(), 1);
        assert!(session.tabs[0].expanded.is_empty());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A session is a cache. Failing to parse it must cost the tabs and
    /// nothing else -- never the launch.
    #[test]
    fn a_corrupt_session_opens_empty_rather_than_failing() {
        let path = temp_path("corrupt");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ not json").unwrap();

        assert_eq!(load(&path), Session::default());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The save is a rename over a temp file, so a reader racing it sees one
    /// whole session or the other -- never half of one. It must also not leave
    /// the temp file behind.
    #[test]
    fn saving_replaces_the_file_atomically() {
        let path = temp_path("atomic");
        let first = Session {
            tabs: vec![tab_with(1, vec![table_tab("users")])],
            active: 0,
        };
        let second = Session {
            tabs: vec![tab_with(2, vec![table_tab("orders")])],
            active: 0,
        };

        save(&path, &first).unwrap();
        save(&path, &second).unwrap();

        assert_eq!(load(&path), second);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn pruning_drops_tabs_whose_connection_is_gone() {
        let mut session = Session {
            tabs: vec![tab_with(1, vec![]), tab_with(2, vec![]), tab_with(3, vec![])],
            active: 0,
        };

        session.prune(&[ConnectionId(1), ConnectionId(3)]);

        assert_eq!(
            session.tabs.iter().map(|t| t.connection).collect::<Vec<_>>(),
            vec![ConnectionId(1), ConnectionId(3)]
        );
    }

    /// Pruning a tab to the left of the active one must not slide the
    /// selection onto its neighbour.
    #[test]
    fn pruning_keeps_the_same_connection_in_front() {
        let mut session = Session {
            tabs: vec![tab_with(1, vec![]), tab_with(2, vec![]), tab_with(3, vec![])],
            active: 2,
        };

        session.prune(&[ConnectionId(2), ConnectionId(3)]);

        assert_eq!(session.active_connection(), Some(ConnectionId(3)));
    }

    /// Two tabs onto one connection would restore into the same tab set, and
    /// the second would quietly take the first one's tabs.
    #[test]
    fn pruning_collapses_a_duplicated_connection() {
        let mut session = Session {
            tabs: vec![
                tab_with(1, vec![table_tab("users")]),
                tab_with(1, vec![table_tab("orders")]),
            ],
            active: 0,
        };

        session.prune(&[ConnectionId(1)]);

        assert_eq!(session.tabs.len(), 1);
        assert_eq!(session.tabs[0].tabs, vec![table_tab("users")]);
    }

    #[test]
    fn an_active_index_past_the_end_falls_back_to_the_first_tab() {
        let mut session = Session {
            tabs: vec![tab_with(1, vec![])],
            active: 9,
        };
        assert_eq!(session.active_connection(), None);

        session.prune(&[ConnectionId(1)]);
        assert_eq!(session.active, 0);
        assert_eq!(session.active_connection(), Some(ConnectionId(1)));
    }
}
