//! G008: distinct count is summed.
//!
//! A column `v = count(distinct x)` per group G1 carries `agree_on`: the G1 columns that
//! `x` determines, so two groups counting the same value agree on them. At `sum(v)` with
//! group-by columns G over input I: fire if `agree_on ∪ G` is not a superkey of I. Two
//! rows of one sum group can then come from different G1 groups that count the same
//! value, and the sum counts it once for each.
//!
//! G counts only plain column references. `group by date_trunc('month', day)` puts many
//! days in one group, so it must not count as `group by day`.

use crate::g001::check_site;
use crate::sites::{Site, aggregate_sites, unalias};
use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use cardinal_grain::{AnalyzedModel, ColSet, DistinctCount, RelationFacts};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::Expr;
use cardinal_sql::{SourceText, SqlSpan};

pub struct SummedDistinctCount;

impl Check for SummedDistinctCount {
    fn code(&self) -> Code {
        Code::G008
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let text = model.text();
        for site in aggregate_sites(&model.plan) {
            if let Some(d) = check(cx, model, &text, &site) {
                out.push(d);
            }
        }
    }
}

/// The column a `sum` argument adds up: `v`, `cast(v as …)` or `coalesce(v, <literal>)`.
/// Anything else (`case when v > 100 then 1 end`) is not a sum of `v`.
fn summed_col(e: &Expr, schema: &DFSchema) -> Option<usize> {
    match unalias(e) {
        Expr::Column(c) => schema.maybe_index_of_column(c),
        Expr::Cast(c) => summed_col(&c.expr, schema),
        Expr::TryCast(c) => summed_col(&c.expr, schema),
        Expr::ScalarFunction(f)
            if matches!(
                f.func.name().to_ascii_lowercase().as_str(),
                "coalesce" | "ifnull" | "nvl"
            ) && f.args.len() == 2
                && matches!(unalias(&f.args[1]), Expr::Literal(..)) =>
        {
            summed_col(&f.args[0], schema)
        }
        _ => None,
    }
}

/// Input positions of the GROUP BY / PARTITION BY expressions that are plain columns.
fn bare_group(schema: &DFSchema, exprs: &[Expr]) -> ColSet {
    exprs
        .iter()
        .filter_map(|e| match unalias(e) {
            Expr::Column(c) => schema.maybe_index_of_column(c),
            _ => None,
        })
        .collect()
}

fn check(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
) -> Option<Diagnostic> {
    if !site.name.eq_ignore_ascii_case("sum") || site.distinct || site.args.len() != 1 {
        return None;
    }
    let input: &RelationFacts = model.facts(site.input)?;
    if !input.status.is_known() || input.keys.is_empty() {
        return None;
    }
    let schema = site.input.schema();
    let c = summed_col(&site.args[0], schema)?;
    let fact = input.distinct.iter().find(|d| d.col == c)?;
    let mut x = bare_group(schema, site.group_exprs);
    x.extend(fact.agree_on.iter().copied());
    if input.is_superkey(&x) {
        return None;
    }
    // A join that repeats the counts is the bigger problem, and G001 reports it.
    if check_site(cx, model, text, site).is_some() {
        return None;
    }
    Some(build(cx, model, text, site, schema, c, fact))
}

/// How the sum groups its rows, as written: "(month)", "date_trunc('month', order_date)".
fn group_text(text: &SourceText<'_>, schema: &DFSchema, exprs: &[Expr]) -> Option<String> {
    let parts: Vec<String> = exprs
        .iter()
        .map(|e| match unalias(e) {
            Expr::Column(c) => c.name.clone(),
            Expr::ScalarFunction(f) => e
                .column_refs()
                .into_iter()
                .filter(|r| schema.maybe_index_of_column(r).is_some())
                .find_map(|r| r.spans().first())
                .and_then(|s| SqlSpan::from_df(&s))
                .and_then(|s| text.enclosing_call(s, f.func.name()))
                .map(|s| text.slice(s).to_string())
                .unwrap_or_else(|| e.schema_name().to_string()),
            other => other.schema_name().to_string(),
        })
        .collect();
    (!parts.is_empty()).then(|| tuple(&parts))
}

fn build(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    schema: &DFSchema,
    c: usize,
    fact: &DistinctCount,
) -> Diagnostic {
    let col = schema.field(c).name().clone();
    let s = &fact.site;
    let counted = s.counted.join(", ");
    let per = tuple(&s.per);
    let across = if s.across.is_empty() {
        per.clone()
    } else {
        s.across.join(", ")
    };
    let col_span: Option<SqlSpan> = site.args[0]
        .column_refs()
        .into_iter()
        .filter(|r| schema.maybe_index_of_column(r) == Some(c))
        .find_map(|r| r.spans().first())
        .and_then(|s| SqlSpan::from_df(&s));
    let call = col_span
        .and_then(|s| text.enclosing_call(s, &site.name))
        .or(col_span);
    let fingerprint = match call {
        Some(sp) => normalize_text(text.slice(sp)),
        None => format!("sum({col})"),
    };
    let primary = cx.locator.label(
        &model.relation,
        call,
        format!("adds up counts of distinct {counted} across {across}"),
    );
    let secondary = vec![cx.locator.label(
        &s.relation,
        s.span,
        format!("counts each {counted} once per {per}"),
    )];
    let group = group_text(text, schema, site.group_exprs);
    let fix = if site.window {
        format!("count each {counted} once, on its first {across}, and sum those instead")
    } else {
        match &group {
            Some(g) => format!(
                "count distinct {counted} per {g} from the rows before they are grouped by {per}"
            ),
            None => {
                format!("count distinct {counted} over the rows before they are grouped by {per}")
            }
        }
    };
    Diagnostic {
        code: Code::G008,
        severity: Severity::Warning,
        relation: model.relation.0.clone(),
        message: Code::G008.title().into(),
        primary,
        secondary,
        notes: vec![format!(
            "a {counted} that appears with more than one {across} is counted once for each"
        )],
        fix: Some(Fix {
            message: fix,
            replacement: None,
        }),
        evidence: vec![
            format!("{col} counts distinct {counted} per {per}"),
            format!("{counted} does not determine {across}"),
            format!(
                "{} plus the counts' shared columns is not a key of the sum's input",
                group.unwrap_or_else(|| "the sum's grouping".into())
            ),
        ],
        fingerprint,
    }
}
