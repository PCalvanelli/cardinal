//! G013: `count(*)` over an outer join counts the preserved side's unmatched rows as 1.
//!
//! `customers left join orders ... group by customer_id` with `count(*)` gives 1, not 0,
//! for a customer with no orders: the left join keeps that customer as one row, with
//! nulls for the order. Reported when the aggregate reads the outer join (through
//! projections, aliases and sorts), the side filled with nulls repeats per preserved row
//! (so the count is meant to count its rows), every GROUP BY column comes from the
//! preserved side, and the count counts every row: `count(*)`, `count(1)`, or a column
//! of the preserved side. `count(o.order_id)` skips the nulls and is correct.

use crate::{Check, CheckContext};
use cardinal_diag::{Code, Diagnostic, Fix, Label, normalize_text};
use cardinal_grain::{AnalyzedModel, col_index, unalias};
use cardinal_sql::TokenKind;
use cardinal_sql::datafusion_common::tree_node::TreeNodeRecursion;
use cardinal_sql::datafusion_expr::expr::AggregateFunction;
use cardinal_sql::datafusion_expr::logical_plan::{Aggregate, Filter, Join, JoinType};
use cardinal_sql::datafusion_expr::utils::split_conjunction;
use cardinal_sql::datafusion_expr::{BinaryExpr, Expr, LogicalPlan, Operator};
use cardinal_sql::{SourceText, SqlSpan, Tokens};

pub struct CountOuterJoin;

impl Check for CountOuterJoin {
    fn code(&self) -> Code {
        Code::G013
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let mut used: Vec<usize> = Vec::new();
        let _ = model.plan.apply_with_subqueries(|p| {
            if let LogicalPlan::Aggregate(a) = p {
                check_aggregate(cx, model, a, &mut used, out);
            }
            Ok(TreeNodeRecursion::Continue)
        });
    }
}

/// The join below `plan` through projections, aliases, sorts and filters, with the
/// filters passed on the way.
fn join_below<'a>(plan: &'a LogicalPlan, filters: &mut Vec<&'a Filter>) -> Option<&'a Join> {
    match plan {
        LogicalPlan::Join(j) => Some(j),
        LogicalPlan::Projection(p) => join_below(&p.input, filters),
        LogicalPlan::SubqueryAlias(s) => join_below(&s.input, filters),
        LogicalPlan::Sort(s) => join_below(&s.input, filters),
        LogicalPlan::Filter(f) => {
            filters.push(f);
            join_below(&f.input, filters)
        }
        _ => None,
    }
}

/// The position in `join`'s output that column `idx` of `plan` copies, following plain
/// columns down to it.
fn trace(plan: &LogicalPlan, idx: usize, join: &Join) -> Option<usize> {
    match plan {
        LogicalPlan::Join(j) if std::ptr::eq(j, join) => Some(idx),
        LogicalPlan::Projection(p) => match unalias(p.expr.get(idx)?) {
            Expr::Column(c) => trace(&p.input, col_index(p.input.schema(), c)?, join),
            _ => None,
        },
        LogicalPlan::SubqueryAlias(s) => trace(&s.input, idx, join),
        LogicalPlan::Sort(s) => trace(&s.input, idx, join),
        LogicalPlan::Filter(f) => trace(&f.input, idx, join),
        _ => None,
    }
}

/// Which side of `join` each column `e` reads comes from: Some(true) when all come from
/// the right, Some(false) when all from the left, None when mixed or untraceable.
fn side_of(plan: &LogicalPlan, e: &Expr, join: &Join) -> Option<Option<bool>> {
    let lw = join.left.schema().fields().len();
    let mut side = None;
    for c in e.column_refs() {
        let i = col_index(plan.schema(), c)?;
        let right = trace(plan, i, join)? >= lw;
        if side.is_some_and(|s| s != right) {
            return None;
        }
        side = Some(right);
    }
    Some(side)
}

fn check_aggregate(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    a: &Aggregate,
    used: &mut Vec<usize>,
    out: &mut Vec<Diagnostic>,
) {
    let mut filters = Vec::new();
    let Some(j) = join_below(&a.input, &mut filters) else {
        return;
    };
    let filled_right = match j.join_type {
        JoinType::Left => true,
        JoinType::Right => false,
        _ => return,
    };
    let Some(record) = model
        .joins
        .iter()
        .find(|r| r.node == j as *const Join as usize)
    else {
        return;
    };
    // The filled side must repeat per preserved row, so the count means its rows.
    let filled_unique = record.sides[usize::from(filled_right)].unique;
    if record.cardinality.is_none() || filled_unique {
        return;
    }
    // A WHERE on the filled side drops the unmatched rows already (G011's to report).
    for f in &filters {
        for c in f.predicate.column_refs() {
            let Some(i) = col_index(f.input.schema(), c) else {
                return;
            };
            match trace(&f.input, i, j) {
                Some(i) if (i >= j.left.schema().fields().len()) == filled_right => return,
                Some(_) => {}
                None => return,
            }
        }
    }
    // Groups must be preserved rows (or one group of all rows).
    for g in &a.group_expr {
        if side_of(&a.input, g, j) != Some(Some(!filled_right)) {
            return;
        }
    }
    let (kept, filled) = if filled_right {
        (&record.left, &record.right)
    } else {
        (&record.right, &record.left)
    };
    let text = model.text();
    for e in &a.aggr_expr {
        let Expr::AggregateFunction(AggregateFunction { func, params }) = unalias(e) else {
            continue;
        };
        if !func.name().eq_ignore_ascii_case("count") || params.distinct || params.args.len() != 1 {
            continue;
        }
        let arg = unalias(&params.args[0]);
        let (span, written) = match arg {
            Expr::Literal(v, _) if !v.is_null() => {
                let span = literal_count_span(&text, used);
                (span, span.map(|s| text.slice(s).to_string()))
            }
            Expr::Column(c) if side_of(&a.input, arg, j) == Some(Some(!filled_right)) => {
                let span = c
                    .spans()
                    .first()
                    .and_then(|s| SqlSpan::from_df(&s))
                    .and_then(|s| text.enclosing_call(s, "count"));
                (span, span.map(|s| text.slice(s).to_string()))
            }
            _ => continue,
        };
        let written = written.unwrap_or_else(|| "count(*)".into());
        let instead = filled_column(j, filled_right)
            .map(|c| format!("count({c})"))
            .unwrap_or_else(|| "count(<a column of the joined side>)".into());
        let kind = if filled_right { "left" } else { "right" };
        let secondary: Vec<Label> = record
            .span
            .map(|s| {
                cx.locator.label(
                    &model.relation,
                    Some(s),
                    format!("{kind} join keeps {kept} rows with no {filled} row"),
                )
            })
            .into_iter()
            .collect();
        out.push(Diagnostic {
            code: Code::G013,
            severity: Code::G013.default_severity(),
            relation: model.relation.clone(),
            message: format!(
                "{written} counts 1, not 0, for {kept} rows with no {filled} row"
            ),
            primary: cx.locator.label(
                &model.relation,
                span,
                format!("counts rows of the {kind} join, including unmatched {kept} rows"),
            ),
            secondary,
            notes: vec![format!(
                "the {kind} join keeps each {kept} row with no match once, with nulls for {filled}; {written} counts that row"
            )],
            fix: Some(Fix {
                message: format!(
                    "count a column of {filled} that is null on unmatched rows: {instead}"
                ),
                replacement: None,
            }),
            evidence: vec![],
            fingerprint: normalize_text(&format!("{written} {kind} join {kept} {filled}")),
        });
    }
}

/// The filled side's first join column, as written (`o.customer_id`): from USING/ON
/// pairs, or the first equality between the two sides in the ON condition.
fn filled_column(j: &Join, filled_right: bool) -> Option<String> {
    let filled = if filled_right { &j.right } else { &j.left };
    let on_filled = |e: &Expr| match unalias(e) {
        Expr::Column(c) if col_index(filled.schema(), c).is_some() => Some(c.flat_name()),
        _ => None,
    };
    let pairs = j.on.iter().map(|(l, r)| (l, r));
    let conds = j
        .filter
        .iter()
        .flat_map(split_conjunction)
        .filter_map(|c| match c {
            Expr::BinaryExpr(BinaryExpr {
                left,
                op: Operator::Eq,
                right,
            }) => Some((left.as_ref(), right.as_ref())),
            _ => None,
        });
    pairs
        .chain(conds)
        .find_map(|(a, b)| on_filled(a).or_else(|| on_filled(b)))
}

/// The next `count(*)` or `count(<number>)` in the SQL not yet used by another finding.
fn literal_count_span(text: &SourceText<'_>, used: &mut Vec<usize>) -> Option<SqlSpan> {
    let t = Tokens::new(text.text);
    for i in t.words("count") {
        let at = t.get(i).start;
        if used.contains(&at) {
            continue;
        }
        let Some(arg) = t
            .next(i)
            .filter(|&o| t.is_punct(o, '('))
            .and_then(|o| t.next(o))
        else {
            continue;
        };
        let literal = t.is_punct(arg, '*')
            || (t.get(arg).kind == TokenKind::Number
                && t.text(arg).bytes().all(|b| b.is_ascii_digit()));
        let Some(close) = t.next(arg).filter(|&c| t.is_punct(c, ')')) else {
            continue;
        };
        if literal {
            used.push(at);
            return Some(text.span(at, t.get(close).end));
        }
    }
    None
}
