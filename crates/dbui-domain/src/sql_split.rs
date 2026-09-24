//! Split SQL into statements without treating `;` inside strings or comments
//! as terminators.
//!
//! Clients (TablePlus, DataGrip, …) run "the statement under the caret" or
//! "everything selected". Both need the same split: on `;` outside quotes and
//! comments. The splitter is pure so the UI and any future CLI share it.

use crate::connection::Driver;
use std::ops::Range;

/// Byte ranges of non-empty statements in `sql`, in order, read the way
/// standard SQL reads strings: a backslash is an ordinary character except in
/// a PostgreSQL `E'...'` string. [`split_statements_for`] reads them the way
/// one engine does, and is what anything about to run the statements wants.
///
/// Ranges cover the statement text itself (trimmed of surrounding whitespace)
/// and do **not** include the terminating `;`. Empty segments between
/// consecutive semicolons are dropped.
pub fn split_statements(sql: &str) -> Vec<Range<usize>> {
    split(sql, false)
}

/// [`split_statements`], as `driver` reads strings: MySQL takes a backslash
/// as an escape in every string, so `'it\'s; fine'` is one literal there and
/// two fragments anywhere else.
pub fn split_statements_for(driver: Driver, sql: &str) -> Vec<Range<usize>> {
    split(sql, backslash_escapes(driver))
}

fn backslash_escapes(driver: Driver) -> bool {
    matches!(driver, Driver::MySql)
}

/// The scan behind both. Besides quotes and comments it knows two things a
/// `;` can sit inside without ending anything:
///
/// - **Routine bodies.** `CREATE PROCEDURE`, `FUNCTION`, `TRIGGER` and `EVENT`
///   can hold a `BEGIN ... END` block of statements -- MySQL and SQLite write
///   them that way, and PostgreSQL's `BEGIN ATOMIC` does too. Inside one of
///   those, `BEGIN` and `CASE` open a block and `END` closes it, and a `;`
///   ends the statement only once every block is closed. `END IF`, `END LOOP`,
///   `END WHILE` and `END REPEAT` close blocks that never counted as opened.
/// - **`DELIMITER`.** The MySQL client's directive, which every dump with a
///   routine in it uses: a line reading `DELIMITER $$` makes `$$` the
///   terminator until the next such line. The directive is the client's, not
///   the server's, so it is left out of every range.
fn split(sql: &str, backslash: bool) -> Vec<Range<usize>> {
    let bytes = sql.as_bytes();
    let mut ranges = Vec::new();
    let mut stmt_start = 0usize;
    let mut i = 0usize;
    let mut delimiter: &[u8] = b";";
    // The statement's first few words, upper-cased, to spot a routine.
    let mut lead: Vec<String> = Vec::new();
    let mut routine = false;
    let mut depth = 0usize;
    // Nothing but whitespace since `stmt_start`.
    let mut blank = true;

    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // A `DELIMITER` directive stands where a statement would start.
        if blank {
            if let Some((next, end)) = delimiter_directive(sql, i) {
                delimiter = &bytes[next];
                i = end;
                stmt_start = end;
                continue;
            }
        }
        blank = false;

        let ends_here = if delimiter == b";" {
            bytes[i] == b';' && depth == 0
        } else {
            bytes[i..].starts_with(delimiter)
        };
        if ends_here {
            if let Some(range) = trim_range(sql, stmt_start..i) {
                ranges.push(range);
            }
            i += delimiter.len();
            stmt_start = i;
            lead.clear();
            routine = false;
            depth = 0;
            blank = true;
            continue;
        }

        match bytes[i] {
            b'\'' => {
                // `E'...'` escapes with a backslash in PostgreSQL whatever the
                // engine does with its other strings.
                let e_string = i > 0
                    && matches!(bytes[i - 1], b'e' | b'E')
                    && (i < 2 || !is_ident_cont(bytes[i - 2]));
                i = skip_quoted(bytes, i, b'\'', backslash || e_string);
            }
            quote @ (b'"' | b'`') => {
                i = skip_quoted(bytes, i, quote, backslash);
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
            b if is_ident_cont(b) && (i == 0 || !is_ident_cont(bytes[i - 1])) => {
                let word_start = i;
                // `$` continues a word, so `END$$` would swallow a `$$`
                // terminator whole without the second check.
                while i < bytes.len()
                    && is_ident_cont(bytes[i])
                    && (delimiter == b";" || !bytes[i..].starts_with(delimiter))
                {
                    i += 1;
                }
                let word = sql[word_start..i].to_ascii_uppercase();
                if lead.len() < 8 {
                    lead.push(word.clone());
                    routine = routine || opens_routine(&lead);
                }
                if routine {
                    match word.as_str() {
                        "BEGIN" | "CASE" => depth += 1,
                        "END" if depth > 0 => {
                            // `END IF` and friends close a block never counted
                            // as opened; `END CASE` closes one that was. The
                            // word after `END` is taken with it either way, or
                            // that `CASE` would read as a new one opening.
                            match next_word(sql, i) {
                                Some((closer, after))
                                    if matches!(
                                        closer.as_str(),
                                        "IF" | "LOOP" | "WHILE" | "REPEAT" | "CASE"
                                    ) =>
                                {
                                    if closer == "CASE" {
                                        depth -= 1;
                                    }
                                    i = after;
                                }
                                _ => depth -= 1,
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => i += 1,
        }
    }

    if let Some(range) = trim_range(sql, stmt_start..bytes.len()) {
        ranges.push(range);
    }

    ranges
}

/// Whether a statement's leading words make it a routine that may carry a
/// `BEGIN ... END` body: `CREATE [OR REPLACE] [DEFINER = x] [TEMP] PROCEDURE`
/// and the like.
fn opens_routine(lead: &[String]) -> bool {
    lead.first().is_some_and(|word| word == "CREATE")
        && lead[1..].iter().any(|word| {
            matches!(
                word.as_str(),
                "PROCEDURE" | "FUNCTION" | "TRIGGER" | "EVENT"
            )
        })
}

/// The word after byte `from`, upper-cased, skipping whitespace only -- and
/// the byte just past it.
fn next_word(sql: &str, from: usize) -> Option<(String, usize)> {
    let rest = sql[from..].trim_start();
    let start = sql.len() - rest.len();
    let len = rest.bytes().take_while(|byte| is_ident_cont(*byte)).count();
    (len > 0).then(|| (rest[..len].to_ascii_uppercase(), start + len))
}

/// A `DELIMITER <token>` line starting at `at`: the byte range of the new
/// terminator, and where the line ends.
fn delimiter_directive(sql: &str, at: usize) -> Option<(Range<usize>, usize)> {
    const WORD: &str = "DELIMITER";
    let head = sql.get(at..at + WORD.len())?;
    if !head.eq_ignore_ascii_case(WORD) {
        return None;
    }
    let line_end = sql[at..].find('\n').map_or(sql.len(), |offset| at + offset);
    let rest = &sql[at + WORD.len()..line_end];
    // `DELIMITER` has to be followed by space; `DELIMITERS` is a word.
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let token = rest.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return None;
    }
    let token_start = at + WORD.len() + rest.find(token)?;
    Some((token_start..token_start + token.len(), line_end))
}

/// The statement that contains `offset`, or the nearest preceding non-empty
/// statement when the caret sits in whitespace between statements / after a
/// trailing `;`.
///
/// Returns `None` when the buffer has no non-empty statement.
pub fn statement_at(sql: &str, offset: usize) -> Option<Range<usize>> {
    nearest(split_statements(sql), offset.min(sql.len()))
}

/// [`statement_at`], as `driver` reads strings. See [`split_statements_for`].
pub fn statement_at_for(driver: Driver, sql: &str, offset: usize) -> Option<Range<usize>> {
    nearest(split_statements_for(driver, sql), offset.min(sql.len()))
}

fn nearest(ranges: Vec<Range<usize>>, offset: usize) -> Option<Range<usize>> {
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

/// Past the string or quoted name opening at `start`. A doubled quote is
/// always an escaped one; a backslash escapes the next byte only when
/// `backslash` says the engine reads it that way (MySQL, or a PostgreSQL
/// `E'...'` string). Standard SQL does not: in PostgreSQL `'C:\'` is a
/// complete string, and reading the `\'` as an escape ran it on over the
/// statements that followed.
pub(crate) fn skip_quoted(bytes: &[u8], start: usize, quote: u8, backslash: bool) -> usize {
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
        if backslash && bytes[i] == b'\\' && i + 1 < bytes.len() {
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

    fn texts_for(driver: Driver, sql: &str) -> Vec<&str> {
        split_statements_for(driver, sql)
            .into_iter()
            .map(|r| &sql[r])
            .collect()
    }

    /// Standard strings take a backslash literally: `'C:\'` is a whole
    /// string in PostgreSQL and SQLite. Reading `\'` as an escape ran the
    /// string on and merged every statement after it into this one.
    #[test]
    fn a_backslash_is_literal_in_a_standard_string() {
        let sql = r"SELECT 'C:\'; SELECT 2";
        assert_eq!(texts(sql), vec![r"SELECT 'C:\'", "SELECT 2"]);
        assert_eq!(
            texts_for(Driver::Postgres, sql),
            vec![r"SELECT 'C:\'", "SELECT 2"]
        );
        assert_eq!(
            texts_for(Driver::Sqlite, sql),
            vec![r"SELECT 'C:\'", "SELECT 2"]
        );
    }

    /// ...but escapes in MySQL, and in a PostgreSQL `E'...'` string.
    #[test]
    fn a_backslash_escapes_in_mysql_and_e_strings() {
        let sql = r"SELECT 'it\'s; fine'; SELECT 2";
        assert_eq!(
            texts_for(Driver::MySql, sql),
            vec![r"SELECT 'it\'s; fine'", "SELECT 2"]
        );
        let sql = r"SELECT E'it\'s; fine'; SELECT 2";
        assert_eq!(
            texts_for(Driver::Postgres, sql),
            vec![r"SELECT E'it\'s; fine'", "SELECT 2"]
        );
        // A name that merely ends in `e` does not make an E-string.
        let sql = r"SELECT name'C:\'; SELECT 2";
        assert_eq!(texts(sql), vec![r"SELECT name'C:\'", "SELECT 2"]);
    }

    /// A routine body's statements are the routine's, not the batch's.
    #[test]
    fn a_begin_end_body_is_one_statement() {
        let proc = "CREATE PROCEDURE p()\nBEGIN\n  DECLARE n INT;\n  IF n > 0 THEN\n    SET n = 1;\n  END IF;\n  CASE n WHEN 1 THEN SELECT 1; ELSE SELECT 2; END CASE;\n  WHILE n < 3 DO SET n = n + 1; END WHILE;\nEND";
        let sql = format!("{proc};\nCALL p();");
        assert_eq!(texts_for(Driver::MySql, &sql), vec![proc, "CALL p()"]);

        let trigger = "CREATE TRIGGER t AFTER INSERT ON a BEGIN\n  UPDATE b SET n = n + 1;\n  DELETE FROM c;\nEND";
        let sql = format!("{trigger}; SELECT 1");
        assert_eq!(texts_for(Driver::Sqlite, &sql), vec![trigger, "SELECT 1"]);

        let atomic =
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; SELECT 2; END";
        let sql = format!("{atomic}; SELECT 3");
        assert_eq!(texts(&sql), vec![atomic, "SELECT 3"]);
    }

    /// Outside a routine, `BEGIN` is a transaction and `CASE` an expression:
    /// the batch splits as it always did.
    #[test]
    fn begin_outside_a_routine_is_a_statement() {
        assert_eq!(
            texts("BEGIN; UPDATE t SET a = CASE WHEN b THEN 1 END; COMMIT"),
            vec!["BEGIN", "UPDATE t SET a = CASE WHEN b THEN 1 END", "COMMIT"]
        );
        // A routine with no body is over at its `;`.
        assert_eq!(
            texts("CREATE TRIGGER t BEFORE INSERT ON a FOR EACH ROW SET NEW.x = 1; SELECT 1"),
            vec![
                "CREATE TRIGGER t BEFORE INSERT ON a FOR EACH ROW SET NEW.x = 1",
                "SELECT 1"
            ]
        );
    }

    /// The MySQL client's `DELIMITER`, as dumps write it. The directive lines
    /// are the client's and are not statements.
    #[test]
    fn delimiter_lines_change_the_terminator() {
        let sql = "DROP PROCEDURE IF EXISTS p;\nDELIMITER $$\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END$$\nDELIMITER ;\nCALL p();";
        assert_eq!(
            texts_for(Driver::MySql, sql),
            vec![
                "DROP PROCEDURE IF EXISTS p",
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "CALL p()"
            ]
        );
        // `delimiter` is case-insensitive, and a word that only starts with it
        // is not the directive.
        assert_eq!(
            texts("delimiter //\nSELECT 1; SELECT 2//\nSELECT 3//"),
            vec!["SELECT 1; SELECT 2", "SELECT 3"]
        );
        assert_eq!(
            texts("DELIMITERS; SELECT 1"),
            vec!["DELIMITERS", "SELECT 1"]
        );
    }

    /// The caret runs the whole routine from anywhere inside its body.
    #[test]
    fn statement_at_covers_a_whole_routine() {
        let sql = "SELECT 1;\nCREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET n = 1; END;\nSELECT 2";
        let caret = sql.find("UPDATE").unwrap();
        let range = statement_at_for(Driver::Sqlite, sql, caret).unwrap();
        assert_eq!(
            &sql[range],
            "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET n = 1; END"
        );
    }
}
