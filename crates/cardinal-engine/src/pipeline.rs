//! From per-model diagnostics to the findings a run reports: project-wide checks,
//! suppressions, severities, baseline, and the minimum severity.

use crate::comparison::Comparison;
use crate::config::{Level, Severities};
use crate::engine::ProjectLocator;
use crate::engine::RunResult;
use cardinal_check::{ProjectContext, Shapes, project_checks, root_causes};
use cardinal_dbt::{Project, RelationId};
use cardinal_diag::baseline::Baseline;
use cardinal_diag::suppress::{self, Suppression};
use cardinal_diag::{Code, Diagnostic, Fix, Label, Layer, Severity, normalize_text};
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
    /// Findings hidden because the base branch (--state) has them too.
    pub on_main: usize,
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
    /// The comparison with the base branch, with --state.
    pub state: Option<&'a Comparison>,
    pub min_severity: Severity,
    /// Layers to report; empty means all.
    pub layers: &'a BTreeSet<Layer>,
    /// No SQL was analyzed (only graph and node selected): run only the checks that
    /// read just the manifest and catalog.
    pub no_sql: bool,
}

/// No layers selected: every layer.
static EVERY_LAYER: BTreeSet<Layer> = BTreeSet::new();

impl<'a> Options<'a> {
    /// Every finding in every layer, with no baseline or comparison: what `coverage`,
    /// `guess` and the base branch's analysis read.
    pub fn all_findings(severities: &'a Severities) -> Self {
        Options {
            severities,
            baseline: None,
            state: None,
            min_severity: Severity::Info,
            layers: &EVERY_LAYER,
            no_sql: false,
        }
    }
}

pub(crate) fn collect(
    project: &Project,
    run: &RunResult,
    shapes: &Shapes,
    selected: &BTreeSet<RelationId>,
    opts: &Options<'_>,
) -> Findings {
    let in_layers = |c: Code| layer_selected(c, opts.layers);
    let mut raw: Vec<Diagnostic> = Vec::new();
    let locator = ProjectLocator { project };
    for id in selected {
        if let Some(r) = run.results.get(id) {
            raw.extend(r.diagnostics.iter().cloned());
        }
    }
    let cx = ProjectContext {
        project,
        shapes,
        selected,
        locator: &locator,
    };
    for check in project_checks() {
        if opts.no_sql && check.needs_sql() {
            continue;
        }
        if check.codes().iter().any(|c| in_layers(*c)) {
            check.run(&cx, &mut raw);
        }
    }
    if let Some(c) = opts.state {
        raw.extend(c.findings.iter().cloned());
    }
    raw.retain(|d| in_layers(d.code));
    without_blocked(&mut raw);

    // Root causes of unknown grain, for the Next: line and coverage, in every layer.
    let root_causes = if opts.no_sql {
        vec![]
    } else {
        root_causes(&cx)
            .into_iter()
            .map(|(root, reason, affected, _)| RootCause {
                root,
                reason,
                affected,
            })
            .collect()
    };

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
        // A lost guarantee about a whole model has no line, so a comment anywhere in
        // the model's file acknowledges it.
        let anywhere = d.code == Code::P003 && line.is_none();
        let hit = sups.iter_mut().find(|(s, _)| {
            s.file == d.primary.file
                && (anywhere || Some(s.target_line) == line)
                && s.codes.contains(&d.code)
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
        // allow(P003) acknowledges a change against the base branch; once the change
        // is merged it matches nothing, as expected. A suppression of a code in a layer
        // that wasn't checked can't be judged stale.
        if !used
            && in_layers(Code::G005)
            && s.codes.iter().any(|c| *c != Code::P003)
            && s.codes.iter().all(|c| in_layers(*c))
        {
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
    // In layer order: code, graph, node, operator.
    all.sort_by_key(|d| d.code.layer());

    let mut baselined = 0;
    let mut on_main = 0;
    let mut below = 0;
    let mut reported = Vec::new();
    for d in &all {
        if opts.state.is_some_and(|c| c.on_main.contains(d)) {
            on_main += 1;
        } else if opts.baseline.is_some_and(|b| b.contains(d)) {
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
        on_main,
        below_threshold: below,
        suppressed,
        all,
        root_causes,
    }
}

/// Whether findings with `code` are reported when `layers` are selected (empty = all).
fn layer_selected(code: Code, layers: &BTreeSet<Layer>) -> bool {
    layers.is_empty() || layers.contains(&code.layer())
}

/// Each layer depends on the ones before it. When Cardinal couldn't analyze a model at
/// some layer (an E-code), its findings from later layers are dropped: they would rest
/// on what failed.
fn without_blocked(raw: &mut Vec<Diagnostic>) {
    let mut failed: BTreeMap<String, Layer> = BTreeMap::new();
    for d in raw.iter().filter(|d| d.code.is_tool_error()) {
        let l = failed.entry(d.relation.clone()).or_insert(d.code.layer());
        *l = (*l).min(d.code.layer());
    }
    raw.retain(|d| {
        d.code.is_tool_error() || failed.get(&d.relation).is_none_or(|l| d.code.layer() <= *l)
    });
}
