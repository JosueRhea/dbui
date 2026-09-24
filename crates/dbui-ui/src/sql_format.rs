//! Lightweight SQL syntax highlighting for the query editor.
//!
//! Not a parser — a scanner that colours keywords, strings, comments, numbers,
//! identifiers and punctuation. Good enough to scan a query; not a substitute
//! for understanding it.

use gpui::Rgba;

use crate::theme::Theme;
use dbui_app::domain::Driver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlStyle {
    Keyword,
    String,
    Comment,
    Number,
    Identifier,
    Punct,
}

impl SqlStyle {
    pub fn color(self, theme: &Theme) -> Rgba {
        match self {
            SqlStyle::Keyword => theme.accent,
            SqlStyle::String => theme.value_text,
            SqlStyle::Comment => theme.text_muted,
            SqlStyle::Number => theme.value_number,
            SqlStyle::Identifier => theme.text,
            SqlStyle::Punct => theme.text_faint,
        }
    }
}

/// Absolute byte spans covering `text`, with strings read the way `driver`
/// reads them: a backslash escapes in MySQL and in a PostgreSQL `E'...'`
/// string, and is an ordinary character anywhere else.
pub fn highlight_spans(text: &str, driver: Driver) -> Vec<(usize, usize, SqlStyle)> {
    tokenize(text, matches!(driver, Driver::MySql))
}

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS",
    "NULL",
    "AS",
    "ON",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "ALTER",
    "DROP",
    "TABLE",
    "INDEX",
    "VIEW",
    "WITH",
    "RECURSIVE",
    "ORDER",
    "BY",
    "GROUP",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "RETURNING",
    "DISTINCT",
    "ALL",
    "UNION",
    "EXCEPT",
    "INTERSECT",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "EXISTS",
    "BETWEEN",
    "LIKE",
    "ILIKE",
    "TRUE",
    "FALSE",
    "ASC",
    "DESC",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "DEFAULT",
    "CASCADE",
    "RESTRICT",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "TRANSACTION",
    "EXPLAIN",
    "ANALYZE",
    "SHOW",
    "DESCRIBE",
    "USE",
    "GRANT",
    "REVOKE",
    "IF",
    "OVER",
    "PARTITION",
    "WINDOW",
    "LATERAL",
    "USING",
];

/// A byte that continues a word, so `name'x'` is not an `E'...'` string.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

fn is_keyword(word: &str) -> bool {
    let upper = word.to_ascii_uppercase();
    KEYWORDS.iter().any(|k| *k == upper)
}

fn tokenize(text: &str, backslash: bool) -> Vec<(usize, usize, SqlStyle)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Line comment
        if c == b'-' && bytes.get(i + 1) == Some(&b'-') {
            let start = i;
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push((start, i, SqlStyle::Comment));
            continue;
        }

        // Block comment
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let start = i;
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            if i + 1 < bytes.len() {
                i += 2;
            } else {
                i = bytes.len();
            }
            out.push((start, i, SqlStyle::Comment));
            continue;
        }

        // Strings / quoted identifiers
        if c == b'\'' || c == b'"' || c == b'`' {
            let start = i;
            let quote = c;
            let escapes = backslash
                || quote == b'\''
                    && start > 0
                    && matches!(bytes[start - 1], b'e' | b'E')
                    && (start < 2 || !is_word_byte(bytes[start - 2]));
            i += 1;
            while i < bytes.len() {
                if bytes[i] == quote {
                    if bytes.get(i + 1) == Some(&quote) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                if escapes && bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            let style = if quote == b'\'' {
                SqlStyle::String
            } else {
                SqlStyle::Identifier
            };
            out.push((start, i, style));
            continue;
        }

        // Numbers
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            out.push((start, i, SqlStyle::Number));
            continue;
        }

        // Identifiers / keywords
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'$')
            {
                i += 1;
            }
            let word = &text[start..i];
            let style = if is_keyword(word) {
                SqlStyle::Keyword
            } else {
                SqlStyle::Identifier
            };
            out.push((start, i, style));
            continue;
        }

        // Punctuation
        out.push((i, i + 1, SqlStyle::Punct));
        i += 1;
    }

    out
}

/// Keywords offered to the SQL autocomplete popup.
pub fn completion_keywords() -> &'static [&'static str] {
    KEYWORDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_keywords_and_strings() {
        let spans = highlight_spans("SELECT 'hi' FROM t", Driver::Postgres);
        assert!(spans.iter().any(|&(s, e, style)| {
            style == SqlStyle::Keyword && &"SELECT 'hi' FROM t"[s..e] == "SELECT"
        }));
        assert!(spans.iter().any(|&(s, e, style)| {
            style == SqlStyle::String && &"SELECT 'hi' FROM t"[s..e] == "'hi'"
        }));
        assert!(spans.iter().any(|&(s, e, style)| {
            style == SqlStyle::Keyword && &"SELECT 'hi' FROM t"[s..e] == "FROM"
        }));
    }

    #[test]
    fn colours_comments() {
        let spans = highlight_spans("SELECT 1 -- note\n/* block */", Driver::Postgres);
        assert!(spans
            .iter()
            .any(|&(_, _, style)| style == SqlStyle::Comment));
    }

    #[test]
    fn colours_numbers() {
        let spans = highlight_spans("SELECT 42, 3.14", Driver::Postgres);
        assert!(spans.iter().any(|&(s, e, style)| {
            style == SqlStyle::Number && &"SELECT 42, 3.14"[s..e] == "42"
        }));
    }

    /// `'C:\'` is a whole string in PostgreSQL, so what follows it is code,
    /// not more string -- but in MySQL the `\'` is an escaped quote.
    #[test]
    fn a_backslash_escapes_only_where_the_engine_says() {
        let text = r"SELECT 'C:\', 1";
        let string_end = |driver| {
            highlight_spans(text, driver)
                .into_iter()
                .find(|(_, _, style)| *style == SqlStyle::String)
                .map(|(_, end, _)| end)
                .unwrap()
        };
        assert_eq!(string_end(Driver::Postgres), 12);
        assert_eq!(string_end(Driver::Sqlite), 12);
        assert_eq!(string_end(Driver::MySql), text.len(), "runs to the end");

        let text = r"SELECT E'it\'s', 1";
        let spans = highlight_spans(text, Driver::Postgres);
        let (start, end, _) = spans
            .into_iter()
            .find(|(_, _, style)| *style == SqlStyle::String)
            .unwrap();
        assert_eq!(&text[start..end], r"'it\'s'");
    }
}
