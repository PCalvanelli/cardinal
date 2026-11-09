//! G007: a window function whose result depends on row order is ordered by columns that
//! do not identify a row within the partition, so ties are broken arbitrarily.
//!
//! For `row_number() over (partition by P order by O)`: fire when P ∪ O is not a superkey
//! of the window's input. A dedup on that row_number (`= 1`) then keeps an arbitrary row
//! among the ties, which can change from run to run. lag and lead are judged the same
//! way. first_value, last_value and nth_value only fire when the tied rows can disagree on
//! the argument, i.e. P ∪ O does not determine it.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_grain::{AnalyzedModel, ColSet, RelationFacts, col_index, unalias};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_common::tree_node::{TreeNode, TreeNodeRecursion};
use cardinal_sql::datafusion_expr::expr::WindowFunction;
use cardinal_sql::datafusion_expr::{Expr, LogicalPlan};
use cardinal_sql::{SourceText, SqlSpan};

pub struct WindowOrderTies;

/// Window functions whose value for a row depends on where ties land.
const ORDER_SENSITIVE: &[&str] = &["row_number", "lag", "lead"];
/// Window functions that only read a value, so ties with equal values are harmless.
const VALUE_PICKING: &[&str] = &["first_value", "last_value", "nth_value"];

impl Check for WindowOrderTies {
    fn code(&self) -> Code {
        Code::G007
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let text = model.text();
        let _ = model.plan.apply(|node| {
            if let LogicalPlan::Window(w) = node
                && let Some(input) = model.facts(&w.input)
                // Precision over recall: only judge inputs whose grain is fully known.
                && input.status.is_known()
                && !input.keys.is_empty()
            {
                for e in &w.window_expr {
                    if let Expr::WindowFunction(wf) = unalias(e)
                        && let Some(d) = check(cx, model, &text, wf, w.input.schema(), input)
                    {
                        out.push(d);
                    }
                }
            }
            Ok(TreeNodeRecursion::Continue)
        });
    }
}

fn plain_col(schema: &DFSchema, e: &Expr) -> Option<usize> {
    match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        _ => None,
    }
}

fn names(schema: &DFSchema, cols: &ColSet) -> Vec<String> {
    cols.iter()
        .map(|c| schema.field(*c).name().clone())
        .collect()
}

fn check(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    wf: &WindowFunction,
    schema: &DFSchema,
    input: &RelationFacts,
) -> Option<Diagnostic> {
    let name = wf.fun.name().to_ascii_lowercase();
    let value_picking = VALUE_PICKING.contains(&name.as_str());
    if !value_picking && !ORDER_SENSITIVE.contains(&name.as_str()) {
        return None;
    }
    // A partition by an expression is not judged: its ties cannot be named.
    let partition: ColSet = wf
        .params
        .partition_by
        .iter()
        .map(|p| plain_col(schema, p))
        .collect::<Option<_>>()?;
    // Only plain columns break ties: date_trunc('day', ts) can tie where ts does not.
    let order: ColSet = wf
        .params
        .order_by
        .iter()
        .filter_map(|s| plain_col(schema, &s.expr))
        .collect();
    let x: ColSet = partition.union(&order).copied().collect();
    if input.is_superkey(&x) {
        return None;
    }
    if value_picking {
        let mut args = ColSet::new();
        for a in wf.params.args.iter().take(1) {
            for c in a.column_refs() {
                args.insert(col_index(schema, c)?);
            }
        }
        if args.is_subset(&input.closure(&x)) {
            return None;
        }
    }

    // The tie-breaker to suggest: the fewest key columns that x does not determine.
    let reach = input.closure(&x);
    let (key, add) = input
        .keys
        .iter()
        .map(|k| (k, k.cols.difference(&reach).copied().collect::<ColSet>()))
        .min_by_key(|(k, add)| (add.len(), k.cols.len()))?;

    let span = over_span(text, wf);
    let p = tuple(&names(schema, &partition));
    let o = tuple(&names(schema, &order));
    let within = if partition.is_empty() {
        String::new()
    } else {
        format!(" within PARTITION BY {p}")
    };
    let label = if wf.params.order_by.is_empty() {
        format!("no ORDER BY, so rows{within} come in any order")
    } else if order.is_empty() {
        format!("the ORDER BY expressions can tie{within}")
    } else {
        format!("several rows can share the same {o}{within}")
    };
    let mut notes = vec![format!(
        "the window reads one row per {}: {}",
        tuple(&names(schema, &key.cols)),
        model.arena.explain(key.why)
    )];
    notes.push(match name.as_str() {
        "row_number" => "tied rows are numbered in any order, so a dedup on this row_number keeps a random one, and a rerun can keep a different one".into(),
        "lag" | "lead" => format!("{name}() reads the next or previous row, and among tied rows that can be any of them"),
        _ => format!("tied rows can hold different values, so {name}() can return any of them"),
    });
    let add_names = names(schema, &add).join(", ");
    Some(Diagnostic {
        code: Code::G007,
        severity: Code::G007.default_severity(),
        relation: model.relation.clone(),
        message: format!("{name}() ordering can tie"),
        primary: cx.locator.label(&model.relation, span, label),
        secondary: vec![],
        notes,
        fix: Some(Fix {
            message: format!("add {add_names} to the ORDER BY as a tie-breaker"),
            replacement: None,
        }),
        evidence: vec![
            format!(
                "{} doesn't identify one row of the window's input",
                tuple(&names(schema, &x))
            ),
            model.arena.explain(key.why),
        ],
        fingerprint: match span {
            Some(s) => normalize_text(text.slice(s)),
            None => normalize_text(&format!("{name} {p} {o}")),
        },
    })
}

/// `row_number() over (...)`, found from a column inside the OVER clause.
fn over_span(text: &SourceText<'_>, wf: &WindowFunction) -> Option<SqlSpan> {
    let inner = wf
        .params
        .partition_by
        .iter()
        .chain(wf.params.order_by.iter().map(|s| &s.expr))
        .flat_map(|e| e.column_refs())
        .find_map(|c| c.spans().first())
        .and_then(|s| SqlSpan::from_df(&s))?;
    let over = text.enclosing_call(inner, "over")?;
    // Extend back over the call before OVER: `row_number()` or `lag(status, 1)`.
    Some(text.with_call_before(over))
}
