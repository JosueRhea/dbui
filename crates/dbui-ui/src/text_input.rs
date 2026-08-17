//! An editable text buffer, shared by the SQL editor and the form fields.
//!
//! GPUI ships no text input -- Zed's lives in Zed. This holds a `String`, a
//! selection range in bytes, an undo stack, and the editing keys people expect.
//! It does not do IME. The caret and selection are drawn by splitting the line
//! rather than by measuring glyphs; mouse hit-testing uses monospace metrics.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{div, point, px, App, Bounds, ClipboardItem, Keystroke, Pixels, Point, ScrollHandle};

const UNDO_LIMIT: usize = 100;

/// Put caps lock back into a keystroke's typed character.
///
/// GPUI asks the keyboard layout what a key produces while passing it only
/// shift and option -- never the alpha-lock bit -- so with caps lock on macOS
/// hands us `a` for a key the rest of the system reads as `A`. Every field
/// here types `keystroke.key_char`, so every field types lowercase until this
/// runs. `keystroke.key` is left alone on purpose: it is what shortcuts match
/// on, and ⌘S has to keep working with caps lock down.
///
/// Uppercase is the whole rule. Asking the layout itself -- `UCKeyTranslate`
/// over every key of `com.apple.keylayout.ABC` -- says caps lock changes
/// exactly the 26 letters and nothing else: a digit stays a digit, `⇧1` stays
/// `!`, and `⇧`+caps stays *upper*case rather than inverting the way it does
/// on Windows. So shift needs no special case here; GPUI has already applied
/// it, and uppercasing an `A` again changes nothing.
///
/// Returns `None` when nothing needed changing, so the common keystroke does
/// not clone.
pub(crate) fn with_capslock(keystroke: &Keystroke, capslock: bool) -> Option<Keystroke> {
    if !capslock {
        return None;
    }
    let typed = keystroke.key_char.as_deref()?;
    let raised: String = typed.chars().map(upper).collect();
    if raised == typed {
        return None;
    }
    let mut fixed = keystroke.clone();
    fixed.key_char = Some(raised);
    Some(fixed)
}

/// One character in, one character out.
///
/// A key press is one character, so a mapping that grows -- German `ß`
/// uppercasing to `SS` -- is not something a keyboard does, and the layout is
/// the authority on those anyway. Leave those alone rather than invent them.
fn upper(c: char) -> char {
    let mut mapped = c.to_uppercase();
    match (mapped.next(), mapped.next()) {
        (Some(only), None) => only,
        _ => c,
    }
}

#[derive(Clone)]
struct Snapshot {
    value: String,
    selection: Range<usize>,
    selection_reversed: bool,
}

pub struct TextInput {
    value: String,
    /// Selected byte range. Empty (`start == end`) means a caret.
    /// Always on character boundaries.
    selection: Range<usize>,
    /// When true the head (moving end) is `selection.start`.
    selection_reversed: bool,
    multiline: bool,
    /// Mouse drag is in progress.
    selecting: bool,
    /// Laid-out bounds of the editable surface. Written from paint via
    /// [`hit_bounds_slot`] without going through `entity.update` (that was
    /// re-rendering every field on every frame and made typing crawl).
    hit_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Horizontal scroll for single-line fields (detail sidebar, WHERE, …).
    scroll_handle: ScrollHandle,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
}

impl Default for TextInput {
    fn default() -> Self {
        Self::new(false)
    }
}

impl TextInput {
    pub fn new(multiline: bool) -> Self {
        Self {
            value: String::new(),
            selection: 0..0,
            selection_reversed: false,
            multiline,
            selecting: false,
            hit_bounds: Rc::new(Cell::new(None)),
            scroll_handle: ScrollHandle::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn with_text(text: impl Into<String>, multiline: bool) -> Self {
        let value: String = text.into();
        let end = value.len();
        Self {
            value,
            selection: end..end,
            selection_reversed: false,
            multiline,
            selecting: false,
            hit_bounds: Rc::new(Cell::new(None)),
            scroll_handle: ScrollHandle::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn scroll_handle(&self) -> &ScrollHandle {
        &self.scroll_handle
    }

    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    /// Cloneable slot for the paint callback — write bounds without notifying.
    pub fn hit_bounds_slot(&self) -> Rc<Cell<Option<Bounds<Pixels>>>> {
        self.hit_bounds.clone()
    }

    /// Keep the caret inside the visible viewport of the field.
    pub fn ensure_caret_visible(&self) {
        let Some(bounds) = self.hit_bounds.get() else {
            return;
        };

        let pad = px(12.);
        let offset = self.scroll_handle.offset();
        let mut new_x = offset.x;
        let mut new_y = offset.y;

        let (caret_x, caret_y, line_h) = if self.multiline {
            let layout = self.layout();
            let line = layout.lines.get(layout.caret_line).copied().unwrap_or("");
            let col = layout.caret_column.min(line.len());
            let col_chars = line[..col].chars().count() as f32;
            let line_h = f32::from(field_line_height());
            (
                px(col_chars * field_char_width()),
                px(layout.caret_line as f32 * line_h),
                line_h,
            )
        } else {
            let caret_chars = self.value[..self.cursor()].chars().count() as f32;
            (px(caret_chars * char_width()), px(0.), 0.)
        };

        let viewport_w = bounds.size.width;
        if viewport_w > px(0.) {
            let view_left = -offset.x;
            let view_right = view_left + viewport_w;
            if caret_x < view_left + pad {
                new_x = -((caret_x - pad).max(px(0.)));
            } else if caret_x > view_right - pad {
                new_x = -(caret_x - viewport_w + pad);
            }
        }

        if self.multiline {
            let viewport_h = bounds.size.height;
            if viewport_h > px(0.) {
                let view_top = -offset.y;
                let view_bottom = view_top + viewport_h;
                let caret_bottom = caret_y + px(line_h);
                if caret_y < view_top {
                    new_y = -caret_y;
                } else if caret_bottom > view_bottom {
                    new_y = -(caret_bottom - viewport_h);
                }
            }
            let max = self.scroll_handle.max_offset();
            new_x = new_x.clamp(-max.width, px(0.));
            new_y = new_y.clamp(-max.height, px(0.));
        } else {
            // No scroll container — clamp X from content width, Y always 0.
            // Use a slightly generous advance so we pan before the real caret
            // disappears off the right edge (underestimates leave it stranded).
            let advance = char_width() * 1.05;
            let content_w = px(self.value.chars().count() as f32 * advance + 8.);
            let max_x = (content_w - viewport_w).max(px(0.));
            new_x = new_x.clamp(-max_x, px(0.));
            new_y = px(0.);
        }
        if new_x != offset.x || new_y != offset.y {
            self.scroll_handle.set_offset(point(new_x, new_y));
        }
    }

    pub fn text(&self) -> &str {
        &self.value
    }

    /// The caret / head of the selection.
    pub fn cursor(&self) -> usize {
        if self.selection_reversed {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    pub fn selection(&self) -> Range<usize> {
        self.selection.clone()
    }

    pub fn has_selection(&self) -> bool {
        self.selection.start != self.selection.end
    }

    /// The selected slice, if the range is non-empty.
    pub fn selected_text(&self) -> Option<&str> {
        if !self.has_selection() {
            return None;
        }
        self.value.get(self.selection.clone())
    }

    pub fn is_selecting(&self) -> bool {
        self.selecting
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.value = text.into();
        let end = self.value.len();
        self.selection = end..end;
        self.selection_reversed = false;
        self.undo.clear();
        self.redo.clear();
    }

    pub fn clear(&mut self) {
        self.push_undo();
        self.value.clear();
        self.selection = 0..0;
        self.selection_reversed = false;
        self.scroll_handle.set_offset(point(px(0.), px(0.)));
    }

    /// The lines, and where the caret sits within its own line.
    pub fn layout(&self) -> Layout<'_> {
        let lines: Vec<&str> = self.value.split('\n').collect();
        let cursor = self.cursor();

        let mut consumed = 0usize;
        for (index, line) in lines.iter().enumerate() {
            let end = consumed + line.len();
            // `<=` so a caret at end-of-line belongs to that line, not the next.
            if cursor <= end {
                return Layout {
                    lines,
                    caret_line: index,
                    caret_column: cursor - consumed,
                    selection: self.selection.clone(),
                };
            }
            consumed = end + 1; // the '\n'
        }

        Layout {
            caret_line: lines.len().saturating_sub(1),
            caret_column: lines.last().map(|line| line.len()).unwrap_or(0),
            lines,
            selection: self.selection.clone(),
        }
    }

    /// Absolute byte range of a line (not including its trailing newline).
    pub fn line_range(&self, line_index: usize) -> Range<usize> {
        let mut start = 0usize;
        for (index, line) in self.value.split('\n').enumerate() {
            let end = start + line.len();
            if index == line_index {
                return start..end;
            }
            start = end + 1;
        }
        let len = self.value.len();
        len..len
    }

    pub fn move_to(&mut self, offset: usize) {
        let offset = self.clamp_boundary(offset);
        self.selection = offset..offset;
        self.selection_reversed = false;
    }

    pub fn select_to(&mut self, offset: usize) {
        let offset = self.clamp_boundary(offset);
        if self.selection_reversed {
            self.selection.start = offset;
        } else {
            self.selection.end = offset;
        }
        if self.selection.end < self.selection.start {
            self.selection_reversed = !self.selection_reversed;
            self.selection = self.selection.end..self.selection.start;
        }
    }

    pub fn select_all(&mut self) {
        self.selection = 0..self.value.len();
        self.selection_reversed = false;
    }

    pub fn begin_selecting(&mut self) {
        self.selecting = true;
    }

    pub fn end_selecting(&mut self) {
        self.selecting = false;
    }

    /// Place the caret, extend a selection, or select a word/line from a click.
    ///
    /// `click_count` follows the platform: 1 = caret, 2 = word, 3 = line.
    pub fn click_at(&mut self, offset: usize, extend: bool, click_count: usize) {
        match click_count {
            2 => self.select_word_at(offset),
            3.. => self.select_line_at(offset),
            _ if extend => self.select_to(offset),
            _ => self.move_to(offset),
        }
    }

    /// Select the word (or whitespace run) under `offset`.
    pub fn select_word_at(&mut self, offset: usize) {
        let offset = self.clamp_boundary(offset);
        if self.value.is_empty() {
            self.move_to(0);
            return;
        }

        let probe = if offset >= self.value.len() {
            self.prev_boundary(offset)
        } else {
            offset
        };
        let Some(ch) = self.value[probe..].chars().next() else {
            self.move_to(offset);
            return;
        };

        let class_word = if ch.is_whitespace() {
            None
        } else {
            Some(Self::is_word_char(ch))
        };

        let mut start = probe;
        while start > 0 {
            let prev = self.prev_boundary(start);
            let Some(prev_ch) = self.value[prev..].chars().next() else {
                break;
            };
            let matches = match class_word {
                None => prev_ch.is_whitespace(),
                Some(word) => !prev_ch.is_whitespace() && Self::is_word_char(prev_ch) == word,
            };
            if !matches {
                break;
            }
            start = prev;
        }

        let mut end = self.next_boundary(probe);
        while end < self.value.len() {
            let Some(next_ch) = self.value[end..].chars().next() else {
                break;
            };
            let matches = match class_word {
                None => next_ch.is_whitespace(),
                Some(word) => !next_ch.is_whitespace() && Self::is_word_char(next_ch) == word,
            };
            if !matches {
                break;
            }
            end = self.next_boundary(end);
        }

        self.selection = start..end;
        self.selection_reversed = false;
    }

    /// Select the whole line containing `offset`, including its trailing newline.
    pub fn select_line_at(&mut self, offset: usize) {
        let offset = self.clamp_boundary(offset);
        let start = self.line_start(offset);
        let mut end = self.line_end(offset);
        if end < self.value.len() && self.value.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        self.selection = start..end;
        self.selection_reversed = false;
    }

    /// Map a window mouse position to a byte offset using stored hit bounds.
    ///
    /// `gutter` covers the editor's line-number column; form fields pass zero.
    /// `line_height` is used only when multiline.
    pub fn offset_for_mouse(
        &self,
        position: Point<Pixels>,
        gutter: Pixels,
        line_height: Pixels,
        char_width: f32,
    ) -> usize {
        let Some(bounds) = self.hit_bounds.get() else {
            return self.cursor();
        };
        // Hit bounds are the visible viewport; convert to content coords.
        let scroll = self.scroll_handle.offset();
        let local_x = position.x - bounds.left() + (-scroll.x);
        let line_index = if self.multiline {
            let local_y = f32::from(position.y - bounds.top() + (-scroll.y)).max(0.);
            let height = f32::from(line_height).max(1.);
            let lines = self.value.split('\n').count().max(1);
            ((local_y / height) as usize).min(lines.saturating_sub(1))
        } else {
            0
        };
        self.offset_for_click(line_index, local_x, gutter, char_width)
    }

    /// Map a local x position within a line to a byte offset in the buffer.
    ///
    /// `gutter` is subtracted first (editor line numbers). Uses a fixed
    /// character width because the UI is monospace.
    pub fn offset_for_click(
        &self,
        line_index: usize,
        local_x: Pixels,
        gutter: Pixels,
        char_width: f32,
    ) -> usize {
        let range = self.line_range(line_index);
        let line = &self.value[range.clone()];
        let x = f32::from(local_x) - f32::from(gutter);
        if x <= 0.0 || char_width <= 0.0 {
            return range.start;
        }
        let char_col = ((x / char_width) + 0.5) as usize;
        let mut chars = 0usize;
        for (byte_idx, _) in line.char_indices() {
            if chars >= char_col {
                return range.start + byte_idx;
            }
            chars += 1;
        }
        range.end
    }

    pub fn insert(&mut self, text: &str) {
        self.replace_selection(text);
    }

    pub fn replace_selection(&mut self, text: &str) {
        self.push_undo();
        let range = self.selection.clone();
        self.value.replace_range(range.clone(), text);
        let cursor = range.start + text.len();
        self.selection = cursor..cursor;
        self.selection_reversed = false;
    }

    /// Replace an absolute byte range and leave the caret after the insert.
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        let start = range.start.min(self.value.len());
        let end = range.end.min(self.value.len()).max(start);
        self.push_undo();
        self.value.replace_range(start..end, text);
        let cursor = start + text.len();
        self.selection = cursor..cursor;
        self.selection_reversed = false;
    }

    pub fn backspace(&mut self) {
        if self.has_selection() {
            self.replace_selection("");
            return;
        }
        if self.cursor() == 0 {
            return;
        }
        let previous = self.prev_boundary(self.cursor());
        self.push_undo();
        self.value.replace_range(previous..self.cursor(), "");
        self.selection = previous..previous;
        self.selection_reversed = false;
    }

    pub fn delete(&mut self) {
        if self.has_selection() {
            self.replace_selection("");
            return;
        }
        if self.cursor() >= self.value.len() {
            return;
        }
        let next = self.next_boundary(self.cursor());
        self.push_undo();
        let cursor = self.cursor();
        self.value.replace_range(cursor..next, "");
        self.selection = cursor..cursor;
        self.selection_reversed = false;
    }

    pub fn delete_word_backward(&mut self) {
        if self.has_selection() {
            self.replace_selection("");
            return;
        }
        let cursor = self.cursor();
        if cursor == 0 {
            return;
        }
        let target = self.prev_word_boundary(cursor);
        self.push_undo();
        self.value.replace_range(target..cursor, "");
        self.selection = target..target;
        self.selection_reversed = false;
    }

    pub fn delete_word_forward(&mut self) {
        if self.has_selection() {
            self.replace_selection("");
            return;
        }
        let cursor = self.cursor();
        if cursor >= self.value.len() {
            return;
        }
        let target = self.next_word_boundary(cursor);
        self.push_undo();
        self.value.replace_range(cursor..target, "");
        self.selection = cursor..cursor;
        self.selection_reversed = false;
    }

    pub fn delete_to_line_start(&mut self) {
        if self.has_selection() {
            self.replace_selection("");
            return;
        }
        let cursor = self.cursor();
        let start = self.line_start(cursor);
        if start == cursor {
            return;
        }
        self.push_undo();
        self.value.replace_range(start..cursor, "");
        self.selection = start..start;
        self.selection_reversed = false;
    }

    pub fn move_left(&mut self) {
        if self.has_selection() {
            let edge = self.selection.start;
            self.move_to(edge);
            return;
        }
        self.move_to(self.prev_boundary(self.cursor()));
    }

    pub fn move_right(&mut self) {
        if self.has_selection() {
            let edge = self.selection.end;
            self.move_to(edge);
            return;
        }
        self.move_to(self.next_boundary(self.cursor()));
    }

    pub fn select_left(&mut self) {
        self.select_to(self.prev_boundary(self.cursor()));
    }

    pub fn select_right(&mut self) {
        self.select_to(self.next_boundary(self.cursor()));
    }

    pub fn move_word_left(&mut self) {
        if self.has_selection() {
            self.move_to(self.selection.start);
            return;
        }
        self.move_to(self.prev_word_boundary(self.cursor()));
    }

    pub fn move_word_right(&mut self) {
        if self.has_selection() {
            self.move_to(self.selection.end);
            return;
        }
        self.move_to(self.next_word_boundary(self.cursor()));
    }

    pub fn select_word_left(&mut self) {
        self.select_to(self.prev_word_boundary(self.cursor()));
    }

    pub fn select_word_right(&mut self) {
        self.select_to(self.next_word_boundary(self.cursor()));
    }

    pub fn move_home(&mut self) {
        self.move_to(self.line_start(self.cursor()));
    }

    pub fn move_end(&mut self) {
        self.move_to(self.line_end(self.cursor()));
    }

    pub fn select_home(&mut self) {
        self.select_to(self.line_start(self.cursor()));
    }

    pub fn select_end(&mut self) {
        self.select_to(self.line_end(self.cursor()));
    }

    /// Vertical motion keeps the column where it can.
    ///
    /// Moving onto a shorter line clamps to its end. This one does not remember
    /// the "goal column" across several moves -- deliberate: that state is the
    /// beginning of a real editor.
    pub fn move_up(&mut self) {
        let (line, column, widths) = self.caret_position();
        if line == 0 {
            self.move_to(0);
            return;
        }
        let offset = self.offset_on_line(line - 1, column, &widths);
        self.move_to(offset);
    }

    pub fn move_down(&mut self) {
        let (line, column, widths) = self.caret_position();
        if line + 1 >= widths.len() {
            self.move_to(self.value.len());
            return;
        }
        let offset = self.offset_on_line(line + 1, column, &widths);
        self.move_to(offset);
    }

    pub fn select_up(&mut self) {
        let (line, column, widths) = self.caret_position();
        if line == 0 {
            self.select_to(0);
            return;
        }
        let offset = self.offset_on_line(line - 1, column, &widths);
        self.select_to(offset);
    }

    pub fn select_down(&mut self) {
        let (line, column, widths) = self.caret_position();
        if line + 1 >= widths.len() {
            self.select_to(self.value.len());
            return;
        }
        let offset = self.offset_on_line(line + 1, column, &widths);
        self.select_to(offset);
    }

    pub fn copy(&self, cx: &App) {
        if !self.has_selection() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(
            self.value[self.selection.clone()].to_string(),
        ));
    }

    pub fn cut(&mut self, cx: &App) {
        if !self.has_selection() {
            return;
        }
        self.copy(cx);
        self.replace_selection("");
    }

    pub fn paste(&mut self, cx: &App) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let Some(text) = item.text() else {
            return;
        };
        let text = if self.multiline {
            text
        } else {
            text.replace(['\n', '\r'], "")
        };
        self.replace_selection(&text);
    }

    pub fn undo(&mut self) {
        let Some(previous) = self.undo.pop() else {
            return;
        };
        self.redo.push(self.snapshot());
        self.restore(previous);
    }

    pub fn redo(&mut self) {
        let Some(next) = self.redo.pop() else {
            return;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
    }

    /// Apply one keystroke. Returns whether it was consumed -- an unconsumed
    /// key falls through to the window's shortcuts.
    pub fn handle_key(&mut self, keystroke: &Keystroke, cx: &App) -> bool {
        let handled = self.dispatch_key(keystroke, cx);
        if handled {
            self.ensure_caret_visible();
        }
        handled
    }

    fn dispatch_key(&mut self, keystroke: &Keystroke, cx: &App) -> bool {
        let key = keystroke.key.as_str();
        let cmd = keystroke.modifiers.platform;
        let alt = keystroke.modifiers.alt;
        let shift = keystroke.modifiers.shift;
        let ctrl = keystroke.modifiers.control;

        // Ctrl alone is unused on macOS for these; leave it for the app.
        if ctrl && !cmd {
            return false;
        }

        if cmd {
            return match key {
                "a" => {
                    self.select_all();
                    true
                }
                "c" => {
                    self.copy(cx);
                    true
                }
                "x" => {
                    self.cut(cx);
                    true
                }
                "v" => {
                    self.paste(cx);
                    true
                }
                "z" if shift => {
                    self.redo();
                    true
                }
                "z" => {
                    self.undo();
                    true
                }
                "backspace" => {
                    self.delete_to_line_start();
                    true
                }
                // macOS: ⌘←/→ line ends, ⌘↑/↓ document ends.
                "left" if shift => {
                    self.select_home();
                    true
                }
                "left" => {
                    self.move_home();
                    true
                }
                "right" if shift => {
                    self.select_end();
                    true
                }
                "right" => {
                    self.move_end();
                    true
                }
                "up" if shift => {
                    self.select_to(0);
                    true
                }
                "up" => {
                    self.move_to(0);
                    true
                }
                "down" if shift => {
                    self.select_to(self.value.len());
                    true
                }
                "down" => {
                    self.move_to(self.value.len());
                    true
                }
                // App shortcuts: run, new connection, refresh, …
                _ => false,
            };
        }

        if alt {
            return match key {
                "left" if shift => {
                    self.select_word_left();
                    true
                }
                "left" => {
                    self.move_word_left();
                    true
                }
                "right" if shift => {
                    self.select_word_right();
                    true
                }
                "right" => {
                    self.move_word_right();
                    true
                }
                "backspace" => {
                    self.delete_word_backward();
                    true
                }
                "delete" => {
                    self.delete_word_forward();
                    true
                }
                _ => false,
            };
        }

        match key {
            "backspace" => self.backspace(),
            "delete" => self.delete(),
            "left" if shift => self.select_left(),
            "left" => self.move_left(),
            "right" if shift => self.select_right(),
            "right" => self.move_right(),
            "up" if shift => self.select_up(),
            "up" => self.move_up(),
            "down" if shift => self.select_down(),
            "down" => self.move_down(),
            "home" if shift => self.select_home(),
            "home" => self.move_home(),
            "end" if shift => self.select_end(),
            "end" => self.move_end(),
            "enter" if self.multiline => self.insert("\n"),
            "enter" => return false,
            // A literal tab in a text field would move focus in any other app;
            // in SQL it is indentation. Spaces, so the width is not a guess.
            "tab" if self.multiline && !shift => self.insert("  "),
            "tab" => return false,
            "space" => self.insert(" "),
            _ => {
                let Some(typed) = keystroke.key_char.as_ref() else {
                    return false;
                };
                if typed.is_empty() || typed.chars().any(|c| c.is_control()) {
                    return false;
                }
                self.insert(typed);
            }
        }
        true
    }

    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            value: self.value.clone(),
            selection: self.selection.clone(),
            selection_reversed: self.selection_reversed,
        }
    }

    fn restore(&mut self, snapshot: Snapshot) {
        self.value = snapshot.value;
        self.selection = snapshot.selection;
        self.selection_reversed = snapshot.selection_reversed;
    }

    fn caret_position(&self) -> (usize, usize, Vec<usize>) {
        let layout = self.layout();
        let widths = layout.lines.iter().map(|line| line.len()).collect();
        (layout.caret_line, layout.caret_column, widths)
    }

    fn offset_on_line(&self, line: usize, column: usize, widths: &[usize]) -> usize {
        let start: usize = widths[..line].iter().map(|width| width + 1).sum();
        let width = widths[line];
        let mut target = start + column.min(width);
        while target > start && !self.value.is_char_boundary(target) {
            target -= 1;
        }
        target
    }

    fn line_start(&self, from: usize) -> usize {
        self.value[..from]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    }

    fn line_end(&self, from: usize) -> usize {
        self.value[from..]
            .find('\n')
            .map(|offset| from + offset)
            .unwrap_or(self.value.len())
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    fn prev_word_boundary(&self, from: usize) -> usize {
        if from == 0 {
            return 0;
        }
        let bytes = self.value.as_bytes();
        let mut index = self.prev_boundary(from);

        // Skip whitespace first.
        while index > 0 {
            let ch = self.value[index..].chars().next().unwrap();
            if !ch.is_whitespace() {
                break;
            }
            index = self.prev_boundary(index);
        }

        if index == 0 && (self.value.is_empty() || bytes.is_empty()) {
            return 0;
        }

        let Some(ch) = self.value[index..].chars().next() else {
            return 0;
        };
        let word = Self::is_word_char(ch);
        while index > 0 {
            let prev = self.prev_boundary(index);
            let Some(prev_ch) = self.value[prev..].chars().next() else {
                break;
            };
            if Self::is_word_char(prev_ch) != word || prev_ch.is_whitespace() {
                break;
            }
            index = prev;
        }
        index
    }

    /// Where `⌥→` lands: the **end** of the word ahead.
    ///
    /// This is the asymmetry macOS has and Windows does not -- going right the
    /// caret stops after a word, going left it stops before one -- so a gap is
    /// crossed on the way rather than stopped in. The code here used to do the
    /// Windows thing (stop at the start of the next word) under a comment
    /// claiming it was the macOS one.
    fn next_word_boundary(&self, from: usize) -> usize {
        let mut index = from;

        // Cross any gap first: ⌥→ from inside whitespace goes on to the end of
        // the word after it, never stopping in the space.
        while index < self.value.len() {
            let Some(c) = self.value[index..].chars().next() else {
                break;
            };
            if !c.is_whitespace() {
                break;
            }
            index = self.next_boundary(index);
        }

        let Some(ch) = self.value[index..].chars().next() else {
            return self.value.len();
        };
        let word = Self::is_word_char(ch);
        while index < self.value.len() {
            let Some(c) = self.value[index..].chars().next() else {
                break;
            };
            if Self::is_word_char(c) != word || c.is_whitespace() {
                break;
            }
            index = self.next_boundary(index);
        }
        index
    }

    fn clamp_boundary(&self, offset: usize) -> usize {
        let offset = offset.min(self.value.len());
        if self.value.is_char_boundary(offset) {
            offset
        } else {
            self.prev_boundary(offset + 1)
        }
    }

    fn prev_boundary(&self, from: usize) -> usize {
        let mut index = from.saturating_sub(1);
        while index > 0 && !self.value.is_char_boundary(index) {
            index -= 1;
        }
        index
    }

    fn next_boundary(&self, from: usize) -> usize {
        let mut index = (from + 1).min(self.value.len());
        while index < self.value.len() && !self.value.is_char_boundary(index) {
            index += 1;
        }
        index
    }
}

/// A buffer split for rendering, with the caret and selection located.
pub struct Layout<'a> {
    pub lines: Vec<&'a str>,
    pub caret_line: usize,
    /// Byte offset of the caret within its line.
    pub caret_column: usize,
    pub selection: Range<usize>,
}

/// Intersection of a global selection with a line's absolute byte range,
/// returned as offsets within the line.
pub fn selection_on_line(
    selection: &Range<usize>,
    line_range: &Range<usize>,
) -> Option<Range<usize>> {
    let start = selection.start.max(line_range.start);
    let end = selection.end.min(line_range.end);
    if start >= end {
        None
    } else {
        Some((start - line_range.start)..(end - line_range.start))
    }
}

/// A caret that takes no horizontal space. Height matches the line so
/// `items_center` keeps it on the glyph box (not painted below the field).
pub fn caret_element(color: gpui::Rgba, height: Pixels) -> impl gpui::IntoElement {
    use gpui::prelude::*;
    div().w(px(0.)).h(height).flex_shrink_0().relative().child(
        div()
            .absolute()
            .top(px(0.))
            .left(px(0.))
            .w(px(1.5))
            .h(height)
            .bg(color),
    )
}

/// Re-export for callers that place the caret from a click.
pub fn char_width() -> f32 {
    crate::theme::metrics::char_width()
}

/// Advance width for detail multiline fields (`text_size_small`).
pub fn field_char_width() -> f32 {
    crate::theme::metrics::field_char_width()
}

/// Line height for multiline form fields (detail sidebar). Keep in sync with
/// the chrome in `text_field`.
pub fn field_line_height() -> Pixels {
    px(18. * crate::theme::metrics::zoom())
}

pub fn editor_gutter() -> Pixels {
    px(32. * crate::theme::metrics::zoom())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn input(text: &str) -> TextInput {
        TextInput::with_text(text, true)
    }

    fn key(key: &str) -> Keystroke {
        Keystroke {
            modifiers: Default::default(),
            key: key.into(),
            key_char: None,
        }
    }

    fn key_char(c: char) -> Keystroke {
        Keystroke {
            modifiers: Default::default(),
            key: c.to_string(),
            key_char: Some(c.to_string()),
        }
    }

    fn cmd(name: &str) -> Keystroke {
        let mut k = key(name);
        k.modifiers.platform = true;
        k
    }

    fn alt(name: &str) -> Keystroke {
        let mut k = key(name);
        k.modifiers.alt = true;
        k
    }

    fn shift(name: &str) -> Keystroke {
        let mut k = key(name);
        k.modifiers.shift = true;
        k
    }

    #[test]
    fn typing_lands_at_the_caret() {
        let mut field = input("ab");
        field.move_left();
        field.insert("X");
        assert_eq!(field.text(), "aXb");
        assert_eq!(field.cursor(), 2);
    }

    #[test]
    fn editing_multi_byte_text_stays_on_boundaries() {
        let mut field = input("héllo");
        field.move_home();
        field.move_right();
        field.move_right();
        assert!(field.text().is_char_boundary(field.cursor()));

        field.backspace();
        assert_eq!(field.text(), "hllo");

        let mut emoji = input("a🎉b");
        emoji.move_home();
        emoji.move_right();
        emoji.delete();
        assert_eq!(emoji.text(), "ab");
    }

    #[test]
    fn the_caret_belongs_to_the_line_it_ends() {
        let mut field = input("one\ntwo");
        field.move_home();
        let layout = field.layout();
        assert_eq!(layout.caret_line, 1);
        assert_eq!(layout.caret_column, 0);

        field.move_up();
        let layout = field.layout();
        assert_eq!(layout.caret_line, 0);
        assert_eq!(layout.lines, vec!["one", "two"]);
    }

    #[test]
    fn vertical_motion_clamps_to_a_shorter_line() {
        let mut field = input("longer\nab");
        field.move_up();
        field.move_end();
        assert_eq!(field.cursor(), 6, "end of 'longer'");

        field.move_down();
        assert_eq!(field.cursor(), field.text().len(), "clamped to end of 'ab'");
    }

    #[test]
    fn home_and_end_stay_within_the_line() {
        let mut field = input("one\ntwo");
        field.move_home();
        assert_eq!(field.cursor(), 4);
        field.move_end();
        assert_eq!(field.cursor(), 7);

        field.move_up();
        field.move_home();
        assert_eq!(field.cursor(), 0);
        field.move_end();
        assert_eq!(field.cursor(), 3);
    }

    #[test]
    fn edges_do_not_run_off_the_buffer() {
        let mut field = input("");
        field.backspace();
        field.delete();
        field.move_left();
        field.move_up();
        assert_eq!(field.cursor(), 0);
        assert_eq!(field.text(), "");
    }

    #[gpui::test]
    fn a_single_line_field_ignores_enter(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = TextInput::with_text("name", false);
            assert!(!field.handle_key(&key("enter"), cx));
            assert_eq!(field.text(), "name");
        });
    }

    #[gpui::test]
    fn typing_replaces_the_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("hello");
            field.selection = 1..4; // "ell"
            field.handle_key(&key_char('X'), cx);
            assert_eq!(field.text(), "hXo");
            assert_eq!(field.cursor(), 2);
        });
    }

    #[gpui::test]
    fn shift_motion_extends_and_arrow_collapses(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("abcd");
            field.move_to(1);
            field.handle_key(&shift("right"), cx);
            field.handle_key(&shift("right"), cx);
            assert_eq!(field.selection(), 1..3);

            field.handle_key(&key("right"), cx);
            assert!(!field.has_selection());
            assert_eq!(field.cursor(), 3);
        });
    }

    #[gpui::test]
    fn option_arrow_jumps_words_and_option_backspace_deletes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("foo bar baz");
            field.move_to(field.text().len());
            field.handle_key(&alt("left"), cx);
            assert_eq!(&field.text()[field.cursor()..], "baz");

            // From the start of "baz", Option+Backspace removes the previous
            // word ("bar") and the space after it.
            field.handle_key(&alt("backspace"), cx);
            assert_eq!(field.text(), "foo baz");
        });
    }

    #[gpui::test]
    fn undo_and_redo_restore_value_and_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("ab");
            field.move_to(1);
            field.handle_key(&key_char('X'), cx);
            assert_eq!(field.text(), "aXb");

            field.handle_key(&cmd("z"), cx);
            assert_eq!(field.text(), "ab");
            assert_eq!(field.cursor(), 1);

            field.handle_key(
                &{
                    let mut k = cmd("z");
                    k.modifiers.shift = true;
                    k
                },
                cx,
            );
            assert_eq!(field.text(), "aXb");
        });
    }

    #[gpui::test]
    fn cut_copy_paste_round_trip(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("hello");
            field.selection = 0..5;
            field.handle_key(&cmd("c"), cx);
            assert_eq!(
                cx.read_from_clipboard().and_then(|i| i.text()),
                Some("hello".into())
            );

            field.move_to(5);
            field.handle_key(&cmd("v"), cx);
            assert_eq!(field.text(), "hellohello");

            field.selection = 0..5;
            field.handle_key(&cmd("x"), cx);
            assert_eq!(field.text(), "hello");
            assert_eq!(
                cx.read_from_clipboard().and_then(|i| i.text()),
                Some("hello".into())
            );
        });
    }

    #[gpui::test]
    fn cmd_arrows_jump_line_and_document_ends(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let mut field = input("one\ntwo\nthree");
            field.move_to(5); // on "two"
            field.handle_key(&cmd("left"), cx);
            assert_eq!(field.cursor(), 4, "⌘← to start of line");
            field.handle_key(&cmd("right"), cx);
            assert_eq!(field.cursor(), 7, "⌘→ to end of line");

            field.handle_key(&cmd("up"), cx);
            assert_eq!(field.cursor(), 0, "⌘↑ to start of buffer");
            field.handle_key(&cmd("down"), cx);
            assert_eq!(field.cursor(), field.text().len(), "⌘↓ to end of buffer");
        });
    }

    #[test]
    fn double_click_selects_the_word_under_the_caret() {
        let mut field = input("foo bar baz");
        field.click_at(5, false, 2); // inside "bar"
        assert_eq!(field.selection(), 4..7);
        assert_eq!(&field.text()[field.selection()], "bar");
    }

    #[test]
    fn triple_click_selects_the_line() {
        let mut field = input("one\ntwo\nthree");
        field.click_at(5, false, 3);
        assert_eq!(&field.text()[field.selection()], "two\n");
    }

    /// Going right the caret stops *after* a word, going left *before* one.
    /// That asymmetry is the macOS convention, and it used to stop at the
    /// start of the next word in both directions.
    #[test]
    fn option_arrows_walk_words_the_way_macos_does() {
        let mut field = input("alpha beta gamma");
        field.move_to(0);

        field.move_word_right();
        assert_eq!(field.cursor(), 5, "after alpha, not before beta");
        field.move_word_right();
        assert_eq!(field.cursor(), 10, "after beta");
        field.move_word_right();
        assert_eq!(field.cursor(), 16, "after gamma");
        field.move_word_right();
        assert_eq!(field.cursor(), 16, "and stays at the end");

        field.move_word_left();
        assert_eq!(field.cursor(), 11, "before gamma");
        field.move_word_left();
        assert_eq!(field.cursor(), 6, "before beta");
    }

    /// From inside a gap it crosses the gap rather than stopping in it.
    #[test]
    fn option_right_from_whitespace_crosses_it() {
        let mut field = input("alpha   beta");
        field.move_to(6); // inside the run of spaces
        field.move_word_right();
        assert_eq!(field.cursor(), 12, "the end of beta");
    }

    /// ⌥⌦ takes the word, and leaves the space that followed it.
    #[test]
    fn option_delete_forward_takes_the_word_not_the_gap() {
        let mut field = input("alpha beta");
        field.move_to(0);
        field.delete_word_forward();
        assert_eq!(field.text(), " beta");
    }

    #[test]
    fn selection_on_line_intersects() {
        let line = 4..7; // "two" in "one\ntwo"
        assert_eq!(selection_on_line(&(0..10), &line), Some(0..3));
        assert_eq!(selection_on_line(&(5..6), &line), Some(1..2));
        assert_eq!(selection_on_line(&(0..4), &line), None);
    }

    fn typed(keystroke: &Keystroke, capslock: bool) -> Option<String> {
        with_capslock(keystroke, capslock).and_then(|fixed| fixed.key_char)
    }

    #[test]
    fn capslock_uppercases_only_when_it_is_on() {
        assert_eq!(typed(&key_char('a'), false), None);
        assert_eq!(typed(&key_char('a'), true).as_deref(), Some("A"));
    }

    /// Shift does *not* invert caps lock on macOS -- both down is still
    /// uppercase, which is where this differs from Windows. Checked against
    /// `UCKeyTranslate`, not from memory.
    #[test]
    fn shift_and_capslock_are_still_uppercase() {
        let mut shifted = key_char('A');
        shifted.modifiers.shift = true;
        // Already uppercase, so there is nothing to change.
        assert_eq!(typed(&shifted, true), None);
    }

    /// `None` means "nothing changed", which is what keeps the common
    /// keystroke from cloning.
    #[test]
    fn capslock_leaves_caseless_keys_untouched() {
        assert_eq!(typed(&key_char('7'), true), None);
        assert_eq!(typed(&key_char('-'), true), None);
        assert_eq!(typed(&key("backspace"), true), None, "no key_char at all");
    }

    /// `ß` uppercases to two characters, which no key press does. The layout
    /// decides what that key types; we do not get to invent `SS`.
    #[test]
    fn capslock_does_not_grow_a_character() {
        assert_eq!(typed(&key_char('ß'), true), None);
        assert_eq!(typed(&key_char('ö'), true).as_deref(), Some("Ö"));
    }

    /// Only the typed character moves. `key` is what a binding matches on, and
    /// rewriting it would break ⌘S with caps lock down.
    #[test]
    fn capslock_leaves_the_binding_key_alone() {
        let fixed = with_capslock(&key_char('s'), true).expect("the character changed");
        assert_eq!(fixed.key, "s");
        assert_eq!(fixed.key_char.as_deref(), Some("S"));
    }
}
