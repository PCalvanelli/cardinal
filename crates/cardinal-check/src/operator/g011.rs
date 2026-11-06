//! G011: a filter drops rows because of nulls, where the SQL suggests it means to keep
//! them. Two forms, both decidable from the plan and the nullability facts:
//!
//! - `x NOT IN (subquery)` when the subquery's column can be null. One null in the
//!   subquery makes `NOT IN` unknown for every row, so the filter keeps no rows. Also
//!   `NOT IN (..., NULL)` with a null in the list.
//! - A WHERE condition on the columns an outer join fills with nulls, such as
//!   `left join b ... where b.status = 'x'`. The condition is unknown on the rows with no
//!   match, so it drops exactly the rows the outer join was written to keep.
//!
//! Conditions that test for nulls on purpose (`is null`, `is not null`, `coalesce`,
//! `or ... is null`) are not reported.

use crate::{Check, CheckContext};
use cardinal_diag::{Code, Diagnostic, Fix, Label, normalize_text};
use cardinal_grain::{AnalyzedModel, col_index};
use cardinal_sql::SqlSpan;
use cardinal_sql::datafusion_common::Column;
use cardinal_sql::datafusion_common::tree_node::{TreeNode, TreeNodeRecursion};
use cardinal_sql::datafusion_expr::expr::{Alias, InSubquery};
use cardinal_sql::datafusion_expr::logical_plan::{Filter, Join, JoinType};
use cardinal_sql::datafusion_expr::utils::split_conjunction;
use cardinal_sql::datafusion_expr::{BinaryExpr, Expr, LogicalPlan, Operator};

pub struct FilterDropsNulls;

impl Check for FilterDropsNulls {
    fn code(&self) -> Code {
        Code::G011
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let _ = model.plan.apply_with_subqueries(|p| {
            if let LogicalPlan::Filter(f) = p {
                not_in(cx, model, f, out);
                outer_join_turned_inner(cx, model, f, out);
            }
            Ok(TreeNodeRecursion::Continue)
        });
    }
}

/// The expression under any aliases and casts.
fn uncast(e: &Expr) -> &Expr {
    match e {
        Expr::Alias(Alias { expr, .. }) => uncast(expr),
        Expr::Cast(c) => uncast(&c.expr),
        Expr::TryCast(c) => uncast(&c.expr),
        other => other,
    }
}

fn span_of(c: &Column) -> Option<SqlSpan> {
    c.spans().first().and_then(|s| SqlSpan::from_df(&s))
}

/// The first column `e` reads, and where it is written.
fn first_column(e: &Expr) -> Option<&Column> {
    let mut found = None;
    let _ = e.apply(|x| {
        if let Expr::Column(c) = x {
            found = Some(c);
            return Ok(TreeNodeRecursion::Stop);
        }
        Ok(TreeNodeRecursion::Continue)
    });
    found
}

fn not_in(cx: &CheckContext<'_>, model: &AnalyzedModel, f: &Filter, out: &mut Vec<Diagnostic>) {
    let _ = f.predicate.apply(|e| {
        match e {
            Expr::InSubquery(InSubquery {
                expr,
                subquery,
                negated: true,
            }) => {
                let sub = &subquery.subquery;
                // Facts are recorded for every IN subquery; without them, stay quiet.
                let nullable = model
                    .facts(sub)
                    .is_some_and(|facts| !facts.not_null.contains(&0));
                if nullable {
                    let col = sub
                        .schema()
                        .fields()
                        .first()
                        .map(|f| f.name().clone())
                        .unwrap_or_default();
                    push_not_in(cx, model, expr, &format!("the subquery's {col}"), out);
                }
            }
            Expr::InList(l)
                if l.negated
                    && l.list
                        .iter()
                        .any(|x| matches!(uncast(x), Expr::Literal(v, _) if v.is_null())) =>
            {
                push_not_in(cx, model, &l.expr, "the list", out);
            }
            _ => {}
        }
        Ok(TreeNodeRecursion::Continue)
    });
}

fn push_not_in(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    outer: &Expr,
    what: &str,
    out: &mut Vec<Diagnostic>,
) {
    let col = first_column(outer);
    let name = col.map(|c| c.name.clone()).unwrap_or_else(|| "it".into());
    let span = col.and_then(span_of);
    out.push(Diagnostic {
        code: Code::G011,
        severity: Code::G011.default_severity(),
        relation: model.relation.0.clone(),
        message: "NOT IN keeps no rows when the values it checks include a null".into(),
        primary: cx.locator.label(
            &model.relation,
            span,
            format!("if {what} has a null, `{name} not in (...)` is never true"),
        ),
        secondary: vec![],
        notes: vec![format!(
            "nothing shows that {what} is never null: no not_null test, grain or filter covers it"
        )],
        fix: Some(Fix {
            message: "use NOT EXISTS, or add `where ... is not null` to the subquery".into(),
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&format!("not in {name} {what}")),
    });
}

/// Columns a condition compares directly, so the condition is unknown (and the row
/// dropped) when the column is null.
fn null_rejected(e: &Expr) -> Vec<&Column> {
    fn col(x: &Expr) -> Option<&Column> {
        match uncast(x) {
            Expr::Column(c) => Some(c),
            _ => None,
        }
    }
    match uncast(e) {
        Expr::BinaryExpr(BinaryExpr {
            left,
            op:
                Operator::Eq
                | Operator::NotEq
                | Operator::Lt
                | Operator::LtEq
                | Operator::Gt
                | Operator::GtEq
                | Operator::LikeMatch
                | Operator::ILikeMatch
                | Operator::NotLikeMatch
                | Operator::NotILikeMatch,
            right,
        }) => col(left).into_iter().chain(col(right)).collect(),
        Expr::Like(l) => col(&l.expr).into_iter().collect(),
        Expr::SimilarTo(l) => col(&l.expr).into_iter().collect(),
        Expr::InList(l) => col(&l.expr).into_iter().collect(),
        Expr::Between(b) => col(&b.expr).into_iter().collect(),
        Expr::Not(inner) => null_rejected(inner),
        _ => vec![],
    }
}

/// The outer join that fills column `idx` of `plan` with nulls on rows with no match,
/// looking through aliases, projections, sorts, limits and inner joins.
fn null_filling_join(plan: &LogicalPlan, idx: usize) -> Option<&Join> {
    match plan {
        LogicalPlan::SubqueryAlias(s) => null_filling_join(&s.input, idx),
        LogicalPlan::Sort(s) => null_filling_join(&s.input, idx),
        LogicalPlan::Limit(l) => null_filling_join(&l.input, idx),
        LogicalPlan::Projection(p) => match uncast(p.expr.get(idx)?) {
            Expr::Column(c) => {
                let i = col_index(p.input.schema(), c)?;
                null_filling_join(&p.input, i)
            }
            _ => None,
        },
        LogicalPlan::Join(j) => {
            let lw = j.left.schema().fields().len();
            let right = idx >= lw;
            let filled = match j.join_type {
                JoinType::Left => right,
                JoinType::Right => !right,
                JoinType::Full => true,
                _ => false,
            };
            if filled && idx < j.schema.fields().len() {
                return Some(j);
            }
            match j.join_type {
                JoinType::Inner | JoinType::Left | JoinType::Right | JoinType::Full => {
                    if right {
                        null_filling_join(&j.right, idx - lw)
                    } else {
                        null_filling_join(&j.left, idx)
                    }
                }
                JoinType::LeftSemi | JoinType::LeftAnti => null_filling_join(&j.left, idx),
                JoinType::RightSemi | JoinType::RightAnti => null_filling_join(&j.right, idx),
                _ => None,
            }
        }
        _ => None,
    }
}

fn outer_join_turned_inner(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    f: &Filter,
    out: &mut Vec<Diagnostic>,
) {
    let schema = f.input.schema();
    let mut reported: Vec<usize> = Vec::new();
    for conjunct in split_conjunction(&f.predicate) {
        for c in null_rejected(conjunct) {
            let Some(i) = col_index(schema, c) else {
                continue;
            };
            let Some(j) = null_filling_join(&f.input, i) else {
                continue;
            };
            let node = j as *const Join as usize;
            if reported.contains(&node) {
                continue;
            }
            reported.push(node);
            let record = model.joins.iter().find(|r| r.node == node);
            let (kind, filled, kept) = match (j.join_type, record) {
                (JoinType::Left, Some(r)) => ("left", r.right.clone(), r.left.clone()),
                (JoinType::Right, Some(r)) => ("right", r.left.clone(), r.right.clone()),
                (JoinType::Full, Some(r)) => (
                    "full",
                    format!("{} or {}", r.left, r.right),
                    "either side".into(),
                ),
                (JoinType::Left, None) => ("left", "the right side".into(), "the left side".into()),
                (JoinType::Right, None) => {
                    ("right", "the left side".into(), "the right side".into())
                }
                _ => ("full", "either side".into(), "either side".into()),
            };
            let span = span_of(c);
            let secondary: Vec<Label> = record
                .and_then(|r| r.span)
                .map(|s| {
                    cx.locator.label(
                        &model.relation,
                        Some(s),
                        format!("{kind} join: {filled}'s columns are null on rows with no match"),
                    )
                })
                .into_iter()
                .collect();
            out.push(Diagnostic {
                code: Code::G011,
                severity: Code::G011.default_severity(),
                relation: model.relation.0.clone(),
                message: format!("WHERE on {filled}'s columns turns the {kind} join into an inner join"),
                primary: cx.locator.label(
                    &model.relation,
                    span,
                    format!("`{}` is null for rows of {kept} with no match, so this drops them", c.name),
                ),
                secondary,
                notes: vec![format!(
                    "the {kind} join keeps rows of {kept} that have no match; this condition is unknown on them, and WHERE drops unknown rows"
                )],
                fix: Some(Fix {
                    message: "move the condition into the join's ON clause to keep unmatched rows, or write an inner join if dropping them is intended".into(),
                    replacement: None,
                }),
                evidence: vec![],
                fingerprint: normalize_text(&format!("{kind} join where {conjunct}")),
            });
        }
    }
}
