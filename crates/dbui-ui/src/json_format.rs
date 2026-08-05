//! Pretty-print and syntax-highlight JSON for detail fields.

use gpui::Rgba;

use crate::theme::Theme;

/// If `text` is a JSON object or array, return a pretty-printed form.
/// Scalars (`"hi"`, `1`, `true`) stay as-is — only structured values expand.
pub fn pretty_if_json(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("NULL") {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    match value {
        serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
            serde_json::to_string_pretty(&value).ok()
        }
        _ => None,
    }
}

/// Display text for a cell: pretty JSON when applicable.
pub fn display_text(text: &str) -> String {
    pretty_if_json(text).unwrap_or_else(|| text.to_string())
}

/// True when the strings are identical, or both parse as equal JSON values.
pub fn texts_equivalent(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (
        serde_json::from_str::<serde_json::Value>(a.trim()),
        serde_json::from_str::<serde_json::Value>(b.trim()),
    ) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// True when the buffer is a JSON object or array (highlight-worthy).
pub fn is_structured_json(text: &str) -> bool {
    let trimmed = text.trim();
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(serde_json::Value::Object(_)) | Ok(serde_json::Value::Array(_)) => true,
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonStyle {
    Punct,
    Key,
    String,
    Number,
    Bool,
    Null,
}

impl JsonStyle {
    pub fn color(self, theme: &Theme) -> Rgba {
        match self {
            JsonStyle::Punct => theme.text_muted,
            JsonStyle::Key => theme.value_structured,
            JsonStyle::String => theme.value_text,
            JsonStyle::Number => theme.value_number,
            JsonStyle::Bool => theme.value_bool,
            JsonStyle::Null => theme.value_null,
        }
    }
}

/// Absolute byte spans covering `text`, if it is structured JSON.
pub fn highlight_spans(text: &str) -> Option<Vec<(usize, usize, JsonStyle)>> {
    if !is_structured_json(text) {
        return None;
    }
    Some(tokenize(text))
}

/// Styles overlapping a line's absolute byte range, clipped to that line.
pub fn styles_on_line(
    spans: &[(usize, usize, JsonStyle)],
    line_range: &std::ops::Range<usize>,
) -> Vec<(usize, usize, JsonStyle)> {
    let mut out = Vec::new();
    for &(start, end, style) in spans {
        let lo = start.max(line_range.start);
        let hi = end.min(line_range.end);
        if lo < hi {
            out.push((lo - line_range.start, hi - line_range.start, style));
        }
    }
    out
}

fn tokenize(text: &str) -> Vec<(usize, usize, JsonStyle)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    // Stack of containers: (is_object, expect_key)
    let mut stack: Vec<(bool, bool)> = Vec::new();

    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        match c {
            b'{' => {
                out.push((i, i + 1, JsonStyle::Punct));
                stack.push((true, true));
                i += 1;
            }
            b'[' => {
                out.push((i, i + 1, JsonStyle::Punct));
                stack.push((false, false));
                i += 1;
            }
            b'}' | b']' => {
                out.push((i, i + 1, JsonStyle::Punct));
                stack.pop();
                i += 1;
            }
            b':' => {
                out.push((i, i + 1, JsonStyle::Punct));
                if let Some((_, expect_key)) = stack.last_mut() {
                    *expect_key = false;
                }
                i += 1;
            }
            b',' => {
                out.push((i, i + 1, JsonStyle::Punct));
                if let Some((is_object, expect_key)) = stack.last_mut() {
                    if *is_object {
                        *expect_key = true;
                    }
                }
                i += 1;
            }
            b'"' => {
                let start = i;
                i = end_of_string(bytes, i);
                let is_key = stack
                    .last()
                    .map(|(is_object, expect_key)| *is_object && *expect_key)
                    .unwrap_or(false);
                out.push((
                    start,
                    i,
                    if is_key {
                        JsonStyle::Key
                    } else {
                        JsonStyle::String
                    },
                ));
            }
            b't' if bytes[i..].starts_with(b"true") => {
                out.push((i, i + 4, JsonStyle::Bool));
                i += 4;
            }
            b'f' if bytes[i..].starts_with(b"false") => {
                out.push((i, i + 5, JsonStyle::Bool));
                i += 5;
            }
            b'n' if bytes[i..].starts_with(b"null") => {
                out.push((i, i + 4, JsonStyle::Null));
                i += 4;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                i += 1;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                {
                    i += 1;
                }
                out.push((start, i, JsonStyle::Number));
            }
            _ => i += 1,
        }
    }
    out
}

fn end_of_string(bytes: &[u8], start: usize) -> usize {
    // `start` points at the opening quote.
    let mut i = start + 1;
    let mut escape = false;
    while i < bytes.len() {
        let c = bytes[i];
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        if c == b'\\' {
            escape = true;
            i += 1;
            continue;
        }
        if c == b'"' {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_expands_objects_only() {
        let pretty = pretty_if_json(r#"{"a":1}"#).unwrap();
        assert!(pretty.contains('\n'));
        assert!(pretty.contains("\"a\""));
        assert_eq!(pretty_if_json("just text"), None);
        assert_eq!(pretty_if_json("42"), None);
        assert_eq!(pretty_if_json(r#""hi""#), None);
    }

    #[test]
    fn equivalent_ignores_whitespace() {
        assert!(texts_equivalent(r#"{"a":1}"#, "{\n  \"a\": 1\n}"));
        assert!(!texts_equivalent(r#"{"a":1}"#, r#"{"a":2}"#));
    }

    #[test]
    fn highlight_marks_keys_and_values() {
        let text = "{\n  \"name\": \"Ada\",\n  \"n\": 1\n}";
        let spans = highlight_spans(text).unwrap();
        let keyed = spans
            .iter()
            .any(|&(s, e, style)| style == JsonStyle::Key && &text[s..e] == "\"name\"");
        let stringed = spans
            .iter()
            .any(|&(s, e, style)| style == JsonStyle::String && &text[s..e] == "\"Ada\"");
        let numbered = spans
            .iter()
            .any(|&(s, e, style)| style == JsonStyle::Number && &text[s..e] == "1");
        assert!(keyed && stringed && numbered);
    }
}
