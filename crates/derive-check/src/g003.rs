//! G003: a join is many-to-many: neither side's join columns are a superkey.
//! Joins with no equality columns (cross joins, pure range joins) are left alone:
//! they are usually deliberate, and their grain is still tracked as the union of keys.

use crate::{Check, CheckContext, tuple};
use derive_diag::{Code, Diagnostic, Fix, normalize_text};
use derive_grain::{AnalyzedModel, Cardinality};

pub struct ManyToManyJoin;

impl Check for ManyToManyJoin {
    fn code(&self) -> Code {
        Code::G003
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let text = model.text();
        for j in &model.joins {
            if j.cardinality != Some(Cardinality::ManyToMany) || j.on_left.is_empty() {
                continue;
            }
            let on_l = tuple(&j.on_left);
            let on_r = tuple(&j.on_right);
            let grain = |g: &Option<Vec<String>>| {
                g.as_ref()
                    .map(|g| tuple(g))
                    .unwrap_or_else(|| "unknown".into())
            };
            out.push(Diagnostic {
                code: Code::G003,
                severity: Code::G003.default_severity(),
                relation: model.relation.0.clone(),
                message: Code::G003.title().into(),
                primary: cx.locator.label(
                    &model.relation,
                    j.span,
                    format!("neither {} {on_l} nor {} {on_r} is unique", j.left, j.right),
                ),
                secondary: vec![],
                notes: vec![
                    format!("{} has grain {}", j.left, grain(&j.left_grain)),
                    format!("{} has grain {}", j.right, grain(&j.right_grain)),
                    "every pair of matching rows becomes one output row".into(),
                ],
                fix: Some(Fix {
                    message: format!(
                        "reduce {} to one row per {on_r} before joining, or join on its full key {}",
                        j.right,
                        grain(&j.right_grain)
                    ),
                    replacement: None,
                }),
                evidence: vec![format!("{} join on {}", Cardinality::ManyToMany.label(), j.on_left.join(", "))],
                fingerprint: j
                    .span
                    .map(|s| normalize_text(text.slice(s)))
                    .unwrap_or_else(|| normalize_text(&format!("{} {} {}", j.left, j.right, on_l))),
            });
        }
    }
}
