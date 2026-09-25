//! Query plans: what `EXPLAIN` returned, as a tree someone can read.
//!
//! Every engine answers `EXPLAIN` differently -- Postgres with indented text
//! or a JSON document, MySQL with JSON (or an indented tree), SQLite with rows
//! of `(id, parent, detail)` -- and all of them arrive in the grid as a column
//! of strings. [`Plan::from_result`] recognises each shape and turns it into
//! the same thing: a list of steps in tree order, each with its depth, what it
//! does, and what the engine thinks it costs.
//!
//! Recognised by shape rather than by remembering what was sent, so a plan the
//! user typed by hand (`EXPLAIN ANALYZE SELECT ...`) is drawn as a plan too.

use dbui_domain::{Driver, ResultSet, Value};
use serde_json::Value as Json;

/// One operation in a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanStep {
    /// Nesting: 0 for the root, children one deeper than their parent.
    pub depth: usize,
    /// "Seq Scan on orders", "Hash Join", "customers (ALL)".
    pub title: String,
    /// Conditions and keys: "Filter: (total > 100)", "Sort Key: id".
    pub details: Vec<String>,
    /// The engine's estimate for this step *including* everything under it,
    /// in its own units. Only comparable within one plan.
    pub cost: Option<f64>,
    /// Estimated rows out of this step.
    pub rows: Option<f64>,
    /// Measured milliseconds including children, from `EXPLAIN ANALYZE`.
    pub actual_ms: Option<f64>,
}

impl PlanStep {
    fn new(depth: usize, title: impl Into<String>) -> Self {
        Self {
            depth,
            title: title.into(),
            details: Vec::new(),
            cost: None,
            rows: None,
            actual_ms: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// Pre-order: each step is followed by its children.
    pub steps: Vec<PlanStep>,
}

impl Plan {
    /// A plan, if `set` is the output of an `EXPLAIN`.
    pub fn from_result(set: &ResultSet) -> Option<Plan> {
        let plan = match set.columns.len() {
            1 => one_column(set),
            4 => sqlite_rows(set),
            _ => None,
        }?;
        (!plan.steps.is_empty()).then_some(plan)
    }

    /// Each step's share of the work done *by that step alone*, 0..=1.
    ///
    /// Inclusive figures make every root look like the bottleneck -- it
    /// contains everything. Subtracting what the children account for leaves
    /// what the step itself costs, which is what points at the slow part.
    /// Measured time is used when the plan has it, estimated cost otherwise.
    pub fn self_shares(&self) -> Vec<f64> {
        let measured = self.steps.iter().any(|step| step.actual_ms.is_some());
        let figure = |step: &PlanStep| {
            if measured {
                step.actual_ms
            } else {
                step.cost
            }
        };
        let own: Vec<f64> = (0..self.steps.len())
            .map(|index| {
                let Some(total) = figure(&self.steps[index]) else {
                    return 0.0;
                };
                let children: f64 = self
                    .children(index)
                    .filter_map(|child| figure(&self.steps[child]))
                    .sum();
                (total - children).max(0.0)
            })
            .collect();
        let sum: f64 = own.iter().sum();
        if sum <= 0.0 {
            return vec![0.0; own.len()];
        }
        own.iter().map(|own| own / sum).collect()
    }

    /// Indices of the direct children of step `index`.
    pub fn children(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let depth = self.steps[index].depth;
        self.steps[index + 1..]
            .iter()
            .enumerate()
            .take_while(move |(_, step)| step.depth > depth)
            .filter(move |(_, step)| step.depth == depth + 1)
            .map(move |(offset, _)| index + 1 + offset)
    }
}

/// The statement that asks `driver` for a plan of `sql`, in the form
/// [`Plan::from_result`] reads best. Never runs `sql`: no `ANALYZE`.
///
/// Returned unchanged when it already is an `EXPLAIN`, so the command can be
/// pressed on one the user wrote themselves.
pub fn explain_sql(driver: Driver, sql: &str) -> String {
    let sql = sql.trim().trim_end_matches(';').trim();
    let first = sql
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if first == "EXPLAIN" || first == "DESCRIBE" || first == "DESC" {
        return sql.to_string();
    }
    match driver {
        Driver::Postgres => format!("EXPLAIN (FORMAT JSON) {sql}"),
        // JSON rather than TREE: MariaDB has no TREE, and both have JSON.
        Driver::MySql => format!("EXPLAIN FORMAT=JSON {sql}"),
        Driver::Sqlite => format!("EXPLAIN QUERY PLAN {sql}"),
    }
}

fn cell_text(value: &Value) -> Option<&str> {
    match value {
        Value::Text(text) | Value::Json(text) => Some(text),
        _ => None,
    }
}

fn one_column(set: &ResultSet) -> Option<Plan> {
    let column = set.columns[0].name.to_ascii_uppercase();
    let first = set.rows.first()?.0.first().and_then(cell_text)?;
    let trimmed = first.trim_start();

    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        let json: Json = serde_json::from_str(first).ok()?;
        if let Some(plan) = postgres_json(&json) {
            return Some(plan);
        }
        return mysql_json(&json);
    }

    // Indented text: Postgres returns one line per row under `QUERY PLAN`,
    // MySQL's TREE format returns the whole tree in one cell under `EXPLAIN`.
    if column == "QUERY PLAN" || (column == "EXPLAIN" && trimmed.starts_with("->")) {
        let lines: Vec<&str> = set
            .rows
            .iter()
            .filter_map(|row| row.0.first().and_then(cell_text))
            .flat_map(str::lines)
            .collect();
        return Some(indented_text(&lines));
    }
    None
}

// -- indented text: Postgres's default, MySQL's TREE ------------------------

fn indented_text(lines: &[&str]) -> Plan {
    let mut steps: Vec<PlanStep> = Vec::new();
    // (indent of the node's text, index into steps) for the open ancestry.
    let mut stack: Vec<(usize, usize)> = Vec::new();

    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let text = line.trim();
        let is_node = text.starts_with("->") || steps.is_empty();

        if is_node {
            let body = text.trim_start_matches("->").trim_start();
            while stack.last().is_some_and(|(open, _)| *open >= indent) {
                stack.pop();
            }
            let mut step = PlanStep::new(stack.len(), "");
            read_figures(body, &mut step);
            stack.push((indent, steps.len()));
            steps.push(step);
        } else if text.starts_with("Planning")
            || text.starts_with("Execution")
            || text.starts_with("JIT:")
        {
            // Totals printed after the tree by EXPLAIN ANALYZE; the root's
            // own time already says the same, and they belong to no step.
            continue;
        } else if let Some(&(_, owner)) = stack.iter().rev().find(|(open, _)| *open < indent) {
            steps[owner].details.push(text.to_string());
        } else if let Some(last) = steps.last_mut() {
            last.details.push(text.to_string());
        }
    }
    Plan { steps }
}

/// Split "Hash Join  (cost=1.0..2.5 rows=10 width=4) (actual time=...)" into
/// the title and the numbers.
fn read_figures(body: &str, step: &mut PlanStep) {
    let mut title = body;
    for (open, _) in body.match_indices('(') {
        let rest = &body[open + 1..];
        if rest.starts_with("cost=") || rest.starts_with("actual ") || rest.starts_with("never") {
            title = &body[..open];
            break;
        }
    }
    step.title = title.trim().trim_end_matches(':').to_string();

    for group in body[title.len()..].split('(').skip(1) {
        let group = group.split(')').next().unwrap_or_default();
        let mut loops = 1.0;
        let mut time = None;
        for pair in group.split_whitespace() {
            let Some((key, value)) = pair.split_once('=') else {
                continue;
            };
            let last = value.rsplit("..").next().unwrap_or(value);
            let number = last.parse::<f64>().ok();
            match key {
                "cost" => step.cost = number,
                "rows" if group.starts_with("cost") => step.rows = number,
                "time" => time = number,
                "loops" => loops = number.unwrap_or(1.0),
                _ => {}
            }
        }
        if let Some(time) = time {
            step.actual_ms = Some(time * loops);
        }
    }
}

// -- Postgres JSON -----------------------------------------------------------

fn postgres_json(json: &Json) -> Option<Plan> {
    let root = json.as_array()?.first()?.get("Plan")?;
    let mut steps = Vec::new();
    postgres_node(root, 0, &mut steps);
    Some(Plan { steps })
}

fn postgres_node(node: &Json, depth: usize, steps: &mut Vec<PlanStep>) {
    let text = |key: &str| node.get(key).and_then(Json::as_str);
    let number = |key: &str| node.get(key).and_then(Json::as_f64);

    let mut title = text("Node Type").unwrap_or("Step").to_string();
    if let Some(strategy) =
        text("Join Type").filter(|_| title.contains("Join") || title == "Nested Loop")
    {
        title = format!("{strategy} {title}");
    }
    if let Some(index) = text("Index Name") {
        title.push_str(&format!(" using {index}"));
    }
    if let Some(relation) = text("Relation Name") {
        title.push_str(&format!(" on {relation}"));
        if let Some(alias) = text("Alias").filter(|alias| *alias != relation) {
            title.push_str(&format!(" {alias}"));
        }
    }

    let mut step = PlanStep::new(depth, title);
    for key in [
        "Index Cond",
        "Recheck Cond",
        "Hash Cond",
        "Merge Cond",
        "Join Filter",
        "Filter",
    ] {
        if let Some(condition) = text(key) {
            step.details.push(format!("{key}: {condition}"));
        }
    }
    for key in ["Sort Key", "Group Key"] {
        if let Some(keys) = node.get(key).and_then(Json::as_array) {
            let keys: Vec<&str> = keys.iter().filter_map(Json::as_str).collect();
            step.details.push(format!("{key}: {}", keys.join(", ")));
        }
    }
    if let Some(removed) = number("Rows Removed by Filter").filter(|n| *n > 0.0) {
        step.details
            .push(format!("Rows Removed by Filter: {removed}"));
    }
    step.cost = number("Total Cost");
    step.rows = number("Plan Rows");
    step.actual_ms =
        number("Actual Total Time").map(|time| time * number("Actual Loops").unwrap_or(1.0));
    steps.push(step);

    for child in node
        .get("Plans")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        postgres_node(child, depth + 1, steps);
    }
}

// -- MySQL / MariaDB JSON ----------------------------------------------------

/// Keys that are an operation in their own right, drawn as a step. Anything
/// else holding an object or array is looked through for steps.
const MYSQL_OPERATIONS: [(&str, &str); 12] = [
    ("nested_loop", "Nested loop"),
    ("ordering_operation", "Sort"),
    ("grouping_operation", "Group"),
    ("duplicates_removal", "Remove duplicates"),
    ("windowing", "Window"),
    ("union_result", "Union"),
    ("buffer_result", "Buffer result"),
    ("materialized_from_subquery", "Materialized subquery"),
    ("attached_subqueries", "Subqueries"),
    ("filesort", "Filesort"),
    ("temporary_table", "Temporary table"),
    ("read_sorted_file", "Read sorted file"),
];

fn mysql_json(json: &Json) -> Option<Plan> {
    let block = json.get("query_block")?;
    let mut steps = Vec::new();
    mysql_block(block, 0, &mut steps);
    Some(Plan { steps })
}

fn mysql_block(block: &Json, depth: usize, steps: &mut Vec<PlanStep>) {
    let id = block.get("select_id").and_then(Json::as_u64).unwrap_or(1);
    let mut step = PlanStep::new(depth, format!("Query block #{id}"));
    step.cost = block.pointer("/cost_info/query_cost").and_then(json_number);
    steps.push(step);
    mysql_walk(block, depth + 1, steps);
}

fn mysql_walk(value: &Json, depth: usize, steps: &mut Vec<PlanStep>) {
    match value {
        Json::Array(items) => {
            for item in items {
                mysql_walk(item, depth, steps);
            }
        }
        Json::Object(map) if map.contains_key("table_name") => mysql_table(value, depth, steps),
        Json::Object(map) => {
            for (key, child) in map {
                if key == "query_block" {
                    mysql_block(child, depth, steps);
                } else if let Some((_, label)) =
                    MYSQL_OPERATIONS.iter().find(|(name, _)| name == key)
                {
                    let mut step = PlanStep::new(depth, *label);
                    if child.get("using_filesort").and_then(Json::as_bool) == Some(true) {
                        step.details.push("Using filesort".into());
                    }
                    if child.get("using_temporary_table").and_then(Json::as_bool) == Some(true) {
                        step.details.push("Using temporary table".into());
                    }
                    steps.push(step);
                    mysql_walk(child, depth + 1, steps);
                } else if child.is_object() || child.is_array() {
                    mysql_walk(child, depth, steps);
                }
            }
        }
        _ => {}
    }
}

fn mysql_table(table: &Json, depth: usize, steps: &mut Vec<PlanStep>) {
    let text = |key: &str| table.get(key).and_then(Json::as_str);
    let name = text("table_name").unwrap_or("?");
    let mut title = match text("access_type") {
        Some(access) => format!("{name} ({access})"),
        None => name.to_string(),
    };
    if let Some(key) = text("key") {
        title.push_str(&format!(" using {key}"));
    }
    let mut step = PlanStep::new(depth, title);
    if let Some(condition) = text("attached_condition") {
        step.details.push(format!("Condition: {condition}"));
    }
    if let Some(extra) = text("index_condition") {
        step.details.push(format!("Index condition: {extra}"));
    }
    step.rows = table
        .get("rows_examined_per_scan")
        .or_else(|| table.get("rows"))
        .and_then(json_number);
    step.cost = table
        .pointer("/cost_info/prefix_cost")
        .and_then(json_number);
    steps.push(step);

    // A derived table carries its own plan underneath it.
    for (key, child) in table.as_object().into_iter().flatten() {
        if key == "materialized_from_subquery" || key == "attached_subqueries" {
            mysql_walk(child, depth + 1, steps);
        }
    }
}

/// MySQL writes most numbers in its plan as strings: `"query_cost": "1.20"`.
fn json_number(value: &Json) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
}

// -- SQLite ------------------------------------------------------------------

fn sqlite_rows(set: &ResultSet) -> Option<Plan> {
    let names: Vec<String> = set
        .columns
        .iter()
        .map(|column| column.name.to_ascii_lowercase())
        .collect();
    if names != ["id", "parent", "notused", "detail"] {
        return None;
    }
    let int = |value: &Value| match value {
        Value::Int(n) => Some(*n),
        Value::Text(text) => text.parse().ok(),
        _ => None,
    };
    // Rows arrive parent-first, so a parent's depth is always known by the
    // time a child names it.
    let mut depth_of: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut steps = Vec::new();
    for row in &set.rows {
        let (Some(id), Some(parent)) = (int(&row.0[0]), int(&row.0[1])) else {
            continue;
        };
        let detail = row.0[3].to_text();
        let depth = if parent == 0 {
            0
        } else {
            depth_of.get(&parent).map_or(0, |depth| depth + 1)
        };
        depth_of.insert(id, depth);
        steps.push(PlanStep::new(depth, detail));
    }
    // Pre-order is what the rows already are: SQLite emits each subtree
    // straight after its parent.
    Some(Plan { steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbui_domain::{ColumnInfo, Row};

    fn set(columns: &[&str], rows: Vec<Vec<Value>>) -> ResultSet {
        ResultSet {
            columns: columns
                .iter()
                .map(|name| ColumnInfo {
                    name: name.to_string(),
                    type_name: "text".into(),
                })
                .collect(),
            rows: rows.into_iter().map(Row).collect(),
            truncated: false,
        }
    }

    fn text_rows(column: &str, lines: &[&str]) -> ResultSet {
        set(
            &[column],
            lines
                .iter()
                .map(|line| vec![Value::Text(line.to_string())])
                .collect(),
        )
    }

    fn titles(plan: &Plan) -> Vec<(usize, &str)> {
        plan.steps
            .iter()
            .map(|step| (step.depth, step.title.as_str()))
            .collect()
    }

    #[test]
    fn postgres_text_nests_by_the_arrows() {
        let plan = Plan::from_result(&text_rows(
            "QUERY PLAN",
            &[
                "Hash Join  (cost=10.00..45.50 rows=100 width=40)",
                "  Hash Cond: (o.customer_id = c.id)",
                "  ->  Seq Scan on orders o  (cost=0.00..30.00 rows=1000 width=20)",
                "        Filter: (total > 100)",
                "  ->  Hash  (cost=8.00..8.00 rows=160 width=20)",
                "        ->  Seq Scan on customers c  (cost=0.00..8.00 rows=160 width=20)",
            ],
        ))
        .expect("a plan");
        assert_eq!(
            titles(&plan),
            vec![
                (0, "Hash Join"),
                (1, "Seq Scan on orders o"),
                (1, "Hash"),
                (2, "Seq Scan on customers c"),
            ]
        );
        assert_eq!(
            plan.steps[0].details,
            vec!["Hash Cond: (o.customer_id = c.id)"]
        );
        assert_eq!(plan.steps[1].details, vec!["Filter: (total > 100)"]);
        assert_eq!(plan.steps[0].cost, Some(45.5));
        assert_eq!(plan.steps[1].rows, Some(1000.0));
        assert_eq!(plan.children(0).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn explain_analyze_reads_measured_time_times_loops() {
        let plan = Plan::from_result(&text_rows(
            "QUERY PLAN",
            &[
                "Nested Loop  (cost=0.29..16.6 rows=1 width=8) (actual time=0.020..0.500 rows=3 loops=1)",
                "  ->  Index Scan using orders_pkey on orders  (cost=0.29..8.3 rows=1 width=4) (actual time=0.010..0.100 rows=1 loops=3)",
                "Planning Time: 0.1 ms",
                "Execution Time: 0.6 ms",
            ],
        ))
        .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].actual_ms, Some(0.5));
        assert!((plan.steps[1].actual_ms.unwrap() - 0.3).abs() < 1e-9);
        assert!(plan.steps[0].details.is_empty(), "totals are not a detail");
    }

    #[test]
    fn postgres_json_names_relations_and_conditions() {
        let json = r#"[{"Plan": {
            "Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 45.5, "Plan Rows": 100,
            "Hash Cond": "(o.customer_id = c.id)",
            "Plans": [
                {"Node Type": "Seq Scan", "Relation Name": "orders", "Alias": "o",
                 "Total Cost": 30.0, "Plan Rows": 1000, "Filter": "(total > 100)"},
                {"Node Type": "Hash", "Total Cost": 8.0, "Plan Rows": 160, "Plans": [
                    {"Node Type": "Index Scan", "Index Name": "customers_pkey",
                     "Relation Name": "customers", "Alias": "customers", "Total Cost": 8.0}
                ]}
            ]}}]"#;
        let plan =
            Plan::from_result(&set(&["QUERY PLAN"], vec![vec![Value::Json(json.into())]])).unwrap();
        assert_eq!(
            titles(&plan),
            vec![
                (0, "Inner Hash Join"),
                (1, "Seq Scan on orders o"),
                (1, "Hash"),
                (2, "Index Scan using customers_pkey on customers"),
            ]
        );
        assert_eq!(plan.steps[1].details, vec!["Filter: (total > 100)"]);
    }

    #[test]
    fn the_hotspot_is_the_step_itself_not_its_parent() {
        let plan = Plan::from_result(&text_rows(
            "QUERY PLAN",
            &[
                "Sort  (cost=0.00..100.00 rows=10 width=4)",
                "  ->  Seq Scan on big  (cost=0.00..90.00 rows=10 width=4)",
            ],
        ))
        .unwrap();
        let shares = plan.self_shares();
        assert!((shares[0] - 0.1).abs() < 1e-9, "{shares:?}");
        assert!((shares[1] - 0.9).abs() < 1e-9, "{shares:?}");
    }

    #[test]
    fn mysql_json_walks_nested_loops_and_sorts() {
        let json = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "12.50"},
            "ordering_operation": {"using_filesort": true,
                "nested_loop": [
                    {"table": {"table_name": "o", "access_type": "ALL",
                               "rows_examined_per_scan": 1000,
                               "cost_info": {"prefix_cost": "10.00"},
                               "attached_condition": "(o.total > 100)"}},
                    {"table": {"table_name": "c", "access_type": "eq_ref", "key": "PRIMARY",
                               "rows_examined_per_scan": 1,
                               "cost_info": {"prefix_cost": "12.50"}}}
                ]}}}"#;
        let plan =
            Plan::from_result(&set(&["EXPLAIN"], vec![vec![Value::Json(json.into())]])).unwrap();
        assert_eq!(
            titles(&plan),
            vec![
                (0, "Query block #1"),
                (1, "Sort"),
                (2, "Nested loop"),
                (3, "o (ALL)"),
                (3, "c (eq_ref) using PRIMARY"),
            ]
        );
        assert_eq!(plan.steps[0].cost, Some(12.5));
        assert_eq!(plan.steps[1].details, vec!["Using filesort"]);
        assert_eq!(plan.steps[3].rows, Some(1000.0));
        assert_eq!(plan.steps[3].details, vec!["Condition: (o.total > 100)"]);
    }

    #[test]
    fn mariadb_json_is_the_same_walk() {
        let json = r#"{"query_block": {"select_id": 1,
            "table": {"table_name": "t", "access_type": "ALL", "rows": 10, "filtered": 100}}}"#;
        let plan =
            Plan::from_result(&set(&["EXPLAIN"], vec![vec![Value::Text(json.into())]])).unwrap();
        assert_eq!(titles(&plan), vec![(0, "Query block #1"), (1, "t (ALL)")]);
        assert_eq!(plan.steps[1].rows, Some(10.0));
    }

    #[test]
    fn mysql_tree_text_is_one_cell() {
        let tree = "-> Nested loop inner join  (cost=1.10 rows=2)\n    \
                    -> Table scan on o  (cost=0.45 rows=2)\n    \
                    -> Single-row index lookup on c using PRIMARY (id=o.customer_id)  (cost=0.30 rows=1)\n";
        let plan =
            Plan::from_result(&set(&["EXPLAIN"], vec![vec![Value::Text(tree.into())]])).unwrap();
        assert_eq!(
            titles(&plan),
            vec![
                (0, "Nested loop inner join"),
                (1, "Table scan on o"),
                (
                    1,
                    "Single-row index lookup on c using PRIMARY (id=o.customer_id)"
                ),
            ]
        );
        assert_eq!(plan.steps[2].cost, Some(0.3));
    }

    #[test]
    fn sqlite_rows_nest_by_parent() {
        let plan = Plan::from_result(&set(
            &["id", "parent", "notused", "detail"],
            vec![
                vec![
                    Value::Int(3),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Text("SCAN o".into()),
                ],
                vec![
                    Value::Int(5),
                    Value::Int(0),
                    Value::Int(0),
                    Value::Text("SEARCH c USING INTEGER PRIMARY KEY (rowid=?)".into()),
                ],
                vec![
                    Value::Int(7),
                    Value::Int(5),
                    Value::Int(0),
                    Value::Text("CORRELATED SCALAR SUBQUERY".into()),
                ],
            ],
        ))
        .unwrap();
        assert_eq!(
            titles(&plan),
            vec![
                (0, "SCAN o"),
                (0, "SEARCH c USING INTEGER PRIMARY KEY (rowid=?)"),
                (1, "CORRELATED SCALAR SUBQUERY"),
            ]
        );
    }

    #[test]
    fn an_ordinary_result_is_not_a_plan() {
        assert!(Plan::from_result(&text_rows("name", &["Ada", "Grace"])).is_none());
        assert!(Plan::from_result(&text_rows("QUERY PLAN", &[])).is_none());
        assert!(
            Plan::from_result(&set(&["j"], vec![vec![Value::Json("{\"a\":1}".into())]])).is_none()
        );
    }

    #[test]
    fn explaining_asks_each_engine_its_own_way_and_never_analyzes() {
        assert_eq!(
            explain_sql(Driver::Postgres, "SELECT 1;"),
            "EXPLAIN (FORMAT JSON) SELECT 1"
        );
        assert_eq!(
            explain_sql(Driver::MySql, "SELECT 1"),
            "EXPLAIN FORMAT=JSON SELECT 1"
        );
        assert_eq!(
            explain_sql(Driver::Sqlite, "SELECT 1"),
            "EXPLAIN QUERY PLAN SELECT 1"
        );
        assert_eq!(
            explain_sql(Driver::Postgres, "explain analyze select 1"),
            "explain analyze select 1",
            "one the user wrote is theirs"
        );
    }
}
