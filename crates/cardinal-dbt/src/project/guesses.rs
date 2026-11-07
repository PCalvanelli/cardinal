//! Guess tests and their recorded results: attaching them, merging results from
//! run_results.json, and declaring what passed.

use super::decl::add_not_null;
use super::{
    DataTest, DeclOrigin, DeclaredKey, DeclaredReference, NotNullOrigin, Project, Relation,
    TestKind,
};
use crate::artifacts::ManifestNode;
use crate::guess::{
    Guess, GuessLog, GuessResult, GuessTest, Hypothesis, results_from_run, rfc3339,
};
use crate::names::RelationId;
use serde_json::Value;
use std::collections::BTreeMap;

/// A Cardinal guess test: it checks its hypothesis like any data test of that fact, but
/// declares nothing; only a recorded pass does (see `Project::apply_guesses`).
pub(super) fn attach_guess_test(
    n: &ManifestNode,
    relations: &mut BTreeMap<RelationId, Relation>,
) -> Option<GuessTest> {
    let h = Hypothesis::from_meta(n.config.meta.as_ref())
        .or_else(|| Hypothesis::from_meta(n.meta.as_ref()))?;
    let warn_only = n
        .config
        .severity
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("warn"));
    let test = |kind| DataTest {
        name: n.name.clone(),
        kind,
        filtered: false,
        warn_only,
    };
    if let Some(rel) = relations.get_mut(h.relation()) {
        match &h {
            Hypothesis::Key { columns, .. } => {
                rel.tests.push(test(TestKind::Unique(columns.clone())));
                for c in columns {
                    rel.tests.push(test(TestKind::NotNull(c.clone())));
                }
            }
            Hypothesis::References {
                columns,
                to,
                to_columns,
                ..
            } if columns.len() == 1 => rel.tests.push(test(TestKind::Relationships {
                column: columns[0].clone(),
                to: to.clone(),
                field: to_columns[0].clone(),
            })),
            Hypothesis::References { .. } => {}
        }
    }
    Some(GuessTest {
        unique_id: n.unique_id.clone(),
        name: n.name.clone(),
        file: n.original_file_path.clone(),
        hypothesis: h,
    })
}

impl Project {
    /// Merge guess results from target/run_results.json into `self.guesses`, and
    /// whether that added anything `.cardinal/guesses.json` doesn't have yet.
    pub(super) fn merge_guess_results(&mut self) -> bool {
        let path = self.target_dir.join("run_results.json");
        if self.guess_tests.is_empty() || !path.is_file() {
            return false;
        }
        let Ok(v) = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Value>(&b).map_err(|e| e.to_string()))
        else {
            return false;
        };
        let file_time = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(rfc3339);
        let results = results_from_run(&v, &self.guess_tests, file_time.as_deref());
        !results.is_empty() && self.guesses.merge(results)
    }

    /// Write the guess results loading found in target/run_results.json to
    /// `.cardinal/guesses.json`, since the next `dbt compile` overwrites run_results.json.
    /// Does nothing when there are none.
    pub fn save_guesses(&mut self) -> Result<(), String> {
        if !self.guesses_unsaved {
            return Ok(());
        }
        self.guesses
            .save(&self.root)
            .map_err(|e| format!("{}: {e}", GuessLog::path(&self.root).display()))?;
        self.guesses_unsaved = false;
        Ok(())
    }

    /// Use every guess dbt passed as a declaration with a guess origin. A guess about a
    /// relation or column that no longer exists is skipped.
    pub(super) fn apply_guesses(&mut self) {
        let passed: Vec<GuessResult> = self.guesses.passed().cloned().collect();
        for g in passed {
            let note = g.passed_note();
            let has_cols = |p: &Project, id: &RelationId, cols: &[String]| {
                p.get(id).is_some_and(|r| {
                    r.columns.is_empty()
                        || cols.iter().all(|c| r.columns.iter().any(|x| &x.name == c))
                })
            };
            match &g.hypothesis {
                Hypothesis::Key { relation, columns } => {
                    if !has_cols(self, relation, columns) {
                        continue;
                    }
                    let rel = self.relations.get_mut(relation).unwrap();
                    let mut want = columns.clone();
                    want.sort();
                    let known = rel.declared_keys.iter().any(|k| {
                        let mut c = k.cols.clone();
                        c.sort();
                        c == want
                    });
                    if !known {
                        rel.declared_keys.push(DeclaredKey {
                            cols: columns.clone(),
                            origin: DeclOrigin::Guess(note.clone()),
                        });
                    }
                    for c in columns {
                        add_not_null(
                            &mut rel.not_null,
                            c.clone(),
                            NotNullOrigin::Guess(note.clone()),
                        );
                    }
                }
                Hypothesis::References {
                    relation,
                    columns,
                    to,
                    to_columns,
                } => {
                    if !has_cols(self, relation, columns) || !has_cols(self, to, to_columns) {
                        continue;
                    }
                    let rel = self.relations.get_mut(relation).unwrap();
                    let known = rel
                        .references
                        .iter()
                        .any(|r| &r.cols == columns && &r.to == to && &r.to_cols == to_columns);
                    if !known {
                        rel.references.push(DeclaredReference {
                            cols: columns.clone(),
                            to: to.clone(),
                            to_cols: to_columns.clone(),
                            guess: Some(note),
                        });
                    }
                }
            }
        }
    }
}

impl Project {
    /// The dbt singular test that checks `g` in this project.
    pub fn guess_test_file(&self, g: &Guess) -> String {
        let relation_sql = |id: &RelationId| match self.get(id) {
            Some(r) => format!("{{{{ {} }}}}", r.ref_call()),
            None => id.0.clone(),
        };
        g.test_sql(&|id| self.display(id), &relation_sql)
    }
}
