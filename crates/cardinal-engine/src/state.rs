//! `--state`: compare with the base branch's dbt artifacts, the directory slim CI
//! already downloads for dbt's `state:modified`. Every guarantee the base proves becomes
//! a check on the head (P003), and findings the base already has are not new.

use crate::analysis::Analysis;
use crate::comparison::{BaseShapes, Comparison};
use crate::config::{Config, Severities};
use crate::engine::ProjectLocator;
use crate::pipeline::Options;
use cardinal_check::{Lost, SHAPES_FILE, Shapes, Side, compare_guarantees, lost_guarantees};
use cardinal_dbt::{Kind, Project, RelationId, load_state};
use cardinal_diag::Baseline;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// The shapes file in the state directory, when it was written from exactly these
/// artifacts by this version of Cardinal and covers every model to compare.
fn saved_shapes(
    dir: &Path,
    base: &Project,
    selected: &BTreeSet<RelationId>,
) -> Result<Shapes, String> {
    let path = dir.join(SHAPES_FILE);
    if !path.is_file() {
        return Err(format!("no {SHAPES_FILE}"));
    }
    let shapes = Shapes::load(&path)?;
    if base.invocation_id.is_none() || shapes.manifest != base.invocation_id {
        return Err(format!("{SHAPES_FILE} is from a different manifest.json"));
    }
    if let Some(id) = selected
        .iter()
        .find(|id| !shapes.reported_for.contains(*id) || shapes.get(id).is_none())
    {
        return Err(format!("{SHAPES_FILE} does not cover {id}"));
    }
    Ok(shapes)
}

impl Analysis<'_> {
    /// Compare the selected models with their versions in the base branch's artifacts in
    /// `dir`, analyzing those artifacts unless the shapes file the base's own check wrote
    /// fits them.
    pub fn compare(
        &self,
        config: &Config,
        severities: &Severities,
        dir: &Path,
        use_cache: bool,
    ) -> Result<Comparison, String> {
        let (head_run, selected) = (&self.run, &self.selected);
        let head = self.project;
        let base = load_state(&head.root, &head.root.join(dir)).map_err(|e| e.to_string())?;
        // Shown relative to the project root or the working directory, when under one.
        let cwd = std::env::current_dir().ok();
        let shown = dir
            .strip_prefix(&head.root)
            .ok()
            .or_else(|| cwd.as_deref().and_then(|c| dir.strip_prefix(c).ok()))
            .unwrap_or(dir);
        let base_selected: BTreeSet<RelationId> = selected
            .iter()
            .filter(|id| base.get(id).is_some_and(|r| r.is_model()))
            .cloned()
            .collect();
        // Main's shapes, from the file its own `cardinal check` wrote when it fits these
        // artifacts, or else by analyzing them.
        let (base_shapes, on_main, source) =
            match saved_shapes(&head.root.join(dir), &base, &base_selected) {
                Ok(s) => {
                    let on_main = s.findings.clone();
                    (
                        s,
                        on_main,
                        BaseShapes::Saved(head.root.join(dir).join(SHAPES_FILE)),
                    )
                }
                Err(why) => {
                    let a = Analysis::for_state(
                        &base,
                        config,
                        base_selected.clone(),
                        use_cache,
                        head_run,
                    );
                    let shapes = a.shapes();
                    let base_findings = a.findings(&shapes, &Options::all_findings(severities));
                    (
                        shapes,
                        Baseline::from_findings(&base_findings.all),
                        BaseShapes::Analyzed(why),
                    )
                }
            };

        let mut lost: BTreeMap<RelationId, Vec<Lost>> = BTreeMap::new();
        let (mut gained, mut kept, mut skipped) = (0, 0, 0);
        for id in &base_selected {
            let sql_model = base
                .get(id)
                .is_some_and(|r| matches!(r.kind, Kind::Model { .. } | Kind::Ephemeral));
            if sql_model && base.get(id).is_some_and(|r| r.compiled_sql.is_none()) {
                skipped += 1;
                continue;
            }
            let (Some(b), Some(h)) = (base_shapes.get(id), head_run.results.get(id)) else {
                continue;
            };
            // A model that does not plan has an E-code instead.
            if !b.planned || !h.planned {
                continue;
            }
            let change = compare_guarantees(
                Side {
                    summary: &b.summary,
                    verdicts: &b.verdicts,
                },
                Side {
                    summary: &h.summary,
                    verdicts: &h.verdicts,
                },
            );
            gained += change.gained;
            kept += change.kept;
            if !change.lost.is_empty() {
                lost.insert(id.clone(), change.lost);
            }
        }
        let findings = lost_guarantees(&base, head, &lost, &ProjectLocator { project: head });
        Ok(Comparison {
            dir: shown.display().to_string(),
            findings,
            on_main,
            lost: lost.values().map(Vec::len).sum(),
            gained,
            kept,
            skipped,
            base: source,
        })
    }
}
