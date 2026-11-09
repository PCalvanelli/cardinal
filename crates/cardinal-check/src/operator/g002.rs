//! G002: a model declares a key (test, meta or contract) that the analysis cannot
//! prove. The test remains the only guarantee.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_grain::{AnalyzedModel, GrainStatus};

pub struct UnprovenDeclaredKey;

impl Check for UnprovenDeclaredKey {
    fn code(&self) -> Code {
        Code::G002
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let facts = model.output();
        // An unknown grain is reported once, at its root cause (G004).
        if matches!(facts.status, GrainStatus::Unknown(_)) {
            return;
        }
        let schema = model.plan.schema();
        let inferred: Vec<String> = facts
            .keys
            .iter()
            .map(|k| {
                tuple(
                    &k.cols
                        .iter()
                        .map(|c| schema.field(*c).name().clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        for d in cx.declared.iter().filter(|d| !d.proven) {
            let key = tuple(&d.key.cols);
            let (message, label) = if d.missing_columns.is_empty() {
                (
                    format!("declared key {key} isn't guaranteed by the SQL"),
                    if inferred.is_empty() {
                        "the SQL guarantees no key for this model".to_string()
                    } else {
                        format!(
                            "the SQL guarantees one row per {}",
                            inferred.join(" and per ")
                        )
                    },
                )
            } else {
                (
                    format!("declared key {key} names columns the model doesn't output"),
                    format!("missing: {}", d.missing_columns.join(", ")),
                )
            };
            out.push(Diagnostic {
                code: Code::G002,
                severity: Code::G002.default_severity(),
                relation: model.relation.clone(),
                message,
                primary: cx.locator.label(&model.relation, None, label),
                secondary: vec![],
                notes: vec![format!("only {} checks it", d.key.origin.describe())],
                fix: Some(Fix {
                    message: "declare the keys of the tables it reads (meta.cardinal.grain), or make the SQL return one row per key, e.g. with GROUP BY or a row_number() dedup".into(),
                    replacement: None,
                }),
                evidence: facts.keys.iter().map(|k| model.arena.explain(k.why)).collect(),
                fingerprint: normalize_text(&key),
            });
        }
    }
}
