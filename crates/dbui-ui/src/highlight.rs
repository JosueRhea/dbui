//! Shared painting of colored text runs with an in-flow caret and selection.
//!
//! JSON detail fields and the SQL editor both need the same layout: split a
//! line on style / selection / caret boundaries, then emit coloured `div`s.
//! Styles arrive as absolute byte spans already clipped to the line.

use gpui::{div, prelude::*, px, AnyElement, Pixels, Rgba, SharedString};

use crate::text_input;
use crate::theme::{metrics, Theme};

/// Styles overlapping a line's absolute byte range, clipped and made relative.
pub fn styles_on_line<S: Copy>(
    spans: &[(usize, usize, S)],
    line_range: &std::ops::Range<usize>,
) -> Vec<(usize, usize, S)> {
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

/// Paint one line as colored runs with in-flow caret and selection.
///
/// `styles` are relative to `line` (byte offsets). `color_at` maps each style
/// tag to a colour; unstyled regions use `theme.text`.
pub fn render_highlighted_line<S: Copy>(
    line: &str,
    styles: &[(usize, usize, S)],
    selection: Option<std::ops::Range<usize>>,
    caret_at: Option<usize>,
    caret_color: Rgba,
    theme: &Theme,
    line_h: Pixels,
    mut color_at: impl FnMut(S) -> Rgba,
) -> AnyElement {
    let caret_h = px(12. * metrics::zoom());
    let default = theme.text;

    let colored: Vec<(usize, usize, Rgba)> = styles
        .iter()
        .map(|&(start, end, style)| (start, end, color_at(style)))
        .collect();

    let color_at_offset = |offset: usize| -> Rgba {
        colored
            .iter()
            .find(|&&(start, end, _)| offset >= start && offset < end)
            .map(|&(_, _, color)| color)
            .unwrap_or(default)
    };

    let selected = |offset: usize| -> bool {
        selection
            .as_ref()
            .is_some_and(|sel| offset >= sel.start && offset < sel.end)
    };

    let mut cuts = vec![0usize, line.len()];
    for &(start, end, _) in &colored {
        cuts.push(start.min(line.len()));
        cuts.push(end.min(line.len()));
    }
    if let Some(sel) = &selection {
        cuts.push(sel.start.min(line.len()));
        cuts.push(sel.end.min(line.len()));
    }
    if let Some(caret) = caret_at {
        cuts.push(caret.min(line.len()));
    }
    cuts.retain(|&offset| offset <= line.len() && line.is_char_boundary(offset));
    cuts.sort_unstable();
    cuts.dedup();

    let mut runs: Vec<AnyElement> = Vec::new();
    let caret_byte = caret_at.filter(|&c| c <= line.len());

    for window in cuts.windows(2) {
        let start = window[0];
        let end = window[1];

        if caret_byte == Some(start) {
            runs.push(text_input::caret_element(caret_color, caret_h).into_any_element());
        }

        if start >= end {
            continue;
        }

        let color = color_at_offset(start);
        let in_selection = selected(start);
        let piece = SharedString::from(line[start..end].to_string());
        runs.push(if in_selection {
            div()
                .bg(theme.selection)
                .text_color(color)
                .child(piece)
                .into_any_element()
        } else {
            div().text_color(color).child(piece).into_any_element()
        });
    }

    if line.is_empty() {
        if caret_byte.is_some() {
            runs.push(text_input::caret_element(caret_color, caret_h).into_any_element());
        }
    } else if caret_byte == Some(line.len()) {
        runs.push(text_input::caret_element(caret_color, caret_h).into_any_element());
    }

    div()
        .flex_shrink_0()
        .h(line_h)
        .flex()
        .items_center()
        .whitespace_nowrap()
        .children(runs)
        .into_any_element()
}
