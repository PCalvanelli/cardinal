//! G014: a model keeps rows its source marks deleted, and drops the flag that marks them.
//!
//! Sync tools such as Fivetran, Stitch and Airbyte (CDC) keep a row deleted at the source
//! and set a flag on it (`_fivetran_deleted`, `_sdc_deleted_at`, `_ab_cdc_deleted_at`).
//! Until something filters on the flag, deleted rows count like live ones. The flag can
//! travel: a staging model may keep it for a later model to filter. Reported once, where
//! a model stops carrying it with no filter, join condition or aggregate on the way that
//! read it: after that, nothing downstream can tell deleted rows apart.

use crate::{Check, CheckContext};
use cardinal_dbt::RelationId;
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_grain::AnalyzedModel;
use cardinal_sql::{SqlSpan, Tokens};

pub struct SoftDeleteDropped;

impl Check for SoftDeleteDropped {
    fn code(&self) -> Code {
        Code::G014
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let mut seen: Vec<(&RelationId, &str)> = Vec::new();
        for d in &model.output().deleted {
            let Some(drop) = d.dropped.as_ref().filter(|x| x.model == model.relation) else {
                continue;
            };
            if seen.contains(&(&d.relation, d.flag.as_str())) {
                continue;
            }
            seen.push((&d.relation, &d.flag));
            let source = cx.locator.display(&d.relation);
            let via = cx.locator.display(&drop.via);
            let flag = &d.flag;
            let mut notes = vec![format!(
                "{source} is synced with soft deletes: a row deleted at the source stays, with `{flag}` set"
            )];
            if drop.via != d.relation {
                notes.push(format!(
                    "{via} keeps `{flag}` without filtering on it, so its rows include deleted ones"
                ));
            }
            notes.push(format!(
                "this model doesn't select `{flag}`, so nothing after it can filter deleted rows out"
            ));
            let filter = if flag.ends_with("_at") {
                format!("where {flag} is null")
            } else {
                format!("where not coalesce({flag}, false)")
            };
            out.push(Diagnostic {
                code: Code::G014,
                severity: Code::G014.default_severity(),
                relation: model.relation.0.clone(),
                message: format!(
                    "{} keeps rows deleted in {source}: `{flag}` is dropped without a filter",
                    cx.locator.display(&model.relation)
                ),
                primary: cx.locator.label(
                    &model.relation,
                    reference_span(model, &via),
                    format!("reads every row of {via}, deleted ones included"),
                ),
                secondary: vec![],
                notes,
                fix: Some(Fix {
                    message: format!(
                        "filter them out ({filter}), or select `{flag}` so a later model can; if keeping deleted rows is intended, add `-- cardinal: allow(G014) reason: …`"
                    ),
                    replacement: None,
                }),
                evidence: vec![],
                fingerprint: normalize_text(&format!("{} {flag}", d.relation.0)),
            });
        }
    }
}

/// Where the SQL reads relation `display` (`shop.orders`, `stg_orders`): the first
/// occurrence of its name, quoted or not.
fn reference_span(model: &AnalyzedModel, display: &str) -> Option<SqlSpan> {
    let name = display.rsplit('.').next()?;
    let t = Tokens::new(&model.sql);
    let at = t.name_range(t.names(name).next()?)?;
    Some(model.text().span(at.start, at.end))
}
