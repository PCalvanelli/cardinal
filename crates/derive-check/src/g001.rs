//! G001: aggregation fans out.
//!
//! For an aggregate with group-by columns G over input I, and each non-idempotent
//! aggregate over a column c: fire if some minimal determinant D of c in I makes
//! D ∪ G not a superkey of I. Rows sharing D then repeat c within a group, and the
//! aggregate overcounts.

use crate::{Check, CheckContext, tuple};
use derive_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use derive_grain::{AnalyzedModel, ColSet, RelationFacts};
use derive_sql::datafusion_common::DFSchema;
use derive_sql::datafusion_common::tree_node::{TreeNode, TreeNodeRecursion};
use derive_sql::datafusion_expr::expr::AggregateFunction;
use derive_sql::datafusion_expr::{Expr, LogicalPlan, WindowFunctionDefinition};
use derive_sql::provider::{FunctionOrigin, Functions};
use derive_sql::{SourceText, SqlSpan};

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

const NON_IDEMPOTENT: &[&str] = &[
    "sum",
    "avg",
    "mean",
    "count",
    "var",
    "var_samp",
    "var_pop",
    "variance",
    "variance_samp",
    "variance_pop",
    "stddev",
    "stddev_samp",
    "stddev_pop",
    "string_agg",
    "array_agg",
    "listagg",
];
const IDEMPOTENT: &[&str] = &["min", "max", "any_value", "bool_and", "bool_or"];

pub fn classify(name: &str, distinct: bool, functions: &Functions) -> AggClass {
    let n = name.to_ascii_lowercase();
    if distinct && n == "count" {
        return AggClass::Idempotent;
    }
    if IDEMPOTENT.contains(&n.as_str()) {
        return AggClass::Idempotent;
    }
    if NON_IDEMPOTENT.contains(&n.as_str()) {
        return AggClass::NonIdempotent;
    }
    match functions.aggregate_stubs.get(&n) {
        Some((_, true)) => AggClass::Idempotent,
        Some((FunctionOrigin::AdapterStub | FunctionOrigin::ConfigStub, false)) => {
            AggClass::NonIdempotent
        }
        _ => AggClass::Unknown,
    }
}

struct Site<'a> {
    name: String,
    distinct: bool,
    args: &'a [Expr],
    group: ColSet,
    input: &'a LogicalPlan,
}

fn col_positions(schema: &DFSchema, exprs: &[Expr]) -> Option<ColSet> {
    let mut out = ColSet::new();
    for e in exprs {
        for c in e.column_refs() {
            out.insert(schema.maybe_index_of_column(c)?);
        }
    }
    Some(out)
}

fn is_count_star(name: &str, args: &[Expr]) -> bool {
    name == "count" && args.iter().all(|a| a.column_refs().is_empty())
}

impl Check for FanOutAggregation {
    fn code(&self) -> Code {
        Code::G001
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let mut sites: Vec<Site<'_>> = Vec::new();
        let _ = model.plan.apply(|node| {
            match node {
                LogicalPlan::Aggregate(a) => {
                    let schema = a.input.schema();
                    let Some(group) = col_positions(schema, &a.group_expr) else {
                        return Ok(TreeNodeRecursion::Continue);
                    };
                    for e in &a.aggr_expr {
                        if let Expr::AggregateFunction(AggregateFunction { func, params }) =
                            unalias(e)
                        {
                            sites.push(Site {
                                name: func.name().to_string(),
                                distinct: params.distinct,
                                args: &params.args,
                                group: group.clone(),
                                input: &a.input,
                            });
                        }
                    }
                }
                LogicalPlan::Window(w) => {
                    let schema = w.input.schema();
                    for e in &w.window_expr {
                        if let Expr::WindowFunction(wf) = unalias(e)
                            && let WindowFunctionDefinition::AggregateUDF(u) = &wf.fun
                            && let Some(group) = col_positions(schema, &wf.params.partition_by)
                        {
                            sites.push(Site {
                                name: u.name().to_string(),
                                distinct: wf.params.distinct,
                                args: &wf.params.args,
                                group,
                                input: &w.input,
                            });
                        }
                    }
                }
                _ => {}
            }
            Ok(TreeNodeRecursion::Continue)
        });

        let text = model.text();
        for site in sites {
            if let Some(d) = check_site(cx, model, &text, &site) {
                out.push(d);
            }
        }
    }
}

fn unalias(e: &Expr) -> &Expr {
    match e {
        Expr::Alias(a) => unalias(&a.expr),
        o => o,
    }
}

fn names(schema: &DFSchema, cols: &ColSet) -> Vec<String> {
    cols.iter()
        .map(|c| schema.field(*c).name().clone())
        .collect()
}

fn check_site(
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
    let args = col_positions(schema, site.args)?;
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
        .filter(|r| schema.maybe_index_of_column(r) == Some(c))
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
        format!("{col} is determined by {det}"),
    );
    let mut secondary = Vec::new();
    if let Some(j) = input.repeats.get(c).cloned().flatten() {
        secondary.push(cx.locator.label(&j.relation, j.span, j.message()));
    }
    let key = &input.keys[0];
    let key_names = tuple(&names(schema, &key.cols));
    let subject = input_name(site.input, cx);
    let why = match model.arena.declared_root(key.why) {
        Some((_, origin)) => format!("from {}", origin.describe()),
        None => format!("proven: {}", model.arena.explain(key.why)),
    };
    let mut notes = vec![format!("{subject} has grain {key_names}, {why}")];
    if class == AggClass::Unknown {
        notes.push(format!(
            "Derive does not know whether {}() is affected by duplicate rows; declare it in derive.toml with idempotent = true if it is not",
            site.name
        ));
    }
    let evidence = vec![
        format!("{col} is determined by {det}"),
        format!("{det} plus the GROUP BY columns is not a key of the aggregate input"),
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
                "{} {col} per {det} before the join, or read it from a relation at that grain",
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
            .downcast_ref::<derive_sql::DeriveTable>()
            .map(|t| {
                cx.locator
                    .display(&derive_dbt::RelationId(t.relation.clone()))
            })
            .unwrap_or_else(|| s.table_name.table().to_string()),
        LogicalPlan::Join(_) => "the joined rows".into(),
        _ => "the aggregate's input".into(),
    }
}
