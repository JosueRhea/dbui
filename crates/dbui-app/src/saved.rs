//! Queries the user has named, to come back to.
//!
//! History is everything that ran, whether or not it was worth keeping;
//! this is the short list someone chose to keep, under a name of their own.
//! It lives beside the history in the config directory, in its own file:
//! losing it would be losing work, so it is never trimmed, and it is written
//! the same atomic way as everything else there.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub sql: String,
    /// Seconds since the epoch, of the last save under this name.
    pub saved_at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQueries {
    #[serde(default)]
    pub queries: Vec<SavedQuery>,
}

impl SavedQueries {
    /// Keep `sql` under `name`, most recently saved first.
    ///
    /// A name is one query: saving under a name already taken -- in any case,
    /// since `Monthly revenue` and `monthly revenue` are one name to a person
    /// -- replaces it rather than adding a twin nobody could tell apart.
    pub fn save(&mut self, name: &str, sql: &str, saved_at: u64) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.queries
            .retain(|query| !query.name.eq_ignore_ascii_case(name));
        self.queries.insert(
            0,
            SavedQuery {
                name: name.to_string(),
                sql: sql.to_string(),
                saved_at,
            },
        );
    }

    /// Forget the query saved under `name`. Whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.queries.len();
        self.queries
            .retain(|query| !query.name.eq_ignore_ascii_case(name));
        self.queries.len() != before
    }

    /// Queries whose name or SQL holds every whitespace-separated term.
    pub fn search(&self, query: &str) -> Vec<&SavedQuery> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        self.queries
            .iter()
            .filter(|saved| {
                let haystack = format!("{}\n{}", saved.name, saved.sql).to_lowercase();
                terms.iter().all(|term| haystack.contains(term))
            })
            .collect()
    }
}

pub fn saved_queries_path() -> Result<PathBuf, crate::store::StoreError> {
    Ok(crate::store::config_dir()?.join("saved_queries.json"))
}

/// Read the saved queries, or none.
///
/// A missing file is no saved queries yet. A file that is there but will not
/// parse is an error, not an empty list: unlike history, these are work
/// someone kept on purpose, and a caller that took an unreadable file for an
/// empty one would overwrite it on the next save.
pub fn load(path: &Path) -> Result<SavedQueries, crate::store::StoreError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| crate::store::StoreError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(SavedQueries::default()),
        Err(error) => Err(crate::store::StoreError::Read {
            path: path.to_path_buf(),
            message: error.to_string(),
        }),
    }
}

/// Write them out, by rename so a crash mid-write cannot leave half a file.
pub fn save(path: &Path, saved: &SavedQueries) -> Result<(), crate::store::StoreError> {
    let text =
        serde_json::to_string_pretty(saved).map_err(|error| crate::store::StoreError::Write {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    crate::store::write_atomic(path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_save_is_first() {
        let mut saved = SavedQueries::default();
        saved.save("first", "SELECT 1", 1);
        saved.save("second", "SELECT 2", 2);
        let names: Vec<&str> = saved.queries.iter().map(|q| q.name.as_str()).collect();
        assert_eq!(names, ["second", "first"]);
    }

    #[test]
    fn saving_under_a_taken_name_replaces_it_whatever_the_case() {
        let mut saved = SavedQueries::default();
        saved.save("Monthly revenue", "SELECT 1", 1);
        saved.save("monthly REVENUE", "SELECT 2", 2);
        assert_eq!(saved.queries.len(), 1);
        assert_eq!(saved.queries[0].sql, "SELECT 2");
        assert_eq!(
            saved.queries[0].name, "monthly REVENUE",
            "the newer spelling"
        );
    }

    #[test]
    fn a_blank_name_saves_nothing() {
        let mut saved = SavedQueries::default();
        saved.save("   ", "SELECT 1", 1);
        assert!(saved.queries.is_empty());
    }

    #[test]
    fn search_reads_names_and_sql() {
        let mut saved = SavedQueries::default();
        saved.save("Revenue by month", "SELECT sum(total) FROM orders", 1);
        saved.save("Active users", "SELECT * FROM users WHERE active", 2);
        let found =
            |q: &str| -> Vec<String> { saved.search(q).iter().map(|s| s.name.clone()).collect() };
        assert_eq!(found("revenue"), ["Revenue by month"]);
        assert_eq!(
            found("users active"),
            ["Active users"],
            "every term, any order"
        );
        assert_eq!(found("").len(), 2);
    }

    #[test]
    fn remove_forgets_by_name() {
        let mut saved = SavedQueries::default();
        saved.save("a", "SELECT 1", 1);
        assert!(saved.remove("A"));
        assert!(!saved.remove("a"));
        assert!(saved.queries.is_empty());
    }

    #[test]
    fn a_round_trip_through_the_file() {
        let mut path = std::env::temp_dir();
        path.push(format!("dbui-saved-{}.json", std::process::id()));
        let mut saved = SavedQueries::default();
        saved.save("a", "SELECT 1", 7);
        save(&path, &saved).expect("write");
        assert_eq!(load(&path).expect("read"), saved);
        std::fs::remove_file(&path).ok();
        assert_eq!(
            load(&path).expect("missing is empty"),
            SavedQueries::default()
        );
    }
}
