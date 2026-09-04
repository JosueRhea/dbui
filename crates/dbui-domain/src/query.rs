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

    /// What the statement did, with no timing attached.
    ///
    /// `CREATE TABLE` and friends get their own verdict rather than the row
    /// count the engine reports for them, which is always zero: a DDL
    /// statement that says "0 rows affected" reads as a statement that did
    /// nothing, and that is the whole of the feedback it used to give.
    pub fn verdict(&self) -> String {
        match &self.outcome {
            QueryOutcome::Rows(set) => {
                let plural = if set.rows.len() == 1 { "row" } else { "rows" };
                let truncated = if set.truncated { "+" } else { "" };
                format!("{}{truncated} {plural}", set.rows.len())
            }
            QueryOutcome::Affected(n) => {
                let info = crate::statement::describe(&self.statement);
                // A count the engine put a number on is a fact about the data.
                // Zero from a statement that never writes rows is not a small
                // number -- it is the absence of one, and reading it as
                // "0 rows affected" is what made `CREATE TABLE` look like a
                // statement that had done nothing.
                if *n > 0 || info.affects_rows || info.verb.is_empty() {
                    let plural = if *n == 1 { "row" } else { "rows" };
                    format!("{n} {plural} affected")
                } else {
                    match &info.object {
                        Some(object) => format!("{} {object} · OK", info.verb),
                        None => format!("{} · OK", info.verb),
                    }
                }
            }
        }
    }

    /// The one-line verdict for the status bar.
    pub fn summary(&self) -> String {
        let ms = self.stats.elapsed.as_secs_f64() * 1000.0;
        format!("{} in {ms:.0} ms", self.verdict())
    }
}

/// One column of an `ORDER BY`.
///
/// Paging is `LIMIT`/`OFFSET`, and neither engine defines a row order without
/// an `ORDER BY` -- so a table read without one can hand back the same row on
/// two pages and skip another entirely. Every table read therefore carries an
/// order, defaulting to the primary key, and the user's chosen sort is put in
/// front of it rather than replacing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub column: String,
    pub ascending: bool,
}

impl SortKey {
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            ascending: true,
        }
    }

    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            ascending: false,
        }
    }

    /// Ascending, descending, then unsorted -- what a third click on the same
    /// header should leave behind.
    pub fn cycled(current: Option<&SortKey>, column: &str) -> Option<SortKey> {
        match current {
            Some(key) if key.column == column && key.ascending => Some(SortKey::desc(column)),
            Some(key) if key.column == column => None,
            _ => Some(SortKey::asc(column)),
        }
    }
}

/// The order a table page is read in: the user's sort, then the key.
///
/// The key columns stay on the end even when the user picked a sort, because a
/// sort on a column full of duplicates is not by itself a total order -- and a
/// page boundary landing inside a run of equal values is exactly where rows go
/// missing.
pub fn order_for(sort: Option<&SortKey>, key_columns: &[String]) -> Vec<SortKey> {
    let mut order = Vec::new();
    if let Some(sort) = sort {
        order.push(sort.clone());
    }
    for column in key_columns {
        if order.iter().any(|key| &key.column == column) {
            continue;
        }
        order.push(SortKey::asc(column));
    }
    order
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

    fn affected(statement: &str, rows: u64) -> String {
        QueryResult {
            statement: statement.into(),
            outcome: QueryOutcome::Affected(rows),
            stats: QueryStats {
                elapsed: Duration::from_millis(3),
            },
        }
        .verdict()
    }

    /// The bug this fixes: every engine reports `CREATE TABLE` as zero rows
    /// affected, and "0 rows affected" is what a statement that did nothing
    /// says.
    #[test]
    fn ddl_gets_a_verdict_rather_than_a_row_count() {
        assert_eq!(
            affected("CREATE TABLE notes (id int)", 0),
            "CREATE TABLE notes · OK"
        );
        assert_eq!(affected("DROP VIEW IF EXISTS v", 0), "DROP VIEW v · OK");
        assert_eq!(
            affected("ALTER TABLE public.orders ADD COLUMN paid bool", 0),
            "ALTER TABLE public.orders · OK"
        );
        assert_eq!(affected("TRUNCATE TABLE t", 0), "TRUNCATE TABLE t · OK");
    }

    /// A write's row count still is the verdict, zero included: a `DELETE`
    /// that matched nothing is exactly what the user needs to be told.
    #[test]
    fn a_write_still_reports_its_row_count() {
        assert_eq!(affected("DELETE FROM t WHERE a = 1", 0), "0 rows affected");
        assert_eq!(affected("UPDATE t SET a = 1", 1), "1 row affected");
        assert_eq!(
            affected("INSERT INTO t VALUES (1), (2)", 2),
            "2 rows affected"
        );
    }

    /// And a number the engine went to the trouble of reporting is kept,
    /// whatever this crate thinks the statement was.
    #[test]
    fn a_reported_count_is_never_thrown_away() {
        assert_eq!(affected("CALL rebuild()", 7), "7 rows affected");
        assert_eq!(affected("", 0), "0 rows affected");
    }
}

#[cfg(test)]
mod sort_tests {
    use super::*;

    /// Clicking the same header walks asc -> desc -> unsorted.
    #[test]
    fn a_header_cycles_through_three_states() {
        let first = SortKey::cycled(None, "name").expect("first click sorts");
        assert!(first.ascending);

        let second = SortKey::cycled(Some(&first), "name").expect("second reverses");
        assert!(!second.ascending);

        assert!(
            SortKey::cycled(Some(&second), "name").is_none(),
            "a third click clears it"
        );
    }

    /// A different header starts its own cycle rather than inheriting the
    /// direction of the one before it.
    #[test]
    fn a_different_header_starts_ascending() {
        let sorted = SortKey::desc("name");
        let next = SortKey::cycled(Some(&sorted), "score").expect("sorts the new column");
        assert_eq!(next.column, "score");
        assert!(next.ascending);
    }

    /// The bug this exists to prevent: `LIMIT`/`OFFSET` with no total order
    /// can repeat a row on one page and drop another. The key always trails
    /// the sort, so equal values still have a defined order between them.
    #[test]
    fn the_key_always_breaks_ties_behind_the_sort() {
        let key = vec!["id".to_string()];

        let unsorted = order_for(None, &key);
        assert_eq!(unsorted, vec![SortKey::asc("id")]);

        let sorted = order_for(Some(&SortKey::desc("name")), &key);
        assert_eq!(sorted, vec![SortKey::desc("name"), SortKey::asc("id")]);
    }

    /// Sorting *by* the key must not name it twice.
    #[test]
    fn sorting_by_the_key_does_not_repeat_it() {
        let key = vec!["id".to_string()];
        assert_eq!(
            order_for(Some(&SortKey::desc("id")), &key),
            vec![SortKey::desc("id")]
        );
    }

    /// A composite key contributes every column, in declaration order.
    #[test]
    fn a_composite_key_contributes_all_of_itself() {
        let key = vec!["tenant".to_string(), "id".to_string()];
        assert_eq!(
            order_for(None, &key),
            vec![SortKey::asc("tenant"), SortKey::asc("id")]
        );
    }

    /// Nothing to order by is left unordered rather than guessed at: ordering
    /// a keyless view by an arbitrary column can be a sort of the whole table.
    #[test]
    fn no_key_and_no_sort_is_no_order_at_all() {
        assert!(order_for(None, &[]).is_empty());
    }
}
