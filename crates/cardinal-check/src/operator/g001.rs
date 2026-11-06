//! G001: a join repeats values that are then added up.
//!
//! For an aggregate with group-by columns G over input I, and each non-idempotent
//! aggregate over a column c: fire if some minimal determinant D of c in I makes
//! D ∪ G not a superkey of I. Rows sharing D then repeat c within a group, and the
//! aggregate overcounts.

use crate::sites::{Site, aggregate_sites};
use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use cardinal_grain::{AnalyzedModel, ColSet, RelationFacts, aggregate, col_index, exprs_cols};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::{Expr, LogicalPlan};
use cardinal_sql::provider::{FunctionOrigin, Functions};
use cardinal_sql::{SourceText, SqlSpan};

pub struct FanOutAggregation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggClass {
    /// Duplicate rows cannot change the result.
    Idempotent,
    /// Duplicate rows change the result.
    NonIdempotent,
    /// Not known either way: treated as non-idempotent at warning severity.
    Unknown,
}

pub fn classify(name: &str, distinct: bool, functions: &Functions) -> AggClass {
    let n = name.to_ascii_lowercase();
    if distinct && n == "count" {
        return AggClass::Idempotent;
    }
    match aggregate(&n).and_then(|a| a.idempotent) {
        Some(true) => return AggClass::Idempotent,
        Some(false) => return AggClass::NonIdempotent,
        None => {}
    }
    match functions.aggregate_stubs.get(&n) {
        Some((_, true)) => AggClass::Idempotent,
        Some((FunctionOrigin::AdapterStub | FunctionOrigin::ConfigStub, false)) => {
            AggClass::NonIdempotent
        }
        _ => AggClass::Unknown,
    }
}

fn is_count_star(name: &str, args: &[Expr]) -> bool {
    name == "count" && args.iter().all(|a| a.column_refs().is_empty())
}

impl Check for FanOutAggregation {
    fn code(&self) -> Code {
        Code::G001
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let sites = aggregate_sites(&model.plan);
        let text = model.text();
        for site in sites {
            if let Some(d) = check_site(cx, model, &text, &site) {
                out.push(d);
            }
        }
    }
}

fn names(schema: &DFSchema, cols: &ColSet) -> Vec<String> {
    cols.iter()
        .map(|c| schema.field(*c).name().clone())
        .collect()
}

pub(crate) fn check_site(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
) -> Option<Diagnostic> {
    if is_count_star(&site.name, site.args) {
        return None;
    }
    let class = classify(&site.name, site.distinct, cx.functions);
    if class == AggClass::Idempotent {
        return None;
    }
    let input: &RelationFacts = model.facts(site.input)?;
    // Precision over recall: only judge inputs whose grain is fully known.
    if !input.status.is_known() || input.keys.is_empty() {
        return None;
    }
    let schema = site.input.schema();
    let args = exprs_cols(site.args, schema)?;
    if args.is_empty() {
        return None;
    }
    let dets: Vec<(usize, Vec<ColSet>)> =
        args.iter().map(|c| (*c, input.determinants(*c))).collect();
    for (c, cdets) in &dets {
        // Other argument columns count with everything that determines them, which can
        // only make a superkey more likely: sum(qty * unit_price) is judged at line grain.
        let mut others = ColSet::new();
        for (o, odets) in &dets {
            if o != c {
                others.insert(*o);
                for d in odets {
                    others.extend(d.iter().copied());
                }
            }
        }
        for d in cdets {
            let mut x: ColSet = d.union(&site.group).copied().collect();
            x.extend(others.iter().copied());
            if input.is_superkey(&x) {
                continue;
            }
            return Some(build(cx, model, text, site, input, schema, *c, d, class));
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn build(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    input: &RelationFacts,
    schema: &DFSchema,
    c: usize,
    d: &ColSet,
    class: AggClass,
) -> Diagnostic {
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
