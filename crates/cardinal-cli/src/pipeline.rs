//! From per-model diagnostics to the findings a run reports: project-wide checks,
//! suppressions, severities, baseline, and the minimum severity.

use crate::config::{Level, Severities};
use crate::engine::ProjectLocator;
use crate::engine::{RunResult, statuses};
use cardinal_check::{UnknownGrainModel, contract_findings, unknown_grain};
use cardinal_dbt::{Project, RelationId};
use cardinal_diag::baseline::Baseline;
use cardinal_diag::suppress::{self, Suppression};
use cardinal_diag::{Code, Diagnostic, Fix, Label, Severity, normalize_text};
use cardinal_grain::UnknownReason;
use cardinal_sql::{LineCol, SqlSpan};
use std::collections::{BTreeMap, BTreeSet};

pub struct RootCause {
    pub root: RelationId,
    pub reason: UnknownReason,
    pub affected: Vec<RelationId>,
}

pub struct Findings {
    pub reported: Vec<Diagnostic>,
    /// Findings hidden by the baseline.
    pub baselined: usize,
    /// Findings below --min-severity.
    pub below_threshold: usize,
    pub suppressed: usize,
    /// Every finding after suppressions and severities, before baseline and threshold.
    pub all: Vec<Diagnostic>,
    pub root_causes: Vec<RootCause>,
}

pub struct Options<'a> {
    pub severities: &'a Severities,
    pub baseline: Option<&'a Baseline>,
    pub min_severity: Severity,
}

pub fn collect(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
    opts: &Options<'_>,
) -> Findings {
    let mut raw: Vec<Diagnostic> = Vec::new();
    let locator = ProjectLocator { project };
    for id in selected {
        if let Some(r) = run.results.get(id) {
            raw.extend(r.diagnostics.iter().cloned());
            if let Some(rel) = project.get(id) {
                contract_findings(project, rel, &r.verdicts, &locator, &mut raw);
            }
        }
    }

    // G004: once per root cause, over the selected models.
    let st = statuses(run);
    let unknown: Vec<UnknownGrainModel> = selected
        .iter()
        .filter(|id| project.get(id).is_some_and(|r| r.is_model()))
        .filter_map(|id| {
            run.results.get(id).map(|r| UnknownGrainModel {
                id: id.clone(),
                status: r.summary.status.clone(),
            })
        })
        .collect();
    let mut root_causes = Vec::new();
    for (root, reason, affected, d) in unknown_grain(project, &unknown, &st) {
        // A model that failed to plan already has its E-code; its own G004 adds nothing
        // unless other models are blocked by it.
        let only_self =
            matches!(reason, UnknownReason::PlanError(_)) && affected == vec![root.clone()];
        if !only_self {
            raw.push(d);
        }
        root_causes.push(RootCause {
            root,
            reason,
            affected,
        });
    }

    // Suppressions in the selected models' source files.
    let mut sups: Vec<(Suppression, bool)> = Vec::new();
    for id in selected {
        if let Some(rel) = project.get(id)
            && let Some(rawsql) = &rel.raw_sql
        {
            sups.extend(
                suppress::parse(&rel.file, rawsql)
                    .into_iter()
                    .map(|s| (s, false)),
            );
        }
    }
    let mut suppressed = 0;
    let mut kept = Vec::new();
    for mut d in raw {
        let line = d.primary.span.map(|s| s.start.line);
        let hit = sups.iter_mut().find(|(s, _)| {
            s.file == d.primary.file && Some(s.target_line) == line && s.codes.contains(&d.code)
        });
        match hit {
            Some((s, used)) if s.reason.is_some() => {
                *used = true;
                suppressed += 1;
            }
            Some((s, used)) => {
                *used = true;
                d.notes.push(format!(
                    "the suppression on line {} is ignored: add `reason: ...` after allow({})",
                    s.line, d.code
                ));
                kept.push(d);
            }
            None => kept.push(d),
        }
    }
    for (s, used) in &sups {
        if !used {
            let model = project
                .models()
                .find(|r| r.file == s.file)
                .map(|r| r.id.0.clone())
                .unwrap_or_default();
            let codes: Vec<String> = s.codes.iter().map(|c| c.to_string()).collect();
            kept.push(Diagnostic {
                code: Code::G005,
                severity: Code::G005.default_severity(),
                relation: model,
                message: Code::G005.title().into(),
                primary: Label {
                    file: s.file.clone(),
                    span: Some(SqlSpan {
                        start: LineCol {
                            line: s.line,
                            col: s.col_start,
                        },
                        end: LineCol {
                            line: s.line,
                            col: s.col_end,
                        },
                    }),
                    message: format!("allow({}) no longer matches any finding", codes.join(", ")),
                },
                secondary: vec![],
                notes: vec![],
                fix: Some(Fix {
                    message: "remove the comment".into(),
                    replacement: None,
                }),
                evidence: vec![],
                fingerprint: normalize_text(&format!(
                    "{} {}",
                    codes.join(","),
                    s.reason.clone().unwrap_or_default()
                )),
            });
        }
    }

    // Severities.
    let mut all = Vec::new();
    for mut d in kept {
        // Unconfigured codes keep the severity the check chose (G006 is an error when
        // unique_key names a column the model does not output, a warning otherwise).
        match opts.severities.configured(d.code, &d.primary.file) {
            None => {}
            Some(Level::Off) => continue,
            Some(Level::On(sev)) => {
                // Unknown aggregates stay at warning unless configured otherwise.
                if !(d.code == Code::G001
                    && d.severity == Severity::Warning
                    && sev == Severity::Error)
                {
                    d.severity = sev;
                }
            }
        }
        all.push(d);
    }
    all.sort_by_key(|d| d.sort_key());
    all.dedup_by(|a, b| {
        a.code == b.code
            && a.relation == b.relation
            && a.fingerprint == b.fingerprint
            && a.primary == b.primary
    });

    let mut baselined = 0;
    let mut below = 0;
    let mut reported = Vec::new();
    for d in &all {
        if opts.baseline.is_some_and(|b| b.contains(d)) {
            baselined += 1;
        } else if d.severity < opts.min_severity {
            below += 1;
        } else {
            reported.push(d.clone());
        }
    }
    Findings {
        reported,
        baselined,
        below_threshold: below,
        suppressed,
        all,
        root_causes,
    }
}

pub fn count_by_code(findings: &[Diagnostic]) -> BTreeMap<Code, usize> {
    let mut m = BTreeMap::new();
    for d in findings {
        *m.entry(d.code).or_default() += 1;
    }
    m
}
