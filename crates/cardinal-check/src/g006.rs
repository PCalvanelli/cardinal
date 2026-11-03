//! G006: an incremental model's `unique_key` is not a key of the rows its SQL produces.
//!
//! merge and delete+insert match new rows to existing ones on unique_key. When two new
//! rows share a unique_key, merge either fails (Snowflake, BigQuery, Databricks reject a
//! target row matched by several source rows) or updates it from an arbitrary row, and
//! delete+insert inserts both, so the table holds duplicates that no rebuild shows.

use crate::{Check, CheckContext, tuple};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};
use cardinal_grain::{AnalyzedModel, ColSet};

pub struct IncrementalKeyNotUnique;

impl Check for IncrementalKeyNotUnique {
    fn code(&self) -> Code {
        Code::G006
    }

    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>) {
        let Some(inc) = &cx.relation.incremental else {
            return;
        };
        // No unique_key means append; an expression key cannot be mapped to columns.
        if !inc.merges_on_key() || inc.unique_key.is_empty() {
            return;
        }
        let facts = model.output();
        let schema = model.plan.schema();
        let names: Vec<String> = schema
            .fields()
            .iter()
            .map(|f| f.name().to_lowercase())
            .collect();
        let key = tuple(&inc.unique_key);
        let raw = inc.unique_key_raw.clone().unwrap_or_else(|| key.clone());
        let strategy = inc
            .strategy
            .as_deref()
            .unwrap_or("the adapter's default strategy");

        let missing: Vec<String> = inc
            .unique_key
            .iter()
            .filter(|c| !names.contains(c))
            .cloned()
            .collect();
        if !missing.is_empty() {
            out.push(Diagnostic {
                code: Code::G006,
                severity: Severity::Error,
                relation: model.relation.0.clone(),
                message: format!("unique_key {key} names columns the model does not output"),
                primary: cx
                    .locator
                    .label(&model.relation, None, format!("missing: {}", missing.join(", "))),
                secondary: vec![],
                notes: vec![format!("unique_key = {raw}, incremental_strategy: {strategy}")],
                fix: Some(Fix {
                    message: "output the unique_key columns, or set unique_key to columns the model selects".into(),
                    replacement: None,
                }),
                evidence: vec![],
                fingerprint: normalize_text(&format!("missing {key}")),
            });
            return;
        }

        // Precision over recall: judge only models whose grain is fully known.
        if !facts.status.is_known() {
            return;
        }
        let cols: ColSet = inc
            .unique_key
            .iter()
            .filter_map(|c| names.iter().position(|n| n == c))
            .collect();
        if facts.is_superkey(&cols) {
            return;
        }
        // A unique test on the same columns already gets G002; one finding is enough.
        if cx.declared.iter().any(|d| {
            !d.proven && {
                let mut a = d.key.cols.clone();
                let mut b = inc.unique_key.clone();
                a.sort();
                b.sort();
                a == b
            }
        }) {
            return;
        }

        let proven: Vec<String> = facts
            .keys
            .iter()
            .map(|k| tuple(&k.cols.iter().map(|c| names[*c].clone()).collect::<Vec<_>>()))
            .collect();
        let label = if proven.is_empty() {
            "no key can be proven for this model's rows".to_string()
        } else {
            format!("the SQL proves grain {}", proven.join(" and "))
        };
        let fix = match proven.first() {
            Some(g) => format!(
                "set unique_key to the model's grain {g}, or make the SQL produce one row per {key}, e.g. a row_number() dedup"
            ),
            None => format!("make the SQL produce one row per {key}, e.g. a row_number() dedup"),
        };
        out.push(Diagnostic {
            code: Code::G006,
            severity: Code::G006.default_severity(),
            relation: model.relation.0.clone(),
            message: format!("incremental unique_key {key} is not a key of the model's rows"),
            primary: cx.locator.label(&model.relation, None, label),
            secondary: vec![],
            notes: vec![
                format!("unique_key = {raw}, incremental_strategy: {strategy}"),
                format!(
                    "two new rows with the same {key} make merge fail or update from an arbitrary row, and delete+insert keep both"
                ),
            ],
            fix: Some(Fix {
                message: fix,
                replacement: None,
            }),
            evidence: facts.keys.iter().map(|k| model.arena.explain(k.why)).collect(),
            fingerprint: normalize_text(&key),
        });
    }
}
