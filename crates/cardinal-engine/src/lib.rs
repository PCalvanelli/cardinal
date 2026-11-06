//! Cardinal's analysis engine. It analyzes a dbt project's models in DAG order, runs the
//! checks, and turns their diagnostics into the findings a run reports: suppressions,
//! severities, the baseline and the comparison with the base branch (`--state`).
//! The `cardinal` CLI is one frontend; benchmarks and tests drive the engine directly.

mod analysis;
mod comparison;
mod config;
mod engine;
mod pipeline;
mod selection;
mod state;

pub use analysis::Analysis;
pub use comparison::{BaseShapes, Comparison};
pub use config::{BaselineConfig, Config, GuessConfig, Level, PathOverride, Severities};
pub use engine::{Engine, JoinSummary, ModelResult, RunResult};
pub use pipeline::{Findings, Options, RootCause};
pub use selection::selection;
