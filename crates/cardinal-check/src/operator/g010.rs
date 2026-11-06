//! G010: a set operation (UNION, UNION ALL) matches its branches' columns by position,
//! and a later branch puts a different column where the first branch has another name.
//!
//! An error when the later branch's column has the name of another of the first branch's
//! columns (the columns are swapped, so values land under the wrong name). A warning
//! when its name appears nowhere in the first branch (matched by position only: alias
//! it to say so). Unaliased expressions, such as a literal, take the first branch's name
//! when planned, so they are not reported.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use cardinal_grain::{AnalyzedModel, unalias};
use cardinal_sql::SqlSpan;
use cardinal_sql::datafusion_common::tree_node::TreeNodeRecursion;
use cardinal_sql::datafusion_expr::{Expr, LogicalPlan};

pub struct SetOpByPosition;

impl Check for SetOpByPosition {
    fn code(&self) -> Code {
        Code::G010
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let _ = model.plan.apply_with_subqueries(|p| {
            if let LogicalPlan::Union(u) = p {
                check_union(cx, model, &u.inputs, out);
            }
            Ok(TreeNodeRecursion::Continue)
        });
    }
}

fn names(plan: &LogicalPlan) -> Vec<String> {
    plan.schema()
        .fields()
        .iter()
        .map(|f| f.name().to_lowercase())
        .collect()
}

fn check_union(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    inputs: &[std::sync::Arc<LogicalPlan>],
    out: &mut Vec<Diagnostic>,
) {
    let Some(first) = inputs.first() else {
        return;
    };
    let first = names(first);
    for (b, branch) in inputs.iter().enumerate().skip(1) {
        for (j, name) in names(branch).iter().enumerate() {
            let Some(expected) = first.get(j) else {
                break;
            };
            if name == expected || !is_identifier(name) {
                continue;
            }
            let swapped = first.iter().any(|f| f == name);
            let (severity, label, fix) = if swapped {
                (
                    Severity::Error,
                    format!("`{name}` lands in the `{expected}` column"),
                    format!(
                        "list the columns in the first branch's order {}",
                        tuple(&first)
                    ),
                )
            } else {
                (
                    Code::G010.default_severity(),
                    format!("`{name}` goes into the `{expected}` column by position"),
                    format!("alias it to the first branch's name: {name} as {expected}"),
                )
            };
            let mut notes = vec![format!(
                "the first branch's columns are {}; branch {} lists `{name}` at position {}",
                tuple(&first),
                b + 1,
                j + 1
            )];
            if swapped {
                notes.push(format!(
                    "the first branch has `{name}` at another position, so these values end up under the wrong name"
                ));
            }
            out.push(Diagnostic {
                code: Code::G010,
                severity,
                relation: model.relation.0.clone(),
                message: Code::G010.title().into(),
                primary: cx
                    .locator
                    .label(&model.relation, column_span(branch, j), label),
                secondary: vec![],
                notes,
                fix: Some(Fix {
                    message: fix,
                    replacement: None,
                }),
                evidence: vec![],
                fingerprint: normalize_text(&format!("union {} {name} {expected}", b + 1)),
            });
        }
    }
}

/// Where the branch's j-th column is written, when it is a column reference.
fn column_span(branch: &LogicalPlan, j: usize) -> Option<SqlSpan> {
    match branch {
        LogicalPlan::Projection(p) => match unalias(p.expr.get(j)?) {
            Expr::Column(c) => c.spans().first().and_then(|s| SqlSpan::from_df(&s)),
            _ => None,
        },
        LogicalPlan::SubqueryAlias(s) => column_span(&s.input, j),
        LogicalPlan::Distinct(d) => column_span(d.input(), j),
        _ => None,
    }
}

/// A plain name, as opposed to the name the planner gives an unaliased expression.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
