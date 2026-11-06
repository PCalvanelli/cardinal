//! cardinal: a static grain checker for dbt projects. This crate is the command line:
//! arguments, output and the files commands write. The analysis itself is in
//! `cardinal-engine`; the binary is a thin wrapper over this library so xtask can run
//! commands directly.

pub mod cli;
pub mod commands;
pub mod config;
pub mod guess;
pub mod state;

pub use cli::{CheckArgs, Format};
