//! Loads dbt's compiled artifacts into a project model: relations, their columns,
//! declared keys, the DAG, and the order to analyze it in. Never parses Jinja.

pub mod artifacts;
pub mod project;
pub mod select;

pub use project::{
    Column, DeclOrigin, DeclaredKey, ForeignKey, Incremental, Kind, LoadError, LoadWarning,
    Project, Relation, RelationId, find_project_root, load_project,
};
