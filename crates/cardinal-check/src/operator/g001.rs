//! G001: a join repeats values that are then added up.
//!
//! For an aggregate with group-by columns G over input I, and each non-idempotent
//! aggregate over a column c: fire if some minimal determinant D of c in I makes
//! D ∪ G not a superkey of I. Rows sharing D then repeat c within a group, and the
//! aggregate overcounts.

use crate::sites::{AggClass, FanOut, Site, aggregate_sites};
use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use cardinal_grain::{AnalyzedModel, ColSet, col_index};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::LogicalPlan;
use cardinal_sql::{SourceText, SqlSpan};

pub struct FanOutAggregation;

impl Check for FanOutAggregation {
    fn code(&self) -> Code {
        Code::G001
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let text = model.text();
        for site in aggregate_sites(&model.plan) {
            if let Some(f) = site.fan_out(cx, model) {
                out.push(build(cx, model, &text, &site, &f));
            }
        }
    }
}

fn names(schema: &DFSchema, cols: &ColSet) -> Vec<String> {
    cols.iter()
        .map(|c| schema.field(*c).name().clone())
        .collect()
}

fn build(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    f: &FanOut<'_>,
) -> Diagnostic {
    let (input, schema, c, d, class) = (f.input, site.input.schema(), f.col, &f.det, f.class);
    let col = schema.field(c).name().clone();
    let det = tuple(&names(schema, d));
    let col_span: Option<SqlSpan> = site
        .args
        .iter()
        .flat_map(|a| a.column_refs())
        .filter(|r| col_index(schema, r) == Some(c))
        .find_map(|r| r.spans().first())
        .and_then(|s| SqlSpan::from_df(&s));
    let call = col_span
        .and_then(|s| text.enclosing_call(s, &site.name))
        .or(col_span);
    let fingerprint = match call {
        Some(s) => normalize_text(text.slice(s)),
        None => format!("{}({col})", site.name),
    };
    let primary = cx.locator.label(
        &model.relation,
        call,
        format!("{col} has one value per {det}"),
    );
    let mut secondary = Vec::new();
    if let Some(j) = input.repeats.get(c).cloned().flatten() {
        secondary.push(cx.locator.label(&j.relation, j.span, j.message()));
    }
    let key = &input.keys[0];
    let key_names = tuple(&names(schema, &key.cols));
    let subject = input_name(site.input, cx);
    let why = match model.arena.declared_root(key.why) {
        Some((_, origin)) => format!(" (declared by {})", origin.describe()),
        None => format!(": {}", model.arena.explain(key.why)),
    };
    let mut notes = vec![format!("{subject} has one row per {key_names}{why}")];
    if class == AggClass::Unknown {
        notes.push(format!(
            "Cardinal doesn't know whether repeated rows change {}(); if they don't, mark it idempotent = true in cardinal.toml",
            site.name
        ));
    }
    let evidence = vec![
        format!("{col} has one value per {det}"),
        format!("{det} plus the GROUP BY columns don't identify one input row"),
        model.arena.explain(key.why),
    ];
    Diagnostic {
        code: Code::G001,
        severity: if class == AggClass::Unknown {
            Severity::Warning
        } else {
            Severity::Error
        },
        relation: model.relation.0.clone(),
        message: Code::G001.title().into(),
        primary,
        secondary,
        notes,
        fix: Some(Fix {
            message: format!(
                "{} {col} per {det} before the join, or read it from a table with one row per {det}",
                site.name
            ),
            replacement: None,
        }),
        evidence,
        fingerprint,
    }
}

/// What the aggregate reads: an upstream model's name, or "the aggregate's input".
fn input_name(plan: &LogicalPlan, cx: &CheckContext<'_>) -> String {
    match plan {
        LogicalPlan::SubqueryAlias(s) => input_name(&s.input, cx),
        LogicalPlan::Filter(f) => input_name(&f.input, cx),
        LogicalPlan::Projection(p) => input_name(&p.input, cx),
        LogicalPlan::TableScan(s) => s
            .source
            .as_ref()
            .downcast_ref::<cardinal_sql::CardinalTable>()
            .map(|t| {
                cx.locator
                    .display(&cardinal_dbt::RelationId(t.relation.clone()))
            })
            .unwrap_or_else(|| s.table_name.table().to_string()),
        LogicalPlan::Join(_) => "the joined rows".into(),
        _ => "the aggregate's input".into(),
    }
}
