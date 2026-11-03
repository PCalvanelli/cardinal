//! Loads dbt's compiled artifacts into a project model: relations, their columns,
//! declared keys, the DAG, and the order to analyze it in. Never parses Jinja.

pub mod artifacts;
pub mod assumption;
pub mod project;
pub mod select;

pub use assumption::{Assumption, CheckedBy, Status};
pub use project::{
    Column, DataTest, DeclOrigin, DeclaredKey, DeclaredReference, ExactlyOnce, Incremental, Kind,
    LoadError, LoadWarning, NotNullOrigin, Project, Relation, RelationId, TestKind,
    find_project_root, load_project,
};
