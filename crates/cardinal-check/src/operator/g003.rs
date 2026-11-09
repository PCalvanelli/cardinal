//! G003: a join is many-to-many: neither side's join columns are a superkey.
//! Joins with no equality columns (cross joins, pure range joins) are left alone:
//! they are usually deliberate, and their grain is still tracked as the union of keys.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_grain::{AnalyzedModel, Cardinality};

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
            let rows = |name: &str, g: &Option<Vec<String>>| match g {
                Some(g) => format!("{name} has one row per {}", tuple(g)),
                None => format!("{name}'s key is unknown"),
            };
            out.push(Diagnostic {
                code: Code::G003,
                severity: Code::G003.default_severity(),
                relation: model.relation.clone(),
                message: Code::G003.title().into(),
                primary: cx.locator.label(
                    &model.relation,
                    j.span,
                    format!("neither {} {on_l} nor {} {on_r} is unique", j.left, j.right),
                ),
                secondary: vec![],
                notes: vec![
                    rows(&j.left, &j.left_grain),
                    rows(&j.right, &j.right_grain),
                    "every pair of matching rows becomes one output row".into(),
                ],
                fix: Some(Fix {
                    message: match &j.right_grain {
                        Some(g) => format!(
                            "reduce {} to one row per {on_r} before joining, or join on its full key {}",
                            j.right,
                            tuple(g)
                        ),
                        None => format!("reduce {} to one row per {on_r} before joining", j.right),
                    },
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
