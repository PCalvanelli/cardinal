use super::{EXIT_TOOL, eprint_error, load};
use crate::engine::Engine;
use cardinal_dbt::{Assumption, Kind, Project, RelationId};
use cardinal_grain::{ColSet, EvidenceArena, GrainStatus, Half, Site};
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
    if !result.verdicts.is_empty() {
        println!();
        println!("rows");
        let w = result
            .verdicts
            .iter()
            .map(|v| v.display.len())
            .max()
            .unwrap_or(0)
            + 4;
        for v in &result.verdicts {
            let mut tags = Vec::new();
            if v.declared {
                tags.push("contract");
            }
            if v.site == Site::AggregateInput {
                tags.push("rows the aggregate reads");
            }
            let tags = if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join("; "))
            };
            println!("  {:<w$}{}{tags}", v.display, v.summary());
            for (name, h) in [
                ("at least once", &v.at_least_once),
                ("at most once", &v.at_most_once),
            ] {
                if let Half::NotProven { reasons } = h {
                    for r in reasons {
                        println!("      not {name}: {}", r.text(&v.display, &display));
                    }
                }
            }
        }
    }
    let mut assumed: BTreeSet<Assumption> = BTreeSet::new();
    if let Some(k) = s.keys.first() {
        assumed.extend(k.assumptions.iter().cloned());
    }
    for v in &result.verdicts {
        if v.at_least_once.is_proven() || v.at_most_once.is_proven() {
            assumed.extend(v.assumptions.iter().cloned());
        }
    }
    if !assumed.is_empty() {
        println!();
        println!("assumes");
        for a in &assumed {
            println!("  {}", a.describe(&display));
            println!("      {}", status_line(&project, a));
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

/// "checked by test unique_orders_order_id" or "unchecked: declared by meta.cardinal.grain".
pub fn status_line(project: &Project, a: &Assumption) -> String {
    let st = project.assumption_status(a);
    let mut line = if st.is_checked() {
        format!(
            "checked by {}",
            st.checked_by
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" and ")
        )
    } else if st.declared_by.is_empty() {
        "unchecked".to_string()
    } else {
        format!("unchecked: declared by {}", st.declared_by.join(" and "))
    };
    if !st.is_checked() {
        for c in &st.checked_by {
            line.push_str(&format!("; {c} does not fail the build"));
        }
    }
    for c in &st.caveats {
        line.push_str(&format!("; {c}"));
    }
    line
}
