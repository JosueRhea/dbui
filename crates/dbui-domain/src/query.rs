//! What comes back from running SQL.

use crate::value::Value;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// One column of a result set, as the grid header shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// The engine's name for the type: `int8`, `VARCHAR`, `jsonb`.
    pub type_name: String,
}

/// One row, positionally aligned with [`ResultSet::columns`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Row(pub Vec<Value>);

impl Row {
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.0.get(index)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A grid of decoded values.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ResultSet {
    pub columns: Vec<ColumnInfo>,
    pub rows: Vec<Row>,
    /// Set when the adapter stopped at [`Page::limit`] and the server had more.
    /// The status bar says so, because a silently clipped result set is how
    /// someone concludes a table has 500 rows when it has 5 million.
    pub truncated: bool,
}

impl ResultSet {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }
}

/// What a statement produced: rows, or a count of rows it changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum QueryOutcome {
    Rows(ResultSet),
    Affected(u64),
}

/// Timings, for the status bar.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QueryStats {
    pub elapsed: Duration,
}

/// A statement, what it returned, and how long it took.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryResult {
    pub statement: String,
    pub outcome: QueryOutcome,
    pub stats: QueryStats,
}

impl QueryResult {
    pub fn rows(&self) -> Option<&ResultSet> {
        match &self.outcome {
            QueryOutcome::Rows(set) => Some(set),
            QueryOutcome::Affected(_) => None,
        }
    }

    /// The one-line verdict for the status bar.
    pub fn summary(&self) -> String {
        let ms = self.stats.elapsed.as_secs_f64() * 1000.0;
        match &self.outcome {
            QueryOutcome::Rows(set) => {
                let plural = if set.rows.len() == 1 { "row" } else { "rows" };
                let truncated = if set.truncated { "+" } else { "" };
                format!("{}{} {} in {:.0} ms", set.rows.len(), truncated, plural, ms)
            }
            QueryOutcome::Affected(n) => {
                let plural = if *n == 1 { "row" } else { "rows" };
                format!("{n} {plural} affected in {ms:.0} ms")
            }
        }
    }
}

/// A window over a table's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub limit: u32,
    pub offset: u64,
}

impl Page {
    /// Enough to fill any window several times over, small enough that opening
    /// a billion-row table is still instant.
    pub const DEFAULT_LIMIT: u32 = 500;

    pub fn first() -> Self {
        Self {
            limit: Self::DEFAULT_LIMIT,
            offset: 0,
        }
    }

    pub fn next(self) -> Self {
        Self {
            limit: self.limit,
            offset: self.offset + self.limit as u64,
        }
    }

    pub fn previous(self) -> Self {
        Self {
            limit: self.limit,
            offset: self.offset.saturating_sub(self.limit as u64),
        }
    }

    /// One more than asked for.
    ///
    /// Fetching `limit + 1` and discarding the extra is how `truncated` gets
    /// answered without a second `COUNT(*)` round trip -- if the row arrives,
    /// there is more behind it.
    pub fn probe_limit(self) -> i64 {
        self.limit as i64 + 1
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::first()
    }
}

/// Does this statement return rows?
///
/// The two engines need different calls for the two cases -- one gives back
/// rows, the other a modified-row count -- and neither tells you which you have
/// until you have already chosen. So the first keyword decides, which is what
/// every SQL client ends up doing.
///
/// A misjudgement is not fatal: a mislabelled statement still executes, and
/// only its result summary is off.
pub fn returns_rows(sql: &str) -> bool {
    let head = sql
        .trim_start()
        .trim_start_matches(|c: char| c == '(' || c.is_whitespace());

    // Skip leading line comments, so a query under a `-- note` still counts.
    let head = head
        .lines()
        .find(|line| {
            let line = line.trim_start();
            !line.is_empty() && !line.starts_with("--")
        })
        .unwrap_or("");

    let keyword: String = head
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect::<String>()
        .to_ascii_uppercase();

    match keyword.as_str() {
        "SELECT" | "SHOW" | "EXPLAIN" | "DESCRIBE" | "DESC" | "VALUES" | "TABLE" | "CALL"
        | "PRAGMA" => true,
        // `WITH ... INSERT` exists, but the common case is a CTE feeding a
        // SELECT, and `RETURNING` makes writes produce rows too.
        "WITH" => true,
        _ => sql.to_ascii_uppercase().contains(" RETURNING "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_classification() {
        assert!(returns_rows("select 1"));
        assert!(returns_rows("  \n SELECT * FROM t"));
        assert!(returns_rows("(SELECT 1)"));
        assert!(returns_rows("WITH x AS (SELECT 1) SELECT * FROM x"));
        assert!(returns_rows("-- a note\nSELECT 1"));
        assert!(returns_rows("INSERT INTO t VALUES (1) RETURNING id"));

        assert!(!returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!returns_rows("UPDATE t SET a = 1"));
        assert!(!returns_rows("CREATE TABLE t (id int)"));
        assert!(!returns_rows(""));
    }

    #[test]
    fn paging_cannot_walk_backwards_past_zero() {
        let page = Page::first();
        assert_eq!(page.offset, 0);
        assert_eq!(page.previous().offset, 0);
        assert_eq!(page.next().offset, u64::from(Page::DEFAULT_LIMIT));
        assert_eq!(page.next().previous(), page);
    }

    #[test]
    fn the_probe_asks_for_one_extra_row() {
        assert_eq!(Page::first().probe_limit(), Page::DEFAULT_LIMIT as i64 + 1);
    }

    #[test]
    fn summaries_read_as_sentences() {
        let result = QueryResult {
            statement: "SELECT 1".into(),
            outcome: QueryOutcome::Affected(1),
            stats: QueryStats {
                elapsed: Duration::from_millis(12),
            },
        };
        assert_eq!(result.summary(), "1 row affected in 12 ms");
    }
}
