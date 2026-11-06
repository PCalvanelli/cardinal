//! Coverage, counts and hints that several commands print.

use super::plural;
use crate::engine::RunResult;
use crate::pipeline::{Findings, RootCause};
use cardinal_dbt::{Hypothesis, Project, RelationId};
use cardinal_diag::{Code, Diagnostic, Layer};
use cardinal_grain::UnknownReason;
use std::collections::{BTreeMap, BTreeSet};

pub struct Coverage {
    pub models: usize,
    pub known: usize,
}

pub fn coverage(project: &Project, run: &RunResult, selected: &BTreeSet<RelationId>) -> Coverage {
    let models: Vec<&RelationId> = selected
        .iter()
        .filter(|id| project.get(id).is_some_and(|r| r.is_model()))
        .collect();
    let known = models
        .iter()
        .filter(|id| {
            run.results
                .get(**id)
                .is_some_and(|r| r.summary.has_known_grain())
        })
        .count();
    Coverage {
        models: models.len(),
        known,
    }
}

pub fn pct(a: usize, b: usize) -> usize {
    if b == 0 { 100 } else { (a * 100 + b / 2) / b }
}

/// The `Next:` line when sources without a known key hide models: test Cardinal's
/// pending guesses, make some, or declare the keys.
pub fn unlock_hint(project: &Project, findings: &Findings, see_coverage: bool) -> Option<String> {
    let sources: Vec<&RootCause> = findings
        .root_causes
        .iter()
        .filter(|r| matches!(r.reason, UnknownReason::SourceWithoutKey(_)))
        .collect();
    if sources.is_empty() {
        return None;
    }
    let top: Vec<&RootCause> = sources.iter().take(3).copied().collect();
    let covered: BTreeSet<&RelationId> = top.iter().flat_map(|r| r.affected.iter()).collect();
    let display = |id: &RelationId| {
        project
            .get(id)
            .map(|x| x.display.clone())
            .unwrap_or(id.0.clone())
    };
    let names: Vec<String> = top.iter().map(|r| display(&r.root)).collect();
    let n = top.len();
    let m = covered.len();
    let is_key_of = |h: &Hypothesis, id: &RelationId| matches!(h, Hypothesis::Key { relation, .. } if relation == id);
    // Guess tests dbt has not run yet.
    let pending: Vec<String> = top
        .iter()
        .filter(|r| {
            project.guess_tests.iter().any(|t| {
                is_key_of(&t.hypothesis, &r.root) && project.guesses.get(&t.hypothesis).is_none()
            })
        })
        .map(|r| display(&r.root))
        .collect();
    if !pending.is_empty() {
        return Some(format!(
            "run dbt test --select tag:cardinal to check Cardinal's guessed {} for {}.",
            if pending.len() == 1 { "key" } else { "keys" },
            pending.join(", ")
        ));
    }
    let unguessed = top.iter().any(|r| {
        !project
            .guesses
            .guesses
            .iter()
            .any(|g| is_key_of(&g.hypothesis, &r.root))
    });
    let hide = format!(
        "{} without a known key {} {} {} from being checked: {}",
        plural(n, "source", "sources"),
        if n == 1 { "keeps" } else { "keep" },
        m,
        if m == 1 { "model" } else { "models" },
        names.join(", ")
    );
    let see = if see_coverage {
        " (see cardinal coverage)"
    } else {
        ""
    };
    Some(if unguessed {
        format!(
            "{hide}. Run cardinal guess so dbt can test likely keys, or declare {}{see}.",
            if n == 1 { "it" } else { "them" }
        )
    } else {
        format!(
            "{hide}. Declare {} in YAML{see}.",
            if n == 1 { "its key" } else { "their keys" }
        )
    })
}

/// Counts per code, grouped by layer.
pub fn count_by_layer(findings: &[Diagnostic]) -> BTreeMap<Layer, BTreeMap<Code, usize>> {
    let mut m: BTreeMap<Layer, BTreeMap<Code, usize>> = BTreeMap::new();
    for d in findings {
        *m.entry(d.code.layer())
            .or_default()
            .entry(d.code)
            .or_default() += 1;
    }
    m
}

pub fn count_by_code(findings: &[Diagnostic]) -> BTreeMap<Code, usize> {
    let mut m = BTreeMap::new();
    for d in findings {
        *m.entry(d.code).or_default() += 1;
    }
    m
}
