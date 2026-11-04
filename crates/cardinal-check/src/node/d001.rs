//! D001: a meta.cardinal declaration names a column the relation doesn't have, or isn't
//! in a form Cardinal reads, so it is ignored. Reads only declarations and the column
//! lists of the catalog and YAML, so it runs without SQL. A declared key on a model whose
//! SQL was analyzed is G002's: it compares the key with the model's actual output.

use crate::{ProjectCheck, ProjectContext, decl_label, declared_relations, tuple};
use cardinal_dbt::project::ColumnsFrom;
use cardinal_dbt::{DeclOrigin, DeclProblemKind, Relation};
use cardinal_diag::{Code, Diagnostic, Fix, normalize_text};

pub struct DeclarationMismatch;

impl ProjectCheck for DeclarationMismatch {
    fn codes(&self) -> &'static [Code] {
        &[Code::D001]
    }
    fn needs_sql(&self) -> bool {
        false
    }
    fn run(&self, cx: &ProjectContext<'_>, out: &mut Vec<Diagnostic>) {
        for rel in declared_relations(cx) {
            for p in &rel.decl_problems {
                let fix = match p.kind {
                    DeclProblemKind::UnknownRelation => continue, // D002
                    DeclProblemKind::MissingColumn => {
                        "name one of its columns; if you just added the column, run dbt docs generate"
                    }
                    DeclProblemKind::Malformed => {
                        "see docs/configuration.md for the forms Cardinal reads"
                    }
                };
                out.push(finding(
                    rel,
                    format!("meta.cardinal.{} on {} is ignored", p.field, rel.display),
                    p.message.clone(),
                    fix.into(),
                    format!("{} {}", p.field, p.message),
                ));
            }
            missing_key_columns(cx, rel, out);
        }
    }
}

/// A grain or primary_key that names columns the relation doesn't have declares no key.
/// For a model whose SQL was analyzed, G002 reports this against its output instead.
fn missing_key_columns(cx: &ProjectContext<'_>, rel: &Relation, out: &mut Vec<Diagnostic>) {
    if cx.shapes.get(&rel.id).is_some_and(|s| s.planned) {
        return;
    }
    let Some(columns) = known_columns(rel) else {
        return;
    };
    for k in &rel.declared_keys {
        let setting = match k.origin {
            DeclOrigin::MetaGrain => "meta.cardinal.grain",
            DeclOrigin::ContractPrimaryKey => "the primary_key constraint",
            _ => continue,
        };
        let missing: Vec<String> = k
            .cols
            .iter()
            .filter(|c| !columns.contains(c))
            .cloned()
            .collect();
        if missing.is_empty() {
            continue;
        }
        let mut shown: Vec<String> = columns.iter().take(8).cloned().collect();
        if columns.len() > 8 {
            shown.push("…".into());
        }
        out.push(finding(
            rel,
            format!(
                "{setting} on {} names {}, which it doesn't have",
                rel.display,
                missing.join(", ")
            ),
            format!("declares the key {}", tuple(&k.cols)),
            format!("use its columns: {}", shown.join(", ")),
            format!("key {}", k.cols.join(",")),
        ));
    }
}

/// The relation's columns from the catalog or YAML. None when unknown.
fn known_columns(rel: &Relation) -> Option<Vec<String>> {
    if matches!(rel.columns_from, ColumnsFrom::Missing) || rel.columns.is_empty() {
        return None;
    }
    Some(rel.columns.iter().map(|c| c.name.to_lowercase()).collect())
}

fn finding(
    rel: &Relation,
    message: String,
    label: String,
    fix: String,
    fingerprint: String,
) -> Diagnostic {
    Diagnostic {
        code: Code::D001,
        severity: Code::D001.default_severity(),
        relation: rel.id.0.clone(),
        message,
        primary: decl_label(rel, label),
        secondary: vec![],
        notes: vec![],
        fix: Some(Fix {
            message: fix,
            replacement: None,
        }),
        evidence: vec![],
        fingerprint: normalize_text(&fingerprint),
    }
}
