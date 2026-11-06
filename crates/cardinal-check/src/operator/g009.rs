//! G009: amounts in different units are added together.
//!
//! `meta.cardinal.unit: currency` on `amount` says which column holds its unit. The
//! analysis carries a `Unit` fact: columns U that determine the unit, or None when the
//! unit column was dropped while it still varied. At `sum`, `avg`, `min` or `max` of
//! the amount with group-by columns G over input I: fire if U is None or
//! `U ⊄ closure(G)` on I, so one group can hold rows in different units. A filter to one
//! unit (`where currency = 'USD'`) puts it in closure(∅).

use crate::operator::g008::Call;
use crate::sites::{Site, bare_group, sites_without_fan_out};
use crate::{Check, CheckContext};
use cardinal_diag::{Code, Diagnostic, Fix, Severity};
use cardinal_grain::{AnalyzedModel, RelationFacts, Unit, aggregate, summed_col};
use cardinal_sql::SourceText;

pub struct MixedUnits;

impl Check for MixedUnits {
    fn code(&self) -> Code {
        Code::G009
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let text = model.text();
        for site in sites_without_fan_out(cx, model) {
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
    if !aggregate(&name).is_some_and(|a| a.keeps_unit) || site.args.len() != 1 {
        return None;
    }
    let input: &RelationFacts = model.facts(site.input)?;
    let schema = site.input.schema();
    let c = summed_col(&site.args[0], schema)?;
    let unit = input.units.iter().find(|u| u.col == c)?;
    if let Some(x) = &unit.unit
        && x.is_subset(&input.closure(&bare_group(schema, site.group_exprs)))
    {
        return None;
    }
    let call = Call::new(text, site, schema, c);
    Some(build(cx, model, text, site, &call, &name, unit))
}

fn build(
    cx: &CheckContext<'_>,
    model: &AnalyzedModel,
    text: &SourceText<'_>,
    site: &Site<'_>,
    call: &Call,
    name: &str,
    unit: &Unit,
) -> Diagnostic {
    let col = &call.col;
    let u = &unit.site.unit;
    let verb = match name {
        "sum" => "adds up",
        "min" => "takes the smallest",
        "max" => "takes the largest",
        _ => "averages",
    };
    let label = match &unit.unit {
        Some(_) => format!("{verb} {col} across different {u} values"),
        None => format!("{verb} {col} in mixed units: {u} was dropped before this"),
    };
    let declared = cx.locator.label(
        &unit.site.relation,
        None,
        format!("{u} is declared as the unit of {}", unit.site.column),
    );
    let fix = match &unit.unit {
        Some(_) => format!(
            "group by {u}, filter to one {u}, or convert {col} to one unit before the {name}"
        ),
        None => format!(
            "keep {u} next to {col} and group by it, filter to one {u}, or convert {col} to one unit first"
        ),
    };
    Diagnostic {
        code: Code::G009,
        severity: Severity::Error,
        relation: model.relation.0.clone(),
        message: Code::G009.title().into(),
        primary: cx.locator.label(&model.relation, call.span, label.clone()),
        secondary: vec![declared],
        notes: vec![format!(
            "rows with different {u} values go into one result, like adding dollars to yen"
        )],
        fix: Some(Fix {
            message: fix,
            replacement: None,
        }),
        evidence: vec![
            format!("{} {label}", call.text(text, site)),
            format!(
                "{} holds the unit of {} in {}",
                u,
                unit.site.column,
                cx.locator.display(&unit.site.relation)
            ),
            "the grouping does not keep each group to one unit".into(),
        ],
        fingerprint: call.fingerprint.clone(),
    }
}
