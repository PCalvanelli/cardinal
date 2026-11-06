//! cardinal: a static grain checker for dbt projects. The binary is a thin wrapper over
//! this library so benchmarks and xtask can drive the engine directly.

pub mod cli;
pub mod commands;
pub mod config;
pub mod engine;
pub mod guess;
pub mod pipeline;
pub mod state;

pub use cli::{CheckArgs, Format};
