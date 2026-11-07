//! Loading a project from dbt's artifacts: staleness, the manifest and catalog, and
//! building the relations.

use super::data_tests::attach_test;
use super::decl::{
    RawReference, column_measures, constraint_facts, exactly_once_names, grain_facts,
    incremental_config, meta_grain, raw_references, string_list, yaml_columns,
};
use super::graph::levels;
use super::guesses::attach_guess_test;
use super::resolve::{resolve_name, resolve_references, split_relation_name};
use super::root::{ProjectYml, read_project_yml};
use super::{
    Column, ColumnsFrom, DeclOrigin, DeclaredKey, ExactlyOnce, Incremental, Kind, LoadError,
    LoadWarning, Project, Relation, supported_adapters,
};
use crate::artifacts::{Catalog, KNOWN_MANIFEST_VERSIONS, Manifest};
use crate::guess::GuessLog;
use crate::names::RelationId;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn newest_file(dir: &Path) -> Option<(SystemTime, PathBuf)> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(md) = e.metadata() else { continue };
            if md.is_dir() {
                stack.push(p);
            } else if let Ok(m) = md.modified()
                && best.as_ref().is_none_or(|(t, _)| m > *t)
            {
                best = Some((m, p));
            }
        }
    }
    best
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, LoadError> {
    let bytes = std::fs::read(path).map_err(|e| LoadError::Unreadable {
        path: path.into(),
        reason: e.to_string(),
    })?;
    serde_json::from_slice(&bytes).map_err(|e| LoadError::Unreadable {
        path: path.into(),
        reason: e.to_string(),
    })
}

/// Load target/manifest.json and target/catalog.json for the project at `root`.
pub fn load_project(root: &Path) -> Result<Project, LoadError> {
    let yml = read_project_yml(root);
    let target_dir = root.join(&yml.target_path);
    let manifest_path = target_dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(LoadError::MissingArtifact {
            path: manifest_path,
            command: "dbt compile".into(),
        });
    }
    let manifest_mtime = std::fs::metadata(&manifest_path)
        .and_then(|m| m.modified())
        .ok();
    // Checked-in fixtures and tests set CARDINAL_ALLOW_STALE: git does not keep mtimes.
    let check_stale = std::env::var_os("CARDINAL_ALLOW_STALE").is_none();
    if let Some(mt) = manifest_mtime.filter(|_| check_stale) {
        for mp in &yml.model_paths {
            if let Some((t, p)) = newest_file(&root.join(mp))
                && t > mt
            {
                return Err(LoadError::Stale {
                    manifest: manifest_path,
                    newer: p.strip_prefix(root).unwrap_or(&p).to_path_buf(),
                    command: "dbt compile".into(),
                });
            }
        }
    }
    let mut warnings = Vec::new();
    let catalog_path = target_dir.join("catalog.json");
    if !catalog_path.is_file() {
        warnings.push(LoadWarning::MissingCatalog {
            command: "dbt docs generate".into(),
        });
    }
    let mut project = load_artifacts(root, &yml, &target_dir, &[catalog_path], warnings)?;
    project.manifest_mtime = manifest_mtime;
    project.guesses_unsaved = project.merge_guess_results();
    project.apply_guesses();
    Ok(project)
}

/// Load another build of the same project from `dir`, such as the base branch's
/// artifacts that slim CI downloads for `--state`: `dir/manifest.json`, with
/// `dir/catalog.json` when it exists and the project's own catalog otherwise, since
/// source columns rarely change. Never checks whether the artifacts are stale.
pub fn load_state(root: &Path, dir: &Path) -> Result<Project, LoadError> {
    let yml = read_project_yml(root);
    let manifest_path = dir.join("manifest.json");
    if !manifest_path.is_file() {
        return Err(LoadError::MissingArtifact {
            path: manifest_path,
            command: "download the base branch's target/manifest.json into this directory".into(),
        });
    }
    let catalogs = [
        dir.join("catalog.json"),
        root.join(&yml.target_path).join("catalog.json"),
    ];
    let mut project = load_artifacts(root, &yml, dir, &catalogs, vec![])?;
    project.apply_guesses();
    Ok(project)
}

/// Read `target_dir/manifest.json` and the first catalog in `catalogs` that exists.
fn load_artifacts(
    root: &Path,
    yml: &ProjectYml,
    target_dir: &Path,
    catalogs: &[PathBuf],
    mut warnings: Vec<LoadWarning>,
) -> Result<Project, LoadError> {
    let manifest: Manifest = read_json(&target_dir.join("manifest.json"))?;
    let catalog: Catalog = match catalogs.iter().find(|p| p.is_file()) {
        Some(p) => read_json(p)?,
        None => Catalog::default(),
    };
    if let Some(v) = &manifest.metadata.dbt_schema_version {
        let ver = v
            .rsplit('/')
            .find(|s| s.starts_with('v'))
            .unwrap_or("")
            .trim_end_matches(".json");
        if !KNOWN_MANIFEST_VERSIONS.contains(&ver) {
            warnings.push(LoadWarning::UnknownSchemaVersion(ver.to_string()));
        }
    }
    let adapter = manifest
        .metadata
        .adapter_type
        .clone()
        .unwrap_or_else(|| "generic".into());
    let mut project = build(
        root, target_dir, yml, &manifest, &catalog, adapter, warnings,
    );
    project.dbt_version = manifest.metadata.dbt_version.clone();
    match GuessLog::load(root) {
        Ok(g) => project.guesses = g,
        Err(e) => project.warnings.push(LoadWarning::Guesses(format!(
            "ignoring Cardinal's recorded guesses: {e}"
        ))),
    }
    Ok(project)
}

fn catalog_columns(catalog: &Catalog, id: &str, source: bool) -> Option<Vec<Column>> {
    let table = if source {
        catalog.sources.get(id)
    } else {
        catalog.nodes.get(id)
    }?;
    let mut cols: Vec<_> = table.columns.values().collect();
    cols.sort_by_key(|c| c.index);
    Some(
        cols.into_iter()
            .map(|c| Column {
                name: c.name.to_lowercase(),
                data_type: Some(c.data_type.clone()),
            })
            .collect(),
    )
}

pub(super) fn build(
    root: &Path,
    target_dir: &Path,
    yml: &ProjectYml,
    manifest: &Manifest,
    catalog: &Catalog,
    adapter: String,
    mut warnings: Vec<LoadWarning>,
) -> Project {
    let mut relations: BTreeMap<RelationId, Relation> = BTreeMap::new();
    let mut raw_refs: BTreeMap<RelationId, Vec<RawReference>> = BTreeMap::new();
    let mut raw_exactly_once: BTreeMap<RelationId, Vec<String>> = BTreeMap::new();
    let target_rel = target_dir
        .strip_prefix(root)
        .unwrap_or(target_dir)
        .to_path_buf();

    for (id, s) in &manifest.sources {
        let (columns, from) = match catalog_columns(catalog, id, true) {
            Some(c) if !c.is_empty() => (c, ColumnsFrom::Catalog),
            _ if !s.columns.is_empty() => (yaml_columns(&s.columns), ColumnsFrom::Yaml),
            _ => (vec![], ColumnsFrom::Missing),
        };
        let mut declared_keys = Vec::new();
        let mut not_null = BTreeMap::new();
        grain_facts(
            meta_grain(s.meta.as_ref()).or_else(|| meta_grain(s.config.meta.as_ref())),
            &mut declared_keys,
            &mut not_null,
        );
        constraint_facts(&[], &s.columns, &mut declared_keys, &mut not_null);
        let mut decl_problems = Vec::new();
        let refs = raw_references(
            s.meta.as_ref(),
            s.config.meta.as_ref(),
            &s.columns,
            &mut decl_problems,
        );
        let display = format!("{}.{}", s.source_name, s.name);
        let (measures, units) = column_measures(&s.columns, &columns, &mut decl_problems);
        if !refs.is_empty() {
            raw_refs.insert(RelationId(id.clone()), refs);
        }
        relations.insert(
            RelationId(id.clone()),
            Relation {
                id: RelationId(id.clone()),
                kind: Kind::Source,
                display,
                package: s.package_name.clone(),
                file: s.original_file_path.clone(),
                compiled_sql: None,
                raw_sql: None,
                compiled_path: None,
                relation_parts: s
                    .relation_name
                    .as_deref()
                    .map(split_relation_name)
                    .unwrap_or_default(),
                depends_on: vec![],
                columns,
                columns_from: from,
                declared_keys,
                not_null,
                references: vec![],
                exactly_once: vec![],
                measures,
                units,
                tests: vec![],
                contract_enforced: false,
                tags: vec![],
                incremental: None,
                snapshot: None,
                decl_problems,
                yaml_file: Some(s.original_file_path.clone()),
            },
        );
    }

    for (id, n) in &manifest.nodes {
        let kind = match n.resource_type.as_str() {
            "model" if n.language.as_deref() == Some("python") => Kind::PythonModel,
            "model" if n.config.materialized.as_deref() == Some("ephemeral") => Kind::Ephemeral,
            "model" => Kind::Model {
                materialized: n
                    .config
                    .materialized
                    .clone()
                    .unwrap_or_else(|| "view".into()),
            },
            "seed" => Kind::Seed,
            "snapshot" => Kind::Snapshot,
            _ => continue,
        };
        if n.config.enabled == Some(false) {
            continue;
        }
        let (columns, from) = match catalog_columns(catalog, id, false) {
            Some(c) if !c.is_empty() => (c, ColumnsFrom::Catalog),
            _ if !n.columns.is_empty() => (yaml_columns(&n.columns), ColumnsFrom::Yaml),
            _ => (vec![], ColumnsFrom::Missing),
        };
        let mut declared_keys = Vec::new();
        let mut not_null = BTreeMap::new();
        grain_facts(
            meta_grain(n.meta.as_ref()).or_else(|| meta_grain(n.config.meta.as_ref())),
            &mut declared_keys,
            &mut not_null,
        );
        let mut decl_problems = Vec::new();
        let refs = raw_references(
            n.meta.as_ref(),
            n.config.meta.as_ref(),
            &n.columns,
            &mut decl_problems,
        );
        let (measures, units) = column_measures(&n.columns, &columns, &mut decl_problems);
        if !refs.is_empty() {
            raw_refs.insert(RelationId(id.clone()), refs);
        }
        let exactly_once = exactly_once_names(n.meta.as_ref(), n.config.meta.as_ref());
        if !exactly_once.is_empty() {
            raw_exactly_once.insert(RelationId(id.clone()), exactly_once);
        }
        let contract_enforced = n
            .config
            .contract
            .as_ref()
            .and_then(|c| c.get("enforced"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        constraint_facts(
            &n.constraints,
            &n.columns,
            &mut declared_keys,
            &mut not_null,
        );
        if kind == Kind::Snapshot
            && let Some(uk) = &n.config.unique_key
        {
            let mut cols = string_list(uk);
            if !cols.is_empty() {
                cols.push("dbt_valid_from".into());
                declared_keys.push(DeclaredKey {
                    cols,
                    origin: DeclOrigin::SnapshotKey,
                });
            }
        }
        let incremental =
            matches!(&kind, Kind::Model { materialized } if materialized == "incremental")
                .then(|| incremental_config(&n.config));
        let snapshot = (kind == Kind::Snapshot).then(|| Incremental {
            strategy: n.config.strategy.clone(),
            ..incremental_config(&n.config)
        });
        let compiled_path = Some(
            target_rel
                .join("compiled")
                .join(&n.package_name)
                .join(&n.original_file_path)
                .to_string_lossy()
                .into_owned(),
        );
        let relation_parts = if kind == Kind::Ephemeral {
            vec![format!("__dbt__cte__{}", n.name.to_lowercase())]
        } else {
            n.relation_name
                .as_deref()
                .map(split_relation_name)
                .unwrap_or_default()
        };
        relations.insert(
            RelationId(id.clone()),
            Relation {
                id: RelationId(id.clone()),
                kind,
                display: n.name.clone(),
                package: n.package_name.clone(),
                file: n.original_file_path.clone(),
                compiled_sql: n.compiled_code.clone(),
                raw_sql: n.raw_code.clone(),
                compiled_path,
                relation_parts,
                depends_on: n
                    .depends_on
                    .nodes
                    .iter()
                    .map(|d| RelationId(d.clone()))
                    .collect(),
                columns,
                columns_from: from,
                declared_keys,
                not_null,
                references: vec![],
                exactly_once: vec![],
                measures,
                units,
                tests: vec![],
                contract_enforced,
                tags: n.tags.clone(),
                incremental,
                snapshot,
                decl_problems,
                yaml_file: n
                    .patch_path
                    .as_deref()
                    .map(|p| p.split_once("://").map_or(p, |(_, rest)| rest).to_string()),
            },
        );
    }

    // Tests become declared facts on the node they are attached to.
    let mut guess_tests = Vec::new();
    for n in manifest
        .nodes
        .values()
        .filter(|n| n.resource_type == "test")
    {
        if let Some(t) = attach_guess_test(n, &mut relations) {
            guess_tests.push(t);
        } else {
            attach_test(n, &mut relations);
        }
    }
    resolve_references(&mut relations, raw_refs);
    for (id, names) in raw_exactly_once {
        let targets: Vec<ExactlyOnce> = names
            .into_iter()
            .map(|name| ExactlyOnce {
                relation: resolve_name(&relations, &name),
                name,
            })
            .collect();
        if let Some(r) = relations.get_mut(&id) {
            r.exactly_once = targets;
        }
    }

    for r in relations.values_mut() {
        let known: Vec<RelationId> = r.depends_on.drain(..).collect();
        r.depends_on = known;
    }
    let ids: BTreeSet<RelationId> = relations.keys().cloned().collect();
    for r in relations.values_mut() {
        r.depends_on.retain(|d| ids.contains(d));
        r.depends_on.sort();
        r.depends_on.dedup();
    }

    let mut name_index: HashMap<String, Vec<RelationId>> = HashMap::new();
    for r in relations.values() {
        let p = &r.relation_parts;
        for k in 1..=p.len() {
            name_index
                .entry(p[p.len() - k..].join("."))
                .or_default()
                .push(r.id.clone());
        }
    }

    let (levels, children, cycle) = levels(&relations);
    if !cycle.is_empty() {
        warnings.push(LoadWarning::CycleIgnored(cycle));
    }
    let known = supported_adapters();
    if !known.contains(&adapter.to_ascii_lowercase().as_str()) {
        warnings.push(LoadWarning::UnknownAdapter(adapter.clone()));
    }

    Project {
        root: root.to_path_buf(),
        target_dir: target_dir.to_path_buf(),
        name: if yml.name.is_empty() {
            manifest.metadata.project_name.clone().unwrap_or_default()
        } else {
            yml.name.clone()
        },
        adapter,
        dbt_version: None,
        relations,
        levels,
        children,
        warnings,
        name_index,
        manifest_mtime: None,
        invocation_id: manifest.metadata.invocation_id.clone(),
        test_paths: yml.test_paths.clone(),
        guess_tests,
        guesses: GuessLog::default(),
        guesses_unsaved: false,
    }
}
