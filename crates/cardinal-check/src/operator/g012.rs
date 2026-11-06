//! G012: `UNION` (without ALL) removes duplicate rows, and a branch can return the same
//! row twice for different underlying rows, so real rows are lost.
//!
//! Reported when a branch's grain is fully known and it has no key (it dropped the key of
//! what it reads, so two of its rows can be equal), and it returns a quantity: an
//! aggregate's result, a declared measure or amount, or a decimal or floating-point
//! column. Two payments of 10.00 by one customer become one row. A UNION of identifiers
//! (`select customer_id from a union select customer_id from b`) is a deliberate
//! de-duplication and is not reported.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_grain::{AnalyzedModel, RelationFacts};
use cardinal_sql::datafusion_common::arrow::datatypes::DataType;
use cardinal_sql::datafusion_common::tree_node::TreeNodeRecursion;
use cardinal_sql::datafusion_expr::LogicalPlan;
use cardinal_sql::datafusion_expr::logical_plan::{Distinct, Union};
use cardinal_sql::{SourceText, SqlSpan, Tokens};

pub struct UnionMergesRows;

impl Check for UnionMergesRows {
    fn code(&self) -> Code {
        Code::G012
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let mut nth = 0;
        let _ = model.plan.apply_with_subqueries(|p| {
            if let LogicalPlan::Distinct(Distinct::All(input)) = p
                && let Some((node, u)) = union_below(input)
            {
                check_union(cx, model, node, u, nth, out);
                nth += 1;
            }
            Ok(TreeNodeRecursion::Continue)
        });
    }
}

fn union_below(p: &LogicalPlan) -> Option<(&LogicalPlan, &Union)> {
    match p {
        LogicalPlan::Union(u) => Some((p, u)),
        LogicalPlan::SubqueryAlias(s) => union_below(&s.input),
        _ => None,
    }
}

/// Columns of `plan` that hold quantities: aggregate results, declared measures and
/// amounts, decimals and floats.
fn quantities(plan: &LogicalPlan, facts: &RelationFacts) -> Vec<String> {
    plan.schema()
        .fields()
        .iter()
        .enumerate()
        .filter(|(j, f)| {
            facts.aggregated.contains(j)
                || facts.measures.iter().any(|m| m.col == *j)
                || facts.units.iter().any(|u| u.col == *j)
                || matches!(
                    f.data_type(),
                    DataType::Decimal32(..)
                        | DataType::Decimal64(..)
                        | DataType::Decimal128(..)
                        | DataType::Decimal256(..)
                        | DataType::Float16
                        | DataType::Float32
                        | DataType::Float64
                )
        })
        .map(|(_, f)| f.name().clone())
        .collect()
}

/// The key a branch dropped: the smallest key of what its projection reads.
fn dropped_key(model: &AnalyzedModel, branch: &LogicalPlan) -> Option<Vec<String>> {
    match branch {
        LogicalPlan::SubqueryAlias(s) => dropped_key(model, &s.input),
        LogicalPlan::Projection(p) => {
            let f = model.facts(&p.input)?;
            let k = f.keys.first()?;
            let schema = p.input.schema();
            Some(
                k.cols
                    .iter()
                    .map(|c| schema.field(*c).name().clone())
                    .collect(),
            )
        }
        _ => None,
    }
}

fn check_union(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    node: &LogicalPlan,
    u: &Union,
    nth: usize,
    out: &mut Vec<Diagnostic>,
) {
    // Rows that are already distinct lose nothing to the de-duplication.
    if model.facts(node).is_none_or(|f| !f.keys.is_empty()) {
        return;
    }
    for (b, branch) in u.inputs.iter().enumerate() {
        let Some(f) = model.facts(branch) else {
            continue;
        };
        if !f.status.is_known() || !f.keys.is_empty() {
            continue;
        }
        let qty = quantities(branch, f);
        if qty.is_empty() {
            continue;
        }
        let cols: Vec<String> = branch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        let from = f
            .within
            .first()
            .map(|w| cx.locator.display(&w.relation))
            .unwrap_or_else(|| "it".into());
        let key = dropped_key(model, branch);
        let mut notes = vec![format!(
            "branch {} returns {} from {from}, which can repeat: two different rows with the same values become one",
            b + 1,
            tuple(&cols)
        )];
        if let Some(k) = &key {
            notes.push(format!(
                "it drops {from}'s key {}, which kept those rows apart",
                tuple(k)
            ));
        }
        let fix = match &key {
            Some(k) => format!(
                "use UNION ALL to keep every row, or select {} so rows stay distinct",
                tuple(k)
            ),
            None => "use UNION ALL to keep every row".into(),
        };
        let text = model.text();
        out.push(Diagnostic {
            code: Code::G012,
            severity: Code::G012.default_severity(),
            relation: model.relation.0.clone(),
            message: format!(
                "UNION drops repeated rows, so equal {} values from different rows count once",
                qty.join(", ")
            ),
            primary: cx.locator.label(
                &model.relation,
                union_span(&text, nth),
                "UNION keeps one copy of each set of equal rows".into(),
            ),
            secondary: vec![],
            notes,
            fix: Some(Fix {
                message: fix,
                replacement: None,
            }),
            evidence: vec![],
            fingerprint: normalize_text(&format!("union {} {}", b + 1, cols.join(","))),
        });
        return;
    }
}

/// The `nth` UNION in the SQL that is not UNION ALL.
fn union_span(text: &SourceText<'_>, nth: usize) -> Option<SqlSpan> {
    let t = Tokens::new(text.text);
    t.words("union")
        .filter(|&i| !t.next(i).is_some_and(|n| t.is_word(n, "all")))
        .nth(nth)
        .map(|i| text.span(t.get(i).start, t.get(i).end))
}
