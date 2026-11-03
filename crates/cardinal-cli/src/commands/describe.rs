use super::{EXIT_TOOL, eprint_error, load};
use crate::engine::Engine;
use cardinal_dbt::{Kind, RelationId};
use cardinal_grain::{ColSet, EvidenceArena, GrainStatus};
use std::collections::BTreeSet;
use std::path::PathBuf;

pub fn run(dir: Option<PathBuf>, query: &str) -> u8 {
    let (project, config) = match load(dir) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let rel = project.find_model(query).or_else(|| {
        project
            .relations
            .values()
            .find(|r| r.display == query || r.id.0 == query)
    });
    let Some(rel) = rel else {
        eprint_error(&format!("no model or source named `{query}`"));
        return EXIT_TOOL;
    };
    let mut needed: BTreeSet<RelationId> = project.ancestors(&rel.id);
    needed.insert(rel.id.clone());
    let engine = Engine::new(&project, &config, true);
    let run = engine.run(&needed);
    let Some(result) = run.results.get(&rel.id) else {
        eprint_error("analysis produced no result");
        return EXIT_TOOL;
    };
    let s = &result.summary;
    let display = |id: &RelationId| {
        project
            .get(id)
            .map(|r| r.display.clone())
            .unwrap_or(id.0.clone())
    };
    let names = |cols: &[usize]| -> String {
        format!(
            "({})",
            cols.iter()
                .map(|c| s.columns[*c].name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let names_set = |cols: &ColSet| -> String { names(&cols.iter().copied().collect::<Vec<_>>()) };

    println!("{}  ({})", rel.id, rel.file);
    if rel.kind == Kind::Source {
        println!("source  {}", rel.display);
    }
    println!();
    match s.keys.first() {
        Some(k) => {
            println!("grain   {}", names(&k.cols));
            let why = match (&k.declared, k.why.is_empty()) {
                (Some(origin), true) => {
                    if result.planned {
                        format!("declared, not proven: {} (G002)", origin.describe())
                    } else {
                        format!("declared: {}", origin.describe())
                    }
                }
                _ => format!("proven: {}", k.why),
            };
            println!("        {why}");
            for k in s.keys.iter().skip(1) {
                println!("also    {}", names(&k.cols));
            }
        }
        None => {
            let reason = s
                .status
                .reason()
                .map(|r| r.describe(&display))
                .unwrap_or_default();
            println!("grain   unknown");
            if !reason.is_empty() {
                println!("        {reason}");
            }
        }
    }
    if let GrainStatus::Partial(r) = &s.status {
        println!("        partial: {}", r.describe(&display));
    }

    let mut arena = EvidenceArena::default();
    let facts = s.to_facts(&mut arena);
    let width = s
        .columns
        .iter()
        .map(|c| c.name.len())
        .max()
        .unwrap_or(6)
        .max(6)
        + 2;
    let grains: Vec<String> = (0..s.columns.len())
        .map(|c| match facts.native_grain(c) {
            Some(g) if g.is_empty() => "constant".into(),
            Some(g) => names_set(&g),
            None => "unknown".into(),
        })
        .collect();
    let gw = grains.iter().map(|g| g.len()).max().unwrap_or(12).max(12) + 2;
    println!();
    println!("{:<width$} {:<gw$} nulls", "column", "native grain");
    for (c, col) in s.columns.iter().enumerate() {
        let nulls = if s.not_null.contains(&c) {
            "never"
        } else {
            "possible"
        };
        println!("{:<width$} {:<gw$} {nulls}", col.name, grains[c]);
    }
    if !result.joins.is_empty() {
        println!();
        println!("joins");
        let rows: Vec<(String, String)> = result
            .joins
            .iter()
            .map(|j| {
                let on = if j.on.is_empty() {
                    String::new()
                } else {
                    format!(" on {}", j.on.join(", "))
                };
                (
                    format!("{} -> {}{on}", j.left, j.right),
                    j.cardinality
                        .map(|c| c.label().to_string())
                        .unwrap_or_else(|| "unknown".into()),
                )
            })
            .collect();
        let w = rows.iter().map(|r| r.0.len()).max().unwrap_or(0) + 4;
        for (a, b) in rows {
            println!("  {a:<w$}{b}");
        }
    }
    if !result.diagnostics.is_empty() {
        println!();
        println!("findings");
        for d in &result.diagnostics {
            println!("  {}[{}]: {}", d.severity, d.code, d.message);
        }
    }
    0
}
