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
