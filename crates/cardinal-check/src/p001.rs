//! P001 and P002: exactly-once contracts (meta.cardinal.exactly_once).
//!
//! P001: the analysis cannot show that the model contains each row of a named relation
//! exactly once. Like a type checker, a contract fails unless it is proven.
//!
//! P002: the contract is proven, but the proof rests on an assumption that no dbt test
//! and no enforced constraint checks, so bad source data would break it silently.
//!
//! Both run after analysis on cached verdicts, since which tests check an assumption
//! can change without the model changing.

use crate::Locator;
use cardinal_dbt::{Assumption, Project, Relation, RelationId};
use cardinal_diag::{Code, Diagnostic, Fix, Label, normalize_text};
use cardinal_grain::{Half, Reason, Site, Verdict};

pub fn contract_findings(
    project: &Project,
    rel: &Relation,
    verdicts: &[Verdict],
    locator: &dyn Locator,
    out: &mut Vec<Diagnostic>,
) {
    let display = |id: &RelationId| locator.display(id);
    let model = &rel.id;
    for e in rel.exactly_once.iter().filter(|e| e.relation.is_none()) {
        out.push(Diagnostic {
            code: Code::P001,
            severity: Code::P001.default_severity(),
            relation: model.0.clone(),
            message: format!(
                "exactly_once names `{}`, which is not a model, seed, snapshot or source",
                e.name
            ),
            primary: locator.label(model, None, "in meta.cardinal.exactly_once".into()),
            secondary: vec![],
            notes: vec![],
            fix: Some(Fix {
                message: "use a model name, or source_name.table_name for a source".into(),
                replacement: None,
            }),
            evidence: vec![],
            fingerprint: normalize_text(&e.name),
        });
    }
    for e in &rel.exactly_once {
        let Some(target) = &e.relation else { continue };
        if !verdicts.iter().any(|v| v.declared && &v.relation == target) {
            out.push(Diagnostic {
                code: Code::P001,
                severity: Code::P001.default_severity(),
                relation: model.0.clone(),
                message: format!(
                    "exactly once over {} cannot be checked: the model's SQL was not analyzed",
                    display(target)
                ),
                primary: locator.label(
                    model,
                    None,
                    "Python model, or SQL that failed to plan".into(),
                ),
                secondary: vec![],
                notes: vec![],
                fix: None,
                evidence: vec![],
                fingerprint: normalize_text(&format!("exactly_once {}", display(target))),
            });
        }
    }
    for v in verdicts.iter().filter(|v| v.declared) {
        if v.exactly_once() {
            unchecked(project, rel, v, locator, out);
        } else {
            not_proven(rel, v, locator, &display, out);
        }
    }
}

fn not_proven(
    rel: &Relation,
    v: &Verdict,
    locator: &dyn Locator,
    display: &dyn Fn(&RelationId) -> String,
    out: &mut Vec<Diagnostic>,
) {
    let model = &rel.id;
    let of = &v.display;
    let mut primary: Option<Label> = None;
    let mut secondary = Vec::new();
    let mut place = |relation: &RelationId, span, message: String| {
        let l = locator.label(relation, span, message);
        if primary.is_none() && relation == model {
            primary = Some(l);
        } else if !secondary.contains(&l) {
            secondary.push(l);
        }
    };
    for r in v
        .at_least_once
        .reasons()
        .iter()
        .chain(v.at_most_once.reasons())
    {
        match r {
            Reason::Loss { site } => place(
                &site.model,
                site.span,
                format!("rows of {of} may be dropped here"),
            ),
            Reason::Repeats { site } => place(&site.relation, site.span, site.message()),
            _ => {}
        }
    }
    let lost = !v.at_least_once.is_proven();
    let repeated = !v.at_most_once.is_proven();
    let message = match (lost, repeated) {
        (true, true) => format!("rows of {of} may be dropped or repeated"),
        (true, false) => format!("rows of {of} may be dropped"),
        _ => format!("rows of {of} may be repeated"),
    };
    let primary = primary
        .unwrap_or_else(|| locator.label(model, None, format!("declares exactly_once: {of}")));
    let mut notes = Vec::new();
    if v.site == Site::AggregateInput {
        notes.push(format!(
            "checked on the rows the aggregate reads, since the output does not carry {of}'s key"
        ));
    }
    for (name, h) in [
        ("at least once", &v.at_least_once),
        ("at most once", &v.at_most_once),
    ] {
        match h {
            Half::Proven => notes.push(format!("{name}: proven")),
            Half::NotProven { reasons } => {
                for r in reasons {
                    notes.push(format!("{name}: not proven: {}", r.text(of, display)));
                }
            }
        }
    }
    out.push(Diagnostic {
        code: Code::P001,
        severity: Code::P001.default_severity(),
        relation: model.0.clone(),
        message,
        primary,
        secondary,
        notes,
        fix: Some(Fix {
            message: fix_for(v),
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&format!("exactly_once {of}")),
    });
}

/// The most useful next step for the first unproven reason.
fn fix_for(v: &Verdict) -> String {
    let of = &v.display;
    let first = v
        .at_least_once
        .reasons()
        .iter()
        .chain(v.at_most_once.reasons())
        .next();
    match first {
        Some(Reason::NoKey) => format!("declare a key on {of} (meta.cardinal.grain)"),
        Some(Reason::KeyNullable { .. }) => {
            format!("declare {of}'s key columns non-null (a not_null test or meta.cardinal.grain)")
        }
        Some(Reason::NotRead) | Some(Reason::NotCarried { .. }) => format!(
            "select {of}'s key columns, or remove {of} from exactly_once if the model is not about its rows"
        ),
        Some(Reason::Loss { site }) if site.what.contains("join") => {
            "use a left join, or declare the reference that guarantees a match with meta.cardinal.references"
                .into()
        }
        Some(Reason::Loss { .. }) => format!(
            "if dropping these rows is intended, the model does not contain every row of {of}: remove it from exactly_once"
        ),
        Some(Reason::Repeats { .. }) | Some(Reason::NotUnique { .. }) => {
            "reduce the joined relation to one row per join key before the join".into()
        }
        None => String::new(),
    }
}

fn unchecked(
    project: &Project,
    rel: &Relation,
    v: &Verdict,
    locator: &dyn Locator,
    out: &mut Vec<Diagnostic>,
) {
    let display = |id: &RelationId| locator.display(id);
    let open: Vec<(&Assumption, cardinal_dbt::Status)> = v
        .assumptions
        .iter()
        .map(|a| (a, project.assumption_status(a)))
        .filter(|(_, s)| !s.is_checked())
        .collect();
    if open.is_empty() {
        return;
    }
    let n = open.len();
    let notes = open
        .iter()
        .map(|(a, s)| {
            let mut line = a.describe(&display);
            if !s.declared_by.is_empty() {
                line.push_str(&format!(" (declared by {})", s.declared_by.join(" and ")));
            }
            line.push_str(": unchecked");
            for c in &s.checked_by {
                if let cardinal_dbt::CheckedBy::WarnTest(t) = c {
                    line.push_str(&format!(
                        "; test {t} only warns, so it never fails the build"
                    ));
                }
            }
            for c in &s.caveats {
                line.push_str(&format!("; {c}"));
            }
            line
        })
        .collect();
    out.push(Diagnostic {
        code: Code::P002,
        severity: Code::P002.default_severity(),
        relation: rel.id.0.clone(),
        message: format!(
            "exactly once over {} rests on {n} unchecked {}",
            v.display,
            if n == 1 { "assumption" } else { "assumptions" }
        ),
        primary: locator.label(
            &rel.id,
            None,
            format!(
                "proven, if {}",
                if n == 1 { "this holds" } else { "these hold" }
            ),
        ),
        secondary: vec![],
        notes,
        fix: Some(Fix {
            message: "add the dbt tests that `cardinal assumptions --emit-tests` prints".into(),
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&format!("exactly_once {}", v.display)),
    });
}
