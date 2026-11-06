//! E001 to E006: Cardinal couldn't analyze a model's SQL. Each names the fix. They
//! belong to the layer that failed: code (E001 to E003), graph (E004: an unresolved
//! relation), node (E006: an unresolved column) or operator (E005: unsupported SQL).

use crate::Locator;
use cardinal_dbt::Relation;
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};
use cardinal_sql::{PlanFailure, PlanFailureKind};

const UNKNOWN_FUNCTION: &str = "unknown function ";

/// The function an E003 finding names, for `cardinal init` to write a stub for.
pub fn unknown_function(d: &Diagnostic) -> Option<&str> {
    if d.code != Code::E003 {
        return None;
    }
    d.message.strip_prefix(UNKNOWN_FUNCTION)?.strip_suffix("()")
}

/// E001 when `failure` is None (no compiled SQL), otherwise the code for the failure.
pub fn plan_failure(
    rel: &Relation,
    failure: Option<&PlanFailure>,
    locator: &dyn Locator,
) -> Diagnostic {
    let (code, message, label, help) = match failure {
        None => (
            Code::E001,
            format!("{} has no compiled SQL", rel.display),
            String::new(),
            "run: dbt compile".to_string(),
        ),
        Some(f) => match &f.kind {
            PlanFailureKind::Parse => (
                Code::E002,
                "compiled SQL does not parse".into(),
                f.message.clone(),
                "if this SQL runs in your warehouse, the dialect parser lacks this syntax; please report it".into(),
            ),
            PlanFailureKind::UnresolvedFunction(name) => (
                Code::E003,
                format!("{UNKNOWN_FUNCTION}{name}()"),
                format!("{name} is not a DataFusion built-in or a known stub"),
                format!(
                    "add a stub to cardinal.toml:\n         [[stub]]\n         name = \"{name}\"\n         returns = \"varchar\"   # or the function's return type"
                ),
            ),
            PlanFailureKind::UnresolvedRelation(name) => (
                Code::E004,
                format!("unresolved relation {name}"),
                "not found in the manifest".into(),
                "run: dbt compile, so the manifest includes every relation this model reads".into(),
            ),
            PlanFailureKind::UnresolvedColumn(name) => (
                Code::E006,
                format!("unresolved column {name}"),
                "not in any upstream relation's columns".into(),
                "run: dbt docs generate, or document the source's columns in YAML".into(),
            ),
            PlanFailureKind::Unsupported => (
                Code::E005,
                "unsupported SQL".into(),
                f.message.clone(),
                "Cardinal cannot plan this construct yet; its dependents are analyzed with unknown grain".into(),
            ),
        },
    };
    Diagnostic {
        code,
        severity: code.default_severity(),
        relation: rel.id.clone(),
        message,
        primary: locator.label(&rel.id, failure.and_then(|f| f.span), label),
        secondary: vec![],
        notes: vec![],
        fix: Some(Fix {
            message: help,
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&failure.map(|f| f.message.clone()).unwrap_or_default()),
    }
}
