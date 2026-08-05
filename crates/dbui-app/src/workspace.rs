//! Application state: the connections the user has, and what is known of each.
//!
//! Plain data with plain methods -- no rendering, no I/O, no async. The UI owns
//! one of these and mutates it as tasks come back, which is what makes the app
//! layer testable without a window.

use dbui_domain::{Catalog, ConnectionConfig, ConnectionId, TableRef};
use dbui_driver::DatabaseDriver;
use std::sync::Arc;

/// Where a connection is in its lifecycle.
#[derive(Clone)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected(Arc<dyn DatabaseDriver>),
    Failed(String),
}

impl ConnectionStatus {
    pub fn is_connected(&self) -> bool {
        matches!(self, ConnectionStatus::Connected(_))
    }

    pub fn is_busy(&self) -> bool {
        matches!(self, ConnectionStatus::Connecting)
    }

    pub fn driver(&self) -> Option<&Arc<dyn DatabaseDriver>> {
        match self {
            ConnectionStatus::Connected(driver) => Some(driver),
            _ => None,
        }
    }
}

/// One saved connection and everything learned about it since.
pub struct ConnectionEntry {
    pub config: ConnectionConfig,
    pub status: ConnectionStatus,
    pub catalog: Option<Catalog>,
    /// Schemas the user has opened in the tree. Not persisted: it describes a
    /// glance at the data, not the data.
    pub expanded: Vec<String>,
}

impl ConnectionEntry {
    pub fn new(config: ConnectionConfig) -> Self {
        Self {
            config,
            status: ConnectionStatus::Disconnected,
            catalog: None,
            expanded: Vec::new(),
        }
    }

    pub fn id(&self) -> ConnectionId {
        self.config.id
    }

    pub fn is_expanded(&self, schema: &str) -> bool {
        self.expanded.iter().any(|name| name == schema)
    }

    pub fn toggle_schema(&mut self, schema: &str) {
        match self.expanded.iter().position(|name| name == schema) {
            Some(index) => {
                self.expanded.remove(index);
            }
            None => self.expanded.push(schema.to_string()),
        }
    }

    /// Drop everything that only made sense while connected.
    ///
    /// The catalog goes too: showing yesterday's tables under a connection
    /// that is no longer open invites clicking one and wondering why nothing
    /// happens.
    pub fn disconnect(&mut self) {
        self.status = ConnectionStatus::Disconnected;
        self.catalog = None;
        self.expanded.clear();
    }
}

/// Every connection in the window, and which one is in front.
#[derive(Default)]
pub struct Workspace {
    entries: Vec<ConnectionEntry>,
    active: Option<ConnectionId>,
    /// The table whose rows are on screen, if the user got there by clicking
    /// the tree rather than by typing a query.
    pub open_table: Option<TableRef>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_configs(configs: impl IntoIterator<Item = ConnectionConfig>) -> Self {
        let entries: Vec<_> = configs.into_iter().map(ConnectionEntry::new).collect();
        let active = entries.first().map(ConnectionEntry::id);
        Self {
            entries,
            active,
            open_table: None,
        }
    }

    pub fn entries(&self) -> &[ConnectionEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: ConnectionId) -> Option<&ConnectionEntry> {
        self.entries.iter().find(|entry| entry.id() == id)
    }

    pub fn get_mut(&mut self, id: ConnectionId) -> Option<&mut ConnectionEntry> {
        self.entries.iter_mut().find(|entry| entry.id() == id)
    }

    pub fn active_id(&self) -> Option<ConnectionId> {
        self.active
    }

    pub fn active(&self) -> Option<&ConnectionEntry> {
        self.active.and_then(|id| self.get(id))
    }

    pub fn active_mut(&mut self) -> Option<&mut ConnectionEntry> {
        let id = self.active?;
        self.get_mut(id)
    }

    /// The driver for the active connection, if it is open.
    ///
    /// Cloned out rather than borrowed: the caller is about to hand it to a
    /// background task, which cannot hold a borrow of the workspace.
    pub fn active_driver(&self) -> Option<Arc<dyn DatabaseDriver>> {
        self.active()?.status.driver().cloned()
    }

    /// Switching connections clears the open table -- it belonged to the
    /// server we just left.
    pub fn activate(&mut self, id: ConnectionId) {
        if self.active != Some(id) {
            self.open_table = None;
        }
        self.active = Some(id);
    }

    pub fn add(&mut self, config: ConnectionConfig) -> ConnectionId {
        let id = config.id;
        self.entries.push(ConnectionEntry::new(config));
        self.active = Some(id);
        self.open_table = None;
        id
    }

    pub fn remove(&mut self, id: ConnectionId) {
        self.entries.retain(|entry| entry.id() != id);
        if self.active == Some(id) {
            self.active = self.entries.first().map(ConnectionEntry::id);
            self.open_table = None;
        }
    }

    /// The configs worth writing to disk.
    pub fn configs(&self) -> Vec<ConnectionConfig> {
        self.entries.iter().map(|entry| entry.config.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_domain::Driver;

    fn workspace_with(count: usize) -> Workspace {
        Workspace::from_configs(
            (0..count).map(|_| ConnectionConfig::new(Driver::Postgres)),
        )
    }

    #[test]
    fn the_first_connection_starts_active() {
        let workspace = workspace_with(2);
        assert_eq!(workspace.active_id(), Some(workspace.entries()[0].id()));
    }

    #[test]
    fn removing_the_active_connection_promotes_another() {
        let mut workspace = workspace_with(2);
        let first = workspace.entries()[0].id();
        let second = workspace.entries()[1].id();

        workspace.remove(first);
        assert_eq!(workspace.active_id(), Some(second));

        workspace.remove(second);
        assert_eq!(workspace.active_id(), None);
        assert!(workspace.is_empty());
    }

    #[test]
    fn switching_connections_closes_the_open_table() {
        let mut workspace = workspace_with(2);
        let second = workspace.entries()[1].id();
        workspace.open_table = Some(TableRef::new("public", "users"));

        workspace.activate(second);
        assert_eq!(workspace.open_table, None);
    }

    #[test]
    fn reactivating_the_same_connection_keeps_the_open_table() {
        let mut workspace = workspace_with(1);
        let only = workspace.entries()[0].id();
        let table = TableRef::new("public", "users");
        workspace.open_table = Some(table.clone());

        workspace.activate(only);
        assert_eq!(workspace.open_table, Some(table));
    }

    #[test]
    fn disconnecting_drops_the_stale_catalog() {
        let mut workspace = workspace_with(1);
        let entry = workspace.active_mut().unwrap();
        entry.catalog = Some(Catalog::default());
        entry.expanded.push("public".into());

        entry.disconnect();
        assert!(entry.catalog.is_none());
        assert!(entry.expanded.is_empty());
        assert!(!entry.status.is_connected());
    }

    #[test]
    fn schema_expansion_toggles() {
        let mut entry = ConnectionEntry::new(ConnectionConfig::new(Driver::MySql));
        assert!(!entry.is_expanded("shop"));
        entry.toggle_schema("shop");
        assert!(entry.is_expanded("shop"));
        entry.toggle_schema("shop");
        assert!(!entry.is_expanded("shop"));
    }
}
