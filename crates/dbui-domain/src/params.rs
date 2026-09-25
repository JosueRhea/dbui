//! `:name` parameters in the SQL editor.
//!
//! A statement written with `WHERE id = :customer` can be run again and
//! again with a different customer, asked for each time. The engines each
//! spell real parameters differently (`$1`, `?`) and none of them takes a
//! name, so the editor finds the names itself and writes each value into the
//! statement as a literal the engine reads back exactly -- the same literals
//! a dump writes, so quoting is never the user's problem.
//!
//! A colon is only a parameter where the engine would not read it as
//! something else: not inside a string, a quoted identifier, a comment or a
//! Postgres dollar-quoted body; not in `::type` casts, MySQL's `:=`, or an
//! array slice like `a[1:2]`.

use crate::{Driver, Value};
use std::ops::Range;

/// One `:name` in a statement: the name, and the bytes it covers, colon
/// included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub range: Range<usize>,
}

/// Every parameter in `sql`, in order. A name used twice appears twice.
pub fn find(driver: Driver, sql: &str) -> Vec<Param> {
    let bytes = sql.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => i = skip_quoted(bytes, i, b'\'', driver == Driver::MySql),
            b'"' => i = skip_quoted(bytes, i, b'"', driver == Driver::MySql),
            b'`' => i = skip_quoted(bytes, i, b'`', false),
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i = memchr(b'\n', bytes, i).unwrap_or(bytes.len());
            }
            b'#' if driver == Driver::MySql => {
                i = memchr(b'\n', bytes, i).unwrap_or(bytes.len());
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = sql[i + 2..]
                    .find("*/")
                    .map(|end| i + 2 + end + 2)
                    .unwrap_or(bytes.len());
            }
            b'$' if driver == Driver::Postgres => {
                i = crate::sql_split::skip_dollar_quoted(bytes, i).unwrap_or(i + 1);
            }
            b':' => {
                let doubled = bytes.get(i + 1) == Some(&b':') || (i > 0 && bytes[i - 1] == b':');
                let assignment = bytes.get(i + 1) == Some(&b'=');
                // `a[1:n]`, `x:y`: a colon straight after a word or a digit
                // belongs to what is before it.
                let glued = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                let starts_name = bytes
                    .get(i + 1)
                    .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_');
                if !doubled && !assignment && !glued && starts_name {
                    let end = bytes[i + 1..]
                        .iter()
                        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
                        .map(|n| i + 1 + n)
                        .unwrap_or(bytes.len());
                    found.push(Param {
                        name: sql[i + 1..end].to_string(),
                        range: i..end,
                    });
                    i = end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    found
}

/// Each distinct name, in the order first used.
pub fn names(driver: Driver, sql: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for param in find(driver, sql) {
        if !names.contains(&param.name) {
            names.push(param.name);
        }
    }
    names
}

/// `sql` with each parameter replaced by the literal for its value, as typed:
/// a number, `NULL`, `true` or `false` goes in as itself; anything else as a
/// quoted string. A name with no value is left as it was.
pub fn substitute(driver: Driver, sql: &str, value_of: impl Fn(&str) -> Option<String>) -> String {
    let mut out = sql.to_string();
    for param in find(driver, sql).into_iter().rev() {
        if let Some(typed) = value_of(&param.name) {
            out.replace_range(param.range, &literal(driver, &typed));
        }
    }
    out
}

/// A typed value as the literal it means.
pub fn literal(driver: Driver, typed: &str) -> String {
    let trimmed = typed.trim();
    if trimmed.eq_ignore_ascii_case("null") {
        return "NULL".into();
    }
    if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("false") {
        return trimmed.to_ascii_uppercase();
    }
    let numeric = !trimmed.is_empty()
        && trimmed.parse::<f64>().is_ok_and(f64::is_finite)
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | '+' | '.' | 'e' | 'E'));
    if numeric {
        return trimmed.to_string();
    }
    Value::Text(typed.to_string()).sql_literal(driver)
}

fn skip_quoted(bytes: &[u8], start: usize, quote: u8, backslash_escapes: bool) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if backslash_escapes && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            // A doubled quote is an escaped one, not the end.
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
                continue;
            }
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn memchr(needle: u8, bytes: &[u8], from: usize) -> Option<usize> {
    bytes[from..]
        .iter()
        .position(|b| *b == needle)
        .map(|n| from + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_found_in_order_and_once() {
        let sql = "SELECT * FROM orders WHERE customer_id = :customer AND total > :min OR note = :customer";
        assert_eq!(names(Driver::Postgres, sql), vec!["customer", "min"]);
        assert_eq!(find(Driver::Postgres, sql).len(), 3);
    }

    #[test]
    fn colons_that_mean_something_else_are_left_alone() {
        let sql = "SELECT a::int, b[1:2], x := 1, ':inside', \"col:x\", `y:z`, $$ :body $$ \
                   -- :comment\n /* :block */ FROM t WHERE d = :real";
        assert_eq!(names(Driver::Postgres, sql), vec!["real"]);
        assert_eq!(
            names(
                Driver::MySql,
                "SELECT @v := 1, 'it\\'s :not' # :hash\n, :yes"
            ),
            vec!["yes"]
        );
    }

    #[test]
    fn values_become_literals_the_engine_reads_back() {
        let sql = "SELECT * FROM t WHERE a = :a AND b = :b AND c = :c AND d = :d AND e = :e";
        let values = |name: &str| {
            Some(
                match name {
                    "a" => "42",
                    "b" => "O'Brien",
                    "c" => "null",
                    "d" => "-1.5e3",
                    _ => "12abc",
                }
                .to_string(),
            )
        };
        assert_eq!(
            substitute(Driver::Postgres, sql, values),
            "SELECT * FROM t WHERE a = 42 AND b = 'O''Brien' AND c = NULL AND d = -1.5e3 AND e = '12abc'"
        );
        assert_eq!(literal(Driver::MySql, "C:\\temp"), "'C:\\\\temp'");
        assert_eq!(literal(Driver::Sqlite, "TRUE"), "TRUE");
        assert_eq!(literal(Driver::Sqlite, ""), "''");
    }

    #[test]
    fn a_name_with_no_value_stays() {
        assert_eq!(
            substitute(Driver::Sqlite, "SELECT :a, :b", |name| (name == "a")
                .then(|| "1".into())),
            "SELECT 1, :b"
        );
    }
}
