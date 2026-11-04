use crate::artifacts::{
    Catalog, ColumnInfo, KNOWN_MANIFEST_VERSIONS, Manifest, ManifestNode, NodeConfig,
};
use crate::guess::{GuessLog, GuessNote, GuessTest, Hypothesis};
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
    /// A key `cardinal guess` proposed and a dbt test confirmed on the data.
    Guess(GuessNote),
}

impl DeclOrigin {
    pub fn describe(&self) -> String {
        match self {
            DeclOrigin::MetaGrain => "meta.cardinal.grain".into(),
            DeclOrigin::ContractPrimaryKey => "a primary_key contract constraint".into(),
            DeclOrigin::UniqueTest(t) => format!("test {t}"),
            DeclOrigin::UniqueCombination(t) => format!("test {t}"),
            DeclOrigin::SnapshotKey => "the snapshot's unique_key and dbt_valid_from".into(),
            DeclOrigin::Guess(n) => format!(
                "Cardinal's guess, which dbt test {} passed on {}",
                n.test, n.date
            ),
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

/// Why a column is declared non-null.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum NotNullOrigin {
    Test(String),
    Contract,
    ContractPrimaryKey,
    /// meta.cardinal.grain declares a primary key: unique and non-null.
    MetaGrain,
    /// A key guessed by `cardinal guess`, whose dbt test also checked for nulls.
    Guess(GuessNote),
}

impl NotNullOrigin {
    pub fn describe(&self) -> String {
        match self {
            NotNullOrigin::Test(t) => format!("test {t}"),
            NotNullOrigin::Contract => "a not_null contract constraint".into(),
            NotNullOrigin::ContractPrimaryKey => "a primary_key contract constraint".into(),
            NotNullOrigin::MetaGrain => "meta.cardinal.grain".into(),
            NotNullOrigin::Guess(n) => format!(
                "Cardinal's guess, which dbt test {} passed on {}",
                n.test, n.date
            ),
        }
    }
}

/// `meta.cardinal.references`: every non-null value of `cols` appears in `to.to_cols`.
/// A declaration, like meta.cardinal.grain. dbt `relationships` tests never declare a
/// reference; they only show that a declared one is checked at run time.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeclaredReference {
    /// Lowercased.
    pub cols: Vec<String>,
    pub to: RelationId,
    /// Lowercased, same length as `cols`.
    pub to_cols: Vec<String>,
    /// Set when the reference is a guess a dbt test confirmed, not a declaration.
    #[serde(default)]
    pub guess: Option<GuessNote>,
}

/// How `meta.cardinal.measure` says a column may be added up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeasureDecl {
    /// `kind: additive`: any sum is fine. Clears what Cardinal inferred.
    Additive,
    /// `kind: non_additive`: summing it over more than one row is wrong.
    NonAdditive,
    /// `kind: stock, over: [snapshot_date]`: summing rows that differ on `over` is wrong.
    Stock { over: Vec<String> },
}

/// `meta.cardinal.measure` on a column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredMeasure {
    /// Lowercased.
    pub column: String,
    pub kind: MeasureDecl,
}

/// `meta.cardinal.unit` on a column: the column `unit` holds its unit, such as the
/// currency of an amount.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredUnit {
    /// Lowercased.
    pub column: String,
    /// Lowercased.
    pub unit: String,
}

/// A dbt data test attached to a relation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataTest {
    pub name: String,
    pub kind: TestKind,
    /// Has a `where` config, so it checks only some rows and declares nothing.
    pub filtered: bool,
    /// severity: warn, so a failure does not fail the build.
    pub warn_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestKind {
    Unique(Vec<String>),
    NotNull(String),
    Relationships {
        column: String,
        to: RelationId,
        field: String,
    },
    Other,
}

/// One relation named by `meta.cardinal.exactly_once`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExactlyOnce {
    /// As written.
    pub name: String,
    /// None when no model, seed, snapshot or source has that name.
    pub relation: Option<RelationId>,
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
    /// Columns declared non-null, and every declaration of each.
    pub not_null: BTreeMap<String, Vec<NotNullOrigin>>,
    /// From meta.cardinal.references.
    pub references: Vec<DeclaredReference>,
    /// From meta.cardinal.exactly_once.
    pub exactly_once: Vec<ExactlyOnce>,
    /// From meta.cardinal.measure on columns.
    pub measures: Vec<DeclaredMeasure>,
    /// From meta.cardinal.unit on columns.
    pub units: Vec<DeclaredUnit>,
    pub tests: Vec<DataTest>,
    /// `contract: {enforced: true}`, so the warehouse applies its constraints.
    pub contract_enforced: bool,
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
    BadReference { relation: String, problem: String },
    BadMeasure { relation: String, problem: String },
    Guesses(String),
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
            LoadWarning::BadReference { relation, problem } => write!(
                f,
                "meta.cardinal.references on {relation} is ignored: {problem}"
            ),
            LoadWarning::BadMeasure { relation, problem } => {
                write!(f, "meta.cardinal on {relation} is ignored: {problem}")
            }
            LoadWarning::Guesses(m) => f.write_str(m),
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
    /// dbt's `test-paths`, relative to the root.
    pub test_paths: Vec<String>,
    /// Cardinal guess tests in the manifest.
    pub guess_tests: Vec<GuessTest>,
    /// `.cardinal/guesses.json`: what dbt found when it ran Cardinal's guesses.
    pub guesses: GuessLog,
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

/// Values from dbt_project.yml that Cardinal needs, read without a YAML parser.
struct ProjectYml {
    name: String,
    target_path: String,
    model_paths: Vec<String>,
    test_paths: Vec<String>,
}

fn read_project_yml(root: &Path) -> ProjectYml {
    let text = std::fs::read_to_string(root.join("dbt_project.yml")).unwrap_or_default();
    let mut yml = ProjectYml {
        name: String::new(),
        target_path: "target".into(),
        model_paths: vec!["models".into()],
        test_paths: vec!["tests".into()],
    };
    let unquote = |s: &str| s.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
    // The paths key whose block list (`- models` lines) follows, with the items so far.
    let mut block: Option<(&str, Vec<String>)> = None;
    for line in text.lines() {
        let line = strip_yaml_comment(line).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with([' ', '\t', '-']) {
            if let Some((_, items)) = &mut block
                && let Some(item) = line.trim_start().strip_prefix('-')
            {
                items.push(unquote(item));
            }
            continue;
        }
        if let Some((key, items)) = block.take() {
            yml.set_paths(key, items);
        }
        let Some((key, v)) = line.split_once(':') else {
            continue;
        };
        match key.trim_end() {
            "name" => yml.name = unquote(v),
            "target-path" => yml.target_path = unquote(v),
            key @ ("model-paths" | "test-paths") if v.trim().is_empty() => {
                block = Some((key, Vec::new()));
            }
            key @ ("model-paths" | "test-paths") => {
                let v = v.trim().trim_start_matches('[').trim_end_matches(']');
                yml.set_paths(key, v.split(',').map(unquote).collect());
            }
            _ => {}
        }
    }
    if let Some((key, items)) = block {
        yml.set_paths(key, items);
    }
    yml
}

impl ProjectYml {
    /// Set `model-paths` or `test-paths`, keeping the default when no path is given.
    fn set_paths(&mut self, key: &str, paths: Vec<String>) {
        let paths: Vec<String> = paths.into_iter().filter(|p| !p.is_empty()).collect();
        if paths.is_empty() {
            return;
        }
        if key == "model-paths" {
            self.model_paths = paths;
        } else {
            self.test_paths = paths;
        }
    }
}

/// `line` without its YAML comment: a `#` at the start or after whitespace, outside
/// quotes. dbt's older starter project has `target-path: "target"  # directory ...`.
fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut after_space = true;
    for (i, c) in line.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && after_space => return &line[..i],
            None => {}
        }
        after_space = c.is_whitespace();
    }
    line
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
    let g = meta?.get("cardinal")?.get("grain")?;
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
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
) {
    for c in model_constraints {
        if c.kind.as_deref() == Some("primary_key")
            && let Some(cs) = &c.columns
        {
            keys.push(DeclaredKey {
                cols: cs.iter().map(|s| s.to_lowercase()).collect(),
                origin: DeclOrigin::ContractPrimaryKey,
            });
            for c in cs {
                add_not_null(
                    not_null,
                    c.to_lowercase(),
                    NotNullOrigin::ContractPrimaryKey,
                );
            }
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
                    add_not_null(not_null, name.clone(), NotNullOrigin::ContractPrimaryKey);
                }
                Some("not_null") => {
                    add_not_null(not_null, name.clone(), NotNullOrigin::Contract);
                }
                _ => {}
            }
        }
    }
}

pub(crate) fn add_not_null(
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
    col: String,
    origin: NotNullOrigin,
) {
    let v = not_null.entry(col).or_default();
    if !v.contains(&origin) {
        v.push(origin);
    }
}

/// meta.cardinal.grain: a key, and (as a primary key) non-null columns.
fn grain_facts(
    meta: Option<Vec<String>>,
    keys: &mut Vec<DeclaredKey>,
    not_null: &mut BTreeMap<String, Vec<NotNullOrigin>>,
) {
    if let Some(cols) = meta {
        for c in &cols {
            add_not_null(not_null, c.clone(), NotNullOrigin::MetaGrain);
        }
        keys.push(DeclaredKey {
            cols,
            origin: DeclOrigin::MetaGrain,
        });
    }
}

/// A meta.cardinal.references entry before its target is resolved.
#[derive(Clone, Debug)]
struct RawReference {
    cols: Vec<String>,
    to: String,
    /// Empty: same names as `cols`, or (column-level string form) still to be split.
    to_cols: Vec<String>,
}

fn cardinal_meta<'a>(meta: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    meta?.get("cardinal")?.get(key)
}

/// Relation-level `references`: a map or a list of maps with `columns`, `to` and
/// optional `to_columns`.
fn relation_references(v: &Value) -> Result<Vec<RawReference>, String> {
    let items: Vec<&Value> = match v {
        Value::Array(a) => a.iter().collect(),
        Value::Object(_) => vec![v],
        _ => return Err("expected a list of {columns, to, to_columns}".into()),
    };
    items
        .into_iter()
        .map(|it| {
            let cols = it
                .get("columns")
                .or_else(|| it.get("column"))
                .map(string_list)
                .unwrap_or_default();
            let to = it
                .get("to")
                .and_then(Value::as_str)
                .ok_or("an entry has no `to`")?;
            let to_cols = it
                .get("to_columns")
                .or_else(|| it.get("to_column"))
                .map(string_list)
                .unwrap_or_default();
            if cols.is_empty() {
                return Err(format!("the entry for {to} has no `columns`"));
            }
            Ok(RawReference {
                cols,
                to: to.to_string(),
                to_cols,
            })
        })
        .collect()
}

/// Column-level `references`: "relation", "relation.column", or {to, column}.
fn column_reference(col: &str, v: &Value) -> Result<RawReference, String> {
    match v {
        Value::String(s) => Ok(RawReference {
            cols: vec![col.to_string()],
            to: s.clone(),
            to_cols: vec![],
        }),
        Value::Object(_) => {
            let to = v
                .get("to")
                .and_then(Value::as_str)
                .ok_or("expected {to, column}")?;
            let to_col = v
                .get("column")
                .or_else(|| v.get("to_column"))
                .and_then(Value::as_str)
                .map(|s| s.to_lowercase())
                .unwrap_or_else(|| col.to_string());
            Ok(RawReference {
                cols: vec![col.to_string()],
                to: to.to_string(),
                to_cols: vec![to_col],
            })
        }
        _ => Err("expected a relation name or {to, column}".into()),
    }
}

/// Every meta.cardinal.references entry on a relation and its columns.
fn raw_references(
    meta: Option<&Value>,
    config_meta: Option<&Value>,
    cols: &BTreeMap<String, ColumnInfo>,
    problems: &mut Vec<String>,
) -> Vec<RawReference> {
    let mut out = Vec::new();
    if let Some(v) =
        cardinal_meta(meta, "references").or_else(|| cardinal_meta(config_meta, "references"))
    {
        match relation_references(v) {
            Ok(r) => out.extend(r),
            Err(e) => problems.push(e),
        }
    }
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        let cm = info.config.as_ref().and_then(|c| c.get("meta"));
        if let Some(v) = cardinal_meta(info.meta.as_ref(), "references")
            .or_else(|| cardinal_meta(cm, "references"))
        {
            match column_reference(&name, v) {
                Ok(r) => out.push(r),
                Err(e) => problems.push(format!("column {name}: {e}")),
            }
        }
    }
    out
}

/// Every meta.cardinal.measure and meta.cardinal.unit on a relation's columns. Names in
/// `over` and `unit` must be columns of the relation when its columns are known.
fn column_measures(
    cols: &BTreeMap<String, ColumnInfo>,
    columns: &[Column],
    problems: &mut Vec<String>,
) -> (Vec<DeclaredMeasure>, Vec<DeclaredUnit>) {
    let known = |n: &str| columns.is_empty() || columns.iter().any(|c| c.name == n);
    let mut measures = Vec::new();
    let mut units = Vec::new();
    for (k, info) in cols {
        let name = info
            .name
            .clone()
            .unwrap_or_else(|| k.clone())
            .to_lowercase();
        let cm = info.config.as_ref().and_then(|c| c.get("meta"));
        let get =
            |key: &str| cardinal_meta(info.meta.as_ref(), key).or_else(|| cardinal_meta(cm, key));
        if let Some(v) = get("measure") {
            match measure_decl(v) {
                Ok(kind) => {
                    let missing: Vec<&String> = match &kind {
                        MeasureDecl::Stock { over } => over.iter().filter(|o| !known(o)).collect(),
                        _ => vec![],
                    };
                    if missing.is_empty() {
                        measures.push(DeclaredMeasure {
                            column: name.clone(),
                            kind,
                        });
                    } else {
                        problems.push(format!(
                            "column {name}: measure is over {}, which is not a column",
                            missing
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
                Err(e) => problems.push(format!("column {name}: measure {e}")),
            }
        }
        if let Some(v) = get("unit") {
            match v.as_str().and_then(plain_column) {
                Some(unit) if known(&unit) => units.push(DeclaredUnit {
                    column: name.clone(),
                    unit,
                }),
                Some(unit) => problems.push(format!(
                    "column {name}: its unit column {unit} is not a column"
                )),
                None => problems.push(format!(
                    "column {name}: unit should name the column that holds its unit"
                )),
            }
        }
    }
    (measures, units)
}

/// `non_additive`, `{kind: non_additive}`, or `{kind: stock, over: [col, …]}`.
fn measure_decl(v: &Value) -> Result<MeasureDecl, String> {
    let (kind, over) = match v {
        Value::String(s) => (s.as_str(), None),
        Value::Object(_) => (
            v.get("kind")
                .and_then(Value::as_str)
                .ok_or("has no `kind`")?,
            v.get("over"),
        ),
        _ => return Err("should be {kind: stock, over: [...]} or {kind: non_additive}".into()),
    };
    match kind.to_ascii_lowercase().replace('-', "_").as_str() {
        "additive" => Ok(MeasureDecl::Additive),
        "non_additive" | "nonadditive" => Ok(MeasureDecl::NonAdditive),
        "stock" | "semi_additive" | "balance" => {
            let over: Vec<String> = over
                .map(string_list)
                .unwrap_or_default()
                .iter()
                .filter_map(|s| plain_column(s))
                .collect();
            if over.is_empty() {
                Err(format!(
                    "kind {kind} needs `over`: the columns it can't be added across, such as a snapshot date"
                ))
            } else {
                Ok(MeasureDecl::Stock { over })
            }
        }
        other => Err(format!(
            "kind `{other}` is unknown (expected stock, non_additive or additive)"
        )),
    }
}

fn exactly_once_names(meta: Option<&Value>, config_meta: Option<&Value>) -> Vec<String> {
    cardinal_meta(meta, "exactly_once")
        .or_else(|| cardinal_meta(config_meta, "exactly_once"))
        .map(|v| match v {
            Value::String(s) => vec![s.clone()],
            Value::Array(a) => a
                .iter()
                .filter_map(|x| x.as_str())
                .map(str::to_string)
                .collect(),
            _ => vec![],
        })
        .unwrap_or_default()
}

/// Strip `ref('x')` and `source('a', 'b')` to `x` and `a.b`, so either spelling works.
fn plain_relation_name(name: &str) -> String {
    let t = name.trim();
    let args = |inner: &str| -> Vec<String> {
        inner
            .split(',')
            .map(|a| a.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
            .filter(|a| !a.is_empty())
            .collect()
    };
    for (prefix, sep) in [("ref(", ""), ("source(", ".")] {
        if let Some(inner) = t.strip_prefix(prefix).and_then(|r| r.strip_suffix(')')) {
            let a = args(inner);
            return if sep.is_empty() {
                a.last().cloned().unwrap_or_default()
            } else {
                a.join(sep)
            };
        }
    }
    t.to_string()
}

/// Resolve a relation name as a user writes it in YAML: unique id, `source.table` for
/// sources, or the name of a model, seed or snapshot. Names are case-insensitive.
fn resolve_name(relations: &BTreeMap<RelationId, Relation>, name: &str) -> Option<RelationId> {
    let n = plain_relation_name(name);
    if let Some(r) = relations.get(&RelationId(n.clone())) {
        return Some(r.id.clone());
    }
    let lower = n.to_lowercase();
    let hits: Vec<&Relation> = relations
        .values()
        .filter(|r| r.display.to_lowercase() == lower)
        .collect();
    // A model and a source can share a display name only in odd projects; prefer the model.
    match hits.as_slice() {
        [one] => Some(one.id.clone()),
        [] => None,
        many => many
            .iter()
            .filter(|r| r.kind != Kind::Source)
            .map(|r| r.id.clone())
            .next(),
    }
}

/// Turn raw references into resolved ones, checking that every column exists when the
/// column lists are known.
fn resolve_references(
    relations: &mut BTreeMap<RelationId, Relation>,
    raw: BTreeMap<RelationId, Vec<RawReference>>,
    warnings: &mut Vec<LoadWarning>,
) {
    for (id, refs) in raw {
        let display = relations[&id].display.clone();
        let mut resolved = Vec::new();
        for r in refs {
            let mut to_name = r.to.clone();
            let mut to_cols = r.to_cols.clone();
            let mut to = resolve_name(relations, &to_name);
            // Column-level "relation.column" form.
            if to.is_none()
                && to_cols.is_empty()
                && let Some((rel, col)) = plain_relation_name(&r.to).rsplit_once('.')
                && let Some(t) = resolve_name(relations, rel)
            {
                to = Some(t);
                to_name = rel.to_string();
                to_cols = vec![col.to_lowercase()];
            }
            let Some(to) = to else {
                warnings.push(LoadWarning::BadReference {
                    relation: display.clone(),
                    problem: format!("`{}` is not a model, seed, snapshot or source", r.to),
                });
                continue;
            };
            if to_cols.is_empty() {
                to_cols = r.cols.clone();
            }
            let to_cols: Vec<String> = to_cols.iter().map(|c| c.to_lowercase()).collect();
            if to_cols.len() != r.cols.len() {
                warnings.push(LoadWarning::BadReference {
                    relation: display.clone(),
                    problem: format!(
                        "{} columns reference {} columns of {to_name}",
                        r.cols.len(),
                        to_cols.len()
                    ),
                });
                continue;
            }
            let missing = |rel: &Relation, cols: &[String]| -> Vec<String> {
                if rel.columns.is_empty() {
                    return vec![];
                }
                cols.iter()
                    .filter(|c| !rel.columns.iter().any(|x| &x.name == *c))
                    .cloned()
                    .collect()
            };
            let m1 = missing(&relations[&id], &r.cols);
            let m2 = missing(&relations[&to], &to_cols);
            if !m1.is_empty() || !m2.is_empty() {
                let mut parts = Vec::new();
                if !m1.is_empty() {
                    parts.push(format!("{display} has no column {}", m1.join(", ")));
                }
                if !m2.is_empty() {
                    parts.push(format!(
                        "{} has no column {}",
                        relations[&to].display,
                        m2.join(", ")
                    ));
                }
                warnings.push(LoadWarning::BadReference {
                    relation: display.clone(),
                    problem: parts.join("; "),
                });
                continue;
            }
            let d = DeclaredReference {
                cols: r.cols.clone(),
                to,
                to_cols,
                guess: None,
            };
            if !resolved.contains(&d) {
                resolved.push(d);
            }
        }
        if let Some(rel) = relations.get_mut(&id) {
            rel.references = resolved;
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
    if let Err(e) = project.record_guess_results() {
        project.warnings.push(LoadWarning::Guesses(format!(
            "could not record guess results: {e}"
        )));
    }
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
        let mut problems = Vec::new();
        let refs = raw_references(
            s.meta.as_ref(),
            s.config.meta.as_ref(),
            &s.columns,
            &mut problems,
        );
        let display = format!("{}.{}", s.source_name, s.name);
        for p in problems {
            warnings.push(LoadWarning::BadReference {
                relation: display.clone(),
                problem: p,
            });
        }
        let mut problems = Vec::new();
        let (measures, units) = column_measures(&s.columns, &columns, &mut problems);
        for p in problems {
            warnings.push(LoadWarning::BadMeasure {
                relation: display.clone(),
                problem: p,
            });
        }
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
        let mut problems = Vec::new();
        let refs = raw_references(
            n.meta.as_ref(),
            n.config.meta.as_ref(),
            &n.columns,
            &mut problems,
        );
        for p in problems {
            warnings.push(LoadWarning::BadReference {
                relation: n.name.clone(),
                problem: p,
            });
        }
        let mut problems = Vec::new();
        let (measures, units) = column_measures(&n.columns, &columns, &mut problems);
        for p in problems {
            warnings.push(LoadWarning::BadMeasure {
                relation: n.name.clone(),
                problem: p,
            });
        }
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
    resolve_references(&mut relations, raw_refs, &mut warnings);
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
    let known = cardinal_sql_adapters();
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
        test_paths: yml.test_paths.clone(),
        guess_tests,
        guesses: GuessLog::default(),
    }
}

/// A Cardinal guess test: it checks its hypothesis like any data test of that fact, but
/// declares nothing; only a recorded pass does (see `Project::apply_guesses`).
fn attach_guess_test(
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

fn cardinal_sql_adapters() -> &'static [&'static str] {
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
    let deps: Vec<&String> = n
        .depends_on
        .nodes
        .iter()
        .filter(|d| !d.starts_with("macro."))
        .collect();
    // Tests on sources have no attached_node, and depends_on is sorted, so a test with
    // two inputs (relationships) names the tested relation only in its `model` argument.
    let from_model_arg = || {
        let m = tm.kwargs.get("model")?.as_str()?;
        let inner = m.find("source(").or_else(|| m.find("ref("))?;
        let call = &m[inner..];
        let call = &call[..=call.find(')')?];
        let id = resolve_name(relations, call)?;
        deps.iter().any(|d| **d == id.0).then_some(id.0)
    };
    let target = n
        .attached_node
        .clone()
        .or_else(from_model_arg)
        .or_else(|| deps.first().map(|d| d.to_string()));
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
    // A `where` config limits the test to some rows, so it declares nothing about the
    // relation as a whole.
    let filtered = n
        .config
        .where_
        .as_ref()
        .is_some_and(|w| !w.is_null() && w.as_str() != Some(""));
    let warn_only = n
        .config
        .severity
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("warn"));
    let Some(rel) = relations.get_mut(&target) else {
        return;
    };
    let kind = match name {
        "unique" => match column {
            Some(c) => {
                if !filtered {
                    rel.declared_keys.push(DeclaredKey {
                        cols: vec![c.clone()],
                        origin: DeclOrigin::UniqueTest(test_name.clone()),
                    });
                }
                TestKind::Unique(vec![c])
            }
            None => TestKind::Other,
        },
        "unique_combination_of_columns" => {
            let cols = kw
                .get("combination_of_columns")
                .map(string_list)
                .unwrap_or_default();
            let cols: Vec<String> = cols.iter().filter_map(|c| plain_column(c)).collect();
            if cols.is_empty() {
                TestKind::Other
            } else {
                if !filtered {
                    rel.declared_keys.push(DeclaredKey {
                        cols: cols.clone(),
                        origin: DeclOrigin::UniqueCombination(test_name.clone()),
                    });
                }
                TestKind::Unique(cols)
            }
        }
        "not_null" => match column {
            Some(c) => {
                if !filtered {
                    add_not_null(
                        &mut rel.not_null,
                        c.clone(),
                        NotNullOrigin::Test(test_name.clone()),
                    );
                }
                TestKind::NotNull(c)
            }
            None => TestKind::Other,
        },
        // Checks a declared reference; never declares one.
        "relationships" => {
            let field = kw
                .get("field")
                .and_then(Value::as_str)
                .and_then(plain_column);
            match (column, field, other) {
                (Some(column), Some(field), Some(to)) => {
                    TestKind::Relationships { column, to, field }
                }
                _ => TestKind::Other,
            }
        }
        _ => TestKind::Other,
    };
    rel.tests.push(DataTest {
        name: test_name,
        kind,
        filtered,
        warn_only,
    });
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
    fn relation_names_accept_ref_and_source_calls() {
        assert_eq!(plain_relation_name("ref('stg_orders')"), "stg_orders");
        assert_eq!(
            plain_relation_name("ref(\"pkg\", \"stg_orders\")"),
            "stg_orders"
        );
        assert_eq!(
            plain_relation_name("source('shop', 'orders')"),
            "shop.orders"
        );
        assert_eq!(plain_relation_name(" shop.orders "), "shop.orders");
    }

    #[test]
    fn parses_reference_forms() {
        let v: Value = serde_json::json!([
            {"columns": ["order_id"], "to": "shop.orders"},
            {"columns": "a", "to": "x", "to_columns": "b"}
        ]);
        let r = relation_references(&v).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].cols, vec!["order_id"]);
        assert!(r[0].to_cols.is_empty(), "defaults to the same names later");
        assert_eq!(r[1].to_cols, vec!["b"]);
        assert!(relation_references(&serde_json::json!([{"to": "x"}])).is_err());
        assert!(relation_references(&serde_json::json!("x")).is_err());

        let c = column_reference("customer_id", &serde_json::json!("shop.customers")).unwrap();
        assert_eq!(
            (c.cols, c.to, c.to_cols.len()),
            (vec!["customer_id".into()], "shop.customers".into(), 0)
        );
        let c = column_reference(
            "cust",
            &serde_json::json!({"to": "customers", "column": "ID"}),
        )
        .unwrap();
        assert_eq!(c.to_cols, vec!["id"]);
        assert!(column_reference("c", &serde_json::json!(3)).is_err());
    }

    #[test]
    fn reads_project_yml_with_comments_and_block_lists() {
        let root = std::env::temp_dir().join(format!("cardinal-yml-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("dbt_project.yml"),
            "# Name your project!\n\
             name: 'jaffle_shop'  # lowercase\n\
             target-path: \"target\"  # directory which will store compiled SQL files\n\
             model-paths:\n  - \"transform\"   # where models live\n  - staging\n\
             test-paths: [\"checks\"]\n\
             clean-targets:         # directories to be removed by `dbt clean`\n  - \"target\"\n\
             vars:\n  hash: 'a#b'\n",
        )
        .unwrap();
        let yml = read_project_yml(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(yml.name, "jaffle_shop");
        assert_eq!(yml.target_path, "target");
        assert_eq!(yml.model_paths, vec!["transform", "staging"]);
        assert_eq!(yml.test_paths, vec!["checks"]);
        assert_eq!(strip_yaml_comment("x: 'a # b' # c"), "x: 'a # b' ");
        assert_eq!(strip_yaml_comment("x: a#b"), "x: a#b");
    }

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
