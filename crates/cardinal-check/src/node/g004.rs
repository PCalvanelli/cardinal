//! G004: a model's key is unknown. Reported once per root cause (for example one
//! undeclared source), not once per affected model.

use crate::{ProjectCheck, ProjectContext};
use cardinal_dbt::{Kind, Project, RelationId};
use cardinal_diag::{Code, Diagnostic, Fix, Label, normalize_text};
use cardinal_grain::{GrainStatus, UnknownReason};
use std::collections::BTreeMap;

pub struct UnknownGrainModel {
    pub id: RelationId,
    pub status: GrainStatus,
}

pub struct UnknownGrain;

impl ProjectCheck for UnknownGrain {
    fn codes(&self) -> &'static [Code] {
        &[Code::G004]
    }
    fn needs_sql(&self) -> bool {
        true
    }
    fn run(&self, cx: &ProjectContext<'_>, out: &mut Vec<Diagnostic>) {
        for (root, reason, affected, d) in root_causes(cx) {
            // A model that failed to plan already has its E-code; its own G004 adds
            // nothing unless other models are blocked by it.
            let only_self =
                matches!(reason, UnknownReason::PlanError(_)) && affected == vec![root.clone()];
            if !only_self {
                out.push(d);
            }
        }
    }
}

/// The root causes of unknown grain among the selected models, ranked by how many
/// models each blocks.
pub fn root_causes(
    cx: &ProjectContext<'_>,
) -> Vec<(RelationId, UnknownReason, Vec<RelationId>, Diagnostic)> {
    let models: Vec<UnknownGrainModel> = cx
        .selected
        .iter()
        .filter(|id| cx.project.get(id).is_some_and(|r| r.is_model()))
        .filter_map(|id| {
            cx.shapes.get(id).map(|s| UnknownGrainModel {
                id: id.clone(),
                status: s.summary.status.clone(),
            })
        })
        .collect();
    unknown_grain(cx.project, &models, &cx.shapes.statuses())
}

/// Group models with unknown grain by root cause. Returns one finding per cause, plus
/// the causes ranked by how many models declaring them would unlock (for `coverage`).
pub fn unknown_grain(
    project: &Project,
    models: &[UnknownGrainModel],
    statuses: &BTreeMap<RelationId, GrainStatus>,
) -> Vec<(RelationId, UnknownReason, Vec<RelationId>, Diagnostic)> {
    let mut groups: BTreeMap<(RelationId, String), (UnknownReason, Vec<RelationId>)> =
        BTreeMap::new();
    for m in models {
        let GrainStatus::Unknown(reason) = &m.status else {
            continue;
        };
        let root = root_of(project, &m.id, reason, statuses);
        let key = (root.clone(), format!("{reason:?}"));
        groups
            .entry(key)
            .or_insert_with(|| (reason.clone(), vec![]))
            .1
            .push(m.id.clone());
    }
    let mut out = Vec::new();
    for ((root, _), (reason, mut affected)) in groups {
        affected.sort();
        let Some(rel) = project.get(&root) else {
            continue;
        };
        let names: Vec<String> = affected.iter().map(|a| project.display(a)).collect();
        let n = affected.len();
        let plural = if n == 1 { "model" } else { "models" };
        let display = |r: &RelationId| project.display(r);
        let (message, help) = match &reason {
            UnknownReason::SourceWithoutKey(_) => {
                let what = if rel.kind == Kind::Seed {
                    "seed"
                } else {
                    "source"
                };
                (
                    format!(
                        "can't check {n} {plural}: {what} {} has no known key",
                        rel.display
                    ),
                    format!(
                        "declare its key with meta: {{cardinal: {{grain: [...]}}}} on the {what}, add a unique test, or run cardinal guess"
                    ),
                )
            }
            UnknownReason::PlanError(_) => (
                format!(
                    "can't check {n} {plural}: Cardinal couldn't read {}",
                    rel.display
                ),
                format!("fix the E-code reported for {} first", rel.display),
            ),
            UnknownReason::PythonModel(_) => (
                format!(
                    "can't check {n} {plural}: {} is a Python model",
                    rel.display
                ),
                "declare its key with meta.cardinal.grain or a unique test".to_string(),
            ),
            UnknownReason::UnsupportedOperator(op) => (
                format!("can't check {n} {plural}: {op} in {}", rel.display),
                format!(
                    "declare the key of {} with a unique test or meta.cardinal.grain",
                    rel.display
                ),
            ),
        };
        let shown: Vec<String> = names.iter().take(10).cloned().collect();
        let mut affected_note = format!("affects {}", shown.join(", "));
        if n > 10 {
            affected_note.push_str(&format!(" and {} more", n - 10));
        }
        let d = Diagnostic {
            code: Code::G004,
            severity: Code::G004.default_severity(),
            relation: root.clone(),
            message,
            primary: Label {
                file: rel.file.clone(),
                span: None,
                message: String::new(),
            },
            secondary: vec![],
            // "X has no known key" is already the message.
            notes: if matches!(reason, UnknownReason::SourceWithoutKey(_)) {
                vec![affected_note]
            } else {
                vec![affected_note, reason.describe(&display)]
            },
            fix: Some(Fix {
                message: help,
                replacement: None,
            }),
            evidence: vec![],
            fingerprint: normalize_text(&format!("{reason:?}")),
        };
        out.push((root, reason, affected, d));
    }
    out.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then(a.0.cmp(&b.0)));
    out
}

/// The relation a model's unknown grain comes from.
fn root_of(
    project: &Project,
    id: &RelationId,
    reason: &UnknownReason,
    statuses: &BTreeMap<RelationId, GrainStatus>,
) -> RelationId {
    match reason {
        UnknownReason::SourceWithoutKey(r)
        | UnknownReason::PlanError(r)
        | UnknownReason::PythonModel(r) => r.clone(),
        UnknownReason::UnsupportedOperator(_) => {
            // Walk up while a parent carries the same reason; `seen` stops at cycles.
            let mut cur = id.clone();
            let mut seen = std::collections::BTreeSet::from([cur.clone()]);
            while let Some(parent) = project.get(&cur).and_then(|rel| {
                rel.depends_on.iter().find(|p| {
                    !seen.contains(*p) && statuses.get(*p).and_then(|s| s.reason()) == Some(reason)
                })
            }) {
                cur = parent.clone();
                seen.insert(cur.clone());
            }
            cur
        }
    }
}
