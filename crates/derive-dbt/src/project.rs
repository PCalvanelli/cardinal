use crate::artifacts::{
    Catalog, ColumnInfo, KNOWN_MANIFEST_VERSIONS, Manifest, ManifestNode, NodeConfig,
};
use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A dbt node id, e.g. "model.shop.order_lines".
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationId(pub String);

impl fmt::Display for RelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Model { materialized: String },
    Ephemeral,
    PythonModel,
    Seed,
    Snapshot,
    Source,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DeclOrigin {
    MetaGrain,
    ContractPrimaryKey,
    UniqueTest(String),
    UniqueCombination(String),
    SnapshotKey,
}

impl DeclOrigin {
    pub fn describe(&self) -> String {
        match self {
            DeclOrigin::MetaGrain => "meta.derive.grain".into(),
            DeclOrigin::ContractPrimaryKey => "a primary_key contract constraint".into(),
            DeclOrigin::UniqueTest(t) => format!("test {t}"),
            DeclOrigin::UniqueCombination(t) => format!("test {t}"),
            DeclOrigin::SnapshotKey => "the snapshot's unique_key and dbt_valid_from".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredKey {
    /// Lowercased column names.
    pub cols: Vec<String>,
    pub origin: DeclOrigin,
}

/// How an incremental model merges new rows into the existing table.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incremental {
    /// Lowercased `unique_key` columns. Empty when no unique_key is set, or when an
    /// entry is an expression (`a || '-' || b`) rather than a column name.
    pub unique_key: Vec<String>,
    /// The raw unique_key config, for messages.
    pub unique_key_raw: Option<String>,
    pub strategy: Option<String>,
}

impl Incremental {
    /// Whether the strategy matches existing rows on unique_key. append,
    /// insert_overwrite, microbatch and replace_where ignore it.
    pub fn merges_on_key(&self) -> bool {
        !matches!(
            self.strategy
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("append" | "insert_overwrite" | "microbatch" | "replace_where")
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForeignKey {
    pub column: String,
    pub to: RelationId,
    pub to_column: String,
    pub test: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// Lowercased.
    pub name: String,
    pub data_type: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnsFrom {
    Catalog,
    Yaml,
    Missing,
}

#[derive(Clone, Debug)]
pub struct Relation {
    pub id: RelationId,
    pub kind: Kind,
    /// Model name, or "source_name.table" for sources.
    pub display: String,
    pub package: String,
    /// Path of the file that defines it, relative to the project root.
    pub file: String,
    pub compiled_sql: Option<String>,
    pub raw_sql: Option<String>,
    /// Path of the compiled file, relative to the project root.
    pub compiled_path: Option<String>,
    /// Lowercased, unquoted parts of `relation_name` (database, schema, identifier).
    pub relation_parts: Vec<String>,
    pub depends_on: Vec<RelationId>,
    pub columns: Vec<Column>,
    pub columns_from: ColumnsFrom,
    pub declared_keys: Vec<DeclaredKey>,
    pub not_null: BTreeSet<String>,
    pub foreign_keys: Vec<ForeignKey>,
    pub tags: Vec<String>,
    /// Set for models materialized as incremental.
    pub incremental: Option<Incremental>,
}

impl Relation {
    pub fn is_model(&self) -> bool {
        matches!(
            self.kind,
            Kind::Model { .. } | Kind::Ephemeral | Kind::PythonModel
        )
    }
}

#[derive(Clone, Debug)]
pub enum LoadWarning {
    UnknownSchemaVersion(String),
    MissingCatalog { command: String },
    UnknownAdapter(String),
    CycleIgnored(Vec<RelationId>),
}

impl fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadWarning::UnknownSchemaVersion(v) => {
                write!(
                    f,
                    "manifest schema {v} is newer than this build knows; loading it anyway"
                )
            }
            LoadWarning::MissingCatalog { command } => write!(
                f,
                "target/catalog.json is missing, so column lists come from YAML only (run: {command})"
            ),
            LoadWarning::UnknownAdapter(a) => {
                write!(
                    f,
                    "adapter `{a}` has no matching dialect; using the generic SQL dialect"
                )
            }
            LoadWarning::CycleIgnored(ids) => write!(
                f,
                "dependency cycle through {}; those models are analyzed last with unknown inputs",
                ids.iter()
                    .map(|i| i.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

#[derive(Debug)]
pub enum LoadError {
    NoProject {
        from: PathBuf,
    },
    MissingArtifact {
        path: PathBuf,
        command: String,
    },
    Stale {
        manifest: PathBuf,
        newer: PathBuf,
        command: String,
    },
    Unreadable {
        path: PathBuf,
        reason: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::NoProject { from } => write!(
                f,
                "no dbt_project.yml found in {} or any parent directory",
                from.display()
            ),
            LoadError::MissingArtifact { path, command } => {
                write!(f, "{} not found\n  run: {command}", path.display())
            }
            LoadError::Stale {
                manifest,
                newer,
                command,
            } => write!(
                f,
                "{} is older than {}, so findings would describe old SQL\n  run: {command}",
                manifest.display(),
                newer.display()
            ),
            LoadError::Unreadable { path, reason } => {
                write!(f, "could not read {}: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for LoadError {}

pub struct Project {
    pub root: PathBuf,
    pub target_dir: PathBuf,
    pub name: String,
    pub adapter: String,
    pub dbt_version: Option<String>,
    pub relations: BTreeMap<RelationId, Relation>,
    /// Relations grouped by DAG level; every relation's inputs are in earlier levels.
    pub levels: Vec<Vec<RelationId>>,
    pub children: BTreeMap<RelationId, Vec<RelationId>>,
    pub warnings: Vec<LoadWarning>,
    /// Every 1-, 2- and 3-part suffix of each relation_name -> relations with it.
    name_index: HashMap<String, Vec<RelationId>>,
    pub manifest_mtime: Option<SystemTime>,
}

impl Project {
    /// Resolve a relation name as written in compiled SQL. Parts are lowercased.
    pub fn resolve(&self, parts: &[String]) -> Option<&RelationId> {
        let key = parts.join(".");
        match self.name_index.get(&key) {
            Some(ids) if ids.len() == 1 => ids.first(),
            _ => None,
        }
    }

    pub fn get(&self, id: &RelationId) -> Option<&Relation> {
        self.relations.get(id)
    }

    pub fn models(&self) -> impl Iterator<Item = &Relation> {
        self.relations.values().filter(|r| r.is_model())
    }

    /// All transitive downstream relations of `id`.
    pub fn descendants(&self, id: &RelationId) -> BTreeSet<RelationId> {
        let mut out = BTreeSet::new();
        let mut stack = vec![id.clone()];
        while let Some(n) = stack.pop() {
            for c in self.children.get(&n).into_iter().flatten() {
                if out.insert(c.clone()) {
                    stack.push(c.clone());
                }
            }
        }
        out
    }

    /// All transitive upstream relations of `id`.
    pub fn ancestors(&self, id: &RelationId) -> BTreeSet<RelationId> {
        let mut out = BTreeSet::new();
        let mut stack = vec![id.clone()];
        while let Some(n) = stack.pop() {
            for p in self
                .relations
                .get(&n)
                .map(|r| r.depends_on.as_slice())
                .unwrap_or(&[])
            {
                if out.insert(p.clone()) {
                    stack.push(p.clone());
                }
            }
        }
        out
    }

    /// Find a model by name, unique id, or file path.
    pub fn find_model(&self, query: &str) -> Option<&Relation> {
        let q = query.trim_end_matches(".sql");
        self.relations
            .get(&RelationId(query.to_string()))
            .or_else(|| {
                self.models().find(|r| {
                    r.display == q
                        || r.file.trim_end_matches(".sql") == q
                        || r.file.trim_end_matches(".sql").ends_with(&format!("/{q}"))
                })
            })
    }
}

/// Walk up from `start` to the directory holding dbt_project.yml.
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join("dbt_project.yml").is_file() {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Values from dbt_project.yml that Derive needs, read without a YAML parser.
struct ProjectYml {
    name: String,
    target_path: String,
    model_paths: Vec<String>,
}

fn read_project_yml(root: &Path) -> ProjectYml {
    let text = std::fs::read_to_string(root.join("dbt_project.yml")).unwrap_or_default();
    let mut yml = ProjectYml {
        name: String::new(),
        target_path: "target".into(),
        model_paths: vec!["models".into()],
    };
    let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    for line in text.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        if let Some(v) = line.strip_prefix("name:") {
            yml.name = unquote(v);
        } else if let Some(v) = line.strip_prefix("target-path:") {
            yml.target_path = unquote(v);
        } else if let Some(v) = line.strip_prefix("model-paths:") {
            let v = v.trim().trim_start_matches('[').trim_end_matches(']');
            let paths: Vec<String> = v
                .split(',')
                .map(unquote)
                .filter(|s| !s.is_empty())
                .collect();
            if !paths.is_empty() {
                yml.model_paths = paths;
            }
        }
    }
    yml
}

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

/// Split a relation_name like `"db"."main"."orders"` or `` `p.d.t` `` into lowercased parts.
pub fn split_relation_name(name: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in name.chars() {
        match (quote, c) {
            (None, '"' | '`' | '[') => quote = Some(if c == '[' { ']' } else { c }),
            (Some(q), c) if c == q => quote = None,
            (None, '.') => parts.push(std::mem::take(&mut cur)),
            (Some('`'), '.') => parts.push(std::mem::take(&mut cur)),
            (_, c) => cur.push(c),
        }
    }
    parts.push(cur);
    parts
        .into_iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect()
}

fn string_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.to_lowercase()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(|s| s.to_lowercase())
            .collect(),
        _ => vec![],
    }
}

fn incremental_config(config: &NodeConfig) -> Incremental {
    let raw: Vec<String> = match &config.unique_key {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::to_string)
            .collect(),
        _ => vec![],
    };
    let cols: Option<Vec<String>> = raw.iter().map(|s| plain_column(s)).collect();
    Incremental {
        unique_key: cols.unwrap_or_default(),
        unique_key_raw: (!raw.is_empty()).then(|| {
            if raw.len() == 1 {
                raw[0].clone()
            } else {
                format!("[{}]", raw.join(", "))
            }
        }),
        strategy: config.incremental_strategy.clone(),
    }
}

fn meta_grain(meta: Option<&Value>) -> Option<Vec<String>> {
    let g = meta?.get("derive")?.get("grain")?;
    let cols = string_list(g);
    (!cols.is_empty()).then_some(cols)
}

/// A plain column name (possibly quoted), or None for an expression like `lower(email)`.
fn plain_column(s: &str) -> Option<String> {
    let t = s
        .trim()
        .trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']');
    (!t.is_empty() && t.chars().all(|c| c.is_alphanumeric() || c == '_')).then(|| t.to_lowercase())
}

fn yaml_columns(cols: &BTreeMap<String, ColumnInfo>) -> Vec<Column> {
    cols.iter()
        .map(|(k, c)| Column {
            name: c.name.clone().unwrap_or_else(|| k.clone()).to_lowercase(),
            data_type: c.data_type.clone(),
        })
        .collect()
}

fn constraint_facts(
    model_constraints: &[crate::artifacts::Constraint],
    cols: &BTreeMap<String, ColumnInfo>,
    keys: &mut Vec<DeclaredKey>,
    not_null: &mut BTreeSet<String>,
) {
    for c in model_constraints {
        if c.kind.as_deref() == Some("primary_key")
            && let Some(cs) = &c.columns
        {
            keys.push(DeclaredKey {
                cols: cs.iter().map(|s| s.to_lowercase()).collect(),
                origin: DeclOrigin::ContractPrimaryKey,
            });
            not_null.extend(cs.iter().map(|s| s.to_lowercase()));
        }
    }
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        for c in &info.constraints {
            match c.kind.as_deref() {
                Some("primary_key") => {
                    keys.push(DeclaredKey {
                        cols: vec![name.clone()],
                        origin: DeclOrigin::ContractPrimaryKey,
                    });
                    not_null.insert(name.clone());
                }
                Some("not_null") => {
                    not_null.insert(name.clone());
                }
                _ => {}
            }
        }
    }
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
    // Checked-in fixtures and tests set DERIVE_ALLOW_STALE: git does not keep mtimes.
    let check_stale = std::env::var_os("DERIVE_ALLOW_STALE").is_none();
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
    let manifest: Manifest = read_json(&manifest_path)?;
    let mut warnings = Vec::new();
    let catalog_path = target_dir.join("catalog.json");
    let catalog: Catalog = if catalog_path.is_file() {
        read_json(&catalog_path)?
    } else {
        warnings.push(LoadWarning::MissingCatalog {
            command: "dbt docs generate".into(),
        });
        Catalog::default()
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
        root,
        &target_dir,
        &yml,
        &manifest,
        &catalog,
        adapter,
        warnings,
    );
    project.manifest_mtime = manifest_mtime;
    project.dbt_version = manifest.metadata.dbt_version.clone();
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

fn build(
    root: &Path,
    target_dir: &Path,
    yml: &ProjectYml,
    manifest: &Manifest,
    catalog: &Catalog,
    adapter: String,
    mut warnings: Vec<LoadWarning>,
) -> Project {
    let mut relations: BTreeMap<RelationId, Relation> = BTreeMap::new();
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
        if let Some(cols) =
            meta_grain(s.meta.as_ref()).or_else(|| meta_grain(s.config.meta.as_ref()))
        {
            declared_keys.push(DeclaredKey {
                cols,
                origin: DeclOrigin::MetaGrain,
            });
        }
        let mut not_null = BTreeSet::new();
        constraint_facts(&[], &s.columns, &mut declared_keys, &mut not_null);
        relations.insert(
            RelationId(id.clone()),
            Relation {
                id: RelationId(id.clone()),
                kind: Kind::Source,
                display: format!("{}.{}", s.source_name, s.name),
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
                foreign_keys: vec![],
                tags: vec![],
                incremental: None,
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
        if let Some(cols) =
            meta_grain(n.meta.as_ref()).or_else(|| meta_grain(n.config.meta.as_ref()))
        {
            declared_keys.push(DeclaredKey {
                cols,
                origin: DeclOrigin::MetaGrain,
            });
        }
        let mut not_null = BTreeSet::new();
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
                foreign_keys: vec![],
                tags: n.tags.clone(),
                incremental,
            },
        );
    }

    // Tests become declared facts on the node they are attached to.
    for n in manifest
        .nodes
        .values()
        .filter(|n| n.resource_type == "test")
    {
        attach_test(n, &mut relations);
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
    let known = derive_sql_adapters();
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
    }
}

fn derive_sql_adapters() -> &'static [&'static str] {
    &[
        "snowflake",
        "bigquery",
        "postgres",
        "redshift",
        "databricks",
        "duckdb",
        "spark",
    ]
}

fn attach_test(n: &ManifestNode, relations: &mut BTreeMap<RelationId, Relation>) {
    let Some(tm) = &n.test_metadata else { return };
    let name = tm.name.as_deref().unwrap_or("");
    let target = n.attached_node.clone().or_else(|| {
        n.depends_on
            .nodes
            .iter()
            .find(|d| !d.starts_with("macro."))
            .cloned()
    });
    let Some(target) = target.map(RelationId) else {
        return;
    };
    let kw = &tm.kwargs;
    let column = kw
        .get("column_name")
        .and_then(Value::as_str)
        .or(n.column_name.as_deref())
        .or_else(|| kw.get("arg").and_then(Value::as_str))
        .and_then(plain_column);
    let test_name = n.name.clone();
    let other = n
        .depends_on
        .nodes
        .iter()
        .find(|d| !d.starts_with("macro.") && **d != target.0)
        .cloned()
        .map(RelationId);
    let Some(rel) = relations.get_mut(&target) else {
        return;
    };
    match name {
        "unique" => {
            if let Some(c) = column {
                rel.declared_keys.push(DeclaredKey {
                    cols: vec![c],
                    origin: DeclOrigin::UniqueTest(test_name),
                });
            }
        }
        "unique_combination_of_columns" => {
            let cols = kw
                .get("combination_of_columns")
                .map(string_list)
                .unwrap_or_default();
            let cols: Vec<String> = cols.iter().filter_map(|c| plain_column(c)).collect();
            if !cols.is_empty() {
                rel.declared_keys.push(DeclaredKey {
                    cols,
                    origin: DeclOrigin::UniqueCombination(test_name),
                });
            }
        }
        "not_null" => {
            if let Some(c) = column {
                rel.not_null.insert(c);
            }
        }
        "relationships" => {
            let field = kw
                .get("field")
                .and_then(Value::as_str)
                .and_then(plain_column);
            if let (Some(c), Some(f), Some(to)) = (column, field, other) {
                rel.foreign_keys.push(ForeignKey {
                    column: c,
                    to,
                    to_column: f,
                    test: test_name,
                });
            }
        }
        _ => {}
    }
}

type Levels = (
    Vec<Vec<RelationId>>,
    BTreeMap<RelationId, Vec<RelationId>>,
    Vec<RelationId>,
);

fn levels(relations: &BTreeMap<RelationId, Relation>) -> Levels {
    let mut g: DiGraph<RelationId, ()> = DiGraph::new();
    let mut idx: HashMap<&RelationId, NodeIndex> = HashMap::new();
    for id in relations.keys() {
        idx.insert(id, g.add_node(id.clone()));
    }
    let mut children: BTreeMap<RelationId, Vec<RelationId>> = BTreeMap::new();
    for r in relations.values() {
        for d in &r.depends_on {
            g.add_edge(idx[d], idx[&r.id], ());
            children.entry(d.clone()).or_default().push(r.id.clone());
        }
    }
    let mut indeg: HashMap<NodeIndex, usize> = g
        .node_indices()
        .map(|n| (n, g.neighbors_directed(n, petgraph::Incoming).count()))
        .collect();
    let mut frontier: Vec<NodeIndex> = indeg
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| *n)
        .collect();
    let mut levels = Vec::new();
    let mut seen = 0;
    while !frontier.is_empty() {
        frontier.sort_by(|a, b| g[*a].cmp(&g[*b]));
        let mut next = Vec::new();
        for n in &frontier {
            for m in g.neighbors_directed(*n, petgraph::Outgoing) {
                let d = indeg.get_mut(&m).unwrap();
                *d -= 1;
                if *d == 0 {
                    next.push(m);
                }
            }
        }
        seen += frontier.len();
        levels.push(frontier.iter().map(|n| g[*n].clone()).collect());
        frontier = next;
    }
    let mut cycle = Vec::new();
    if seen < g.node_count() {
        cycle = indeg
            .iter()
            .filter(|(_, d)| **d > 0)
            .map(|(n, _)| g[*n].clone())
            .collect();
        cycle.sort();
        levels.push(cycle.clone());
    }
    (levels, children, cycle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_relation_names() {
        assert_eq!(
            split_relation_name("\"Dev\".\"main\".\"orders\""),
            vec!["dev", "main", "orders"]
        );
        assert_eq!(
            split_relation_name("`proj.ds.tbl`"),
            vec!["proj", "ds", "tbl"]
        );
        assert_eq!(
            split_relation_name("ANALYTICS.DBT.ORDERS"),
            vec!["analytics", "dbt", "orders"]
        );
    }
}
