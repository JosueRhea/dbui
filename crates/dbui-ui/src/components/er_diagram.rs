//! An entity-relationship diagram of one schema: every table with its
//! columns, and a line for every foreign key, from the referencing column to
//! the column it references.
//!
//! Laid out by `er_layout` -- parents left of children -- and drawn at a
//! zoom of its own, so a large schema can be taken in at a glance and a small
//! one read comfortably. Hovering a table lights up its keys, both ways;
//! clicking one opens it.
//!
//! Only single-column foreign keys are drawn, because only those are read
//! (see Known limits): a composite key has no one column to point from.

use super::{button, caption, motion};
use crate::er_layout::{self, Layout, Node};
use crate::root::{DbUi, Status};
use crate::theme::metrics;
use dbui_app::commands;
use dbui_app::domain::{Column, TableKind, TableRef};
use gpui::{
    canvas, div, point, prelude::*, px, AnyElement, Context, MouseButton, PathBuilder, Pixels,
    Point, SharedString,
};

/// Diagram units: the size of things at a diagram zoom of 1.
const HEADER: f32 = 30.;
const ROW: f32 = 20.;
const PADDING: f32 = 8.;
const CHAR: f32 = 7.6;
const MIN_WIDTH: f32 = 170.;
const MAX_WIDTH: f32 = 420.;

pub struct ErTable {
    pub table: TableRef,
    pub kind: TableKind,
    pub columns: Vec<Column>,
}

/// A foreign key: `from`'s column at `from_row` references `to`'s column at
/// `to_row` (or its header, when that column is not listed).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ErEdge {
    pub from: usize,
    pub from_row: usize,
    pub to: usize,
    pub to_row: Option<usize>,
}

pub struct ErDiagram {
    pub schema: String,
    pub tables: Vec<ErTable>,
    pub edges: Vec<ErEdge>,
    pub sizes: Vec<Node>,
    pub layout: Layout,
    pub loaded: bool,
    pub zoom: f32,
    pub hover: Option<usize>,
}

impl ErDiagram {
    fn loading(schema: String) -> Self {
        Self {
            schema,
            tables: Vec::new(),
            edges: Vec::new(),
            sizes: Vec::new(),
            layout: er_layout::layout(&[], &[]),
            loaded: false,
            zoom: 1.0,
            hover: None,
        }
    }

    /// Measure, connect and place `tables`.
    pub fn build(schema: String, tables: Vec<ErTable>) -> Self {
        let sizes: Vec<Node> = tables
            .iter()
            .map(|table| {
                let widest_row = table
                    .columns
                    .iter()
                    .map(|column| {
                        (column.name.chars().count() + column.data_type.chars().count()) as f32
                            * CHAR
                            // PK/FK marker, the gaps around it and between
                            // name and type, and the box's own padding.
                            + 72.
                    })
                    .fold(0.0f32, f32::max);
                let title = table.table.name.chars().count() as f32 * (CHAR + 0.6) + 40.;
                Node {
                    width: widest_row.max(title).clamp(MIN_WIDTH, MAX_WIDTH),
                    height: HEADER + table.columns.len().max(1) as f32 * ROW + PADDING,
                }
            })
            .collect();

        let mut edges = Vec::new();
        for (from, table) in tables.iter().enumerate() {
            for (from_row, column) in table.columns.iter().enumerate() {
                let Some(key) = &column.references else {
                    continue;
                };
                let Some(to) = tables.iter().position(|t| t.table == key.references) else {
                    continue;
                };
                let to_row = tables[to]
                    .columns
                    .iter()
                    .position(|c| c.name == key.references_column);
                edges.push(ErEdge {
                    from,
                    from_row,
                    to,
                    to_row,
                });
            }
        }
        let pairs: Vec<(usize, usize)> = edges.iter().map(|e| (e.from, e.to)).collect();
        let layout = er_layout::layout(&sizes, &pairs);
        Self {
            schema,
            tables,
            edges,
            sizes,
            layout,
            loaded: true,
            zoom: 1.0,
            hover: None,
        }
    }

    /// Where a row's line meets the box, in diagram units: the left or the
    /// right edge, at the row's middle.
    fn anchor(&self, table: usize, row: Option<usize>, right: bool) -> (f32, f32) {
        let at = self.layout.positions[table];
        let size = self.sizes[table];
        let y = match row {
            Some(row) => at.y + HEADER + row as f32 * ROW + ROW / 2.,
            None => at.y + HEADER / 2.,
        };
        (if right { at.x + size.width } else { at.x }, y)
    }

    /// Each edge as a cubic curve, in diagram units, and whether it belongs
    /// to the hovered table.
    fn curves(&self) -> Vec<([(f32, f32); 4], bool)> {
        self.edges
            .iter()
            .map(|edge| {
                let lit = self.hover.is_some_and(|h| h == edge.from || h == edge.to);
                let from_x = self.layout.positions[edge.from].x;
                let to_x = self.layout.positions[edge.to].x;
                if edge.from == edge.to {
                    // A table that references itself: a loop off its right side.
                    let a = self.anchor(edge.from, Some(edge.from_row), true);
                    let b = self.anchor(edge.to, edge.to_row, true);
                    return ([a, (a.0 + 48., a.1), (b.0 + 48., b.1), b], lit);
                }
                // The parent is usually to the left: leave from the child's
                // left edge, arrive at the parent's right.
                let parent_left = to_x < from_x;
                let a = self.anchor(edge.from, Some(edge.from_row), !parent_left);
                let b = self.anchor(edge.to, edge.to_row, parent_left);
                let pull = ((a.0 - b.0).abs() / 2.).max(40.);
                let (ca, cb) = if parent_left {
                    ((a.0 - pull, a.1), (b.0 + pull, b.1))
                } else {
                    ((a.0 + pull, a.1), (b.0 - pull, b.1))
                };
                ([a, ca, cb, b], lit)
            })
            .collect()
    }
}

impl DbUi {
    /// Open the diagram for the schema in front: the open table's, else the
    /// first one unfolded in the tree, else the first there is.
    pub(crate) fn open_er_diagram(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.workspace.active() else {
            self.status = Status::error("Not connected");
            cx.notify();
            return;
        };
        let Some(catalog) = entry.catalog.as_ref() else {
            self.status = Status::error("The catalog has not loaded yet");
            cx.notify();
            return;
        };
        let schema = self
            .tabs
            .active()
            .and_then(|tab| tab.table_ref().map(|table| table.schema.clone()))
            .or_else(|| {
                catalog
                    .schemas
                    .iter()
                    .find(|schema| entry.is_expanded(&schema.name))
                    .map(|schema| schema.name.clone())
            })
            .or_else(|| catalog.schemas.first().map(|schema| schema.name.clone()));
        match schema {
            Some(schema) => self.open_er_diagram_for(schema, cx),
            None => {
                self.status = Status::info("No schema to draw");
                cx.notify();
            }
        }
    }

    pub(crate) fn open_er_diagram_for(&mut self, schema: String, cx: &mut Context<Self>) {
        let (Some(entry), Some(driver)) = (self.workspace.active(), self.workspace.active_driver())
        else {
            return;
        };
        let Some(tables) = entry.catalog.as_ref().and_then(|catalog| {
            catalog
                .schemas
                .iter()
                .find(|s| s.name == schema)
                .map(|s| s.tables.clone())
        }) else {
            return;
        };
        let kinds: Vec<TableKind> = tables.iter().map(|table| table.kind).collect();
        let refs: Vec<TableRef> = tables.iter().map(|table| table.reference()).collect();
        self.er_diagram = Some(ErDiagram::loading(schema.clone()));
        self.close_chrome_menus();
        cx.notify();

        let task = commands::fetch_all_columns(&self.runtime, driver, refs);
        cx.spawn(async move |this, cx| {
            let Some(described) = task.await else {
                return;
            };
            this.update(cx, |this, cx| {
                // Closed, or replaced by another schema's, in the meantime.
                if this
                    .er_diagram
                    .as_ref()
                    .is_none_or(|diagram| diagram.schema != schema || diagram.loaded)
                {
                    return;
                }
                let tables = described
                    .into_iter()
                    .zip(kinds)
                    .map(|((table, columns), kind)| ErTable {
                        table,
                        kind,
                        columns,
                    })
                    .collect();
                this.er_diagram = Some(ErDiagram::build(schema, tables));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub(crate) fn close_er_diagram(&mut self, cx: &mut Context<Self>) {
        self.er_diagram = None;
        cx.notify();
    }

    pub(crate) fn zoom_er_diagram(&mut self, by: f32, cx: &mut Context<Self>) {
        if let Some(diagram) = self.er_diagram.as_mut() {
            diagram.zoom = (diagram.zoom * by).clamp(0.3, 2.0);
        }
        cx.notify();
    }

    pub(crate) fn render_er_diagram(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let diagram = self.er_diagram.as_ref()?;
        let theme = &self.theme;
        // Diagram units to pixels: the diagram's own zoom times the app's.
        let scale = diagram.zoom * metrics::zoom();
        let at = |units: f32| px(units * scale);

        let edge_color = theme.text_faint;
        let lit_color = theme.accent;
        let curves = diagram.curves();
        let edges = canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let origin = bounds.origin;
                let to_point = |(x, y): (f32, f32)| -> Point<Pixels> {
                    point(origin.x + px(x * scale), origin.y + px(y * scale))
                };
                // Unlit first, so a lit line is drawn over the ones it crosses.
                for lit_pass in [false, true] {
                    for (curve, lit) in curves.iter().filter(|(_, lit)| *lit == lit_pass) {
                        let mut path = PathBuilder::stroke(px(if *lit { 2.0 } else { 1.2 }));
                        path.move_to(to_point(curve[0]));
                        path.cubic_bezier_to(
                            to_point(curve[3]),
                            to_point(curve[1]),
                            to_point(curve[2]),
                        );
                        if let Ok(built) = path.build() {
                            window.paint_path(built, if *lit { lit_color } else { edge_color });
                        }
                        // A dot where the key lands on its parent.
                        let end = to_point(curve[3]);
                        let r = px(2.5 * scale.max(0.6));
                        let mut dot = PathBuilder::fill();
                        dot.move_to(point(end.x - r, end.y));
                        dot.line_to(point(end.x, end.y - r));
                        dot.line_to(point(end.x + r, end.y));
                        dot.line_to(point(end.x, end.y + r));
                        dot.close();
                        if let Ok(built) = dot.build() {
                            window.paint_path(built, if *lit { lit_color } else { edge_color });
                        }
                    }
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();

        let text = at(12.);
        let boxes: Vec<AnyElement> = diagram
            .tables
            .iter()
            .enumerate()
            .map(|(index, table)| {
                let place = diagram.layout.positions[index];
                let size = diagram.sizes[index];
                let lit = diagram.hover == Some(index)
                    || diagram.hover.is_some_and(|h| {
                        diagram.edges.iter().any(|e| {
                            (e.from == h && e.to == index) || (e.to == h && e.from == index)
                        })
                    });
                let reference = table.table.clone();
                div()
                    .id(("er-table", index))
                    .absolute()
                    .left(at(place.x))
                    .top(at(place.y))
                    .w(at(size.width))
                    .h(at(size.height))
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded(at(6.))
                    .bg(theme.elevated)
                    .border_1()
                    .border_color(if lit { theme.accent } else { theme.border })
                    .text_size(text)
                    .font_family(metrics::MONO_FONT)
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                        if let Some(diagram) = this.er_diagram.as_mut() {
                            if *hovered {
                                diagram.hover = Some(index);
                            } else if diagram.hover == Some(index) {
                                diagram.hover = None;
                            }
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        this.close_er_diagram(cx);
                        this.open_table_tab(reference.clone(), cx);
                    }))
                    .child(
                        div()
                            .h(at(HEADER))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(at(6.))
                            .px(at(PADDING))
                            .bg(if lit { theme.selection } else { theme.panel })
                            .border_b_1()
                            .border_color(theme.border)
                            .child(super::icons::kind_icon(
                                table.kind,
                                if table.kind.is_view() {
                                    theme.value_structured
                                } else {
                                    theme.text_faint
                                },
                            ))
                            .child(
                                div()
                                    .truncate()
                                    .text_color(theme.text)
                                    .font_weight(gpui::FontWeight::MEDIUM)
                                    .child(SharedString::from(table.table.name.clone())),
                            ),
                    )
                    .children(table.columns.iter().map(|column| {
                        let marker = if column.is_primary_key {
                            "PK"
                        } else if column.references.is_some() {
                            "FK"
                        } else {
                            ""
                        };
                        div()
                            .h(at(ROW))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(at(6.))
                            .px(at(PADDING))
                            .child(
                                div()
                                    .w(at(18.))
                                    .flex_shrink_0()
                                    .text_size(at(9.))
                                    .text_color(if column.is_primary_key {
                                        theme.warning
                                    } else {
                                        theme.accent
                                    })
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(if column.nullable {
                                        theme.text_muted
                                    } else {
                                        theme.text
                                    })
                                    .child(SharedString::from(column.name.clone())),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .min_w(px(0.))
                                    .truncate()
                                    .text_color(theme.text_faint)
                                    .child(SharedString::from(column.data_type.clone())),
                            )
                    }))
                    .into_any_element()
            })
            .collect();

        let headline = if diagram.loaded {
            format!(
                "{} tables · {} foreign keys",
                diagram.tables.len(),
                diagram.edges.len()
            )
        } else {
            "Reading columns…".to_string()
        };
        let zoom_label = format!("{:.0}%", diagram.zoom * 100.);

        Some(
            motion::dialog(
                "er-in",
                div()
                    .id("er-scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui::rgba(0x00000099))
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .id("er-panel")
                            .w(gpui::relative(0.95))
                            .h(gpui::relative(0.9))
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .overflow_hidden()
                            .text_size(metrics::text_size_small())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_4()
                                    .py_2()
                                    .bg(theme.elevated)
                                    .border_b_1()
                                    .border_color(theme.border)
                                    .child(div().text_size(metrics::scaled(15.)).child(
                                        SharedString::from(format!("{} — diagram", diagram.schema)),
                                    ))
                                    .child(caption(headline, theme))
                                    .child(div().flex_1())
                                    .child(caption("Hover to trace keys · click to open", theme))
                                    .child(button("er-zoom-out", "−", theme, false).on_click(
                                        cx.listener(|this, _, _window, cx| {
                                            this.zoom_er_diagram(1.0 / 1.25, cx)
                                        }),
                                    ))
                                    .child(caption(zoom_label, theme))
                                    .child(button("er-zoom-in", "+", theme, false).on_click(
                                        cx.listener(|this, _, _window, cx| {
                                            this.zoom_er_diagram(1.25, cx)
                                        }),
                                    ))
                                    .child(button("er-close", "Close", theme, false).on_click(
                                        cx.listener(|this, _, _window, cx| {
                                            this.close_er_diagram(cx)
                                        }),
                                    )),
                            )
                            .child(
                                div()
                                    .id("er-canvas")
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_scroll()
                                    .child(
                                        div()
                                            .relative()
                                            .w(at(diagram.layout.width))
                                            .h(at(diagram.layout.height))
                                            .child(edges)
                                            .children(boxes),
                                    ),
                            ),
                    ),
                px(0.),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_app::domain::ForeignKey;

    fn column(name: &str, pk: bool, references: Option<(&str, &str)>) -> Column {
        Column {
            name: name.into(),
            data_type: "int".into(),
            nullable: !pk,
            default: None,
            is_primary_key: pk,
            ordinal: 0,
            references: references.map(|(table, column)| ForeignKey {
                column: name.into(),
                references: TableRef::new("public", table),
                references_column: column.into(),
            }),
        }
    }

    fn table(name: &str, columns: Vec<Column>) -> ErTable {
        ErTable {
            table: TableRef::new("public", name),
            kind: TableKind::Table,
            columns,
        }
    }

    #[test]
    fn keys_become_edges_between_the_right_rows() {
        let diagram = ErDiagram::build(
            "public".into(),
            vec![
                table("customers", vec![column("id", true, None)]),
                table(
                    "orders",
                    vec![
                        column("id", true, None),
                        column("customer_id", false, Some(("customers", "id"))),
                        column("elsewhere_id", false, Some(("not_here", "id"))),
                    ],
                ),
            ],
        );
        assert_eq!(
            diagram.edges,
            vec![ErEdge {
                from: 1,
                from_row: 1,
                to: 0,
                to_row: Some(0),
            }],
            "a key to a table outside the schema is not drawn"
        );
        // The parent is to the left, so the line leaves the child's left side
        // and lands on the parent's right.
        let curves = diagram.curves();
        let [start, _, _, end] = curves[0].0;
        let child = diagram.layout.positions[1];
        let parent = diagram.layout.positions[0];
        assert_eq!(start.0, child.x);
        assert_eq!(end.0, parent.x + diagram.sizes[0].width);
    }

    #[test]
    fn a_self_reference_loops_off_the_right_side() {
        let diagram = ErDiagram::build(
            "public".into(),
            vec![table(
                "staff",
                vec![
                    column("id", true, None),
                    column("manager_id", false, Some(("staff", "id"))),
                ],
            )],
        );
        let [start, c1, _, end] = diagram.curves()[0].0;
        let right = diagram.layout.positions[0].x + diagram.sizes[0].width;
        assert_eq!(start.0, right);
        assert_eq!(end.0, right);
        assert!(c1.0 > right);
    }
}
