use super::{EXIT_TOOL, color, eprint_error, load, plural, source_files};
use crate::cli::{CheckArgs, Format};
use crate::config::Config;
use crate::engine::{Engine, RunResult};
use crate::pipeline::{self, Findings, Options};
use crate::state::{self, Comparison};
use cardinal_dbt::{Hypothesis, Project, RelationId, select};
use cardinal_diag::baseline::Baseline;
use cardinal_diag::render::{self, Style};
use cardinal_diag::{Code, Severity};
use cardinal_grain::UnknownReason;
use serde_json::json;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::PathBuf;

pub fn run(dir: Option<PathBuf>, args: CheckArgs) -> u8 {
    let (project, config) = match load(dir) {
        Ok(x) => x,
        Err(c) => return c,
    };
    run_loaded(&project, &config, &args)
}

pub fn init_threads(jobs: Option<usize>) {
    if let Some(n) = jobs {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global();
    }
}

/// Models to report on, from --select, positional selectors and --changed-since.
pub fn selection(project: &Project, args: &CheckArgs) -> Result<BTreeSet<RelationId>, String> {
    let mut selectors = args.select.clone();
    selectors.extend(args.positional.iter().cloned());
    let mut sel = if selectors.is_empty() && args.changed_since.is_none() {
        project.models().map(|r| r.id.clone()).collect()
    } else {
        select::select(project, &selectors)?
    };
    if let Some(r) = &args.changed_since {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&project.root)
            .args(["diff", "--name-only", "--relative", r])
            .output()
            .map_err(|e| format!("could not run git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git diff {r} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect();
        sel.extend(select::changed(project, &files));
    }
    Ok(sel)
}

/// The selected models plus everything upstream of them, which analysis needs.
pub fn closure(project: &Project, selected: &BTreeSet<RelationId>) -> BTreeSet<RelationId> {
    let mut needed = selected.clone();
    for id in selected {
        needed.extend(project.ancestors(id));
    }
    needed
}

pub fn run_loaded(project: &Project, config: &Config, args: &CheckArgs) -> u8 {
    init_threads(args.jobs);
    let parse_codes =
        |v: &[String]| -> Result<Vec<Code>, String> { v.iter().map(|s| s.parse()).collect() };
    let (deny, allow) = match (parse_codes(&args.deny), parse_codes(&args.allow)) {
        (Ok(d), Ok(a)) => (d, a),
        (Err(e), _) | (_, Err(e)) => {
            eprint_error(&e);
            return EXIT_TOOL;
        }
    };
    let min_severity: Severity = match args.min_severity.parse() {
        Ok(s) => s,
        Err(e) => {
            eprint_error(&e);
            return EXIT_TOOL;
        }
    };
    let severities = match config.severities(&deny, &allow) {
        Ok(s) => s,
        Err(e) => {
            eprint_error(&e);
            return EXIT_TOOL;
        }
    };
    let selected = match selection(project, args) {
        Ok(s) => s,
        Err(e) => {
            eprint_error(&e);
            return EXIT_TOOL;
        }
    };
    let engine = Engine::new(project, config, !args.no_cache);
    let run = engine.run(&closure(project, &selected));
    // A baseline records this branch's own findings; a comparison with main would add
    // P003 findings to it and hide findings main has, so --update-baseline skips it.
    let state_dir = if args.update_baseline {
        if args.state.is_some() {
            eprintln!("warning: --state is ignored with --update-baseline");
        }
        None
    } else {
        state::state_for(args, &project.root)
    };
    let comparison = match state_dir {
        None => None,
        Some(dir) => {
            let c = state::compare(
                project,
                &run,
                &selected,
                config,
                &severities,
                &dir,
                !args.no_cache,
            );
            match c {
                Ok(c) => Some(c),
                Err(e) => {
                    eprint_error(&format!("--state: {e}"));
                    return EXIT_TOOL;
                }
            }
        }
    };

    let baseline_path = args
        .baseline
        .clone()
        .or_else(|| config.baseline.path.as_ref().map(|p| project.root.join(p)))
        .unwrap_or_else(|| project.root.join(".cardinal/baseline.json"));
    let use_baseline =
        args.baseline.is_some() || (config.baseline.path.is_some() && baseline_path.is_file());
    let baseline = if use_baseline && !args.update_baseline {
        match Baseline::load(&baseline_path) {
            Ok(b) => Some(b),
            Err(e) => {
                eprint_error(&format!("could not read baseline {e}"));
                return EXIT_TOOL;
            }
        }
    } else {
        None
    };
    let opts = Options {
        severities: &severities,
        baseline: baseline.as_ref(),
        state: comparison.as_ref(),
        min_severity,
    };
    let findings = pipeline::collect(project, &run, &selected, &opts);

    if args.update_baseline {
        let own: Vec<_> = findings
            .all
            .iter()
            .filter(|d| d.code != Code::P003)
            .cloned()
            .collect();
        let b = Baseline::from_findings(&own);
        if let Err(e) = b.save(&baseline_path) {
            eprint_error(&format!("could not write {}: {e}", baseline_path.display()));
            return EXIT_TOOL;
        }
        eprintln!(
            "Recorded {} in {}; later runs with --baseline report only new ones.",
            plural(b.entries.len(), "finding", "findings"),
            baseline_path
                .strip_prefix(&project.root)
                .unwrap_or(&baseline_path)
                .display()
        );
        return 0;
    }

    let to_stdout = args.output.is_none();
    let body = match args.format {
        Format::Text => text_report(
            project,
            &run,
            &selected,
            &findings,
            comparison.as_ref(),
            Style {
                color: to_stdout && color(true),
            },
        ),
        Format::Json => render::json(
            &findings.reported,
            summary_json(project, &run, &selected, &findings, comparison.as_ref()),
        ),
        Format::Sarif => render::sarif(&findings.reported),
    };
    let write = match &args.output {
        Some(p) => std::fs::write(p, &body),
        None => std::io::stdout().write_all(body.as_bytes()),
    };
    if let Err(e) = write {
        eprint_error(&format!("could not write output: {e}"));
        return EXIT_TOOL;
    }
    if findings
        .reported
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        1
    } else {
        0
    }
}

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

/// Declared exactly-once contracts in the selection: (proven, not proven).
pub fn contracts(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
) -> (usize, usize) {
    let mut proven = 0;
    let mut not = 0;
    for id in selected {
        let Some(rel) = project.get(id) else { continue };
        for e in &rel.exactly_once {
            let ok = e.relation.as_ref().is_some_and(|t| {
                run.results.get(id).is_some_and(|r| {
                    r.verdicts
                        .iter()
                        .any(|v| v.declared && &v.relation == t && v.exactly_once())
                })
            });
            if ok {
                proven += 1;
            } else {
                not += 1;
            }
        }
    }
    (proven, not)
}

fn pct(a: usize, b: usize) -> usize {
    if b == 0 { 100 } else { (a * 100 + b / 2) / b }
}

/// The `Next:` line when sources without a known key hide models: test Cardinal's
/// pending guesses, make some, or declare the keys.
pub fn unlock_hint(project: &Project, findings: &Findings, see_coverage: bool) -> Option<String> {
    let sources: Vec<&crate::pipeline::RootCause> = findings
        .root_causes
        .iter()
        .filter(|r| matches!(r.reason, UnknownReason::SourceWithoutKey(_)))
        .collect();
    if sources.is_empty() {
        return None;
    }
    let top: Vec<&crate::pipeline::RootCause> = sources.iter().take(3).copied().collect();
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

fn text_report(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
    findings: &Findings,
    comparison: Option<&Comparison>,
    st: Style,
) -> String {
    let files = source_files(project);
    let mut out = String::new();
    for d in &findings.reported {
        out.push_str(&render::text(d, &files, st));
        out.push('\n');
    }
    let cov = coverage(project, run, selected);
    let errors = findings
        .reported
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count();
    let warnings = findings
        .reported
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count();
    let infos = findings.reported.len() - errors - warnings;
    let cached = if run.cache_hits > 0 {
        format!(", {} cached", run.cache_hits)
    } else {
        String::new()
    };
    out.push_str(&format!(
        "Analyzed {} in {:.2}s{cached}: {} with a known key ({}%).\n",
        plural(cov.models, "model", "models"),
        run.elapsed.as_secs_f64(),
        cov.known,
        pct(cov.known, cov.models)
    ));
    let (proven, not) = contracts(project, run, selected);
    if proven + not > 0 {
        out.push_str(&format!(
            "Contracts: {proven} of {} proven.\n",
            plural(
                proven + not,
                "exactly_once contract",
                "exactly_once contracts"
            )
        ));
    }
    if let Some(c) = comparison {
        out.push_str(&comparison_line(c, findings));
    }
    if let Some(g) = guesses_line(project) {
        out.push_str(&g);
    }
    let by_code = pipeline::count_by_code(&findings.reported);
    let codes: Vec<String> = by_code.iter().map(|(c, n)| format!("{c} ×{n}")).collect();
    let mut line = if findings.reported.is_empty() {
        "No findings".to_string()
    } else {
        let mut parts = vec![
            plural(errors, "error", "errors"),
            plural(warnings, "warning", "warnings"),
        ];
        if infos > 0 {
            parts.push(plural(infos, "info", "infos"));
        }
        format!("Findings: {} ({})", parts.join(", "), codes.join(", "))
    };
    let mut hidden = Vec::new();
    if findings.below_threshold > 0 {
        hidden.push(format!("{} below --min-severity", findings.below_threshold));
    }
    if findings.baselined > 0 {
        hidden.push(format!("{} in the baseline", findings.baselined));
    }
    if findings.suppressed > 0 {
        hidden.push(format!("{} suppressed", findings.suppressed));
    }
    if !hidden.is_empty() {
        line.push_str(&format!("; {}", hidden.join(", ")));
    }
    out.push_str(&line);
    out.push_str(".\n");
    if let Some(h) = unlock_hint(project, findings, true) {
        out.push_str(&format!("Next: {h}\n"));
    }
    out
}

/// "Guesses: 2 passed dbt's tests and are used; 1 failed and is not (see cardinal coverage)."
fn guesses_line(project: &Project) -> Option<String> {
    let log = &project.guesses;
    if log.guesses.is_empty() {
        return None;
    }
    let passed = log.passed().count();
    let failed = log.failed().count();
    let mut parts = Vec::new();
    if passed > 0 {
        parts.push(format!(
            "{passed} passed dbt's tests and {} used",
            if passed == 1 { "is" } else { "are" }
        ));
    }
    if failed > 0 {
        parts.push(format!(
            "{failed} failed and {} not",
            if failed == 1 { "is" } else { "are" }
        ));
    }
    Some(format!(
        "Cardinal's guesses: {} (see cardinal coverage).\n",
        parts.join("; ")
    ))
}

/// "Compared with main (state: prod-artifacts/): 1 guarantee lost, 2 gained, 41 kept."
fn comparison_line(c: &Comparison, findings: &Findings) -> String {
    let mut line = format!(
        "Compared with main (state: {}): {} lost, {} gained, {} kept",
        c.dir,
        plural(c.lost, "guarantee", "guarantees"),
        c.gained,
        c.kept
    );
    if findings.on_main > 0 {
        line.push_str(&format!(
            "; {} already on main, not shown",
            plural(findings.on_main, "finding", "findings")
        ));
    }
    if c.skipped > 0 {
        line.push_str(&format!(
            "; {} on main had no compiled SQL, so {} not compared",
            plural(c.skipped, "model", "models"),
            if c.skipped == 1 {
                "it was"
            } else {
                "they were"
            }
        ));
    }
    line.push_str(".\n");
    line
}

fn summary_json(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
    findings: &Findings,
    comparison: Option<&Comparison>,
) -> serde_json::Value {
    let cov = coverage(project, run, selected);
    let (proven, not) = contracts(project, run, selected);
    let mut v = json!({
        "models_analyzed": cov.models,
        "models_with_known_grain": cov.known,
        "contracts_proven": proven,
        "contracts_not_proven": not,
        "findings_by_code": pipeline::count_by_code(&findings.reported).into_iter().map(|(c, n)| (c.to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
        "baselined": findings.baselined,
        "suppressed": findings.suppressed,
        "below_min_severity": findings.below_threshold,
        "elapsed_ms": run.elapsed.as_millis() as u64,
        "cache_hits": run.cache_hits,
        "guesses_used": project.guesses.passed().count(),
        "guesses_failed": project.guesses.failed().count(),
    });
    if let Some(c) = comparison {
        v["comparison"] = json!({
            "state": c.dir,
            "guarantees_lost": c.lost,
            "guarantees_gained": c.gained,
            "guarantees_kept": c.kept,
            "models_not_compared": c.skipped,
            "findings_on_main": findings.on_main,
        });
    }
    v
}
