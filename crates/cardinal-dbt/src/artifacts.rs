//! Minimal serde views of dbt's artifacts. Every struct ignores unknown fields and
//! defaults missing ones, so manifests from different dbt Core versions and from
//! Fusion load unchanged.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Deserialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub metadata: ManifestMetadata,
    #[serde(default)]
    pub nodes: BTreeMap<String, ManifestNode>,
    #[serde(default)]
    pub sources: BTreeMap<String, ManifestSource>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ManifestMetadata {
    #[serde(default)]
    pub dbt_schema_version: Option<String>,
    #[serde(default)]
    pub dbt_version: Option<String>,
    #[serde(default)]
    pub adapter_type: Option<String>,
    #[serde(default)]
    pub project_name: Option<String>,
    /// dbt's id for the invocation that wrote the manifest.
    #[serde(default)]
    pub invocation_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DependsOn {
    #[serde(default)]
    pub nodes: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct NodeConfig {
    #[serde(default)]
    pub materialized: Option<String>,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub unique_key: Option<Value>,
    #[serde(default)]
    pub incremental_strategy: Option<String>,
    /// Snapshots: "timestamp" or "check".
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub contract: Option<Value>,
    /// Tests: a `where` filter limits the rows the test checks.
    #[serde(default, rename = "where")]
    pub where_: Option<Value>,
    /// Tests: "warn" or "error".
    #[serde(default)]
    pub severity: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub struct Constraint {
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub columns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ColumnInfo {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub data_type: Option<String>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub config: Option<Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TestMetadata {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub kwargs: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ManifestNode {
    #[serde(default)]
    pub unique_id: String,
    #[serde(default)]
    pub resource_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub package_name: String,
    #[serde(default)]
    pub original_file_path: String,
    /// The YAML file that documents it, as "package://path".
    #[serde(default)]
    pub patch_path: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default, alias = "compiled_sql")]
    pub compiled_code: Option<String>,
    #[serde(default, alias = "raw_sql")]
    pub raw_code: Option<String>,
    #[serde(default)]
    pub relation_name: Option<String>,
    #[serde(default)]
    pub depends_on: DependsOn,
    #[serde(default)]
    pub config: NodeConfig,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub columns: BTreeMap<String, ColumnInfo>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default)]
    pub test_metadata: Option<TestMetadata>,
    #[serde(default)]
    pub attached_node: Option<String>,
    #[serde(default)]
    pub column_name: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ManifestSource {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub source_name: String,
    #[serde(default)]
    pub package_name: String,
    #[serde(default)]
    pub original_file_path: String,
    #[serde(default)]
    pub relation_name: Option<String>,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub config: NodeConfig,
    #[serde(default)]
    pub columns: BTreeMap<String, ColumnInfo>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Catalog {
    #[serde(default)]
    pub nodes: BTreeMap<String, CatalogTable>,
    #[serde(default)]
    pub sources: BTreeMap<String, CatalogTable>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CatalogTable {
    #[serde(default)]
    pub columns: BTreeMap<String, CatalogColumn>,
}

#[derive(Debug, Deserialize, Default)]
pub struct CatalogColumn {
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "type")]
    pub data_type: String,
    #[serde(default)]
    pub index: i64,
}

/// Schema versions this build was written against. Others load with a warning.
pub const KNOWN_MANIFEST_VERSIONS: &[&str] = &["v10", "v11", "v12", "v13", "v14"];
