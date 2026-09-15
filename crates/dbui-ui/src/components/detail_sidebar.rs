//! Right-hand detail panel for the selected row.

use super::icons::calendar_icon;
use super::text_field::{
    sized_text_field, text_field, DetailInput, FieldHeight, InputTarget, MAX_VISIBLE_LINES,
};
use super::{caption, icon_button, menu_row, menu_surface, type_badge};
use crate::json_format::{self, JsonStyle};
use crate::root::DbUi;
use crate::row_export::RowFormat;
use crate::tabs::WorkspaceTab;
use crate::theme::{metrics, Theme};
use dbui_app::domain::{ColumnInfo, Value};
use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, SharedString,
};
use std::collections::HashSet;

const STRIP_WIDTH_BASE: f32 = 24.;

impl DbUi {
    /// The strip between the grid and the detail panel.
    ///
    /// Nothing when the panel is collapsed: the 24px rail it leaves behind is
    /// a button, not a panel, and has no width to drag.
    pub(crate) fn render_detail_resize(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        if !self.detail_open {
            return None;
        }
        Some(
            super::vertical_resize_handle("detail-resize", self.detail_drag.is_some(), &self.theme)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                        this.begin_detail_drag(event.position.x, cx);
                    }),
                ),
        )
    }

    pub(crate) fn render_detail_sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;

        if !self.detail_open {
            return div()
                .id("detail-strip")
                .w(px(STRIP_WIDTH_BASE * metrics::zoom()))
                .flex_shrink_0()
                .flex()
                .flex_col()
                .bg(theme.panel)
                .border_l_1()
                .border_color(theme.border)
                .child(
                    div()
                        .id("detail-strip-toggle")
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_color(theme.text_faint)
                        .hover(|strip| strip.text_color(theme.text_muted))
                        .on_click(cx.listener(|this, _, _window, cx| this.toggle_detail(cx)))
                        .child("◂"),
                );
        }

        let open_menu = self.detail_value_menu;
        let menu_open = self.detail_menu_open;
        let detail_menu = self.render_detail_menu(cx);
        let columns: &[ColumnInfo] = self
            .tabs
            .active()
            .and_then(|tab| tab.result())
            .map(|view| view.set.columns.as_slice())
            .unwrap_or(&[]);
        let body = match self.tabs.active() {
            Some(WorkspaceTab::Table {
                draft,
                selected_row,
                result,
                ..
            })
            | Some(WorkspaceTab::Sql {
                draft,
                selected_row,
                result,
                ..
            }) => {
                // A staged insert takes the sidebar over: it has no stored
                // row behind it, so the ordinary draft path has nothing to
                // reconcile against.
                if let Some(insert) = self.tabs.active().and_then(|tab| {
                    tab.editing_insert()
                        .and_then(|index| tab.pending_inserts().get(index))
                }) {
                    vec![render_insert_draft(
                        insert,
                        columns,
                        self.detail_input,
                        &self.detail_collapsed,
                        theme,
                        cx,
                    )]
                } else if let Some(draft) = draft.as_ref() {
                    // The lead row only decides which write tokens are on
                    // offer; the values on screen come from the draft, which
                    // already reconciled every selected row.
                    let originals = result
                        .as_ref()
                        .zip(draft.rows.first())
                        .and_then(|(view, lead)| view.set.rows.get(*lead))
                        .map(|row| row.0.as_slice())
                        .unwrap_or(&[]);
                    render_table_draft(
                        draft,
                        originals,
                        columns,
                        open_menu,
                        self.detail_input,
                        self.copied_field,
                        &self.detail_collapsed,
                        theme,
                        cx,
                    )
                } else if selected_row.is_some() {
                    vec![caption("Loading row…", theme).into_any_element()]
                } else {
                    vec![empty_selection(theme)]
                }
            }
            None => vec![empty_selection(theme)],
        };

        div()
            .id("detail-sidebar")
            .w(px(self.detail_width * metrics::zoom()))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.panel)
            .border_l_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .h(metrics::toolbar_height())
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme.divider)
                    .child(div().text_size(metrics::scaled(13.)).child("Row details"))
                    .child(
                        div()
                            .relative()
                            .flex_shrink_0()
                            .child(
                                icon_button("detail-menu-btn", "⋮", theme, menu_open)
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            this.toggle_detail_menu(cx);
                                        }),
                                    ),
                            )
                            .children(detail_menu),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .min_w(px(0.))
                    .w_full()
                    .child(
                        div()
                            .id("detail-body")
                            .size_full()
                            .min_h(px(0.))
                            .min_w(px(0.))
                            .track_scroll(&self.detail_scroll)
                            .overflow_x_hidden()
                            .overflow_y_scroll()
                            .map(|mut el| {
                                // Horizontal trackpad over fields must not remap onto
                                // this vertical sidebar scroll.
                                el.style().restrict_scroll_to_axis = Some(true);
                                el
                            })
                            .p_3()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children(body),
                    )
                    .child(super::scrollbar::vertical_scrollbar(
                        "detail-scrollbar",
                        self.detail_scroll.clone(),
                        &self.theme,
                    )),
            )
    }
}

impl DbUi {
    /// The panel's `⋮` menu.
    ///
    /// What it holds is what the panel is for and had no home: taking the row
    /// somewhere else, and putting the panel away. Both were previously only
    /// reachable from the grid's right-click menu or a 24px rail.
    fn render_detail_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.detail_menu_open {
            return None;
        }
        let theme = &self.theme;
        Some(
            deferred(
                menu_surface("detail-menu", theme)
                    .top_full()
                    .right_0()
                    .mt_1()
                    .w(metrics::scaled(210.))
                    .text_size(metrics::text_size_small())
                    .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_detail_menu(cx)))
                    .child(
                        menu_row("detail-menu-json", "Copy Row as JSON", None, theme).on_click(
                            cx.listener(|this, _, _window, cx| {
                                this.close_detail_menu(cx);
                                this.copy_selected_rows(RowFormat::Json, cx);
                            }),
                        ),
                    )
                    .child(
                        menu_row("detail-menu-insert", "Copy Row as INSERT", None, theme).on_click(
                            cx.listener(|this, _, _window, cx| {
                                this.close_detail_menu(cx);
                                this.copy_selected_rows(RowFormat::Insert, cx);
                            }),
                        ),
                    )
                    .child(
                        div()
                            .my_1()
                            .h(px(1.))
                            .w_full()
                            .flex_shrink_0()
                            .bg(theme.divider),
                    )
                    .child(
                        menu_row("detail-menu-hide", "Hide Panel", None, theme).on_click(
                            cx.listener(|this, _, _window, cx| {
                                this.close_detail_menu(cx);
                                this.toggle_detail(cx);
                            }),
                        ),
                    ),
            )
            .into_any_element(),
        )
    }
}

/// Whether the engine's name for a column is a date or a time.
///
/// Read off the type rather than the value: a `timestamptz` that happens to be
/// NULL in this row is still a timestamp column, and the field has to offer
/// the same thing either way.
fn is_temporal_type(type_name: &str) -> bool {
    let lower = type_name.to_ascii_lowercase();
    ["timestamp", "datetime", "date", "time"]
        .iter()
        .any(|name| lower.contains(name))
}

fn empty_selection(theme: &Theme) -> AnyElement {
    div()
        .py_6()
        .flex()
        .items_center()
        .justify_center()
        .child(caption("No row selected.", theme))
        .into_any_element()
}

#[allow(clippy::too_many_arguments)]
fn render_table_draft(
    draft: &crate::tabs::RowDraft,
    originals: &[Value],
    columns: &[ColumnInfo],
    open_menu: Option<usize>,
    detail_input: Option<DetailInput>,
    copied: Option<usize>,
    collapsed: &HashSet<String>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> Vec<AnyElement> {
    let search = draft.field_search.text().to_ascii_lowercase();
    let search_focused = detail_input == Some(DetailInput::Search);
    let bulk = draft.is_bulk();

    let fields: Vec<AnyElement> = draft
        .fields
        .iter()
        .enumerate()
        .filter(|(_, (name, _, _))| {
            search.is_empty() || name.to_ascii_lowercase().contains(&search)
        })
        .map(|(index, (name, input, is_pk))| {
            let focused = detail_input == Some(DetailInput::Field(index));
            let just_copied = copied == Some(index);
            let original = originals.get(index);
            let allow_empty = original.map(allows_empty_token).unwrap_or(true);
            let height = field_height(name, collapsed);
            let type_name = columns.get(index).map(|column| column.type_name.as_str());
            let menu = TokenMenu {
                open: open_menu == Some(index),
                allow_empty,
                bulk,
                temporal: type_name.is_some_and(is_temporal_type),
            };
            div()
                .id(("detail-field", index))
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(field_header(
                    FieldHeader {
                        index,
                        name,
                        is_pk: *is_pk,
                        type_name,
                        fold: foldable(input, *is_pk).then_some(height),
                        just_copied,
                    },
                    theme,
                    cx,
                ))
                .child(if *is_pk {
                    read_only_field(index, input.text(), true, height, just_copied, theme, cx)
                        .into_any_element()
                } else {
                    value_field(
                        sized_text_field(
                            ("detail-field-input", index),
                            input,
                            InputTarget::DetailField(index),
                            focused,
                            None,
                            height,
                            theme,
                            cx,
                        ),
                        index,
                        &menu,
                        theme,
                        cx,
                    )
                })
                .into_any_element()
        })
        .collect();

    let message = draft.message.as_ref().map(|(ok, text)| {
        div()
            .text_size(metrics::scaled(11.))
            .text_color(if *ok { theme.success } else { theme.danger })
            .child(SharedString::from(text.clone()))
    });

    // Flat, because the scroll handle addresses the body's own children by
    // position: the banner, the search box, then one per visible field. Wrap
    // them in a container and every field shares its index.
    let mut body: Vec<AnyElement> = Vec::with_capacity(fields.len() + 3);
    body.extend(bulk.then(|| bulk_banner(draft.rows.len(), theme).into_any_element()));
    body.push(
        text_field(
            "detail-field-search",
            &draft.field_search,
            InputTarget::DetailSearch,
            search_focused,
            Some("Search for field…"),
            theme,
            cx,
        )
        .into_any_element(),
    );
    body.extend(fields);
    body.extend(message.map(IntoElement::into_any_element));
    body
}

/// The editors for a row that is not on the server yet.
///
/// Every field starts reading `DEFAULT`, which is not a value but an absence:
/// a column left saying it is left out of the INSERT entirely, so the table's
/// own default, sequence or generated value is what lands.
fn render_insert_draft(
    insert: &crate::tabs::PendingRowInsert,
    columns: &[ColumnInfo],
    detail_input: Option<DetailInput>,
    collapsed: &HashSet<String>,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let fields: Vec<AnyElement> = insert
        .fields
        .iter()
        .enumerate()
        .map(|(index, (name, input, _))| {
            let height = field_height(name, collapsed);
            div()
                .id(("insert-field", index))
                .w_full()
                .min_w(px(0.))
                .flex()
                .flex_col()
                .gap_1()
                .child(field_header(
                    FieldHeader {
                        index,
                        name,
                        is_pk: false,
                        type_name: columns.get(index).map(|column| column.type_name.as_str()),
                        fold: foldable(input, false).then_some(height),
                        // A staged insert has no key to copy: it has no
                        // identity until the server gives it one.
                        just_copied: false,
                    },
                    theme,
                    cx,
                ))
                .child(sized_text_field(
                    ("insert-field-input", index),
                    input,
                    InputTarget::InsertField(index),
                    detail_input == Some(DetailInput::Field(index)),
                    None,
                    height,
                    theme,
                    cx,
                ))
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .min_w(px(0.))
        .child(
            div()
                .w_full()
                .px_2()
                .py_1p5()
                .rounded_md()
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.success)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_size(metrics::text_size_small())
                        .text_color(theme.success)
                        .child("New row"),
                )
                .child(
                    div()
                        .text_size(metrics::scaled(11.))
                        .text_color(theme.text_muted)
                        .child(
                            "Nothing is written until you commit. A field left \
                             reading DEFAULT is left out, so the column's own \
                             default fires.",
                        ),
                ),
        )
        .children(fields)
        .into_any_element()
}

/// Says what editing a selection is about to do.
///
/// The fields alone do not: `MIXED` looks like a value until you are told it
/// means "left alone", and a box showing one shared value gives no hint that
/// typing in it rewrites forty rows.
fn bulk_banner(rows: usize, theme: &Theme) -> AnyElement {
    div()
        .w_full()
        .min_w(px(0.))
        .px_2()
        .py_1p5()
        .rounded_md()
        .bg(theme.elevated)
        .border_1()
        .border_color(theme.accent)
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_size(metrics::text_size_small())
                .text_color(theme.text)
                .child(SharedString::from(format!("Editing {rows} rows"))),
        )
        .child(
            div()
                .text_size(metrics::scaled(11.))
                .text_color(theme.text_muted)
                .child(
                    "A field you change is written to all of them. MIXED means \
                     they differ — leave it to keep each row's own value.",
                ),
        )
        .into_any_element()
}

/// What the write-token dropdown should offer for one field.
struct TokenMenu {
    open: bool,
    allow_empty: bool,
    /// A bulk edit gets a `MIXED` entry -- the way back out of having typed
    /// over a field you meant to leave alone.
    bulk: bool,
    /// A date or timestamp column also gets `now` and `today`, and wears a
    /// calendar instead of a chevron.
    temporal: bool,
}

/// The line above one field: its name, and whatever controls it earns.
struct FieldHeader<'a> {
    index: usize,
    name: &'a str,
    is_pk: bool,
    /// The engine's name for the column's type, drawn opposite the name.
    type_name: Option<&'a str>,
    /// The height toggle and the height it is currently showing, for a field
    /// with more lines than a folded box would hold.
    fold: Option<FieldHeight>,
    /// This field's value was the last one copied.
    just_copied: bool,
}

/// An editable field, plus the button that opens what can be written to it.
///
/// The button lives *inside* the box rather than up in the header, which is
/// where it used to be. That is what makes a column with a short list of legal
/// values read as a control you pick from instead of a free-text box with a
/// caret in it -- and it puts the menu directly under the value it rewrites.
fn value_field(
    field: AnyElement,
    index: usize,
    menu: &TokenMenu,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let glyph: AnyElement = if menu.temporal {
        calendar_icon(if menu.open {
            theme.text
        } else {
            theme.text_faint
        })
        .into_any_element()
    } else {
        div()
            .text_size(metrics::text_size_small())
            .child("⌄")
            .into_any_element()
    };

    div()
        .relative()
        .w_full()
        .min_w(px(0.))
        .child(field)
        .child(
            div()
                .id(("detail-value-menu-btn", index))
                .absolute()
                .top_0()
                .right_0()
                .h(metrics::scaled(28.))
                .w(metrics::scaled(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_r_md()
                .cursor_pointer()
                .text_color(if menu.open {
                    theme.text
                } else {
                    theme.text_faint
                })
                .hover(|btn| btn.text_color(theme.text))
                // Down rather than click, and claimed here: the press must not
                // also reach the editor behind it and drop a caret into the
                // value the menu is about to replace.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_detail_value_menu(index, cx);
                    }),
                )
                .child(glyph),
        )
        .children(
            menu.open
                .then(|| special_value_menu(index, menu, theme, cx)),
        )
        .into_any_element()
}

fn field_header(header: FieldHeader<'_>, theme: &Theme, cx: &mut Context<DbUi>) -> AnyElement {
    let FieldHeader {
        index,
        name,
        is_pk,
        type_name,
        fold,
        just_copied,
    } = header;
    let label_color = if is_pk {
        theme.warning
    } else {
        theme.text_muted
    };

    let mut row = div()
        .relative()
        .flex()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .truncate()
                .text_color(label_color)
                .text_size(metrics::text_size_small())
                .child(SharedString::from(name.to_string())),
        );

    if let Some(height) = fold {
        let open = height == FieldHeight::Full;
        let field = name.to_string();
        row = row.child(
            div()
                .id(("detail-field-fold", index))
                .px_1()
                .rounded_sm()
                .text_size(metrics::text_size_small())
                .text_color(if open { theme.text } else { theme.text_faint })
                .cursor_pointer()
                .hover(|btn| btn.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.toggle_detail_collapsed(&field, cx);
                }))
                .child(if open { "⤡" } else { "⤢" }),
        );
    }

    // The key has no value menu -- there is nothing to set it to -- so its
    // slot in the header carries the one thing it does offer instead.
    if is_pk {
        row = row.child(
            div()
                .id(("detail-key-copy", index))
                .px_1()
                .rounded_sm()
                .text_size(metrics::text_size_small())
                .text_color(if just_copied {
                    theme.success
                } else {
                    theme.text_faint
                })
                .cursor_pointer()
                .hover(|btn| btn.bg(theme.hover).text_color(theme.text))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.copy_detail_field(index, cx);
                }))
                // The button answers where it was pressed. A sidebar scrolled
                // well past the status bar has nowhere else to say it landed.
                .child(if just_copied { "✓ Copied" } else { "⧉" }),
        );
    }

    // What the value *is*, opposite what it is called. A row of editors with
    // no types on it is a row of boxes: `char(3)` and `text` take the same
    // keystrokes right up until the server refuses one of them.
    if let Some(type_name) = type_name {
        row = row.child(type_badge(type_name.to_lowercase(), theme));
    }

    row.into_any_element()
}

/// How tall to draw one field: all of it, unless it has been folded down.
fn field_height(name: &str, collapsed: &HashSet<String>) -> FieldHeight {
    if collapsed.contains(name) {
        FieldHeight::Capped
    } else {
        FieldHeight::Full
    }
}

/// Whether a field is long enough for folding to change anything.
///
/// A value that fits inside the cap gets no toggle: a control that does
/// nothing is worse than no control, and most columns in a row are one line.
///
/// A read-only field is measured after [`json_format::display_text`], which is
/// what it actually paints -- a one-line JSON blob that pretty-prints to thirty
/// is thirty lines on screen, whatever its buffer says.
fn foldable(input: &crate::text_input::TextInput, read_only: bool) -> bool {
    let lines = if read_only {
        json_format::display_text(input.text()).split('\n').count()
    } else {
        input.layout().lines.len()
    };
    lines > MAX_VISIBLE_LINES
}

fn special_value_menu(
    index: usize,
    menu: &TokenMenu,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let mut rows: Vec<AnyElement> = Vec::new();
    let items: &[(&'static str, &str, bool)] = &[
        ("NULL", "SQL NULL", true),
        ("EMPTY", "Empty string", menu.allow_empty),
        ("DEFAULT", "Column default", true),
        // The way back out of a bulk edit: having typed over a field, this is
        // how you say "never mind, leave each row as it was".
        (crate::tabs::MIXED, "Leave each row's own", menu.bulk),
    ];
    // A date column gets the two values anyone actually types into one. They
    // are values rather than write tokens, so they go in above the tokens with
    // a rule between -- "set it to this" and "there is no value here" are not
    // the same kind of answer.
    if menu.temporal {
        for (item_index, &(label, date_only)) in
            [("now", false), ("today", true)].iter().enumerate()
        {
            rows.push(
                div()
                    .id(("detail-value-menu-time", index * 8 + item_index))
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .hover(|row| row.bg(theme.hover))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.set_detail_field_text(index, crate::clock::now_utc(date_only), cx);
                    }))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .font_family(metrics::MONO_FONT)
                                    .text_color(theme.text)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .text_size(metrics::text_size_small())
                                    .text_color(theme.text_faint)
                                    .child(if date_only {
                                        "Today's date (UTC)"
                                    } else {
                                        "Current timestamp (UTC)"
                                    }),
                            ),
                    )
                    .into_any_element(),
            );
        }
        rows.push(
            div()
                .my_1()
                .h(px(1.))
                .w_full()
                .flex_shrink_0()
                .bg(theme.divider)
                .into_any_element(),
        );
    }

    for (item_index, &(token, hint, enabled)) in items.iter().enumerate() {
        if !enabled {
            continue;
        }
        rows.push(
            div()
                .id(("detail-value-menu-item", index * 8 + item_index))
                .px_3()
                .py_1()
                .cursor_pointer()
                .hover(|row| row.bg(theme.hover))
                .on_click(cx.listener(move |this, _, _window, cx| {
                    this.set_detail_special_value(index, token, cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .font_family(metrics::MONO_FONT)
                                .text_color(theme.text)
                                .child(token),
                        )
                        .child(
                            div()
                                .text_size(metrics::text_size_small())
                                .text_color(theme.text_faint)
                                .child(hint),
                        ),
                )
                .into_any_element(),
        );
    }

    deferred(
        menu_surface(("detail-value-menu", index), theme)
            .top_full()
            .right_0()
            .mt_1()
            .min_w(metrics::scaled(200.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.close_detail_value_menu(cx);
            }))
            .children(rows),
    )
    .into_any_element()
}

fn allows_empty_token(value: &Value) -> bool {
    matches!(
        value,
        Value::Text(_)
            | Value::Json(_)
            | Value::Uuid(_)
            | Value::Temporal(_)
            | Value::Decimal(_)
            | Value::Null
            | Value::Default
    )
}

/// A value drawn rather than edited -- today, the primary key.
///
/// It is still clickable, and clicking copies it. A read-only box takes no
/// caret, so selecting the text by hand is not on offer; without this the
/// key would be the one value on the row that cannot be taken anywhere.
fn read_only_field(
    index: usize,
    text: &str,
    muted: bool,
    height: FieldHeight,
    just_copied: bool,
    theme: &Theme,
    cx: &mut Context<DbUi>,
) -> AnyElement {
    let display = json_format::display_text(text);
    let color = if muted { theme.text_faint } else { theme.text };

    if !display.contains('\n') && !display.contains('\r') {
        return div()
            .id(("detail-readonly", index))
            .cursor_pointer()
            .hover(|field| field.border_color(theme.accent))
            .on_click(cx.listener(move |this, _, _window, cx| {
                this.copy_detail_field(index, cx);
            }))
            .w_full()
            .min_w(px(0.))
            .flex()
            .items_center()
            .h(metrics::scaled(28.))
            .px_2()
            .rounded_md()
            .bg(theme.background)
            .border_1()
            .border_color(if just_copied {
                theme.success
            } else {
                theme.border
            })
            .font_family(metrics::MONO_FONT)
            .overflow_hidden()
            .text_color(color)
            .child(
                div()
                    .w_full()
                    .min_w(px(0.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(SharedString::from(display)),
            )
            .into_any_element();
    }

    let spans = json_format::highlight_spans(&display);
    let lines: Vec<&str> = display.split('\n').collect();
    let visible = match height {
        FieldHeight::Capped => lines.len().min(MAX_VISIBLE_LINES),
        FieldHeight::Full => lines.len(),
    }
    .max(1);
    let line_h = px(18.);
    // Lines + `py_1` + `border_1`; without the border the last line was clipped.
    let box_h = px(18. * visible as f32 + 8. + 2.);

    let mut consumed = 0usize;
    let painted: Vec<AnyElement> = lines
        .into_iter()
        .take(visible)
        .map(|line| {
            let line_start = consumed;
            let line_end = consumed + line.len();
            let line_range = line_start..line_end;
            consumed = line_end + 1;

            let line_styles = spans
                .as_ref()
                .map(|all| json_format::styles_on_line(all, &line_range))
                .unwrap_or_default();

            div()
                .h(line_h)
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .children(read_only_line_chunks(
                    line,
                    &line_styles,
                    if muted { theme.text_faint } else { theme.text },
                    theme,
                ))
                .into_any_element()
        })
        .collect();

    div()
        .id(("detail-readonly", index))
        .cursor_pointer()
        .hover(|field| field.border_color(theme.accent))
        .on_click(cx.listener(move |this, _, _window, cx| {
            this.copy_detail_field(index, cx);
        }))
        .w_full()
        .min_w(px(0.))
        .h(box_h)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(theme.background)
        .border_1()
        .border_color(if just_copied {
            theme.success
        } else {
            theme.border
        })
        .font_family(metrics::MONO_FONT)
        .text_size(metrics::text_size_small())
        .overflow_hidden()
        .child(
            div()
                .w_full()
                .h_full()
                .min_w(px(0.))
                .overflow_hidden()
                .flex()
                .flex_col()
                .children(painted),
        )
        .into_any_element()
}

fn read_only_line_chunks(
    line: &str,
    styles: &[(usize, usize, JsonStyle)],
    fallback: gpui::Rgba,
    theme: &Theme,
) -> Vec<AnyElement> {
    if line.is_empty() {
        return vec![div().child(SharedString::from(" ")).into_any_element()];
    }
    if styles.is_empty() {
        return vec![div()
            .text_color(fallback)
            .child(SharedString::from(line.to_string()))
            .into_any_element()];
    }

    let mut cuts = vec![0usize, line.len()];
    for &(start, end, _) in styles {
        cuts.push(start.min(line.len()));
        cuts.push(end.min(line.len()));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut out = Vec::new();
    for window in cuts.windows(2) {
        let start = window[0];
        let end = window[1];
        if start >= end {
            continue;
        }
        let color = styles
            .iter()
            .find(|&&(s, e, _)| start >= s && start < e)
            .map(|&(_, _, style)| style.color(theme))
            .unwrap_or(fallback);
        out.push(
            div()
                .text_color(color)
                .child(SharedString::from(line[start..end].to_string()))
                .into_any_element(),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The calendar button and the `now` / `today` entries behind it are
    /// offered on a type, so a type that is not a date must not get them --
    /// and every type that is one must.
    #[test]
    fn a_date_or_time_column_is_temporal_and_nothing_else_is() {
        for name in [
            "timestamptz",
            "timestamp without time zone",
            "TIMESTAMP",
            "date",
            "DATETIME",
            "time",
            "timetz",
        ] {
            assert!(is_temporal_type(name), "{name} is temporal");
        }
        // `int` and `interval` both have to stay out: one is not a time at
        // all, the other is a duration and has no "now".
        for name in [
            "int4", "integer", "text", "numeric", "char(3)", "jsonb", "bool", "uuid",
        ] {
            assert!(!is_temporal_type(name), "{name} is not temporal");
        }
    }

    /// The default is the whole value. Folding is the exception, and it is the
    /// user's to ask for -- nothing puts a column in the set on its own.
    #[test]
    fn a_field_is_full_height_until_it_is_folded() {
        let mut collapsed = HashSet::new();
        assert_eq!(field_height("feature_flags", &collapsed), FieldHeight::Full);

        collapsed.insert("feature_flags".to_string());
        assert_eq!(
            field_height("feature_flags", &collapsed),
            FieldHeight::Capped
        );
        assert_eq!(
            field_height("grupo_id", &collapsed),
            FieldHeight::Full,
            "folding one column says nothing about the next"
        );
    }
}
