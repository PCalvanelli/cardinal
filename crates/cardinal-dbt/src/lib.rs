//! Loads dbt's compiled artifacts into a project model: relations, their columns,
//! declared keys, the DAG, and the order to analyze it in. Never parses Jinja.

pub mod artifacts;
pub mod assumption;
pub mod guess;
pub mod project;
pub mod select;

pub use assumption::{Assumption, CheckedBy, Status, weak_check};
pub use guess::{GuessLog, GuessNote, GuessResult, GuessTest, Hypothesis};
pub use project::{
    Column, DataTest, DeclOrigin, DeclProblem, DeclProblemKind, DeclaredKey, DeclaredMeasure,
    DeclaredReference, DeclaredUnit, ExactlyOnce, Incremental, Kind, LoadError, LoadWarning,
    MeasureDecl, NotNullOrigin, Project, Relation, RelationId, TestKind, find_project_root,
    load_project, load_state,
};
