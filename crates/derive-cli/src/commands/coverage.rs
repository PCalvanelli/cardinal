use super::check::{closure, coverage, init_threads};
use super::{load, plural};
use crate::cli::Format;
use crate::engine::Engine;
use crate::pipeline::{self, Options};
use derive_dbt::RelationId;
use derive_diag::Severity;
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
    let engine = Engine::new(&project, &config, true);
    let run = engine.run(&closure(&project, &selected));
    let severities = match config.severities(&[], &[]) {
        Ok(s) => s,
        Err(e) => {
            super::eprint_error(&e);
            return super::EXIT_TOOL;
        }
    };
    let opts = Options {
        severities: &severities,
        baseline: None,
        min_severity: Severity::Info,
    };
    let findings = pipeline::collect(&project, &run, &selected, &opts);
    let cov = coverage(&project, &run, &selected);
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
            "schema_version": derive_diag::render::JSON_SCHEMA_VERSION,
            "models": cov.models,
            "models_planned": planned,
            "models_with_known_grain": cov.known,
            "root_causes": findings.root_causes.iter().map(|r| json!({
                "relation": r.root.0,
                "name": display(&r.root),
                "reason": r.reason.describe(&display),
                "unlocks": r.affected.iter().map(|a| a.0.clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return 0;
    }
    let pct = |a: usize, b: usize| if b == 0 { 100 } else { (a * 100 + b / 2) / b };
    println!(
        "{} of {} have known grain ({}%); {} planned without E-codes ({}%).",
        cov.known,
        plural(cov.models, "model", "models"),
        pct(cov.known, cov.models),
        planned,
        pct(planned, cov.models)
    );
    if findings.root_causes.is_empty() {
        return 0;
    }
    println!();
    println!("To cover the rest, in order of models unlocked:");
    let rows: Vec<(String, String, usize)> = findings
        .root_causes
        .iter()
        .map(|r| {
            (
                display(&r.root),
                r.reason.describe(&display),
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
    0
}
