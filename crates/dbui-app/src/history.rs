//! Every statement this app has run, newest first.
//!
//! Kept beside the session rather than inside it: losing it is cosmetic, and a
//! history that will not parse must not take the launch down with it. It is
//! also the one file here that grows without bound if left alone, so it is
//! capped on every write.

use dbui_domain::ConnectionId;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Past this the file is trimmed. Large enough to hold weeks of work, small
/// enough that loading it is not something the user waits for.
pub const MAX_ENTRIES: usize = 500;

/// One statement, and how it went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    /// Which connection it ran against, so the list can be narrowed to one.
    pub connection: Option<ConnectionId>,
    /// Seconds since the epoch. Stored rather than derived so the list keeps
    /// its order across restarts without depending on file order.
    pub at: u64,
    /// False when the server rejected it. A failed statement is often the one
    /// you most want back, so it is kept rather than dropped.
    pub ok: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub entries: Vec<HistoryEntry>,
}

impl History {
    /// Add a statement, newest first.
    ///
    /// Re-running the same SQL moves it up rather than adding a second copy:
    /// a history where one statement fills the first ten rows is a history you
    /// cannot find anything in.
    pub fn record(&mut self, entry: HistoryEntry) {
        let normalized = normalize(&entry.sql);
        if normalized.is_empty() {
            return;
        }
        self.entries
            .retain(|existing| normalize(&existing.sql) != normalized);
        self.entries.insert(0, entry);
        self.entries.truncate(MAX_ENTRIES);
    }

    /// Entries whose SQL contains every whitespace-separated term in `query`.
    ///
    /// All terms rather than the whole string, so "insert users" finds an
    /// INSERT into users however much SQL sits between the two words.
    pub fn search(&self, query: &str) -> Vec<&HistoryEntry> {
        let terms: Vec<String> = query
            .split_whitespace()
            .map(|term| term.to_lowercase())
            .collect();
        self.entries
            .iter()
            .filter(|entry| {
                if terms.is_empty() {
                    return true;
                }
                let haystack = entry.sql.to_lowercase();
                terms.iter().all(|term| haystack.contains(term))
            })
            .collect()
    }
}

/// Whitespace-insensitive form, for "is this the same statement".
fn normalize(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn history_path() -> Result<PathBuf, crate::store::StoreError> {
    Ok(crate::store::config_dir()?.join("history.json"))
}

/// Read the history, or an empty one.
///
/// Never an error: a history that will not parse is a history worth losing,
/// not a reason to fail a launch.
pub fn load(path: &Path) -> History {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Write it out, by rename so a crash mid-write cannot leave half a file.
pub fn save(path: &Path, history: &History) -> Result<(), crate::store::StoreError> {
    let text = serde_json::to_string_pretty(history).map_err(|error| {
        crate::store::StoreError::Write {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    })?;
    crate::store::write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(sql: &str, at: u64) -> HistoryEntry {
        HistoryEntry {
            sql: sql.to_string(),
            connection: None,
            at,
            ok: true,
        }
    }

    #[test]
    fn the_newest_statement_is_first() {
        let mut history = History::default();
        history.record(entry("SELECT 1", 1));
        history.record(entry("SELECT 2", 2));
        assert_eq!(history.entries[0].sql, "SELECT 2");
    }

    /// Re-running a statement moves it up rather than filling the list with
    /// copies of itself.
    #[test]
    fn running_the_same_statement_again_moves_it_rather_than_repeating_it() {
        let mut history = History::default();
        history.record(entry("SELECT 1", 1));
        history.record(entry("SELECT 2", 2));
        history.record(entry("SELECT 1", 3));

        assert_eq!(history.entries.len(), 2);
        assert_eq!(history.entries[0].sql, "SELECT 1");
        assert_eq!(history.entries[0].at, 3, "and carries the newer time");
    }

    /// The same statement typed with different whitespace is the same
    /// statement.
    #[test]
    fn whitespace_does_not_make_it_a_different_statement() {
        let mut history = History::default();
        history.record(entry("SELECT  1\nFROM t", 1));
        history.record(entry("SELECT 1 FROM t", 2));
        assert_eq!(history.entries.len(), 1);
    }

    #[test]
    fn blank_statements_are_not_recorded() {
        let mut history = History::default();
        history.record(entry("   \n  ", 1));
        assert!(history.entries.is_empty());
    }

    /// The cap is what stops the file growing without bound.
    #[test]
    fn the_list_is_capped() {
        let mut history = History::default();
        for n in 0..(MAX_ENTRIES + 50) {
            history.record(entry(&format!("SELECT {n}"), n as u64));
        }
        assert_eq!(history.entries.len(), MAX_ENTRIES);
        assert_eq!(
            history.entries[0].sql,
            format!("SELECT {}", MAX_ENTRIES + 49),
            "and it is the oldest that goes"
        );
    }

    /// Every term has to match, so two words find a statement that contains
    /// both however far apart they are.
    #[test]
    fn search_matches_all_terms_anywhere() {
        let mut history = History::default();
        history.record(entry("INSERT INTO users (name) VALUES ('a')", 1));
        history.record(entry("SELECT * FROM orders", 2));

        assert_eq!(history.search("insert users").len(), 1);
        assert_eq!(history.search("insert orders").len(), 0);
        assert_eq!(history.search("").len(), 2, "no query matches everything");
    }

    /// A failed statement is often the one you most want back.
    #[test]
    fn failures_are_kept() {
        let mut history = History::default();
        let mut failed = entry("SELECT nope", 1);
        failed.ok = false;
        history.record(failed);
        assert_eq!(history.entries.len(), 1);
        assert!(!history.entries[0].ok);
    }

    #[test]
    fn a_missing_or_broken_file_reads_as_empty() {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-history-missing-{}", std::process::id()));
        assert_eq!(load(&path), History::default());

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load(&path), History::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn it_round_trips_through_the_file() {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-history-{}.json", std::process::id()));

        let mut history = History::default();
        history.record(entry("SELECT 1", 7));
        save(&path, &history).expect("save");
        assert_eq!(load(&path), history);
        let _ = std::fs::remove_file(&path);
    }
}
