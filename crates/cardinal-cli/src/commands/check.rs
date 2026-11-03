use super::{EXIT_TOOL, color, eprint_error, load, plural, source_files};
use crate::cli::{CheckArgs, Format};
use crate::config::Config;
use crate::engine::{Engine, RunResult};
use crate::pipeline::{self, Findings, Options};
use cardinal_dbt::{Project, RelationId, select};
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
        min_severity,
    };
    let findings = pipeline::collect(project, &run, &selected, &opts);

    if args.update_baseline {
        let b = Baseline::from_findings(&findings.all);
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
            Style {
                color: to_stdout && color(true),
            },
        ),
        Format::Json => render::json(
            &findings.reported,
            summary_json(project, &run, &selected, &findings),
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

fn pct(a: usize, b: usize) -> usize {
    if b == 0 { 100 } else { (a * 100 + b / 2) / b }
}

/// "declare grain on 3 sources to cover 41 more models"
pub fn unlock_hint(project: &Project, findings: &Findings) -> Option<String> {
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
    let names: Vec<String> = top
        .iter()
        .map(|r| {
            project
                .get(&r.root)
                .map(|x| x.display.clone())
                .unwrap_or(r.root.0.clone())
        })
        .collect();
    let n = top.len();
    let m = covered.len();
    Some(format!(
        "declare grain on {} to cover {m} more {}: {}",
        plural(n, "source", "sources"),
        if m == 1 { "model" } else { "models" },
        names.join(", ")
    ))
}

fn text_report(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
    findings: &Findings,
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
        "Analyzed {} in {:.2}s{cached}: {} with known grain ({}%).\n",
        plural(cov.models, "model", "models"),
        run.elapsed.as_secs_f64(),
        cov.known,
        pct(cov.known, cov.models)
    ));
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
    if let Some(h) = unlock_hint(project, findings) {
        out.push_str(&format!("Next: {h} (see cardinal coverage).\n"));
    }
    out
}

fn summary_json(
    project: &Project,
    run: &RunResult,
    selected: &BTreeSet<RelationId>,
    findings: &Findings,
) -> serde_json::Value {
    let cov = coverage(project, run, selected);
    json!({
        "models_analyzed": cov.models,
        "models_with_known_grain": cov.known,
        "findings_by_code": pipeline::count_by_code(&findings.reported).into_iter().map(|(c, n)| (c.to_string(), n)).collect::<std::collections::BTreeMap<_, _>>(),
        "baselined": findings.baselined,
        "suppressed": findings.suppressed,
        "below_min_severity": findings.below_threshold,
        "elapsed_ms": run.elapsed.as_millis() as u64,
        "cache_hits": run.cache_hits,
    })
}
