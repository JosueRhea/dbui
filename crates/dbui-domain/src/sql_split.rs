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
            // Behind the string arms on purpose: a `$$` inside `'...'` is text
            // the engine never reads as a delimiter, and treating it as one
            // swallows every statement after it.
            b'$' => {
                i = skip_dollar_quoted(bytes, i).unwrap_or(i + 1);
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

pub(crate) fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
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

/// Past the closing delimiter of the Postgres dollar-quoted string opening at
/// `start`, or `None` when `start` opens no delimiter at all.
///
/// A body is `$tag$ ... $tag$` with the tag optional, and nothing inside it is
/// special -- no escapes, no comments, no nesting. It ends at the first
/// byte-for-byte copy of its own delimiter, which is why `$TAG$ x $tag$ y $TAG$`
/// is one string: the comparison is case-sensitive, and an inner `$inner$` is
/// body text rather than a nested quote.
///
/// The `None` cases are what keeps `$1` a bind placeholder and `a$b` a single
/// identifier: a tag may not start with a digit, and a `$` that continues an
/// identifier cannot begin one. Reading either as a quote makes the splitter
/// swallow every statement that follows.
pub(crate) fn skip_dollar_quoted(bytes: &[u8], start: usize) -> Option<usize> {
    if start
        .checked_sub(1)
        .is_some_and(|b| is_ident_cont(bytes[b]))
    {
        return None;
    }

    // Walk the tag to the `$` that closes the opening delimiter.
    let mut i = start + 1;
    while i < bytes.len() && bytes[i] != b'$' {
        let first = i == start + 1;
        if !is_tag_byte(bytes[i], first) {
            return None;
        }
        i += 1;
    }
    if i >= bytes.len() {
        return None;
    }

    let delimiter = &bytes[start..=i];
    let mut j = i + 1;
    while j + delimiter.len() <= bytes.len() {
        if &bytes[j..j + delimiter.len()] == delimiter {
            return Some(j + delimiter.len());
        }
        j += 1;
    }
    // Unterminated, so the rest of the buffer is body. That is the answer
    // `skip_quoted` already gives an unterminated `'`, and the one that stops
    // the caller re-entering this `$` and scanning forever.
    Some(bytes.len())
}

/// A byte that continues an identifier. `$` is one of them in PostgreSQL, which
/// is the whole reason `a$b` is a name and not a name followed by a quote, and
/// every byte of a multi-byte character is one too.
///
/// This is also what a word boundary is the absence of, so `query::has_returning`
/// shares it rather than keeping a second copy: the two scans read the same SQL,
/// and a byte they disagree about is a byte one of them gets wrong.
pub(crate) fn is_ident_cont(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// A byte allowed in a dollar-quote tag. Digits are allowed only after the
/// first one: `$1` has to stay a placeholder.
fn is_tag_byte(byte: u8, first: bool) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80 || (!first && byte.is_ascii_digit())
}

pub(crate) fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}

pub(crate) fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
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

    /// The bug this fixes: the splitter knew `'`, `"` and `` ` `` but not
    /// Postgres dollar quoting, so a function body was cut at every `;` inside
    /// it. The first fragment then failed on an unterminated quote and the
    /// function was never created.
    #[test]
    fn ignores_semicolons_in_dollar_quoted_bodies() {
        let sql =
            "CREATE FUNCTION f() RETURNS int AS $$\nBEGIN\n  RETURN 1;\nEND;\n$$ LANGUAGE plpgsql;";
        assert_eq!(texts(sql), vec![&sql[..sql.len() - 1]]);

        assert_eq!(
            texts("SELECT $body$ a; b $body$; SELECT 2"),
            vec!["SELECT $body$ a; b $body$", "SELECT 2"]
        );
    }

    /// Dollar quoting does not nest: PostgreSQL closes on the first copy of the
    /// opening delimiter, so an inner `$inner$` is ordinary body text.
    #[test]
    fn dollar_quoting_does_not_nest() {
        assert_eq!(
            texts("SELECT $$ a; $inner$ b; c $inner$ d; $$; SELECT 2"),
            vec!["SELECT $$ a; $inner$ b; c $inner$ d; $$", "SELECT 2"]
        );
    }

    /// Tags are compared byte for byte, so `$tag$` does not close `$TAG$`.
    #[test]
    fn dollar_tags_are_case_sensitive() {
        assert_eq!(
            texts("SELECT $TAG$ a; $tag$ b; $TAG$; SELECT 2"),
            vec!["SELECT $TAG$ a; $tag$ b; $TAG$", "SELECT 2"]
        );
    }

    /// A `$` that opens nothing stays ordinary text: `$1` is a placeholder
    /// because a tag may not start with a digit, and `a$b` is one identifier
    /// because `$` continues one.
    #[test]
    fn a_lone_dollar_does_not_open_a_quote() {
        assert_eq!(
            texts("SELECT $1; SELECT $2"),
            vec!["SELECT $1", "SELECT $2"]
        );
        assert_eq!(
            texts("SELECT $1, $2 FROM t; DELETE FROM u WHERE a = $1; SELECT 3"),
            vec![
                "SELECT $1, $2 FROM t",
                "DELETE FROM u WHERE a = $1",
                "SELECT 3"
            ]
        );
        assert_eq!(
            texts("SELECT a$b FROM t; SELECT 2"),
            vec!["SELECT a$b FROM t", "SELECT 2"]
        );
        assert_eq!(
            texts("SELECT 100 $ 2; SELECT 3"),
            vec!["SELECT 100 $ 2", "SELECT 3"]
        );
        assert_eq!(
            texts("SELECT 1 $; SELECT 2"),
            vec!["SELECT 1 $", "SELECT 2"]
        );
    }

    /// A string literal still wins, so a `$$` between quotes stays text rather
    /// than opening a body that swallows the rest of the buffer.
    #[test]
    fn dollars_inside_a_string_literal_stay_text() {
        assert_eq!(
            texts("SELECT '$$;$$'; SELECT 2"),
            vec!["SELECT '$$;$$'", "SELECT 2"]
        );
        assert_eq!(
            texts("SELECT '-- $$'; SELECT 2"),
            vec!["SELECT '-- $$'", "SELECT 2"]
        );
    }

    /// An unterminated body runs to the end of the buffer, the way an
    /// unterminated `'` already does: it must not spin and must not panic.
    #[test]
    fn an_unterminated_dollar_body_runs_to_the_end() {
        assert_eq!(texts("SELECT $$ a; b"), vec!["SELECT $$ a; b"]);
        assert_eq!(texts("SELECT $tag$ a; b"), vec!["SELECT $tag$ a; b"]);
    }

    /// Byte scanning must not cut a multi-byte character in half, whether it
    /// sits in a tag, in a body or on its own at the end of an open tag.
    #[test]
    fn non_ascii_never_splits_a_character() {
        assert_eq!(
            texts("SELECT $\u{e9}$ a; b $\u{e9}$; SELECT 2"),
            vec!["SELECT $\u{e9}$ a; b $\u{e9}$", "SELECT 2"]
        );
        assert_eq!(
            texts("SELECT $$ \u{e9}; x $$"),
            vec!["SELECT $$ \u{e9}; x $$"]
        );
        assert_eq!(
            texts("SELECT '\u{e9}'; SELECT $\u{e9}"),
            vec!["SELECT '\u{e9}'", "SELECT $\u{e9}"]
        );
        assert_eq!(
            texts("SELECT \u{e9}; SELECT 2"),
            vec!["SELECT \u{e9}", "SELECT 2"]
        );
    }

    /// The caret runs the whole body, not the fragment it happens to sit in.
    #[test]
    fn statement_at_covers_a_whole_dollar_quoted_body() {
        let sql = "SELECT 1; CREATE FUNCTION f() RETURNS int AS $$ RETURN 1; $$ LANGUAGE plpgsql;";
        let range = statement_at(sql, 60).unwrap();
        assert_eq!(
            &sql[range],
            "CREATE FUNCTION f() RETURNS int AS $$ RETURN 1; $$ LANGUAGE plpgsql"
        );
    }
}
