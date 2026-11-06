//! `cardinal guess`: write dbt tests for likely keys and references, so dbt checks them
//! against the data and Cardinal can use the ones that hold.

use super::report::coverage;
use super::{EXIT_TOOL, eprint_error, load, plural};
use cardinal_dbt::guess::MARKER;
use cardinal_dbt::{Guess, Hypothesis, Kind, Project, RelationId};
use cardinal_engine::guess::{self, Keyless};
use cardinal_engine::{Analysis, Config, Options};
use cardinal_grain::UnknownReason;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Where guess tests go: cardinal.toml's `[guess] path`, else `<first test-path>/cardinal`.
pub fn guess_dir(project: &Project, config: &Config) -> String {
    config.guess.path.clone().unwrap_or_else(|| {
        format!(
            "{}/cardinal",
            project
                .test_paths
                .first()
                .map(|p| p.trim_end_matches('/'))
                .unwrap_or("tests")
        )
    })
}

pub fn run(dir: Option<PathBuf>) -> u8 {
    let (project, config) = match load(dir) {
        Ok(x) => x,
        Err(c) => return c,
    };
    match run_loaded(&project, &config) {
        Ok(out) => {
            print!("{out}");
            0
        }
        Err(e) => {
            eprint_error(&e);
            EXIT_TOOL
        }
    }
}

/// Write the guess tests and return the report.
pub fn run_loaded(project: &Project, config: &Config) -> Result<String, String> {
    let display = |id: &RelationId| {
        project
            .get(id)
            .map(|r| r.display.clone())
            .unwrap_or(id.0.clone())
    };
    let selected: BTreeSet<RelationId> = project.models().map(|r| r.id.clone()).collect();
    let a = Analysis::new(project, config, selected, true);
    let (run, selected) = (&a.run, &a.selected);
    let severities = config.severities(&[], &[])?;
    let findings = a.findings(&a.shapes(), &Options::all_findings(&severities));
    let keyless: Vec<Keyless<'_>> = findings
        .root_causes
        .iter()
        .filter(|r| matches!(r.reason, UnknownReason::SourceWithoutKey(_)))
        .filter_map(|r| {
            let rel = project.get(&r.root)?;
            matches!(rel.kind, Kind::Source | Kind::Seed).then(|| Keyless {
                relation: rel,
                hides: r.affected.clone(),
            })
        })
        .collect();
    let (guesses, no_candidate) = guess::propose(project, run, &keyless, &project.guesses);

    let mut out = String::new();
    out.push_str(&checked_report(project, &mut |path| {
        remove_generated(&project.root.join(path))
    })?);

    let dir = guess_dir(project, config);
    let abs = project.root.join(&dir);
    let mut written = 0;
    for g in &guesses {
        let path = abs.join(format!("{}.sql", g.test_name));
        if write_if_changed(&path, &project.guess_test_file(g))
            .map_err(|e| format!("could not write {}: {e}", path.display()))?
        {
            written += 1;
        }
    }

    if guesses.is_empty() {
        if keyless.is_empty() {
            out.push_str(
                "Nothing to guess: every source and seed your models read has a known key.\n",
            );
        } else {
            out.push_str("Nothing new to guess.\n");
        }
    } else {
        let keys: Vec<&Guess> = guesses
            .iter()
            .filter(|g| matches!(g.hypothesis, Hypothesis::Key { .. }))
            .collect();
        let tables: BTreeSet<&RelationId> = keys.iter().map(|g| g.hypothesis.relation()).collect();
        let hidden: BTreeSet<&RelationId> = keyless
            .iter()
            .filter(|k| tables.contains(&k.relation.id))
            .flat_map(|k| k.hides.iter())
            .collect();
        let refs = guesses.len() - keys.len();
        let mut what = Vec::new();
        if !keys.is_empty() {
            what.push(format!(
                "{} for {} that {} {} from being checked",
                if keys.len() == 1 { "a key" } else { "keys" },
                plural(tables.len(), "table", "tables"),
                if tables.len() == 1 { "keeps" } else { "keep" },
                plural(hidden.len(), "model", "models")
            ));
        }
        if refs > 0 {
            what.push(plural(refs, "reference", "references"));
        }
        out.push_str(&format!(
            "Cardinal guessed {}. It uses none of them until dbt confirms them on your data.\n",
            what.join(", and ")
        ));
        let w = guesses
            .iter()
            .map(|g| display(g.hypothesis.relation()).len())
            .max()
            .unwrap_or(0)
            + 2;
        for g in &guesses {
            let name = display(g.hypothesis.relation());
            let claim = g.hypothesis.claim(&display);
            out.push_str(&format!("\n  {name:<w$}{claim}\n"));
            if !g.why.is_empty() {
                out.push_str(&format!("  {:<w$}why: {}\n", "", g.why.join(", and ")));
            }
            out.push_str(&format!("  {:<w$}{}\n", "", g.unlocks));
        }
        out.push('\n');
        out.push_str(&format!(
            "Wrote {} to {dir}/{}.\n",
            plural(guesses.len(), "test", "tests"),
            if written < guesses.len() {
                format!(" ({} unchanged)", guesses.len() - written)
            } else {
                String::new()
            }
        ));
    }
    // Tables with no candidate left, apart from those whose guesses dbt failed.
    let (tried, untried): (Vec<&RelationId>, Vec<&RelationId>) =
        no_candidate.iter().partition(|id| {
            project.guesses.failed().any(
                |g| matches!(&g.hypothesis, Hypothesis::Key { relation, .. } if relation == *id),
            )
        });
    for (ids, what) in [(untried, "No likely key"), (tried, "No other likely key")] {
        if !ids.is_empty() {
            out.push_str(&format!(
                "{what} in {}: declare {} in YAML (see cardinal coverage).\n",
                ids.iter()
                    .map(|i| display(i))
                    .collect::<Vec<_>>()
                    .join(", "),
                if ids.len() == 1 { "it" } else { "them" }
            ));
        }
    }
    let pending = guesses
        .iter()
        .filter(|g| project.guesses.get(&g.hypothesis).is_none())
        .count();
    if pending > 0 {
        out.push_str("Next: dbt test --select tag:cardinal\n");
        out.push_str(
            "Then run cardinal check. Each guess dbt passes counts as a known key or reference,\n\
             labeled as a guess; each one dbt fails is dropped and never guessed again.\n",
        );
    } else if !guesses.is_empty() || !project.guesses.guesses.is_empty() {
        let cov = coverage(project, run, selected);
        out.push_str(&format!(
            "Next: cardinal check ({} of {} have a known key now).\n",
            cov.known,
            plural(cov.models, "model", "models")
        ));
    }
    Ok(out)
}

/// What dbt found for earlier guesses. Calls `remove` with the file of each failed guess
/// whose test is still in the project, and reports what it removed.
fn checked_report(
    project: &Project,
    remove: &mut dyn FnMut(&str) -> Result<bool, String>,
) -> Result<String, String> {
    let log = &project.guesses;
    if log.guesses.is_empty() {
        return Ok(String::new());
    }
    let display = |id: &RelationId| {
        project
            .get(id)
            .map(|r| r.display.clone())
            .unwrap_or(id.0.clone())
    };
    let mut out = format!(
        "dbt has checked {} of Cardinal's guesses:\n",
        log.guesses.len()
    );
    for g in &log.guesses {
        if g.passed {
            out.push_str(&format!(
                "  passed  {} (on {})\n",
                g.hypothesis.describe(&display),
                g.when()
            ));
        } else {
            out.push_str(&format!(
                "  failed  {}: dbt found {}\n",
                g.hypothesis.refuted(&display),
                g.hypothesis.failing_rows(g.failures)
            ));
            for t in project
                .guess_tests
                .iter()
                .filter(|t| t.hypothesis == g.hypothesis)
            {
                if remove(&t.file)? {
                    out.push_str(&format!("          removed {}\n", t.file));
                }
            }
        }
    }
    out.push('\n');
    Ok(out)
}

/// Remove a file `cardinal guess` wrote; leave anything else alone.
fn remove_generated(path: &Path) -> Result<bool, String> {
    match std::fs::read_to_string(path) {
        Ok(s) if s.starts_with(MARKER) => std::fs::remove_file(path)
            .map(|_| true)
            .map_err(|e| format!("could not remove {}: {e}", path.display())),
        _ => Ok(false),
    }
}

fn write_if_changed(path: &Path, body: &str) -> std::io::Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|s| s == body) {
        return Ok(false);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, body)?;
    Ok(true)
}
