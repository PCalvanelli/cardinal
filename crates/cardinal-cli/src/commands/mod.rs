pub mod assumptions;
pub mod check;
pub mod coverage;
pub mod describe;
pub mod explain;
pub mod guess;
pub mod init;
pub mod report;
pub mod watch;

use cardinal_dbt::{LoadWarning, Project, find_project_root, load_project};
use cardinal_engine::Config;
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

/// The dbt project root: walk up from --project-dir, else $DBT_PROJECT_DIR, else the
/// current directory. When none is found, name the projects below, as in a monorepo.
pub fn root(dir: Option<PathBuf>) -> Result<PathBuf, u8> {
    let start = match dir.or_else(|| {
        std::env::var_os("DBT_PROJECT_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from)
    }) {
        Some(d) => d,
        None => std::env::current_dir().map_err(|_| EXIT_TOOL)?,
    };
    find_project_root(&start).ok_or_else(|| {
        let mut msg = cardinal_dbt::LoadError::NoProject {
            from: start.clone(),
        }
        .to_string();
        let below = cardinal_dbt::find_projects_below(&start, 3);
        if let Some(first) = below.first() {
            let rel = |p: &PathBuf| p.strip_prefix(&start).unwrap_or(p).display().to_string();
            let names: Vec<String> = below.iter().take(5).map(|p| format!("{}/", rel(p))).collect();
            let more = if below.len() > 5 {
                format!(" and {} more", below.len() - 5)
            } else {
                String::new()
            };
            msg.push_str(&format!(
                "\n  found dbt projects in {}{more}\n  pass one with --project-dir, e.g. --project-dir {}",
                names.join(", "),
                rel(first)
            ));
        }
        eprint_error(&msg);
        EXIT_TOOL
    })
}

pub fn load(dir: Option<PathBuf>) -> Result<(Project, Config), u8> {
    let root = root(dir)?;
    let config = Config::load(&root).map_err(|e| {
        eprint_error(&e);
        EXIT_TOOL
    })?;
    let mut project = load_project(&root).map_err(|e| {
        eprint_error(&e.to_string());
        EXIT_TOOL
    })?;
    if let Err(e) = project.save_guesses() {
        project.warnings.push(LoadWarning::Guesses(format!(
            "could not record guess results: {e}"
        )));
    }
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

pub fn init_threads(jobs: Option<usize>) {
    if let Some(n) = jobs {
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(n.max(1))
            .build_global();
    }
}
