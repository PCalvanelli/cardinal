//! `--state`: compare with the base branch's dbt artifacts, the directory slim CI
//! already downloads for dbt's `state:modified`. Every guarantee the base proves becomes
//! a check on the head (P003), and findings the base already has are not new.

use crate::config::{Config, Severities};
use crate::engine::{Engine, ProjectLocator, RunResult};
use crate::pipeline::{self, Options};
use crate::selection::closure;
use cardinal_check::{Lost, SHAPES_FILE, Shapes, Side, compare_guarantees, lost_guarantees};
use cardinal_dbt::{Kind, Project, RelationId, load_state};
use cardinal_diag::baseline::Baseline;
use cardinal_diag::{Diagnostic, Severity};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct Comparison {
    /// The state directory, as given.
    pub dir: String,
    /// P003 findings, before suppressions and severities.
    pub findings: Vec<Diagnostic>,
    /// Every finding on the base, to tell which of the head's are not new.
    pub on_main: Baseline,
    pub lost: usize,
    pub gained: usize,
    pub kept: usize,
    /// Models on the base without compiled SQL, which are not compared.
    pub skipped: usize,
}

/// Where a relative state directory is looked for, in order: the working directory,
/// as dbt resolves it, then the project root.
fn candidates(dir: &Path, root: &Path) -> Vec<PathBuf> {
    if dir.is_absolute() {
        return vec![dir.to_path_buf()];
    }
    let mut out = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        out.push(cwd.join(dir));
    }
    out.push(root.join(dir));
    out
}

fn has_manifest(dir: &Path) -> bool {
    dir.join("manifest.json").is_file()
}

/// The state directory to compare with: `--state`, or else `DBT_STATE`. A relative path
/// is resolved against the working directory first, then the project root.
///
/// `--state` is explicit, so if it has no manifest.json the path is still returned and
/// loading it reports the error. `DBT_STATE` is ambient (dbt reads it too), so it is
/// used only when it names a directory with a manifest.json, and otherwise ignored.
pub fn state_dir(explicit: Option<&Path>, root: &Path) -> Option<PathBuf> {
    if let Some(d) = explicit {
        let c = candidates(d, root);
        return Some(
            c.iter()
                .find(|p| has_manifest(p))
                .or(c.iter().find(|p| p.is_dir()))
                .unwrap_or(c.last().unwrap())
                .clone(),
        );
    }
    let env = std::env::var_os("DBT_STATE").filter(|v| !v.is_empty())?;
    state_from_env(Path::new(&env), root)
}

fn state_from_env(dir: &Path, root: &Path) -> Option<PathBuf> {
    let found = candidates(dir, root).into_iter().find(|p| has_manifest(p));
    if found.is_none() {
        tracing::debug!(
            "ignoring DBT_STATE={}: no manifest.json there",
            dir.display()
        );
    }
    found
}

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

/// Analyze the base artifacts in `dir` and compare the selected models with their base
/// versions.
pub fn compare(
    head: &Project,
    head_run: &RunResult,
    selected: &BTreeSet<RelationId>,
    config: &Config,
    severities: &Severities,
    dir: &Path,
    use_cache: bool,
) -> Result<Comparison, String> {
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
    let (base_shapes, on_main) = match saved_shapes(&head.root.join(dir), &base, &base_selected) {
        Ok(s) => {
            tracing::debug!(
                "--state: using {}",
                head.root.join(dir).join(SHAPES_FILE).display()
            );
            let on_main = s.findings.clone();
            (s, on_main)
        }
        Err(why) => {
            tracing::debug!("--state: analyzing main's artifacts: {why}");
            let base_run = Engine::for_state(&base, config, use_cache, head_run)
                .run(&closure(&base, &base_selected));
            let shapes = base_run.shapes(&base);
            let base_findings = pipeline::collect(
                &base,
                &base_run,
                &shapes,
                &base_selected,
                &Options {
                    severities,
                    baseline: None,
                    state: None,
                    min_severity: Severity::Info,
                    layers: &BTreeSet::new(),
                    no_sql: false,
                },
            );
            (shapes, Baseline::from_findings(&base_findings.all))
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cardinal-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn env_state_without_manifest_is_ignored() {
        let root = tmp("env");
        assert_eq!(state_from_env(&root.join("missing"), &root), None);
        std::fs::create_dir_all(root.join("empty")).unwrap();
        assert_eq!(state_from_env(Path::new("empty"), &root), None);
        std::fs::create_dir_all(root.join("base")).unwrap();
        std::fs::write(root.join("base/manifest.json"), "{}").unwrap();
        assert_eq!(
            state_from_env(Path::new("base"), &root),
            Some(root.join("base")),
            "relative to the project root when the working directory has none"
        );
        assert_eq!(
            state_from_env(&root.join("base"), Path::new("/nowhere")),
            Some(root.join("base"))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_state_is_kept_even_when_missing() {
        let root = tmp("explicit");
        assert_eq!(
            state_dir(Some(Path::new("not-there")), &root),
            Some(root.join("not-there")),
            "loading it then reports the missing manifest"
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
