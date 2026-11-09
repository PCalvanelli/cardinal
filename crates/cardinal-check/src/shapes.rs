//! The shape contract of every analyzed relation: its key, columns, functional
//! dependencies, nullability, measures, units and inclusions, each with the assumptions
//! it rests on, plus the exactly-once verdicts against its parents.
//!
//! Graph- and node-layer checks read shapes, not plans. `cardinal check` writes them to
//! `target/cardinal_shapes.json`, so a later `--state` run can compare against main's
//! shapes without analyzing main again.

use cardinal_dbt::RelationId;
use cardinal_diag::Baseline;
use cardinal_grain::{GrainStatus, RelationSummary, Verdict};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const SHAPES_FILE: &str = "cardinal_shapes.json";
/// Unstable during the beta: any change to RelationSummary or Verdict can change it.
pub const SHAPES_SCHEMA: &str = "cardinal.shapes/v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shape {
    /// True when the SQL parsed and planned; otherwise the shape comes from declarations.
    pub planned: bool,
    pub summary: RelationSummary,
    /// Exactly-once verdicts: every meta.cardinal.exactly_once target, then each parent.
    #[serde(default)]
    pub verdicts: Vec<Verdict>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Shapes {
    pub schema: String,
    pub cardinal_version: String,
    /// The check set's fingerprint, so shapes from a different check set are not reused.
    pub checks: String,
    pub adapter: String,
    /// The invocation_id of the manifest the shapes were computed from, so shapes are
    /// never used with a different manifest.
    #[serde(default)]
    pub manifest: Option<String>,
    pub relations: BTreeMap<RelationId, Shape>,
    /// Every finding of the run in every layer, after suppressions and severities.
    #[serde(default)]
    pub findings: Baseline,
    /// The models `findings` were collected for: the run's selection. `relations` also
    /// holds their ancestors.
    #[serde(default)]
    pub reported_for: BTreeSet<RelationId>,
}

impl Shapes {
    pub fn new(adapter: &str, manifest: Option<String>) -> Self {
        Shapes {
            schema: SHAPES_SCHEMA.into(),
            cardinal_version: env!("CARGO_PKG_VERSION").into(),
            checks: crate::CHECKS_FINGERPRINT.into(),
            adapter: adapter.into(),
            manifest,
            relations: BTreeMap::new(),
            findings: Baseline {
                version: 1,
                ..Default::default()
            },
            reported_for: BTreeSet::new(),
        }
    }

    pub fn get(&self, id: &RelationId) -> Option<&Shape> {
        self.relations.get(id)
    }

    /// Grain status of every relation, for root-cause grouping.
    pub fn statuses(&self) -> BTreeMap<RelationId, GrainStatus> {
        self.relations
            .iter()
            .map(|(k, v)| (k.clone(), v.summary.status.clone()))
            .collect()
    }

    /// Write atomically, so a reader never sees half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let s = serde_json::to_string(self).expect("shapes serialize");
        let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
        std::fs::write(&tmp, s)?;
        std::fs::rename(&tmp, path)
    }

    /// Load shapes written by this exact version and check set. Err says why not.
    pub fn load(path: &Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        #[derive(Deserialize)]
        struct Header {
            schema: String,
            cardinal_version: String,
            checks: String,
        }
        let h: Header = serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))?;
        if h.schema != SHAPES_SCHEMA {
            return Err(format!("schema {} (expected {SHAPES_SCHEMA})", h.schema));
        }
        if h.cardinal_version != env!("CARGO_PKG_VERSION") {
            return Err(format!(
                "written by Cardinal {} (this is {})",
                h.cardinal_version,
                env!("CARGO_PKG_VERSION")
            ));
        }
        if h.checks != crate::CHECKS_FINGERPRINT {
            return Err(format!(
                "check set {} (this is {})",
                h.checks,
                crate::CHECKS_FINGERPRINT
            ));
        }
        serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
    }
}
