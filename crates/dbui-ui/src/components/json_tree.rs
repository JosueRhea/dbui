//! A JSON value in the detail sidebar, as a tree.
//!
//! Pretty-printed text is fine for a small object and a wall for a big one:
//! a `metadata` column with forty keys and nested arrays is read by opening
//! the branch you care about, not by scrolling. Each object and array folds;
//! a scalar is drawn in its type's colour, and clicking one copies it -- with
//! its path, `$.address.city`, said on the status line so it can be used in
//! a `->>` or `JSON_EXTRACT` next.
//!
//! Read-only: editing stays in the text box, one toggle away.

use crate::root::{DbUi, Status};
use crate::theme::{metrics, Theme};
use gpui::{div, prelude::*, px, AnyElement, ClipboardItem, Context, SharedString};
use serde_json::Value as Json;
use std::collections::HashSet;

/// Rows past this are not drawn; a JSON cell that large is better read as
/// text, which scrolls.
const MAX_ROWS: usize = 400;

/// One drawn line of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    /// `$.a.b[2]`: where this is, for copying and for folding.
    pub path: String,
    /// The key or index in front, if any.
    pub label: Option<String>,
    pub kind: TreeKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TreeKind {
    /// An object or array: how many children, and whether it is open.
    Branch {
        array: bool,
        len: usize,
        open: bool,
    },
    Leaf {
        text: String,
        style: LeafStyle,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafStyle {
    String,
    Number,
    Bool,
    Null,
}

/// The rows to draw for `value`, skipping the children of every path in
/// `closed`. The root is always open.
pub fn rows(value: &Json, closed: &HashSet<String>) -> Vec<TreeRow> {
    let mut out = Vec::new();
    walk(value, "$".into(), None, 0, closed, &mut out);
    out
}

fn walk(
    value: &Json,
    path: String,
    label: Option<String>,
    depth: usize,
    closed: &HashSet<String>,
    out: &mut Vec<TreeRow>,
) {
    if out.len() >= MAX_ROWS {
        return;
    }
    match value {
        Json::Object(map) => {
            let open = depth == 0 || !closed.contains(&path);
            out.push(TreeRow {
                depth,
                path: path.clone(),
                label,
                kind: TreeKind::Branch {
                    array: false,
                    len: map.len(),
                    open,
                },
            });
            if open {
                for (key, child) in map {
                    let child_path = if is_plain_key(key) {
                        format!("{path}.{key}")
                    } else {
                        format!("{path}[{}]", serde_json::to_string(key).unwrap_or_default())
                    };
                    walk(child, child_path, Some(key.clone()), depth + 1, closed, out);
                }
            }
        }
        Json::Array(items) => {
            let open = depth == 0 || !closed.contains(&path);
            out.push(TreeRow {
                depth,
                path: path.clone(),
                label,
                kind: TreeKind::Branch {
                    array: true,
                    len: items.len(),
                    open,
                },
            });
            if open {
                for (index, child) in items.iter().enumerate() {
                    walk(
                        child,
                        format!("{path}[{index}]"),
                        Some(index.to_string()),
                        depth + 1,
                        closed,
                        out,
                    );
                }
            }
        }
        leaf => out.push(TreeRow {
            depth,
            path,
            label,
            kind: TreeKind::Leaf {
                text: match leaf {
                    Json::String(text) => text.clone(),
                    other => other.to_string(),
                },
                style: match leaf {
                    Json::String(_) => LeafStyle::String,
                    Json::Number(_) => LeafStyle::Number,
                    Json::Bool(_) => LeafStyle::Bool,
                    _ => LeafStyle::Null,
                },
            },
        }),
    }
}

/// A key that reads as a bare word in a path: `$.city`, not `$["first name"]`.
fn is_plain_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl DbUi {
    /// Show a JSON field as a tree, or back as text.
    pub(crate) fn toggle_json_tree(&mut self, field: &str, cx: &mut Context<Self>) {
        if !self.json_tree_fields.remove(field) {
            self.json_tree_fields.insert(field.to_string());
        }
        cx.notify();
    }

    fn toggle_json_node(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.json_tree_closed.remove(&key) {
            self.json_tree_closed.insert(key);
        }
        cx.notify();
    }

    fn copy_json_leaf(&mut self, path: &str, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.status = Status::info(format!("Copied {path}"));
        cx.notify();
    }
}

/// The tree for the JSON text of field `field`. `None` when the text is not
/// an object or array, which the caller then draws as text.
pub(crate) fn render(
    field: &str,
    index: usize,
    text: &str,
    closed: &HashSet<String>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> Option<AnyElement> {
    let value: Json = serde_json::from_str(text).ok()?;
    if !(value.is_object() || value.is_array()) {
        return None;
    }
    // Folds are kept per field, so `$.a` in one column is not `$.a` in another.
    let prefix = format!("{field}\u{1f}");
    let field_closed: HashSet<String> = closed
        .iter()
        .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
        .collect();
    let rows = rows(&value, &field_closed);
    let clipped = rows.len() >= MAX_ROWS;

    let drawn: Vec<AnyElement> = rows
        .into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            let indent = metrics::scaled(14.) * row.depth as f32;
            let label = row.label.clone().map(|label| {
                div()
                    .flex_shrink_0()
                    .text_color(theme.text_muted)
                    .child(SharedString::from(format!("{label}:")))
            });
            let base = div()
                .id(("json-row", index * 10_000 + row_index))
                .flex()
                .items_center()
                .gap_1()
                .pl(indent)
                .h(px(18.))
                .whitespace_nowrap()
                .rounded_sm()
                .hover(|line| line.bg(theme.hover))
                .cursor_pointer();
            match row.kind {
                TreeKind::Branch { array, len, open } => {
                    let key = format!("{prefix}{}", row.path);
                    let summary = match (array, len) {
                        (true, 1) => "[1 item]".to_string(),
                        (true, n) => format!("[{n} items]"),
                        (false, 1) => "{1 key}".to_string(),
                        (false, n) => format!("{{{n} keys}}"),
                    };
                    base.on_click(cx.listener(move |this, _, _window, cx| {
                        this.toggle_json_node(key.clone(), cx)
                    }))
                    .child(
                        div()
                            .w(metrics::scaled(10.))
                            .flex_shrink_0()
                            .text_color(theme.text_faint)
                            .child(if open { "▾" } else { "▸" }),
                    )
                    .children(label)
                    .child(
                        div()
                            .text_color(theme.text_faint)
                            .child(SharedString::from(summary)),
                    )
                    .into_any_element()
                }
                TreeKind::Leaf { text, style } => {
                    let color = match style {
                        LeafStyle::String => theme.value_text,
                        LeafStyle::Number => theme.value_number,
                        LeafStyle::Bool => theme.value_bool,
                        LeafStyle::Null => theme.value_null,
                    };
                    let shown = match style {
                        LeafStyle::String => format!("\"{text}\""),
                        _ => text.clone(),
                    };
                    let path = row.path.clone();
                    base.on_click(cx.listener(move |this, _, _window, cx| {
                        this.copy_json_leaf(&path, text.clone(), cx)
                    }))
                    .child(div().w(metrics::scaled(10.)).flex_shrink_0())
                    .children(label)
                    .child(
                        div()
                            .min_w(px(0.))
                            .truncate()
                            .text_color(color)
                            .child(SharedString::from(shown)),
                    )
                    .into_any_element()
                }
            }
        })
        .collect();

    Some(
        div()
            .id(("json-tree", index))
            .w_full()
            .min_w(px(0.))
            .max_h(metrics::scaled(360.))
            .overflow_y_scroll()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .font_family(metrics::MONO_FONT)
            .text_size(metrics::text_size_small())
            .children(drawn)
            .when(clipped, |tree| {
                tree.child(
                    div()
                        .text_color(theme.text_faint)
                        .child("… more than the tree shows; read it as text"),
                )
            })
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_walks_in_order_with_paths() {
        let value: Json = serde_json::from_str(
            r#"{"name": "Ada", "tags": ["a", 2], "first name": null, "ok": true}"#,
        )
        .unwrap();
        let rows = rows(&value, &HashSet::new());
        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "$",
                "$.name",
                "$.tags",
                "$.tags[0]",
                "$.tags[1]",
                "$[\"first name\"]",
                "$.ok"
            ]
        );
        assert_eq!(rows[3].depth, 2);
        assert!(matches!(
            &rows[4].kind,
            TreeKind::Leaf { text, style: LeafStyle::Number } if text == "2"
        ));
    }

    #[test]
    fn a_closed_branch_hides_its_children_but_not_itself() {
        let value: Json = serde_json::from_str(r#"{"a": {"b": 1}, "c": 2}"#).unwrap();
        let closed: HashSet<String> = ["$.a".to_string()].into();
        let rows = rows(&value, &closed);
        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, vec!["$", "$.a", "$.c"]);
        assert!(matches!(
            rows[1].kind,
            TreeKind::Branch {
                open: false,
                len: 1,
                ..
            }
        ));
    }

    #[test]
    fn a_huge_value_stops_at_the_row_limit() {
        let items: Vec<u32> = (0..10_000).collect();
        let value = serde_json::to_value(items).unwrap();
        assert_eq!(rows(&value, &HashSet::new()).len(), MAX_ROWS);
    }
}
