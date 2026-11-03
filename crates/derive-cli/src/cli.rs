//! Command-line interface.

use crate::commands;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "derive",
    version,
    about = "Find grain errors in a dbt project before any data moves"
)]
pub struct Cli {
    /// dbt project directory (default: walk up from the current directory)
    #[arg(long, global = true, value_name = "DIR")]
    project_dir: Option<PathBuf>,
    /// Log more (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze the project (or selected models) and print findings
    Check(CheckArgs),
    /// Show a model's grain, keys, each column's native grain, joins and evidence
    Describe {
        /// Model name, unique id or path
        model: String,
    },
    /// Print the long explanation of a finding code, with a broken and a fixed example
    Explain {
        /// A code such as G001
        code: String,
    },
    /// Share of models with known grain, and the root causes for the rest
    Coverage {
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Write derive.toml with the detected dialect and an empty baseline
    Init,
    /// Re-run check whenever target/manifest.json changes
    Watch(CheckArgs),
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Sarif,
}

#[derive(Args, Clone)]
pub struct CheckArgs {
    /// Models to check: name, path (models/marts/), or tag:x; prefix + for upstream, suffix + for downstream
    #[arg(long, short, num_args = 1.., value_name = "SELECTOR")]
    pub select: Vec<String>,
    /// Positional selectors, same as --select
    #[arg(value_name = "SELECT")]
    pub positional: Vec<String>,
    /// Check models changed since a git ref, and their descendants
    #[arg(long, value_name = "GIT_REF")]
    pub changed_since: Option<String>,
    #[arg(long, value_enum, default_value_t = Format::Text)]
    pub format: Format,
    /// Write to a file instead of stdout
    #[arg(long, short, value_name = "FILE")]
    pub output: Option<PathBuf>,
    /// Report only findings not in the baseline
    #[arg(long, value_name = "FILE")]
    pub baseline: Option<PathBuf>,
    /// Rewrite the baseline with current findings
    #[arg(long)]
    pub update_baseline: bool,
    /// error | warning | info
    #[arg(long, default_value = "warning", value_name = "LEVEL")]
    pub min_severity: String,
    /// Treat a code as an error for this run
    #[arg(long, value_name = "CODE")]
    pub deny: Vec<String>,
    /// Turn a code off for this run
    #[arg(long, value_name = "CODE")]
    pub allow: Vec<String>,
    /// Parallelism (default: logical cores)
    #[arg(long, short, value_name = "N")]
    pub jobs: Option<usize>,
    /// Ignore cached facts
    #[arg(long)]
    pub no_cache: bool,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "warn",
        1 => "derive=debug,derive_cli=debug",
        _ => "trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("DERIVE_LOG")
                .unwrap_or_else(|_| filter.into()),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .try_init();
    let code = match cli.command {
        Command::Check(args) => commands::check::run(cli.project_dir, args),
        Command::Describe { model } => commands::describe::run(cli.project_dir, &model),
        Command::Explain { code } => commands::explain::run(&code),
        Command::Coverage { format } => commands::coverage::run(cli.project_dir, format),
        Command::Init => commands::init::run(cli.project_dir),
        Command::Watch(args) => commands::watch::run(cli.project_dir, args),
    };
    ExitCode::from(code)
}
