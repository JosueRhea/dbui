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
    /// Schemas the user has opened in the tree.
    ///
    /// Outlives the connection and is written to the session: which folders
    /// are unfolded is where the user was, not what the server said. The
    /// catalog beside it is the opposite, and goes on disconnect.
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
    /// happens. `expanded` stays -- it names folders, not rows, and
    /// reconnecting should put the tree back the way it was rather than
    /// collapsing everything the user had opened.
    pub fn disconnect(&mut self) {
        self.status = ConnectionStatus::Disconnected;
        self.catalog = None;
    }
}

/// Every connection the user has saved, which of them are open as tabs, and
/// which tab is in front.
///
/// `entries` is the address book and `open` is the tab bar: a subset of the
/// same ids, held separately because tab order is the user's arrangement and
/// has nothing to do with the order connections were created in. Every id in
/// `open` names an entry, `active` is always one of them, and both invariants
/// are maintained here rather than trusted to callers.
#[derive(Default)]
pub struct Workspace {
    entries: Vec<ConnectionEntry>,
    /// Connections open as tabs, in tab order.
    open: Vec<ConnectionId>,
    active: Option<ConnectionId>,
    /// The table whose rows are on screen, if the user got there by clicking
    /// the tree rather than by typing a query.
    pub open_table: Option<TableRef>,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load the address book, opening the first connection as a tab.
    ///
    /// A restored session replaces that opening tab via [`Workspace::restore_open`];
    /// this is what a first launch, or a launch with no session, gets.
    pub fn from_configs(configs: impl IntoIterator<Item = ConnectionConfig>) -> Self {
        let entries: Vec<_> = configs.into_iter().map(ConnectionEntry::new).collect();
        let active = entries.first().map(ConnectionEntry::id);
        Self {
            entries,
            open: active.into_iter().collect(),
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

    // -- open tabs ----------------------------------------------------------

    /// The connections open as tabs, in tab order.
    pub fn open_ids(&self) -> &[ConnectionId] {
        &self.open
    }

    /// The open tabs' entries, in tab order — what the tab bar draws.
    pub fn open_entries(&self) -> impl Iterator<Item = &ConnectionEntry> {
        self.open.iter().filter_map(|id| self.get(*id))
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    pub fn is_open(&self, id: ConnectionId) -> bool {
        self.open.contains(&id)
    }

    /// Position of the front tab in the tab bar.
    pub fn active_index(&self) -> Option<usize> {
        let active = self.active?;
        self.open.iter().position(|id| *id == active)
    }

    /// Bring the tab at `index` to the front. Returns the connection it names.
    pub fn activate_index(&mut self, index: usize) -> Option<ConnectionId> {
        let id = *self.open.get(index)?;
        self.activate(id);
        Some(id)
    }

    /// Replace the tab bar wholesale, dropping ids with no saved connection.
    ///
    /// Used once at startup to apply a restored session. Anything unknown is
    /// skipped rather than rejected: a session naming a deleted connection
    /// should cost that one tab, not the restore.
    pub fn restore_open(&mut self, ids: impl IntoIterator<Item = ConnectionId>, active: usize) {
        let mut open = Vec::new();
        for id in ids {
            if self.get(id).is_some() && !open.contains(&id) {
                open.push(id);
            }
        }
        if open.is_empty() {
            return;
        }
        self.active = Some(open[active.min(open.len() - 1)]);
        self.open = open;
        self.open_table = None;
    }

    /// Open `id` as a tab, or focus the tab it already has. Returns its index.
    pub fn open_connection(&mut self, id: ConnectionId) -> Option<usize> {
        self.get(id)?;
        if !self.open.contains(&id) {
            self.open.push(id);
        }
        self.activate(id);
        self.open.iter().position(|open| *open == id)
    }

    /// Close a tab without forgetting the connection.
    ///
    /// The neighbour to the right takes over, which is where the eye already
    /// is; at the end of the bar it falls back to the left. Returns whichever
    /// connection ends up in front.
    pub fn close_connection(&mut self, id: ConnectionId) -> Option<ConnectionId> {
        let Some(index) = self.open.iter().position(|open| *open == id) else {
            return self.active;
        };
        self.open.remove(index);

        if self.active == Some(id) {
            // `index` now names the tab that was to the right.
            self.active = self
                .open
                .get(index)
                .or_else(|| self.open.last())
                .copied();
            self.open_table = None;
        }
        self.active
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

    /// Bring a connection to the front, opening a tab for it if it has none.
    ///
    /// Switching clears the open table -- it belonged to the server we just
    /// left. The caller restores it from the newly-fronted tab's own tab set.
    pub fn activate(&mut self, id: ConnectionId) {
        if self.active != Some(id) {
            self.open_table = None;
        }
        if !self.open.contains(&id) && self.get(id).is_some() {
            self.open.push(id);
        }
        self.active = Some(id);
    }

    /// Save a new connection and open it as a tab.
    pub fn add(&mut self, config: ConnectionConfig) -> ConnectionId {
        let id = config.id;
        self.entries.push(ConnectionEntry::new(config));
        self.open.push(id);
        self.active = Some(id);
        self.open_table = None;
        id
    }

    /// Forget a connection entirely, closing its tab if it had one.
    pub fn remove(&mut self, id: ConnectionId) {
        self.close_connection(id);
        self.entries.retain(|entry| entry.id() != id);
        // `close_connection` only promotes a neighbour when the closed tab was
        // in front; a stale `active` here means it was the last tab open.
        if self.active == Some(id) {
            self.active = self.open.first().copied();
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
        workspace.open_connection(second);

        workspace.remove(first);
        assert_eq!(workspace.active_id(), Some(second));

        workspace.remove(second);
        assert_eq!(workspace.active_id(), None);
        assert!(workspace.is_empty());
    }

    // -- open tabs ----------------------------------------------------------

    /// A saved connection is not an open tab. Only the first one starts open,
    /// which is what the window showed before tabs existed.
    #[test]
    fn saved_connections_are_not_all_open_tabs() {
        let workspace = workspace_with(3);
        assert_eq!(workspace.entries().len(), 3);
        assert_eq!(workspace.open_count(), 1);
        assert_eq!(workspace.open_ids(), &[workspace.entries()[0].id()]);
    }

    #[test]
    fn opening_a_connection_appends_a_tab_and_fronts_it() {
        let mut workspace = workspace_with(3);
        let third = workspace.entries()[2].id();

        assert_eq!(workspace.open_connection(third), Some(1));
        assert_eq!(workspace.active_id(), Some(third));
        assert_eq!(workspace.active_index(), Some(1));
    }

    /// Clicking a connection that is already open focuses its tab rather than
    /// opening a second one onto the same server.
    #[test]
    fn opening_an_open_connection_focuses_its_existing_tab() {
        let mut workspace = workspace_with(2);
        let first = workspace.entries()[0].id();
        let second = workspace.entries()[1].id();
        workspace.open_connection(second);

        assert_eq!(workspace.open_connection(first), Some(0));
        assert_eq!(workspace.open_count(), 2);
        assert_eq!(workspace.active_id(), Some(first));
    }

    #[test]
    fn closing_the_front_tab_promotes_the_one_to_its_right() {
        let mut workspace = workspace_with(3);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();
        workspace.open_connection(ids[1]);
        workspace.open_connection(ids[2]);
        workspace.activate(ids[1]);

        workspace.close_connection(ids[1]);
        assert_eq!(workspace.open_ids(), &[ids[0], ids[2]]);
        assert_eq!(workspace.active_id(), Some(ids[2]));
    }

    /// At the end of the bar there is nothing to the right, so the tab to the
    /// left takes over instead of leaving nothing selected.
    #[test]
    fn closing_the_last_tab_falls_back_to_the_left() {
        let mut workspace = workspace_with(2);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();
        workspace.open_connection(ids[1]);

        workspace.close_connection(ids[1]);
        assert_eq!(workspace.active_id(), Some(ids[0]));
    }

    /// Closing a tab is not deleting a connection: it stays in the address
    /// book, ready to be reopened from the picker.
    #[test]
    fn closing_a_tab_keeps_the_connection_saved() {
        let mut workspace = workspace_with(2);
        let first = workspace.entries()[0].id();

        workspace.close_connection(first);
        assert_eq!(workspace.open_count(), 0);
        assert_eq!(workspace.entries().len(), 2);
        assert!(workspace.get(first).is_some());
    }

    #[test]
    fn closing_the_only_tab_leaves_nothing_in_front() {
        let mut workspace = workspace_with(1);
        let only = workspace.entries()[0].id();

        assert_eq!(workspace.close_connection(only), None);
        assert_eq!(workspace.active_id(), None);
        assert_eq!(workspace.active_index(), None);
    }

    /// Deleting a connection has to take its tab with it, or the bar draws a
    /// name for a server nothing can open.
    #[test]
    fn removing_a_connection_closes_its_tab_too() {
        let mut workspace = workspace_with(2);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();
        workspace.open_connection(ids[1]);

        workspace.remove(ids[0]);
        assert_eq!(workspace.open_ids(), &[ids[1]]);
        assert_eq!(workspace.active_id(), Some(ids[1]));
    }

    #[test]
    fn a_new_connection_opens_as_a_tab() {
        let mut workspace = workspace_with(1);
        let added = workspace.add(ConnectionConfig::new(Driver::Postgres));

        assert_eq!(workspace.open_count(), 2);
        assert_eq!(workspace.active_id(), Some(added));
    }

    #[test]
    fn restoring_a_session_replaces_the_tab_bar() {
        let mut workspace = workspace_with(3);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();

        workspace.restore_open([ids[2], ids[0]], 1);
        assert_eq!(workspace.open_ids(), &[ids[2], ids[0]]);
        assert_eq!(workspace.active_id(), Some(ids[0]));
    }

    /// A session naming a connection that has since been deleted should cost
    /// that one tab, not the whole restore.
    #[test]
    fn restoring_skips_ids_with_no_saved_connection() {
        let mut workspace = workspace_with(2);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();

        workspace.restore_open([ConnectionId(9_999), ids[1]], 1);
        assert_eq!(workspace.open_ids(), &[ids[1]]);
        assert_eq!(workspace.active_id(), Some(ids[1]));
    }

    /// The stored index is clamped rather than trusted: a hand-edited session
    /// must not leave the window with no tab in front.
    #[test]
    fn restoring_clamps_an_active_index_past_the_end() {
        let mut workspace = workspace_with(2);
        let ids: Vec<_> = workspace.entries().iter().map(|e| e.id()).collect();

        workspace.restore_open([ids[0], ids[1]], 9);
        assert_eq!(workspace.active_id(), Some(ids[1]));
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
        assert!(!entry.status.is_connected());
        assert_eq!(
            entry.expanded,
            vec!["public".to_string()],
            "which folders were open is not stale data -- reconnecting should \
             not collapse the tree"
        );
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
