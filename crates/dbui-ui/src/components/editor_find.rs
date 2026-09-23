//! Find and replace in the SQL editor.
//!
//! A bar over the editor, opened with ⌘F on a query tab (⌥⌘F with the
//! replace field showing). The match the user is on is the editor's own
//! selection -- so Esc leaves it selected and ready to type over, and ⌘↵ runs
//! the statement it sits in -- and the bar counts where it is among the rest.
//!
//! Matching ignores ASCII case, which is what SQL itself does with keywords
//! and, on every engine here, with unquoted names: looking for `select`
//! should find `SELECT`.

use super::text_field::{text_field, InputTarget};
use super::{button, icon_button};
use crate::root::{DbUi, Focus, Status};
use crate::tabs::WorkspaceTab;
use crate::text_input::TextInput;
use crate::theme::metrics;
use gpui::{div, prelude::*, AnyElement, Context, Keystroke, SharedString};
use std::ops::Range;

pub(crate) struct EditorFind {
    pub query: TextInput,
    pub replacement: TextInput,
    /// Whether the replace row is showing.
    pub replacing: bool,
    /// Which of the two fields has the keyboard.
    pub field: FindField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FindField {
    Query,
    Replacement,
}

/// Every place `needle` occurs in `hay`, ignoring ASCII case, left to right
/// and without overlapping.
pub(crate) fn find_all(hay: &str, needle: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    if needle.is_empty() {
        return found;
    }
    let (hay_bytes, needle_bytes) = (hay.as_bytes(), needle.as_bytes());
    let mut at = 0;
    while at + needle_bytes.len() <= hay_bytes.len() {
        let end = at + needle_bytes.len();
        // Only ever starting and ending on a character: ASCII folding never
        // makes a multi-byte character equal to anything but itself, but a
        // window could otherwise begin halfway through one.
        if hay.is_char_boundary(at)
            && hay.is_char_boundary(end)
            && hay_bytes[at..end].eq_ignore_ascii_case(needle_bytes)
        {
            found.push(at..end);
            at = end;
        } else {
            at += 1;
        }
    }
    found
}

impl DbUi {
    fn active_editor(&self) -> Option<&TextInput> {
        match self.tabs.active() {
            Some(WorkspaceTab::Sql { editor, .. }) => Some(editor),
            _ => None,
        }
    }

    fn active_editor_mut(&mut self) -> Option<&mut TextInput> {
        match self.tabs.active_mut() {
            Some(WorkspaceTab::Sql { editor, .. }) => Some(editor),
            _ => None,
        }
    }

    /// Where the query occurs in the front editor.
    pub(crate) fn find_matches(&self) -> Vec<Range<usize>> {
        let (Some(find), Some(editor)) = (self.editor_find.as_ref(), self.active_editor()) else {
            return Vec::new();
        };
        find_all(editor.text(), find.query.text())
    }

    /// ⌘F / ⌥⌘F on a query tab.
    ///
    /// Starts from the selection when there is a one-line one: selecting a
    /// word and pressing ⌘F is how you ask where else it is.
    pub(crate) fn open_editor_find(&mut self, replacing: bool, cx: &mut Context<Self>) {
        let selected = self
            .active_editor()
            .and_then(|editor| editor.selected_text())
            .filter(|text| !text.contains('\n'))
            .map(str::to_string);
        let find = self.editor_find.get_or_insert_with(|| EditorFind {
            query: TextInput::new(false),
            replacement: TextInput::new(false),
            replacing,
            field: FindField::Query,
        });
        find.replacing |= replacing;
        find.field = FindField::Query;
        if let Some(text) = selected {
            find.query = TextInput::with_text(text, false);
        }
        find.query.select_all();
        self.focus = Focus::Find;
        cx.notify();
    }

    /// Esc: back to the editor, with the match it was on still selected.
    pub(crate) fn close_editor_find(&mut self, cx: &mut Context<Self>) {
        self.editor_find = None;
        self.focus = Focus::Editor;
        cx.notify();
    }

    /// Select one match in the editor and bring it into view.
    fn select_match(&mut self, range: Range<usize>) {
        if let Some(editor) = self.active_editor_mut() {
            editor.move_to(range.start);
            editor.select_to(range.end);
            editor.ensure_editor_caret_visible();
        }
    }

    /// Enter / ⌘G: the next match after the selection, wrapping at the end.
    /// ⇧Enter / ⇧⌘G the one before.
    pub(crate) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let matches = self.find_matches();
        let Some(selection) = self.active_editor().map(|editor| editor.selection()) else {
            return;
        };
        let next = if forward {
            matches
                .iter()
                .find(|found| found.start >= selection.end && *found != &selection)
                .or_else(|| matches.first())
        } else {
            matches
                .iter()
                .rev()
                .find(|found| found.end <= selection.start)
                .or_else(|| matches.last())
        };
        if let Some(range) = next.cloned() {
            self.select_match(range);
        }
        cx.notify();
    }

    /// The query changed: land on the first match at or after where the
    /// selection starts, so typing more of a word refines the match in place
    /// instead of hopping to the next one.
    fn find_refresh(&mut self) {
        let matches = self.find_matches();
        let Some(from) = self.active_editor().map(|editor| editor.selection().start) else {
            return;
        };
        if let Some(range) = matches
            .iter()
            .find(|found| found.start >= from)
            .or_else(|| matches.first())
            .cloned()
        {
            self.select_match(range);
        }
    }

    /// Replace the match that is selected, then move to the next. With none
    /// selected, the first press only finds one: replacing text the user has
    /// not seen highlighted is replacing blind.
    pub(crate) fn replace_one(&mut self, cx: &mut Context<Self>) {
        let Some(replacement) = self
            .editor_find
            .as_ref()
            .map(|find| find.replacement.text().to_string())
        else {
            return;
        };
        let matches = self.find_matches();
        let on_match = self
            .active_editor()
            .map(|editor| editor.selection())
            .filter(|selection| matches.contains(selection));
        if let Some(range) = on_match {
            if let Some(editor) = self.active_editor_mut() {
                editor.replace_range(range, &replacement);
            }
        }
        self.find_step(true, cx);
    }

    /// Replace every match, as one undoable edit.
    pub(crate) fn replace_all(&mut self, cx: &mut Context<Self>) {
        let Some(replacement) = self
            .editor_find
            .as_ref()
            .map(|find| find.replacement.text().to_string())
        else {
            return;
        };
        let matches = self.find_matches();
        if matches.is_empty() {
            return;
        }
        let Some(editor) = self.active_editor_mut() else {
            return;
        };
        let mut text = editor.text().to_string();
        for range in matches.iter().rev() {
            text.replace_range(range.clone(), &replacement);
        }
        let caret = editor.cursor().min(text.len());
        let end = editor.text().len();
        editor.replace_range(0..end, &text);
        editor.move_to(caret);
        let count = matches.len();
        self.status = Status::info(if count == 1 {
            "Replaced 1 match".to_string()
        } else {
            format!("Replaced {count} matches")
        });
        cx.notify();
    }

    /// Keys while the bar has the keyboard. Anything it does not want falls
    /// through to the app's own shortcuts.
    pub(crate) fn handle_find_key(
        &mut self,
        keystroke: &Keystroke,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = keystroke.key.as_str();
        let command = keystroke.modifiers.platform;
        let shift = keystroke.modifiers.shift;
        let Some(field) = self.editor_find.as_ref().map(|find| find.field) else {
            return false;
        };

        match key {
            "escape" => {
                self.close_editor_find(cx);
                return true;
            }
            "enter" if field == FindField::Replacement && !command => {
                self.replace_one(cx);
                return true;
            }
            "enter" if !command => {
                self.find_step(!shift, cx);
                return true;
            }
            "g" if command => {
                self.find_step(!shift, cx);
                return true;
            }
            "tab" => {
                if let Some(find) = self.editor_find.as_mut() {
                    if find.replacing {
                        find.field = match find.field {
                            FindField::Query => FindField::Replacement,
                            FindField::Replacement => FindField::Query,
                        };
                    }
                }
                cx.notify();
                return true;
            }
            _ => {}
        }

        let Some(find) = self.editor_find.as_mut() else {
            return false;
        };
        let input = match field {
            FindField::Query => &mut find.query,
            FindField::Replacement => &mut find.replacement,
        };
        let before = input.text().to_string();
        if !input.handle_key(keystroke, cx) {
            return false;
        }
        if field == FindField::Query && input.text() != before {
            self.find_refresh();
        }
        cx.notify();
        true
    }

    /// The bar, when it is open and the front tab is a query.
    pub(crate) fn render_editor_find(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.editor_find.as_ref()?;
        self.active_editor()?;
        let theme = &self.theme;
        let matches = self.find_matches();
        let selection = self.active_editor().map(|editor| editor.selection());
        let at =
            selection.and_then(|selection| matches.iter().position(|found| *found == selection));
        let count: SharedString = match (find.query.text().is_empty(), matches.len(), at) {
            (true, _, _) => "".into(),
            (false, 0, _) => "No matches".into(),
            (false, total, Some(index)) => format!("{} of {total}", index + 1).into(),
            (false, total, None) => format!("{total} found").into(),
        };
        let focused = self.focus == Focus::Find;
        let query_focused = focused && find.field == FindField::Query;
        let replace_focused = focused && find.field == FindField::Replacement;
        let replacing = find.replacing;

        let field =
            |id: &'static str, input: &TextInput, target, focused, hint, cx: &mut Context<Self>| {
                div()
                    .w(metrics::scaled(220.))
                    .flex_shrink_0()
                    .child(text_field(
                        id,
                        input,
                        target,
                        focused,
                        Some(hint),
                        theme,
                        cx,
                    ))
            };

        let query_row = div()
            .flex()
            .items_center()
            .gap_2()
            .child(field(
                "find-query",
                &find.query,
                InputTarget::FindQuery,
                query_focused,
                "Find",
                cx,
            ))
            .child(
                div()
                    .min_w(metrics::scaled(72.))
                    .text_size(metrics::text_size_small())
                    .text_color(if matches.is_empty() && !find.query.text().is_empty() {
                        theme.danger
                    } else {
                        theme.text_muted
                    })
                    .child(count),
            )
            .child(
                icon_button("find-prev", "↑", theme, false)
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
            )
            .child(
                icon_button("find-next", "↓", theme, false)
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
            )
            .child(
                icon_button("find-toggle-replace", "⇄", theme, replacing).on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(find) = this.editor_find.as_mut() {
                            find.replacing = !find.replacing;
                            find.field = FindField::Query;
                        }
                        cx.notify();
                    },
                )),
            )
            .child(div().flex_1())
            .child(
                icon_button("find-close", "×", theme, false)
                    .on_click(cx.listener(|this, _, _, cx| this.close_editor_find(cx))),
            );

        let replace_row = replacing.then(|| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(field(
                    "find-replacement",
                    &find.replacement,
                    InputTarget::FindReplacement,
                    replace_focused,
                    "Replace",
                    cx,
                ))
                .child(
                    button("replace-one", "Replace", theme, false)
                        .on_click(cx.listener(|this, _, _, cx| this.replace_one(cx))),
                )
                .child(
                    button("replace-all", "All", theme, false)
                        .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx))),
                )
        });

        Some(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .mx_3()
                .mb_1()
                .p_1p5()
                .rounded_md()
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.border)
                .text_size(metrics::text_size_small())
                .child(query_row)
                .children(replace_row)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::find_all;

    #[test]
    fn finds_every_occurrence_ignoring_case() {
        assert_eq!(
            find_all("SELECT a FROM t WHERE a = 'select'", "select"),
            vec![0..6, 27..33]
        );
    }

    #[test]
    fn matches_do_not_overlap() {
        assert_eq!(find_all("aaaa", "aa"), vec![0..2, 2..4]);
    }

    #[test]
    fn nothing_to_find_finds_nothing() {
        assert!(find_all("SELECT", "").is_empty());
        assert!(find_all("", "x").is_empty());
    }

    #[test]
    fn multibyte_text_is_searched_on_character_boundaries() {
        assert_eq!(find_all("café café", "é"), vec![3..5, 9..11]);
        assert_eq!(find_all("naïve NAÏVE", "na"), vec![0..2, 7..9]);
    }
}
