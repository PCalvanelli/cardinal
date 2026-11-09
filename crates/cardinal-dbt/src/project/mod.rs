//! The project model: relations, their columns, what they declare, and the DAG.
//! `load` builds it from dbt's artifacts.

mod data_tests;
mod decl;
mod graph;
mod guesses;
mod load;
mod resolve;
mod root;

pub use load::{load_project, load_state};
pub use root::{find_project_root, find_projects_below};
pub(crate) use root::{profile_name, strip_yaml_comment};

use crate::guess::{GuessLog, GuessNote, GuessTest};
pub use crate::names::RelationId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::path::PathBuf;
use std::time::SystemTime;

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
    /// Set for snapshots: `unique_key` and the snapshot `strategy`.
    pub snapshot: Option<Incremental>,
    /// meta.cardinal declarations that were ignored, and why (D001, D002).
    pub decl_problems: Vec<DeclProblem>,
    /// The YAML file that holds its declarations, relative to the project root, when
    /// known. For sources, the same as `file`.
    pub yaml_file: Option<String>,
}

/// What is wrong with a meta.cardinal declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclProblemKind {
    /// Not in a shape Cardinal reads.
    Malformed,
    /// Names a column the relation (or the other relation) doesn't have.
    MissingColumn,
    /// Names something that is not a model, seed, snapshot or source.
    UnknownRelation,
}

/// A meta.cardinal declaration Cardinal ignored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclProblem {
    /// The setting: references, measure, unit or exactly_once.
    pub field: &'static str,
    pub kind: DeclProblemKind,
    pub message: String,
}

impl DeclProblem {
    fn new(field: &'static str, kind: DeclProblemKind, message: impl Into<String>) -> Self {
        DeclProblem {
            field,
            kind,
            message: message.into(),
        }
    }
}

impl Relation {
    pub fn is_model(&self) -> bool {
        matches!(
            self.kind,
            Kind::Model { .. } | Kind::Ephemeral | Kind::PythonModel
        )
    }

    /// How a dbt test reads this relation: `ref('orders')` or `source('app', 'events')`.
    pub fn ref_call(&self) -> String {
        match (&self.kind, self.display.split_once('.')) {
            (Kind::Source, Some((src, table))) => format!("source('{src}', '{table}')"),
            _ => format!("ref('{}')", self.display),
        }
    }
}

#[derive(Clone, Debug)]
pub enum LoadWarning {
    UnknownSchemaVersion(String),
    MissingCatalog { command: String },
    UnknownAdapter(String),
    CycleIgnored(Vec<RelationId>),
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
    /// The manifest's metadata.invocation_id: which dbt run wrote it.
    pub invocation_id: Option<String>,
    /// dbt's `test-paths`, relative to the root.
    pub test_paths: Vec<String>,
    /// Cardinal guess tests in the manifest.
    pub guess_tests: Vec<GuessTest>,
    /// `.cardinal/guesses.json`: what dbt found when it ran Cardinal's guesses, with
    /// any newer results from target/run_results.json merged in.
    pub guesses: GuessLog,
    /// Whether `guesses` has results `.cardinal/guesses.json` doesn't; see
    /// [`Project::save_guesses`].
    guesses_unsaved: bool,
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

    /// What `check` reports on by default: models, and snapshots with SQL (each run
    /// merges what that SQL returns).
    pub fn checked(&self) -> impl Iterator<Item = &Relation> {
        self.relations
            .values()
            .filter(|r| r.is_model() || (r.kind == Kind::Snapshot && r.compiled_sql.is_some()))
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
                self.checked().find(|r| {
                    r.display == q
                        || r.file.trim_end_matches(".sql") == q
                        || r.file.trim_end_matches(".sql").ends_with(&format!("/{q}"))
                })
            })
    }

    /// The name people know a relation by, or its id when the project doesn't have it.
    pub fn display(&self, id: &RelationId) -> String {
        self.get(id)
            .map(|r| r.display.clone())
            .unwrap_or_else(|| id.0.clone())
    }
}

/// Adapters with a matching SQL dialect; any other uses the generic one.
pub fn supported_adapters() -> &'static [&'static str] {
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
