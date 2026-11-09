//! D002: a meta.cardinal declaration names a relation that isn't in the project, or, for
//! exactly_once, one the model doesn't read. Reads only the manifest, so it runs
//! without SQL.

use crate::{ProjectCheck, ProjectContext, decl_label, declared_relations};
use cardinal_dbt::{DeclProblemKind, Relation};
use cardinal_diag::{Code, Diagnostic, Fix, Severity, normalize_text};

pub struct WrongRelation;

impl ProjectCheck for WrongRelation {
    fn codes(&self) -> &'static [Code] {
        &[Code::D002]
    }
    fn needs_sql(&self) -> bool {
        false
    }
    fn run(&self, cx: &ProjectContext<'_>, out: &mut Vec<Diagnostic>) {
        for rel in declared_relations(cx) {
            // An ignored reference only weakens proofs: a warning.
            for p in &rel.decl_problems {
                if p.kind == DeclProblemKind::UnknownRelation {
                    out.push(finding(
                        rel,
                        Code::D002.default_severity(),
                        format!("meta.cardinal.{} on {} is ignored", p.field, rel.display),
                        p.message.clone(),
                        "use a model name, or source_name.table_name for a source",
                        format!("{} {}", p.field, p.message),
                    ));
                }
            }
            if rel.exactly_once.is_empty() {
                continue;
            }
            // An exactly_once contract on a relation the model never reads can't hold.
            let ancestors = cx.project.ancestors(&rel.id);
            for e in &rel.exactly_once {
                let (message, fix) = match &e.relation {
                    None => (
                        format!(
                            "exactly_once names `{}`, which is not a model, seed, snapshot or source",
                            e.name
                        ),
                        "use a model name, or source_name.table_name for a source",
                    ),
                    Some(t) if !ancestors.contains(t) => (
                        format!(
                            "exactly_once names {}, which {} doesn't read",
                            cx.locator.display(t),
                            rel.display
                        ),
                        "name a relation upstream of the model",
                    ),
                    Some(_) => continue,
                };
                out.push(finding(
                    rel,
                    Severity::Error,
                    message,
                    "in meta.cardinal.exactly_once".into(),
                    fix,
                    format!("exactly_once {}", e.name),
                ));
            }
        }
    }
}

fn finding(
    rel: &Relation,
    severity: Severity,
    message: String,
    label: String,
    fix: &str,
    fingerprint: String,
) -> Diagnostic {
    Diagnostic {
        code: Code::D002,
        severity,
        relation: rel.id.clone(),
        message,
        primary: decl_label(rel, label),
        secondary: vec![],
        notes: vec![],
        fix: Some(Fix {
            message: fix.into(),
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&fingerprint),
    }
}
