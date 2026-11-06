use super::report::{coverage, pct, unlock_hint};
use super::{init_threads, load, plural};
use crate::cli::Format;
use cardinal_dbt::RelationId;
use cardinal_engine::{Analysis, Findings, Options};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// A to-do list: the sources and models to declare grain on, ranked by how many
/// downstream models each unlocks.
pub fn run(dir: Option<PathBuf>, format: Format) -> u8 {
    let (project, config) = match load(dir) {
        Ok(x) => x,
        Err(c) => return c,
    };
    init_threads(None);
    let selected: BTreeSet<RelationId> = project.models().map(|r| r.id.clone()).collect();
    let a = Analysis::new(&project, &config, selected, true);
    let (run, selected) = (&a.run, &a.selected);
    let severities = match config.severities(&[], &[]) {
        Ok(s) => s,
        Err(e) => {
            super::eprint_error(&e);
            return super::EXIT_TOOL;
        }
    };
    let findings = a.findings(&a.shapes(), &Options::all_findings(&severities));
    let cov = coverage(&project, run, selected);
    let planned = selected
        .iter()
        .filter(|id| run.results.get(*id).is_some_and(|r| r.planned))
        .count();
    let display = |id: &RelationId| {
        project
            .get(id)
            .map(|r| r.display.clone())
            .unwrap_or(id.0.clone())
    };

    if format != Format::Text {
        let v = json!({
            "schema_version": cardinal_diag::render::JSON_SCHEMA_VERSION,
            "models": cov.models,
            "models_planned": planned,
            "models_with_known_grain": cov.known,
            "root_causes": findings.root_causes.iter().map(|r| json!({
                "relation": r.root.0,
                "name": display(&r.root),
                "reason": r.reason.describe(&display),
                "unlocks": r.affected.iter().map(|a| a.0.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "guesses": project.guesses.guesses.iter().map(|g| json!({
                "hypothesis": g.hypothesis,
                "description": g.hypothesis.describe(&display),
                "passed": g.passed,
                "failures": g.failures,
                "test": g.test,
                "generated_at": g.generated_at,
                "target": g.target,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return 0;
    }
    println!(
        "{} of {} have a known key ({}%); Cardinal could read the SQL of {} ({}%).",
        cov.known,
        plural(cov.models, "model", "models"),
        pct(cov.known, cov.models),
        planned,
        pct(planned, cov.models)
    );
    if !findings.root_causes.is_empty() {
        root_causes(&findings, &display);
    }
    let log = &project.guesses;
    if !log.guesses.is_empty() {
        println!();
        println!("Cardinal's guesses, after dbt tested them:");
        for g in &log.guesses {
            if g.passed {
                println!(
                    "  used     {} (dbt test passed on {})",
                    g.hypothesis.describe(&display),
                    g.when()
                );
            } else {
                println!(
                    "  dropped  {}: dbt found {}",
                    g.hypothesis.refuted(&display),
                    g.hypothesis.failing_rows(g.failures)
                );
            }
        }
    }
    if let Some(h) = unlock_hint(&project, &findings, false) {
        println!();
        println!("Next: {h}");
    }
    0
}

fn root_causes(findings: &Findings, display: &dyn Fn(&RelationId) -> String) {
    println!();
    println!("To check the rest, fix these, most models first:");
    let rows: Vec<(String, String, usize)> = findings
        .root_causes
        .iter()
        .map(|r| {
            (
                display(&r.root),
                r.reason.describe(display),
                r.affected.len(),
            )
        })
        .collect();
    let w = rows.iter().map(|r| r.1.len()).max().unwrap_or(0) + 3;
    for (i, (_, reason, n)) in rows.iter().enumerate() {
        println!(
            "  {:>2}. {reason:<w$}{}",
            i + 1,
            plural(*n, "model", "models")
        );
    }
}
