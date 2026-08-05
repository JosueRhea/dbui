//! Split SQL into statements without treating `;` inside strings or comments
//! as terminators.
//!
//! Clients (TablePlus, DataGrip, …) run "the statement under the caret" or
//! "everything selected". Both need the same split: on `;` outside quotes and
//! comments. The splitter is pure so the UI and any future CLI share it.

use std::ops::Range;

/// Byte ranges of non-empty statements in `sql`, in order.
///
/// Ranges cover the statement text itself (trimmed of surrounding whitespace)
/// and do **not** include the terminating `;`. Empty segments between
/// consecutive semicolons are dropped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut ranges = Vec::new();
    let mut stmt_start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i = skip_quoted(bytes, i, b'\'');
            }
            b'"' => {
                i = skip_quoted(bytes, i, b'"');
            }
            b'`' => {
                i = skip_quoted(bytes, i, b'`');
            }
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = skip_line_comment(bytes, i);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = skip_block_comment(bytes, i);
            }
            b';' => {
                if let Some(range) = trim_range(sql, stmt_start..i) {
                    ranges.push(range);
                }
                i += 1;
                stmt_start = i;
            }
            _ => i += 1,
        }
    }

    if let Some(range) = trim_range(sql, stmt_start..bytes.len()) {
        ranges.push(range);
    }

    ranges
}

/// The statement that contains `offset`, or the nearest preceding non-empty
/// statement when the caret sits in whitespace between statements / after a
/// trailing `;`.
///
/// Returns `None` when the buffer has no non-empty statement.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    let offset = offset.min(sql.len());
    let ranges = split_statements(sql);
    if ranges.is_empty() {
        return None;
    }

    for range in &ranges {
        if offset <= range.end {
            // Prefer the statement that still contains the caret, including
            // sitting right after its last character (before the `;`).
            if offset >= range.start {
                return Some(range.clone());
            }
            // Caret is in leading whitespace before this statement — take the
            // previous one if any, otherwise this one.
            break;
        }
    }

    // After the last statement (trailing `;` / whitespace), or in the gap
    // before a later statement: nearest preceding.
    for range in ranges.iter().rev() {
        if range.end <= offset || range.start <= offset {
            return Some(range.clone());
        }
    }

    ranges.into_iter().next()
}

/// Slice `sql[range]` with leading/trailing ASCII whitespace removed from the
/// range bounds. `None` when nothing remains.
fn trim_range(sql: &str, range: Range<usize>) -> Option<Range<usize>> {
    let slice = sql.get(range.clone())?;
    let trimmed = slice.trim();
    if trimmed.is_empty() {
        return None;
    }
    let lead = slice.len() - slice.trim_start().len();
    let start = range.start + lead;
    let end = start + trimmed.len();
    Some(start..end)
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            // SQL escaped quote: '' or "" or ``
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        // Backslash escape (MySQL / Postgres standard_conforming_strings off).
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(sql: &str) -> Vec<&str> {
        split_statements(sql).into_iter().map(|r| &sql[r]).collect()
    }

    #[test]
    fn splits_on_semicolons() {
        assert_eq!(texts("SELECT 1; SELECT 2"), vec!["SELECT 1", "SELECT 2"]);
        assert_eq!(texts("SELECT 1;"), vec!["SELECT 1"]);
        assert_eq!(texts("SELECT 1"), vec!["SELECT 1"]);
    }

    #[test]
    fn ignores_semicolons_in_strings() {
        assert_eq!(
            texts("SELECT ';'; SELECT 2"),
            vec!["SELECT ';'", "SELECT 2"]
        );
        assert_eq!(
            texts(r#"SELECT ";"; SELECT 2"#),
            vec![r#"SELECT ";""#, "SELECT 2"]
        );
        assert_eq!(
            texts("SELECT `a;b`; SELECT 2"),
            vec!["SELECT `a;b`", "SELECT 2"]
        );
        assert_eq!(
            texts("SELECT 'it''s; fine'; SELECT 2"),
            vec!["SELECT 'it''s; fine'", "SELECT 2"]
        );
    }

    #[test]
    fn ignores_semicolons_in_comments() {
        assert_eq!(
            texts("SELECT 1; -- note; still\nSELECT 2"),
            vec!["SELECT 1", "-- note; still\nSELECT 2"]
        );
        assert_eq!(
            texts("SELECT 1; /* ; */ SELECT 2"),
            vec!["SELECT 1", "/* ; */ SELECT 2"]
        );
    }

    #[test]
    fn drops_empty_segments() {
        assert_eq!(texts(";;SELECT 1;;;"), vec!["SELECT 1"]);
        assert!(texts("   ;  ; ").is_empty());
        assert!(texts("").is_empty());
    }

    #[test]
    fn statement_at_finds_the_caret() {
        let sql = "SELECT 1; SELECT 2; SELECT 3";
        let first = statement_at(sql, 0).unwrap();
        assert_eq!(&sql[first], "SELECT 1");

        let second = statement_at(sql, 12).unwrap();
        assert_eq!(&sql[second], "SELECT 2");

        let third = statement_at(sql, sql.len()).unwrap();
        assert_eq!(&sql[third], "SELECT 3");
    }

    #[test]
    fn statement_at_after_trailing_semicolon_picks_previous() {
        let sql = "SELECT 1;";
        let range = statement_at(sql, sql.len()).unwrap();
        assert_eq!(&sql[range], "SELECT 1");
    }

    #[test]
    fn statement_at_empty_buffer() {
        assert!(statement_at("", 0).is_none());
        assert!(statement_at("   ;  ", 2).is_none());
    }
}
