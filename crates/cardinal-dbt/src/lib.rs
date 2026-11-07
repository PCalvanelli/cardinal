//! Loads dbt's compiled artifacts into a project model: relations, their columns,
//! declared keys, the DAG, and the order to analyze it in. Never parses Jinja.

mod artifacts;
mod assumption;
mod guess;
mod names;
mod profile;
mod project;
pub mod select;

pub use assumption::{Assumption, CheckedBy, Status, weak_check};
pub use guess::{GUESS_MARKER, Guess, GuessLog, GuessNote, GuessResult, GuessTest, Hypothesis};
pub use profile::{ProfileAdapter, profile_adapter};
pub use project::{
    Column, ColumnsFrom, DataTest, DeclOrigin, DeclProblem, DeclProblemKind, DeclaredKey,
    DeclaredMeasure, DeclaredReference, DeclaredUnit, ExactlyOnce, Incremental, Kind, LoadError,
    LoadWarning, MeasureDecl, NotNullOrigin, Project, Relation, RelationId, TestKind,
    find_project_root, find_projects_below, load_project, load_state, supported_adapters,
};
