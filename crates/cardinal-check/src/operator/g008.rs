//! G008: adds up a number that can't be added.
//!
//! A measure fact on the summed column says how it may be added (see `Measure`):
//!
//! - Non-additive with shared columns `agree_on` (distinct counts; empty for averages,
//!   ratios, percentiles and declared non-additive columns). At `sum(v)` with group-by
//!   columns G over input I: fire if `agree_on ∪ G` is not a superkey of I. Two rows of
//!   one sum group can then be different groups' values, and adding them is wrong (a
//!   distinct count counts a shared value once for each).
//! - A stock over T (balances, running totals): fire if `T ⊄ closure(G)` on I, so one
//!   sum group can hold rows from different T values. A stock whose T was projected
//!   away while it still varied ("over unknown") fires on any sum: nothing in I says
//!   the rows agree on T.
//! - `avg(v)` of an average: fire when G is not a superkey of I, since the groups' own
//!   averages merge without their weights.
//!
//! G counts only plain column references. `group by date_trunc('month', day)` puts many
//! days in one group, so it must not count as `group by day`.

use crate::operator::g001::check_site;
use crate::sites::{Site, aggregate_sites, bare_group};
use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Label, Severity, normalize_text};
use cardinal_grain::{
    AnalyzedModel, Computed, DistinctSite, Measure, MeasureKind, MeasureSite, RelationFacts,
    aggregate, col_index, summed_col, unalias,
};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::Expr;
use cardinal_sql::{SourceText, SqlSpan};

pub struct NonAdditiveSum;

impl Check for NonAdditiveSum {
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

fn check(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
) -> Option<Diagnostic> {
    let name = site.name.to_ascii_lowercase();
    let avg = aggregate(&name).is_some_and(|a| a.computed == Some(Computed::Average));
    if !(name == "sum" || avg) || site.distinct || site.args.len() != 1 {
        return None;
    }
    let input: &RelationFacts = model.facts(site.input)?;
    let schema = site.input.schema();
    let c = summed_col(&site.args[0], schema)?;
    let fact = input.measures.iter().find(|d| d.col == c)?;
    let group = bare_group(schema, site.group_exprs);
    match &fact.kind {
        MeasureKind::NonAdditive { agree_on } => {
            if avg && fact.computed() != Some(Computed::Average) {
                return None;
            }
            // Precision over recall: only judge inputs whose grain is fully known.
            if !input.status.is_known() || input.keys.is_empty() {
                return None;
            }
            let mut x = group;
            x.extend(agree_on.iter().copied());
            if input.is_superkey(&x) {
                return None;
            }
        }
        MeasureKind::Stock { over } => {
            // Averaging, or taking the latest, of a balance is fine.
            if avg
                || over
                    .as_ref()
                    .is_some_and(|o| o.is_subset(&input.closure(&group)))
            {
                return None;
            }
        }
    }
    // A join that repeats the values is the bigger problem, and G001 reports it.
    if check_site(cx, model, text, site).is_some() {
        return None;
    }
    let call = Call::new(text, site, schema, c);
    Some(match fact.distinct() {
        Some(d) => build_distinct(cx, model, text, site, schema, &call, d),
        None => build(cx, model, text, site, schema, &call, fact, avg),
    })
}

/// The aggregate call as written, and what to fingerprint it by.
pub(crate) struct Call {
    pub col: String,
    pub span: Option<SqlSpan>,
    pub fingerprint: String,
}

impl Call {
    pub(crate) fn new(text: &SourceText<'_>, site: &Site<'_>, schema: &DFSchema, c: usize) -> Self {
        let col = schema.field(c).name().clone();
        let col_span: Option<SqlSpan> = site.args[0]
            .column_refs()
            .into_iter()
            .filter(|r| col_index(schema, r) == Some(c))
            .find_map(|r| r.spans().first())
            .and_then(|s| SqlSpan::from_df(&s));
        let span = col_span
            .and_then(|s| text.enclosing_call(s, &site.name))
            .or(col_span);
        let fingerprint = match span {
            Some(sp) => normalize_text(text.slice(sp)),
            None => format!("{}({col})", site.name.to_ascii_lowercase()),
        };
        Call {
            col,
            span,
            fingerprint,
        }
    }

    pub(crate) fn text(&self, text: &SourceText<'_>, site: &Site<'_>) -> String {
        match self.span {
            Some(sp) => text.slice(sp).to_string(),
            None => format!("{}({})", site.name.to_ascii_lowercase(), self.col),
        }
    }
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
                .filter(|r| col_index(schema, r).is_some())
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

#[allow(clippy::too_many_arguments)]
fn build_distinct(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    schema: &DFSchema,
    call: &Call,
    s: &DistinctSite,
) -> Diagnostic {
    let col = &call.col;
    let counted = s.counted.join(", ");
    let per = tuple(&s.per);
    let across = if s.across.is_empty() {
        per.clone()
    } else {
        s.across.join(", ")
    };
    let primary = cx.locator.label(
        &model.relation,
        call.span,
        format!("adds up distinct counts of {counted} across {across}"),
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
        fingerprint: call.fingerprint.clone(),
    }
}

/// Averages, ratios, percentiles, declared non-additive columns and stocks.
#[allow(clippy::too_many_arguments)]
fn build(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    schema: &DFSchema,
    call: &Call,
    fact: &Measure,
    avg: bool,
) -> Diagnostic {
    let col = &call.col;
    let written = call.text(text, site);
    let group = group_text(text, schema, site.group_exprs);
    let per = group
        .clone()
        .map(|g| format!("per {g}"))
        .unwrap_or_else(|| "over all rows".into());
    let declared_at = |msg: String| -> Label { cx.locator.label(fact.site.relation(), None, msg) };
    // A stock whose `over` columns were dropped on the way here: the rows may differ there.
    let over_unknown = matches!(fact.kind, MeasureKind::Stock { over: None });
    let from = |over: &str| {
        if over_unknown {
            format!("from rows that may come from different {over} values")
        } else {
            format!("from different {over} values")
        }
    };
    let (label, origin, notes, fix, evidence) = match (&*fact.site, &fact.kind) {
        (MeasureSite::Computed { what, over, .. }, MeasureKind::Stock { .. }) => {
            let over = over.join(", ");
            (
                format!("adds {} {}", what.plural(), from(&over)),
                cx.locator.label(
                    fact.site.relation(),
                    fact.site.span(),
                    format!("{} over {over}", what.noun()),
                ),
                format!(
                    "each row's {col} already includes the rows before it, so adding rows from several {over} values counts those rows again"
                ),
                format!(
                    "add up the underlying values {per} instead, or keep only the last {over} of each group"
                ),
                format!("{col} is {} over {over}", what.noun()),
            )
        }
        (MeasureSite::Declared { column, over, .. }, MeasureKind::Stock { .. }) => {
            let over = over.join(", ");
            (
                format!("adds {col} {}", from(&over)),
                declared_at(format!("{column} is declared a stock over {over}")),
                format!(
                    "{col} is a level at each {over}, like an account balance; adding several {over} values counts the same amount once per {over}"
                ),
                format!(
                    "group by {over}, or keep one {over} per group (such as the latest) before summing"
                ),
                format!("{col} is declared a stock over {over}"),
            )
        }
        (MeasureSite::Computed { what, .. }, _) if avg => (
            format!(
                "takes an average of {}; each counts the same however many rows it covers",
                what.plural()
            ),
            cx.locator.label(
                fact.site.relation(),
                fact.site.span(),
                format!("{} per group", what.noun()),
            ),
            "an average of averages weights a group of one row like a group of a thousand".into(),
            format!(
                "average the underlying rows {per}, or weight each average by its row count: sum({col} * n) / sum(n)"
            ),
            format!("{col} is {}", what.noun()),
        ),
        (MeasureSite::Computed { what, .. }, _) => {
            let (note, fix) = match what {
                Computed::Ratio => (
                    "adding ratios is not the ratio of the totals: 1/2 + 1/2 is 1, but 2/4 is 1/2"
                        .to_string(),
                    format!("sum the numerator and the denominator separately {per}, then divide"),
                ),
                Computed::Percentile => (
                    "percentiles of parts don't add up to a percentile of the whole".to_string(),
                    format!("compute the percentile from the underlying rows {per}"),
                ),
                _ => (
                    "a sum of averages grows with the number of groups, not with the values"
                        .to_string(),
                    format!(
                        "sum the values and count the rows separately {per}, then divide, or use avg() over the underlying rows"
                    ),
                ),
            };
            (
                format!("adds up {}; {} can't be added", what.plural(), what.noun()),
                cx.locator.label(
                    fact.site.relation(),
                    fact.site.span(),
                    format!("{} is computed here", what.noun()),
                ),
                note,
                fix,
                format!("{col} is {}", what.noun()),
            )
        }
        (_, _) => (
            format!("adds up {col}, which is declared non-additive"),
            declared_at(format!("{col} is declared non-additive")),
            format!("{col} can't be added: summing it over more than one row is wrong"),
            format!("use avg, min or max, or compute {col} again from the underlying rows {per}"),
            format!("{col} is declared non-additive"),
        ),
    };
    // Why the grouping doesn't save the sum: a stock needs one `over` value per group; any
    // other measure needs one input row per group.
    let stock_over = match (&*fact.site, &fact.kind) {
        (
            MeasureSite::Computed { over, .. } | MeasureSite::Declared { over, .. },
            MeasureKind::Stock { .. },
        ) => Some(over.join(", ")),
        _ => None,
    };
    let why_group = match stock_over {
        Some(over) if over_unknown => format!(
            "{over} is not a column of the sum's input, so nothing keeps it to one value per group"
        ),
        Some(over) => match (&group, site.window) {
            (Some(_), false) => {
                format!("the GROUP BY does not keep {over} to one value per group")
            }
            (Some(_), true) => {
                format!("the PARTITION BY does not keep {over} to one value per partition")
            }
            (None, _) => format!("the sum adds rows with more than one {over} value"),
        },
        None => format!(
            "{} does not keep each group to one row of the input",
            group.clone().unwrap_or_else(|| "the grouping".into())
        ),
    };
    Diagnostic {
        code: Code::G008,
        severity: Severity::Warning,
        relation: model.relation.0.clone(),
        message: Code::G008.title().into(),
        primary: cx.locator.label(&model.relation, call.span, label.clone()),
        secondary: vec![origin],
        notes: vec![notes],
        fix: Some(Fix {
            message: fix,
            replacement: None,
        }),
        // "sum(avg_order_value) adds up averages; an average can't be added".
        evidence: vec![format!("{written} {label}"), evidence, why_group],
        fingerprint: call.fingerprint.clone(),
    }
}
