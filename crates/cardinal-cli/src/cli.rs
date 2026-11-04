//! Command-line interface.

use crate::commands;
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "cardinal",
    version,
    about = "Find where your dbt models can repeat, drop or miscount rows, and prove where they can't, from the compiled SQL alone"
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
    /// Check the project (or selected models) and print findings
    Check(CheckArgs),
    /// Show what Cardinal knows about a model: its key, columns, joins and the rows it keeps
    Describe {
        /// Model name, unique id or path
        model: String,
    },
    /// Explain a finding code, with a broken and a fixed example
    Explain {
        /// A code such as G001
        code: String,
    },
    /// List the facts Cardinal takes on trust, and whether a dbt test checks each one
    Assumptions {
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
        /// Print the dbt tests (YAML) that would check every unchecked one
        #[arg(long)]
        emit_tests: bool,
    },
    /// Guess the keys of sources that have none, and write dbt tests to check them
    ///
    /// Writes one dbt test per guess to tests/cardinal/ (set path under [guess] in
    /// cardinal.toml to change it). Then run `dbt test --select tag:cardinal`. Cardinal
    /// uses each guess dbt passes, always labeled as a guess, and never guesses again one
    /// dbt fails. Cardinal never connects to the warehouse; dbt runs the tests.
    Guess,
    /// How many models Cardinal can check, and which keys to declare next
    Coverage {
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },
    /// Set Cardinal up in a dbt project: check its artifacts and write cardinal.toml
    ///
    /// Checks that the dbt artifacts Cardinal reads exist and are current, detects the
    /// dialect, writes a stub for each warehouse function Cardinal doesn't know, records
    /// today's findings in .cardinal/baseline.json for CI, and adds .cardinal/cache/ to
    /// .gitignore. Safe to run again: it keeps an existing cardinal.toml and baseline.
    Init {
        /// Rewrite cardinal.toml and the baseline even if they exist
        #[arg(long)]
        force: bool,
    },
    /// Run check again whenever target/manifest.json changes
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
    /// Compare with main's dbt artifacts in DIR, like dbt's --state: fail on guarantees
    /// main has that this branch loses, and hide findings main already has. Default: $DBT_STATE
    #[arg(long, value_name = "DIR")]
    pub state: Option<PathBuf>,
    /// Report only findings not in this baseline file
    #[arg(long, value_name = "FILE")]
    pub baseline: Option<PathBuf>,
    /// Write the current findings to the baseline file
    #[arg(long)]
    pub update_baseline: bool,
    /// Lowest severity to show: error, warning or info
    #[arg(long, default_value = "warning", value_name = "LEVEL")]
    pub min_severity: String,
    /// Treat a code as an error
    #[arg(long, value_name = "CODE")]
    pub deny: Vec<String>,
    /// Turn a code off
    #[arg(long, value_name = "CODE")]
    pub allow: Vec<String>,
    /// Check only these layers: code, graph, node, operator (comma-separated; default all).
    /// With only graph and node, no SQL is read, so this works on the output of dbt parse
    #[arg(long, value_delimiter = ',', value_name = "LAYER")]
    pub layer: Vec<String>,
    /// Parallelism (default: logical cores)
    #[arg(long, short, value_name = "N")]
    pub jobs: Option<usize>,
    /// Ignore the cache in .cardinal/cache
    #[arg(long)]
    pub no_cache: bool,
}

pub fn main() -> ExitCode {
    let cli = Cli::parse();
    let filter = match cli.verbose {
        0 => "warn",
        1 => "cardinal=debug,cardinal_cli=debug",
        _ => "trace",
    };
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CARDINAL_LOG")
                .unwrap_or_else(|_| filter.into()),
        )
        .with_writer(std::io::stderr)
        .without_time()
        .try_init();
    let code = match cli.command {
        Command::Check(args) => commands::check::run(cli.project_dir, args),
        Command::Describe { model } => commands::describe::run(cli.project_dir, &model),
        Command::Explain { code } => commands::explain::run(&code),
        Command::Guess => commands::guess::run(cli.project_dir),
        Command::Coverage { format } => commands::coverage::run(cli.project_dir, format),
        Command::Assumptions { format, emit_tests } => {
            commands::assumptions::run(cli.project_dir, format, emit_tests)
        }
        Command::Init { force } => commands::init::run(cli.project_dir, force),
        Command::Watch(args) => commands::watch::run(cli.project_dir, args),
    };
    ExitCode::from(code)
}
