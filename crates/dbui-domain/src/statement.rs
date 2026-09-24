//! What a statement is, as far as the layers above need to care.
//!
//! Two questions get asked of a statement once it has run, and the driver's
//! reply answers neither. What to call it: `CREATE TABLE` comes back as
//! `Affected(0)`, which renders as "0 rows affected" -- true, and a verdict on
//! nothing. And whether the schema tree on screen is still the schema: a
//! `DROP TABLE` that leaves the table in the sidebar looks exactly like a
//! `DROP TABLE` that never ran.
//!
//! Both are answered from the leading keywords, which is the part of the
//! grammar the engines agree on. Nothing here parses SQL. A statement this
//! does not recognise is reported as its first word and assumed to leave the
//! catalog alone, so an unknown verb costs a missing refresh rather than a
//! wrong label.

/// The leading keywords of a statement, and what they imply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementInfo {
    /// `CREATE TABLE`, `DROP VIEW`, `SELECT`, … upper-cased. Empty when the
    /// statement is nothing but whitespace and comments.
    pub verb: String,
    /// What the statement named, when the verb is one that names a thing.
    /// Quoting is stripped: `CREATE TABLE "user list"` names `user list`.
    pub object: Option<String>,
    /// Whether "N rows affected" says anything. Only the statements that
    /// write rows report a count that means something; everything else
    /// reports zero.
    pub affects_rows: bool,
    /// Whether running it can change what the catalog reports, and so whether
    /// the schema tree needs re-reading.
    pub changes_catalog: bool,
}

/// Verbs whose row count is a fact about the data.
const ROW_VERBS: [&str; 5] = ["INSERT", "UPDATE", "DELETE", "REPLACE", "MERGE"];

/// Verbs that change the shape of the database.
const SCHEMA_VERBS: [&str; 4] = ["CREATE", "ALTER", "DROP", "RENAME"];

/// The words that name a kind of thing, so `CREATE OR REPLACE VIEW v` is a
/// `CREATE VIEW` and not a `CREATE OR`.
const OBJECT_WORDS: [&str; 13] = [
    "TABLE",
    "VIEW",
    "INDEX",
    "SCHEMA",
    "DATABASE",
    "SEQUENCE",
    "TRIGGER",
    "FUNCTION",
    "PROCEDURE",
    "TYPE",
    "EXTENSION",
    "ROLE",
    "USER",
];

/// Words allowed to stand between the kind and the name.
const BEFORE_NAME: [&str; 4] = ["IF", "NOT", "EXISTS", "CONCURRENTLY"];

/// How many leading words are worth reading. The longest prefix that matters
/// is `CREATE OR REPLACE MATERIALIZED VIEW IF NOT EXISTS schema.name`.
const LOOKAHEAD: usize = 12;

/// Read the leading keywords of one statement.
pub fn describe(sql: &str) -> StatementInfo {
    let words = lead_words(sql, LOOKAHEAD);
    let Some(first) = words.first() else {
        return StatementInfo {
            verb: String::new(),
            object: None,
            affects_rows: false,
            changes_catalog: false,
        };
    };

    let verb = first.to_uppercase();
    let changes_catalog = SCHEMA_VERBS.contains(&verb.as_str());
    // `TRUNCATE` names its table the same way and reports zero rows, but it
    // leaves the shape of the database alone.
    let names_object = changes_catalog || verb == "TRUNCATE";

    if !names_object {
        return StatementInfo {
            affects_rows: ROW_VERBS.contains(&verb.as_str()),
            verb,
            object: None,
            changes_catalog: false,
        };
    }

    // The kind is the first recognised word after the verb; whatever sits
    // between them -- `OR REPLACE`, `UNIQUE`, `TEMPORARY`, `MATERIALIZED` --
    // is a modifier, not the thing being made.
    let Some(at) = words
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, word)| OBJECT_WORDS.contains(&word.to_uppercase().as_str()))
        .map(|(index, _)| index)
    else {
        return StatementInfo {
            verb,
            object: None,
            affects_rows: false,
            changes_catalog,
        };
    };

    let mut kind = words[at].to_uppercase();
    if words[at - 1].eq_ignore_ascii_case("MATERIALIZED") {
        kind = format!("MATERIALIZED {kind}");
    }

    let object = words[at + 1..]
        .iter()
        .find(|word| !BEFORE_NAME.contains(&word.to_uppercase().as_str()))
        .cloned();

    StatementInfo {
        verb: format!("{verb} {kind}"),
        object,
        affects_rows: false,
        changes_catalog,
    }
}

/// Verbs that only read, or only steer the session in ways a read-only
/// connection keeps safe. Anything else is taken to write.
const READ_VERBS: [&str; 19] = [
    "SELECT",
    "SHOW",
    "EXPLAIN",
    "DESCRIBE",
    "DESC",
    "VALUES",
    "TABLE",
    "WITH",
    "PRAGMA",
    "BEGIN",
    "START",
    "COMMIT",
    "ROLLBACK",
    "END",
    "ABORT",
    "SAVEPOINT",
    "RELEASE",
    "SET",
    "USE",
];

/// Words that write wherever they stand: a CTE feeding a `DELETE`, an
/// `EXPLAIN ANALYZE` that runs the `UPDATE` it explains.
const WRITE_WORDS: [&str; 4] = ["INSERT", "UPDATE", "DELETE", "MERGE"];

/// Whether `sql` may write, or may lift the read-only setting that stops it
/// writing -- the question a connection marked read only asks before it
/// sends anything.
///
/// The server enforces read only too, but only through a session setting,
/// and a session setting is one statement away from being switched off:
/// `SET default_transaction_read_only = off`, `BEGIN READ WRITE`,
/// `SELECT set_config(...)`, `RESET ALL`. So this errs towards "writes": a
/// statement it cannot place is refused, and so is anything that names the
/// setting or asks for a writable transaction. `SELECT ... FOR UPDATE` is
/// refused as well, which the server would do anyway.
pub fn writes(sql: &str) -> bool {
    // Read both ways a backslash can be taken, so a string this misreads
    // cannot hide a word from the check: `'C:\' , ...` means different
    // things to MySQL and to PostgreSQL.
    writes_in(&bare_words(sql, false)) || writes_in(&bare_words(sql, true))
}

fn writes_in(words: &[String]) -> bool {
    let Some(first) = words.first() else {
        return false;
    };
    if !READ_VERBS.contains(&first.as_str()) {
        return true;
    }
    let lifts_read_only = words
        .windows(2)
        .any(|pair| pair[0] == "READ" && pair[1] == "WRITE")
        || words
            .iter()
            .any(|word| word.contains("READ_ONLY") || word == "SET_CONFIG");
    lifts_read_only
        || words
            .iter()
            .any(|word| WRITE_WORDS.contains(&word.as_str()))
}

/// Verbs after which the session may have opened or closed a transaction.
/// `SET` for `autocommit`; `CALL` and `DO` because a procedure may commit.
const TRANSACTION_VERBS: [&str; 13] = [
    "BEGIN",
    "START",
    "COMMIT",
    "ROLLBACK",
    "END",
    "ABORT",
    "SAVEPOINT",
    "RELEASE",
    "SET",
    "XA",
    "LOCK",
    "CALL",
    "DO",
];

/// Whether running `sql` may have changed whether a transaction is open, so
/// the session is worth asking afterwards. Anything else leaves an
/// autocommit session in autocommit, and asking after every `SELECT` would
/// be a second round trip for each one.
pub fn may_change_transaction(sql: &str) -> bool {
    bare_words(sql, false)
        .first()
        .is_some_and(|verb| TRANSACTION_VERBS.contains(&verb.as_str()))
}

/// Every unquoted word of `sql`, upper-cased, in order. Strings, quoted
/// names, dollar-quoted bodies and comments are skipped whole, so a column
/// called `"update"` or a string reading `'delete me'` is not a verb.
fn bare_words(sql: &str, backslash: bool) -> Vec<String> {
    use crate::sql_split::{
        is_ident_cont, skip_block_comment, skip_dollar_quoted, skip_line_comment, skip_quoted,
    };

    let bytes = sql.as_bytes();
    let mut words = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            quote @ (b'\'' | b'"' | b'`') => i = skip_quoted(bytes, i, quote, backslash),
            b'$' => i = skip_dollar_quoted(bytes, i).unwrap_or(i + 1),
            b'-' if bytes.get(i + 1) == Some(&b'-') => i = skip_line_comment(bytes, i),
            b'/' if bytes.get(i + 1) == Some(&b'*') => i = skip_block_comment(bytes, i),
            b if is_ident_cont(b) => {
                let start = i;
                while i < bytes.len() && is_ident_cont(bytes[i]) {
                    i += 1;
                }
                words.push(sql[start..i].to_ascii_uppercase());
            }
            _ => i += 1,
        }
    }
    words
}

/// The first `limit` words of `sql`, with comments skipped and quoting
/// stripped. A dotted name comes back whole: `public.orders`.
fn lead_words(sql: &str, limit: usize) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    let mut rest = sql;

    while !rest.is_empty() && words.len() < limit {
        let trimmed = rest.trim_start();
        if trimmed.len() != rest.len() {
            rest = trimmed;
            continue;
        }
        if let Some(tail) = rest.strip_prefix("--").or_else(|| rest.strip_prefix('#')) {
            rest = tail.split_once('\n').map_or("", |(_, after)| after);
            continue;
        }
        if let Some(tail) = rest.strip_prefix("/*") {
            rest = tail.split_once("*/").map_or("", |(_, after)| after);
            continue;
        }
        // A string literal is a value, never a name worth reading.
        if rest.starts_with('\'') {
            rest = read_quoted(rest, '\'').1;
            continue;
        }

        let mut word = String::new();
        while let Some(next) = rest.chars().next() {
            match next {
                '"' | '`' => {
                    let (inner, tail) = read_quoted(rest, next);
                    word.push_str(inner);
                    rest = tail;
                }
                '[' => {
                    let (inner, tail) = read_quoted(rest, ']');
                    word.push_str(inner);
                    rest = tail;
                }
                c if c.is_alphanumeric() || c == '_' || c == '$' || c == '.' => {
                    word.push(c);
                    rest = &rest[c.len_utf8()..];
                }
                _ => break,
            }
        }

        if word.is_empty() {
            // Punctuation that is not part of a name: `(`, `,`, `*`, `;`.
            // An empty pair of quotes leaves nothing behind it either, and
            // stepping over a character that is not there is how this would
            // panic on `CREATE TABLE ""`.
            let Some(skipped) = rest.chars().next() else {
                break;
            };
            rest = &rest[skipped.len_utf8()..];
            continue;
        }
        words.push(word);
    }

    words
}

/// Split `rest` -- which opens with one delimiter character -- into the text
/// up to `close` and whatever follows it.
fn read_quoted(rest: &str, close: char) -> (&str, &str) {
    let open = rest.chars().next().expect("called on a delimiter");
    let body = &rest[open.len_utf8()..];
    match body.find(close) {
        Some(end) => (&body[..end], &body[end + close.len_utf8()..]),
        None => (body, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(sql: &str) -> StatementInfo {
        describe(sql)
    }

    #[test]
    fn reads_are_not_writes() {
        for sql in [
            "select * from users",
            "  -- a note\nSELECT 1",
            "(select 1) union (select 2)",
            "with t as (select 1) select * from t",
            "explain select * from users",
            "show tables",
            "values (1), (2)",
            "select * from users where name = 'delete me'",
            "select \"update\" from t",
            "select replace(name, 'a', 'b') from users",
            "begin",
            "start transaction read only",
            "commit",
            "rollback",
            "set search_path = app",
            "pragma table_info(users)",
            "",
            "  -- only a comment",
        ] {
            assert!(!writes(sql), "{sql:?} reads");
        }
    }

    #[test]
    fn writes_and_anything_unknown_are_writes() {
        for sql in [
            "insert into t values (1)",
            "UPDATE t SET a = 1",
            "delete from t",
            "truncate t",
            "create table t (id int)",
            "drop table t",
            "call do_things()",
            "do $$ begin perform 1; end $$",
            "with gone as (delete from t returning *) select * from gone",
            "explain analyze update t set a = 1",
            "select * from t for update",
            "vacuum",
            "grant select on t to bob",
        ] {
            assert!(writes(sql), "{sql:?} writes");
        }
    }

    #[test]
    fn transaction_verbs_are_worth_asking_about() {
        for sql in [
            "BEGIN",
            "start transaction",
            "commit",
            "ROLLBACK",
            "set autocommit = 0",
            "call p()",
        ] {
            assert!(may_change_transaction(sql), "{sql:?}");
        }
        for sql in ["select 1", "update t set a = 1", "", "-- begin"] {
            assert!(!may_change_transaction(sql), "{sql:?}");
        }
    }

    /// The server's read-only mode is a session setting; none of these may
    /// reach it from a connection marked read only.
    #[test]
    fn lifting_read_only_counts_as_writing() {
        for sql in [
            "SET default_transaction_read_only = off",
            "set session characteristics as transaction read write",
            "begin read write",
            "START TRANSACTION READ WRITE",
            "SET SESSION transaction_read_only = 0",
            "set global read_only = 0",
            "select set_config('default_transaction_read_only', 'off', false)",
            "select pg_catalog.set_config('x', 'y', false)",
            "reset all",
            "reset default_transaction_read_only",
            "discard all",
        ] {
            assert!(writes(sql), "{sql:?} lifts read only");
        }
    }

    #[test]
    fn a_select_is_just_its_verb() {
        let it = info("select * from users");
        assert_eq!(it.verb, "SELECT");
        assert_eq!(it.object, None);
        assert!(!it.affects_rows);
        assert!(!it.changes_catalog);
    }

    #[test]
    fn writes_report_a_row_count_that_means_something() {
        for sql in [
            "INSERT INTO t VALUES (1)",
            "update t set a=1",
            "DELETE FROM t",
        ] {
            assert!(info(sql).affects_rows, "{sql}");
            assert!(!info(sql).changes_catalog, "{sql}");
        }
    }

    #[test]
    fn create_table_names_its_table() {
        let it = info("CREATE TABLE users (id int)");
        assert_eq!(it.verb, "CREATE TABLE");
        assert_eq!(it.object.as_deref(), Some("users"));
        assert!(it.changes_catalog);
        assert!(
            !it.affects_rows,
            "zero rows affected is not the verdict here"
        );
    }

    #[test]
    fn modifiers_between_the_verb_and_the_kind_are_not_the_kind() {
        assert_eq!(
            info("CREATE OR REPLACE VIEW v AS SELECT 1").verb,
            "CREATE VIEW"
        );
        assert_eq!(
            info("create unique index idx on t (a)").verb,
            "CREATE INDEX"
        );
        assert_eq!(
            info("CREATE TEMPORARY TABLE scratch (a int)")
                .object
                .as_deref(),
            Some("scratch")
        );
        assert_eq!(
            info("CREATE MATERIALIZED VIEW mv AS SELECT 1").verb,
            "CREATE MATERIALIZED VIEW"
        );
    }

    #[test]
    fn if_not_exists_is_not_the_name() {
        assert_eq!(
            info("CREATE TABLE IF NOT EXISTS public.orders (id int)")
                .object
                .as_deref(),
            Some("public.orders")
        );
        assert_eq!(info("DROP TABLE IF EXISTS t").object.as_deref(), Some("t"));
        assert_eq!(
            info("DROP INDEX CONCURRENTLY idx").object.as_deref(),
            Some("idx")
        );
    }

    #[test]
    fn quoting_comes_off_the_name() {
        assert_eq!(
            info(r#"CREATE TABLE "user list" (a int)"#)
                .object
                .as_deref(),
            Some("user list")
        );
        assert_eq!(
            info("CREATE TABLE `mysql_table` (a int)").object.as_deref(),
            Some("mysql_table")
        );
    }

    #[test]
    fn leading_comments_are_not_the_statement() {
        let it = info("-- make it\n/* really */ CREATE TABLE t (a int)");
        assert_eq!(it.verb, "CREATE TABLE");
        assert_eq!(it.object.as_deref(), Some("t"));
    }

    #[test]
    fn alter_and_rename_change_the_catalog() {
        assert_eq!(info("ALTER TABLE t ADD COLUMN b int").verb, "ALTER TABLE");
        assert!(info("ALTER TABLE t ADD COLUMN b int").changes_catalog);
        assert!(info("RENAME TABLE a TO b").changes_catalog);
    }

    #[test]
    fn truncate_names_its_table_without_changing_the_shape() {
        let with_keyword = info("TRUNCATE TABLE t");
        assert_eq!(with_keyword.verb, "TRUNCATE TABLE");
        assert_eq!(with_keyword.object.as_deref(), Some("t"));
        assert!(!with_keyword.changes_catalog);
        assert!(!with_keyword.affects_rows);

        // Postgres and MySQL both allow the keyword to be left out.
        assert_eq!(info("TRUNCATE t").verb, "TRUNCATE");
    }

    #[test]
    fn nothing_but_whitespace_and_comments_is_no_statement() {
        let it = info("  -- nothing here\n");
        assert!(it.verb.is_empty());
        assert!(!it.changes_catalog);
    }

    #[test]
    fn an_empty_name_is_no_name_and_no_panic() {
        // `read_quoted` consumes the whole of `""` and leaves nothing, which
        // is the one way the word loop can finish with both hands empty.
        let it = info(r#"CREATE TABLE "" (a int)"#);
        assert_eq!(it.verb, "CREATE TABLE");
        assert_eq!(it.object.as_deref(), Some("a"));

        assert!(info(r#"""#).verb.is_empty());
        assert!(info("''").verb.is_empty());
    }

    #[test]
    fn an_unclosed_quote_does_not_hang_or_panic() {
        let it = info(r#"CREATE TABLE "unterminated"#);
        assert_eq!(it.verb, "CREATE TABLE");
        assert_eq!(it.object.as_deref(), Some("unterminated"));
    }
}
