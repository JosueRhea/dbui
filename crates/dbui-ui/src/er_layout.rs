//! Where each table goes in the ER diagram.
//!
//! A layered layout, which is what a schema mostly is: tables that are
//! referenced sit to the left of the tables that reference them, so foreign
//! keys read right to left, from the child to the parent. Each table's
//! column is the longest chain of references below it; within a column the
//! tables are ordered by where their neighbours are (a few barycenter
//! sweeps), which keeps most lines from crossing. Tables with no keys either
//! way are gathered into columns of their own at the right, so a schema of
//! fifty lookup tables does not become one very tall stripe.
//!
//! Pure arithmetic on sizes, so it is tested without a window.

/// One table to place, measured.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Node {
    pub width: f32,
    pub height: f32,
}

/// Where a node landed: its top-left corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub positions: Vec<Placed>,
    pub width: f32,
    pub height: f32,
}

pub const COLUMN_GAP: f32 = 96.;
pub const ROW_GAP: f32 = 28.;
pub const MARGIN: f32 = 32.;

/// Lay out `nodes`, where each edge `(from, to)` says `from` references
/// `to`. Self-references and edges to unknown nodes are ignored for placing.
pub fn layout(nodes: &[Node], edges: &[(usize, usize)]) -> Layout {
    let n = nodes.len();
    let edges: Vec<(usize, usize)> = edges
        .iter()
        .copied()
        .filter(|&(from, to)| from != to && from < n && to < n)
        .collect();
    let mut linked = vec![false; n];
    for &(from, to) in &edges {
        linked[from] = true;
        linked[to] = true;
    }

    // Longest chain of references below each table. A cycle would climb
    // forever; `n` rounds is as deep as any honest chain can go.
    let mut rank = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for &(from, to) in &edges {
            if rank[from] < rank[to] + 1 && rank[to] < n {
                rank[from] = rank[to] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let linked_columns = (0..n)
        .filter(|&i| linked[i])
        .map(|i| rank[i] + 1)
        .max()
        .unwrap_or(0);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); linked_columns];
    for i in (0..n).filter(|&i| linked[i]) {
        columns[rank[i]].push(i);
    }

    // Unlinked tables, in columns no taller than the tallest linked one (or
    // a square-ish grid when nothing is linked at all).
    let loose: Vec<usize> = (0..n).filter(|&i| !linked[i]).collect();
    if !loose.is_empty() {
        let tallest = columns
            .iter()
            .map(|column| column_height(nodes, column))
            .fold(0.0f32, f32::max);
        let limit = if tallest > 0.0 {
            tallest
        } else {
            let total: f32 = loose.iter().map(|&i| nodes[i].height + ROW_GAP).sum();
            (total / (loose.len() as f32).sqrt().ceil()).max(1.0)
        };
        let mut current: Vec<usize> = Vec::new();
        for i in loose {
            let grown = column_height(nodes, &current) + nodes[i].height + ROW_GAP;
            if !current.is_empty() && grown > limit {
                columns.push(std::mem::take(&mut current));
            }
            current.push(i);
        }
        columns.push(current);
    }

    // Barycenter sweeps: order each column by the average position of its
    // neighbours in the column beside it, rightwards then leftwards.
    let neighbours = |i: usize| -> Vec<usize> {
        edges
            .iter()
            .filter_map(|&(from, to)| {
                if from == i {
                    Some(to)
                } else if to == i {
                    Some(from)
                } else {
                    None
                }
            })
            .collect()
    };
    let mut index_in_column = vec![0usize; n];
    let renumber = |columns: &Vec<Vec<usize>>, index: &mut Vec<usize>| {
        for column in columns {
            for (at, &i) in column.iter().enumerate() {
                index[i] = at;
            }
        }
    };
    renumber(&columns, &mut index_in_column);
    for sweep in 0..4 {
        let order: Vec<usize> = if sweep % 2 == 0 {
            (1..linked_columns).collect()
        } else {
            (0..linked_columns.saturating_sub(1)).rev().collect()
        };
        for c in order {
            let beside = if sweep % 2 == 0 { c - 1 } else { c + 1 };
            let mut keyed: Vec<(f32, usize)> = columns[c]
                .iter()
                .map(|&i| {
                    let near: Vec<f32> = neighbours(i)
                        .into_iter()
                        .filter(|&j| linked[j] && rank[j] == beside)
                        .map(|j| index_in_column[j] as f32)
                        .collect();
                    let key = if near.is_empty() {
                        index_in_column[i] as f32
                    } else {
                        near.iter().sum::<f32>() / near.len() as f32
                    };
                    (key, i)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            columns[c] = keyed.into_iter().map(|(_, i)| i).collect();
            renumber(&columns, &mut index_in_column);
        }
    }

    let mut positions = vec![Placed { x: 0., y: 0. }; n];
    let mut x = MARGIN;
    let mut height: f32 = 0.;
    for column in &columns {
        let width = column
            .iter()
            .map(|&i| nodes[i].width)
            .fold(0.0f32, f32::max);
        let mut y = MARGIN;
        for &i in column {
            positions[i] = Placed { x, y };
            y += nodes[i].height + ROW_GAP;
        }
        height = height.max(y - ROW_GAP + MARGIN);
        x += width + COLUMN_GAP;
    }
    Layout {
        positions,
        width: (x - COLUMN_GAP + MARGIN).max(MARGIN * 2.),
        height: height.max(MARGIN * 2.),
    }
}

fn column_height(nodes: &[Node], column: &[usize]) -> f32 {
    column.iter().map(|&i| nodes[i].height + ROW_GAP).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(n: usize) -> Vec<Node> {
        vec![
            Node {
                width: 100.,
                height: 60.
            };
            n
        ]
    }

    fn overlaps(layout: &Layout, nodes: &[Node]) -> bool {
        for i in 0..nodes.len() {
            for j in i + 1..nodes.len() {
                let (a, b) = (layout.positions[i], layout.positions[j]);
                let apart_x = a.x + nodes[i].width <= b.x || b.x + nodes[j].width <= a.x;
                let apart_y = a.y + nodes[i].height <= b.y || b.y + nodes[j].height <= a.y;
                if !apart_x && !apart_y {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn a_parent_sits_left_of_its_children() {
        // orders -> customers, order_items -> orders, order_items -> products
        let edges = [(1, 0), (2, 1), (2, 3)];
        let n = nodes(4);
        let placed = layout(&n, &edges);
        let x = |i: usize| placed.positions[i].x;
        assert!(x(0) < x(1), "customers left of orders");
        assert!(x(1) < x(2), "orders left of order_items");
        assert!(x(3) < x(2), "products left of order_items");
        assert!(!overlaps(&placed, &n));
    }

    #[test]
    fn a_cycle_still_places_everything() {
        let edges = [(0, 1), (1, 2), (2, 0), (0, 0)];
        let n = nodes(3);
        let placed = layout(&n, &edges);
        assert_eq!(placed.positions.len(), 3);
        assert!(!overlaps(&placed, &n));
        assert!(placed.width.is_finite() && placed.height.is_finite());
    }

    #[test]
    fn unlinked_tables_fill_columns_of_their_own() {
        let n = nodes(12);
        let placed = layout(&n, &[(0, 1)]);
        assert!(!overlaps(&placed, &n));
        // Ten loose tables do not stack into one stripe ten tables tall.
        let tallest_linked = 60. + 2. * MARGIN;
        let loose_columns: std::collections::HashSet<i32> =
            (2..12).map(|i| placed.positions[i].x as i32).collect();
        assert!(loose_columns.len() > 1, "{placed:?}");
        assert!(
            placed.height < 10. * (60. + ROW_GAP),
            "{} vs {tallest_linked}",
            placed.height
        );
    }

    #[test]
    fn neighbours_line_up_to_avoid_crossings() {
        // Two parents, each with one child; the children should keep the
        // parents' order rather than crossing over.
        let edges = [(2, 1), (3, 0)];
        let placed = layout(&nodes(4), &edges);
        let y = |i: usize| placed.positions[i].y;
        assert_eq!(y(0) < y(1), y(3) < y(2), "{placed:?}");
    }

    #[test]
    fn nothing_to_draw_is_a_small_empty_canvas() {
        let placed = layout(&[], &[]);
        assert!(placed.positions.is_empty());
        assert!(placed.width > 0. && placed.height > 0.);
    }
}
