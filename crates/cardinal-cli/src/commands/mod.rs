pub mod assumptions;
pub mod check;
pub mod coverage;
pub mod describe;
pub mod explain;
pub mod guess;
pub mod init;
pub mod watch;

use crate::config::Config;
use cardinal_dbt::{Project, find_project_root, load_project};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;

/// Exit code when the tool itself failed (missing artifacts, unreadable manifest).
pub const EXIT_TOOL: u8 = 2;

pub fn color(to_stdout: bool) -> bool {
    std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
        && if to_stdout {
            std::io::stdout().is_terminal()
        } else {
            std::io::stderr().is_terminal()
        }
}

pub fn eprint_error(msg: &str) {
    let c = color(false);
    if c {
        eprintln!("\x1b[1;31merror\x1b[0m\x1b[1m:\x1b[0m {msg}");
    } else {
        eprintln!("error: {msg}");
    }
}

pub fn eprint_warning(msg: &str) {
    if color(false) {
        eprintln!("\x1b[1;33mwarning\x1b[0m\x1b[1m:\x1b[0m {msg}");
    } else {
        eprintln!("warning: {msg}");
    }
}

pub fn root(dir: Option<PathBuf>) -> Result<PathBuf, u8> {
    let start = match dir {
        Some(d) => d,
        None => std::env::current_dir().map_err(|_| EXIT_TOOL)?,
    };
    find_project_root(&start).ok_or_else(|| {
        eprint_error(
            &cardinal_dbt::LoadError::NoProject {
                from: start.clone(),
            }
            .to_string(),
        );
        EXIT_TOOL
    })
}

pub fn load(dir: Option<PathBuf>) -> Result<(Project, Config), u8> {
    let root = root(dir)?;
    let config = Config::load(&root).map_err(|e| {
        eprint_error(&e);
        EXIT_TOOL
    })?;
    let project = load_project(&root).map_err(|e| {
        eprint_error(&e.to_string());
        EXIT_TOOL
    })?;
    for w in &project.warnings {
        eprint_warning(&w.to_string());
    }
    Ok((project, config))
}

/// Raw and compiled SQL for every model, keyed by the paths labels use.
pub fn source_files(project: &Project) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for r in project.relations.values() {
        if let Some(raw) = &r.raw_sql {
            m.insert(r.file.clone(), raw.clone());
        }
        if let (Some(p), Some(c)) = (&r.compiled_path, &r.compiled_sql) {
            m.insert(p.clone(), c.clone());
        }
    }
    m
}

pub fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}
