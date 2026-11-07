//! `cardinal assumptions`: the declared facts Cardinal's proofs rest on, whether a dbt
//! test or the warehouse checks each one, and the tests that would check the rest.

use super::{EXIT_TOOL, eprint_error, load, plural};
use crate::cli::Format;
use cardinal_dbt::{Assumption, Kind, Project, Relation, RelationId, Status};
use cardinal_engine::{Engine, RunResult};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub struct Entry {
    pub assumption: Assumption,
    pub status: Status,
    /// Models whose proofs rest on it.
    pub used_by: BTreeSet<RelationId>,
}

/// Every assumption a model's grain or exactly-once proofs rest on, unchecked first,
/// then most used first.
pub fn ledger(project: &Project, run: &RunResult) -> Vec<Entry> {
    let mut used: BTreeMap<Assumption, BTreeSet<RelationId>> = BTreeMap::new();
    for (id, r) in &run.results {
        if !project.get(id).is_some_and(|r| r.is_model()) {
            continue;
        }
        let mut add = |a: &Assumption| {
            used.entry(a.clone()).or_default().insert(id.clone());
        };
        if let Some(k) = r.summary.keys.first() {
            k.assumptions.iter().for_each(&mut add);
        }
        for v in &r.verdicts {
            if v.at_least_once.is_proven() || v.at_most_once.is_proven() {
                v.assumptions.iter().for_each(&mut add);
            }
        }
    }
    let mut out: Vec<Entry> = used
        .into_iter()
        .map(|(a, used_by)| Entry {
            status: project.assumption_status(&a),
            assumption: a,
            used_by,
        })
        .collect();
    out.sort_by_key(|e| {
        (
            e.status.is_checked(),
            std::cmp::Reverse(e.used_by.len()),
            e.assumption.clone(),
        )
    });
    out
}

pub fn run(dir: Option<PathBuf>, format: Format, emit_tests: bool) -> u8 {
    let (project, config) = match load(dir) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let needed: BTreeSet<RelationId> = project.relations.keys().cloned().collect();
    let engine = Engine::new(&project, &config, true);
    let run = engine.run(&needed);
    let entries = ledger(&project, &run);
    let display = |id: &RelationId| project.display(id);
    if emit_tests {
        print!("{}", emit(&project, &entries));
        return 0;
    }
    match format {
        Format::Json => {
            let v: Vec<serde_json::Value> = entries
                .iter()
                .map(|e| {
                    json!({
                        "assumption": e.assumption,
                        "description": e.assumption.describe(&display),
                        "checked": e.status.is_checked(),
                        "checked_once": e.status.checked_once(),
                        "declared_by": e.status.declared_by,
                        "guessed": e.status.guessed,
                        "checked_by": e.status.checked_by.iter().map(|c| c.to_string()).collect::<Vec<_>>(),
                        "caveats": e.status.caveats,
                        "used_by": e.used_by.iter().map(&display).collect::<Vec<_>>(),
                    })
                })
                .collect();
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({ "assumptions": v })).unwrap()
            );
        }
        Format::Sarif => {
            eprint_error("--format sarif is for findings; use text or json");
            return EXIT_TOOL;
        }
        Format::Text => {
            let unchecked = entries.iter().filter(|e| !e.status.is_checked()).count();
            if entries.is_empty() {
                println!("No proof relies on a declared fact yet.");
                return 0;
            }
            println!(
                "Cardinal takes {} on trust; {}.",
                plural(entries.len(), "fact", "facts"),
                match unchecked {
                    0 => "dbt tests check all of them".to_string(),
                    1 => "1 isn't checked when dbt runs".to_string(),
                    n => format!("{n} aren't checked when dbt runs"),
                }
            );
            for e in &entries {
                println!();
                let tag = if e.status.is_checked() {
                    "checked  "
                } else if e.status.checked_once() {
                    "ONCE     "
                } else {
                    "UNCHECKED"
                };
                println!("{tag}  {}", e.assumption.describe(&display));
                let st = &e.status;
                let mut how = if st.is_checked() {
                    let mut h = format!(
                        "by {}",
                        st.checked_by
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(" and ")
                    );
                    if let Some(g) = &st.guessed {
                        h.push_str(&format!(" (Cardinal's guess, passed on {})", g.date));
                    }
                    h
                } else if st.checked_once() {
                    format!(
                        "{}, but that test is no longer in the project, so dbt does not check it again",
                        st.origin()
                    )
                } else {
                    let mut h = st.origin();
                    for c in &st.checked_by {
                        h.push_str(&format!("; {}", cardinal_dbt::weak_check(c)));
                    }
                    h
                };
                for c in &st.caveats {
                    how.push_str(&format!("; {c}"));
                }
                println!("           {how}");
                let names: Vec<String> = e.used_by.iter().map(&display).collect();
                let shown = if names.len() > 4 {
                    format!("{} and {} more", names[..4].join(", "), names.len() - 4)
                } else {
                    names.join(", ")
                };
                println!("           used by {shown}");
            }
            if unchecked > 0 {
                println!();
                println!(
                    "Run `cardinal assumptions --emit-tests` for dbt tests that check the {}.",
                    if unchecked == 1 {
                        "unchecked one".to_string()
                    } else {
                        format!("{unchecked} unchecked ones")
                    }
                );
            }
        }
    }
    0
}

/// A test to add to one column, or to the relation when `column` is None.
struct TestYaml {
    column: Option<String>,
    lines: Vec<String>,
}

/// YAML for dbt tests that check every unchecked assumption, grouped by relation.
pub fn emit(project: &Project, entries: &[Entry]) -> String {
    let key = if project
        .dbt_version
        .as_deref()
        .is_some_and(|v| version_at_least(v, 1, 8))
    {
        "data_tests"
    } else {
        "tests"
    };
    let mut by_rel: BTreeMap<RelationId, Vec<TestYaml>> = BTreeMap::new();
    let mut skipped: Vec<String> = Vec::new();
    let display = |id: &RelationId| project.display(id);
    let mut fix_existing: Vec<String> = Vec::new();
    for e in entries.iter().filter(|e| !e.status.is_checked()) {
        let a = &e.assumption;
        if !e.status.weak_tests.is_empty() {
            fix_existing.push(format!(
                "# {}: make {} fail the build (severity: error, no `where` filter)",
                a.describe(&display),
                e.status
                    .weak_tests
                    .iter()
                    .map(|t| format!("test {t}"))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ));
            continue;
        }
        let tests = by_rel.entry(a.relation().clone()).or_default();
        match a {
            Assumption::Key { cols, .. } if cols.len() == 1 => tests.push(TestYaml {
                column: Some(cols[0].clone()),
                lines: vec!["- unique".into()],
            }),
            Assumption::Key { cols, .. } => tests.push(TestYaml {
                column: None,
                lines: vec![
                    "- dbt_utils.unique_combination_of_columns:".into(),
                    format!("    combination_of_columns: [{}]", cols.join(", ")),
                ],
            }),
            Assumption::NotNull { column, .. } => tests.push(TestYaml {
                column: Some(column.clone()),
                lines: vec!["- not_null".into()],
            }),
            Assumption::References {
                cols, to, to_cols, ..
            } if cols.len() == 1 => {
                let target = match project.get(to) {
                    Some(r) => r.ref_call(),
                    None => format!("ref('{}')", to.0),
                };
                tests.push(TestYaml {
                    column: Some(cols[0].clone()),
                    lines: vec![
                        "- relationships:".into(),
                        format!("    to: {target}"),
                        format!("    field: {}", to_cols[0]),
                    ],
                });
            }
            Assumption::References { .. } => {
                skipped.push(a.describe(&display));
            }
        }
    }
    let mut out = String::new();
    for f in &fix_existing {
        out.push_str(f);
        out.push('\n');
    }
    if by_rel.values().all(|v| v.is_empty()) {
        if fix_existing.is_empty() {
            out.push_str("# Every assumption is already checked.\n");
        }
        return out;
    }
    out.push_str("# Add these tests to the YAML file that declares each relation.\n");
    // Sources grouped by source name; models, seeds and snapshots by kind.
    let mut sources: BTreeMap<&str, Vec<(&Relation, &[TestYaml])>> = BTreeMap::new();
    let mut others: BTreeMap<&str, Vec<(&Relation, &[TestYaml])>> = BTreeMap::new();
    for (id, tests) in &by_rel {
        let Some(rel) = project.get(id) else { continue };
        if tests.is_empty() {
            continue;
        }
        match (&rel.kind, rel.display.split_once('.')) {
            (Kind::Source, Some((src, _))) => sources.entry(src).or_default().push((rel, tests)),
            (Kind::Seed, _) => others.entry("seeds").or_default().push((rel, tests)),
            (Kind::Snapshot, _) => others.entry("snapshots").or_default().push((rel, tests)),
            _ => others.entry("models").or_default().push((rel, tests)),
        }
    }
    if !sources.is_empty() {
        out.push_str("sources:\n");
        for (src, rels) in sources {
            out.push_str(&format!("  - name: {src}\n    tables:\n"));
            for (rel, tests) in rels {
                let table = rel.display.split_once('.').map(|x| x.1).unwrap_or("");
                out.push_str(&format!("      - name: {table}  # {}\n", rel.file));
                out.push_str(&yaml_tests(tests, "      ", key));
            }
        }
    }
    for (kind, rels) in others {
        out.push_str(&format!("{kind}:\n"));
        for (rel, tests) in rels {
            out.push_str(&format!("  - name: {}  # {}\n", rel.display, rel.file));
            out.push_str(&yaml_tests(tests, "  ", key));
        }
    }
    if tests_need_dbt_utils(&by_rel) {
        out.push_str("# unique_combination_of_columns needs the dbt_utils package.\n");
    }
    for s in skipped {
        out.push_str(&format!(
            "# No built-in test checks a multi-column reference: {s}\n"
        ));
    }
    out
}

fn tests_need_dbt_utils(by_rel: &BTreeMap<RelationId, Vec<TestYaml>>) -> bool {
    by_rel
        .values()
        .flatten()
        .any(|t| t.lines[0].contains("dbt_utils"))
}

/// The `tests` and `columns` entries of one relation, under an item at `indent`.
fn yaml_tests(tests: &[TestYaml], indent: &str, key: &str) -> String {
    let mut s = String::new();
    let body = |lines: &[String], pad: &str, s: &mut String| {
        for l in lines {
            s.push_str(&format!("{pad}{l}\n"));
        }
    };
    let rel_tests: Vec<&TestYaml> = tests.iter().filter(|t| t.column.is_none()).collect();
    if !rel_tests.is_empty() {
        s.push_str(&format!("{indent}  {key}:\n"));
        for t in rel_tests {
            body(&t.lines, &format!("{indent}    "), &mut s);
        }
    }
    let mut cols: BTreeMap<&str, Vec<&TestYaml>> = BTreeMap::new();
    for t in tests {
        if let Some(c) = &t.column {
            cols.entry(c).or_default().push(t);
        }
    }
    if !cols.is_empty() {
        s.push_str(&format!("{indent}  columns:\n"));
        for (c, ts) in cols {
            s.push_str(&format!("{indent}    - name: {c}\n"));
            s.push_str(&format!("{indent}      {key}:\n"));
            for t in ts {
                body(&t.lines, &format!("{indent}        "), &mut s);
            }
        }
    }
    s
}

fn version_at_least(v: &str, major: u32, minor: u32) -> bool {
    let mut parts = v
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty());
    let ma: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let mi: u32 = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (ma, mi) >= (major, minor)
}
