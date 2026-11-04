//! Project automation: `cargo xtask <command>`.
//!
//!   fixtures [NAME...]  regenerate fixtures/*/target/{manifest,catalog}.json (and state/ from
//!                       base/) with dbt and DuckDB; for fixtures/guesses, also run
//!                       `cardinal guess` and its dbt tests (target/run_results.json)
//!   new-check G006      scaffold a check, its explain page and a fixture project
//!   corpus DIR...       run `cardinal check` over compiled dbt projects and report per-project stats
//!   synth DIR [N]       write a synthetic N-model project (default 1000) for benchmarks

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("fixtures") => fixtures(&args[1..]),
        Some("new-check") => new_check(args.get(1).context("usage: cargo xtask new-check G006")?),
        Some("corpus") => corpus(&args[1..]),
        Some("synth") => {
            let dir = PathBuf::from(
                args.get(1)
                    .context("usage: cargo xtask synth DIR [MODELS]")?,
            );
            let n = args.get(2).map(|s| s.parse()).transpose()?.unwrap_or(1000);
            let spec = cardinal_cli::synth::SynthSpec {
                models: n,
                ..Default::default()
            };
            let root = cardinal_cli::synth::generate(&dir, spec)?;
            println!("wrote a {n}-model project to {}", root.display());
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: cargo xtask <fixtures [NAME...] | new-check CODE | corpus DIR... | synth DIR [N]>"
            );
            std::process::exit(2);
        }
    }
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {cmd:?}"))?;
    if !status.success() {
        bail!("{cmd:?} failed with {status}");
    }
    Ok(())
}

/// Regenerate each fixture's artifacts: create source tables from setup.sql, build the
/// models, generate the catalog, then keep only manifest.json and catalog.json. A fixture
/// with a base/ directory also gets state/: the same project with base/'s files laid over
/// it, built the same way, standing in for the base branch's artifacts (`--state`).
fn fixtures(only: &[String]) -> Result<()> {
    let dir = workspace().join("fixtures");
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if !path.join("dbt_project.yml").is_file() || (!only.is_empty() && !only.contains(&name)) {
            continue;
        }
        println!("== {name}");
        build_fixture(&path, &name)?;
        if path.join("base").is_dir() {
            println!("== {name} (base, into state/)");
            let tmp = std::env::temp_dir().join(format!("cardinal-fixture-{name}-base"));
            let _ = std::fs::remove_dir_all(&tmp);
            copy_tree(
                &path,
                &tmp,
                &["target", "state", "base", "logs", ".cardinal"],
            )?;
            copy_tree(&path.join("base"), &tmp, &[])?;
            build_fixture(&tmp, &name)?;
            let state = path.join("state");
            let _ = std::fs::remove_dir_all(&state);
            std::fs::create_dir_all(&state)?;
            for f in ["manifest.json", "catalog.json"] {
                std::fs::copy(tmp.join("target").join(f), state.join(f))?;
            }
            std::fs::remove_dir_all(&tmp)?;
        }
    }
    Ok(())
}

/// Copy `from` into `to`, skipping top-level entries named in `skip`.
fn copy_tree(from: &Path, to: &Path, skip: &[&str]) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let p = e?.path();
        let n = p.file_name().unwrap();
        if skip.iter().any(|s| n == *s) || n.to_string_lossy().starts_with("warehouse.duckdb") {
            continue;
        }
        if p.is_dir() {
            copy_tree(&p, &to.join(n), &[])?;
        } else {
            std::fs::copy(&p, to.join(n))?;
        }
    }
    Ok(())
}

fn build_fixture(path: &Path, name: &str) -> Result<()> {
    let db = path.join("warehouse.duckdb");
    let _ = std::fs::remove_file(&db);
    let _ = std::fs::remove_dir_all(path.join("target"));
    if path.join("setup.sql").is_file() {
        let sql = std::fs::read(path.join("setup.sql"))?;
        let mut child = Command::new("duckdb")
            .arg(&db)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .context("duckdb CLI not found")?;
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), &sql)?;
        if !child.wait()?.success() {
            bail!("setup.sql failed for {name}");
        }
    }
    run(Command::new("dbt")
        .args(["run", "--profiles-dir", "."])
        .current_dir(path))?;
    run(Command::new("dbt")
        .args(["docs", "generate", "--profiles-dir", "."])
        .current_dir(path))?;
    let guesses = GUESS_FIXTURES.contains(&name);
    if guesses {
        run_guesses(path)?;
    }
    for e in std::fs::read_dir(path.join("target"))? {
        let p = e?.path();
        let keep = match p.file_name().and_then(|n| n.to_str()) {
            Some("manifest.json" | "catalog.json") => true,
            Some("run_results.json") => guesses,
            _ => false,
        };
        if !keep {
            if p.is_dir() {
                std::fs::remove_dir_all(&p)?
            } else {
                std::fs::remove_file(&p)?
            }
        }
    }
    for junk in [
        "warehouse.duckdb",
        "warehouse.duckdb.wal",
        "logs",
        ".user.yml",
    ] {
        let p = path.join(junk);
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?;
        } else {
            let _ = std::fs::remove_file(&p);
        }
    }
    Ok(())
}

/// Fixtures that exercise `cardinal guess` end to end.
const GUESS_FIXTURES: &[&str] = &["guesses"];

/// The `cardinal guess` flow on a built fixture: write the guess tests, have dbt run
/// them, and keep that run's run_results.json, which the rebuilt artifacts would
/// otherwise overwrite. Some guesses fail on purpose, so dbt test's exit code is ignored.
fn run_guesses(path: &Path) -> Result<()> {
    let _ = std::fs::remove_dir_all(path.join("tests/cardinal"));
    let _ = std::fs::remove_file(path.join(".cardinal/guesses.json"));
    if cardinal_cli::commands::guess::run(Some(path.to_path_buf())) != 0 {
        bail!("cardinal guess failed");
    }
    let _ = Command::new("dbt")
        .args(["test", "--select", "tag:cardinal", "--profiles-dir", "."])
        .current_dir(path)
        .status()?;
    let results = path.join("target/run_results.json");
    let saved = std::fs::read(&results).context("dbt test wrote no run_results.json")?;
    let saved = scrub_paths(&saved, path)?;
    // Rebuild manifest.json with the guess tests in it, and the catalog.
    run(Command::new("dbt")
        .args(["docs", "generate", "--profiles-dir", "."])
        .current_dir(path))?;
    std::fs::write(&results, saved)?;
    // Loading the project records the results in .cardinal/guesses.json.
    cardinal_dbt_load(path)
}

/// run_results.json records where dbt ran (`args.project_dir`, `args.log_path`). Make
/// those paths relative to the fixture, so no checked-in file names this machine.
fn scrub_paths(json: &[u8], fixture: &Path) -> Result<Vec<u8>> {
    let mut text = String::from_utf8(json.to_vec())?;
    for root in [fixture.canonicalize()?, fixture.to_path_buf()] {
        let root = serde_json::to_string(&root.to_string_lossy())?;
        let root = root.trim_matches('"');
        text = text
            .replace(&format!("\"{root}/"), "\"")
            .replace(&format!("\"{root}\""), "\".\"");
    }
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
        && text.contains(&*home.to_string_lossy())
    {
        bail!(
            "run_results.json still names a path under {}",
            home.to_string_lossy()
        );
    }
    Ok(text.into_bytes())
}

fn cardinal_dbt_load(path: &Path) -> Result<()> {
    if cardinal_cli::commands::coverage::run(Some(path.to_path_buf()), cardinal_cli::Format::Text)
        != 0
    {
        bail!("cardinal coverage failed");
    }
    Ok(())
}

/// Scaffold a new check: source file, explain page, and a fixture project.
fn new_check(code: &str) -> Result<()> {
    let code = code.to_ascii_uppercase();
    if !(code.len() == 4 && code.starts_with('G') && code[1..].chars().all(|c| c.is_ascii_digit()))
    {
        bail!("expected a code like G006");
    }
    let lower = code.to_lowercase();
    let ws = workspace();
    let src = ws.join(format!("crates/cardinal-check/src/{lower}.rs"));
    if src.exists() {
        bail!("{} already exists", src.display());
    }
    std::fs::write(
        &src,
        format!(
            r#"//! {code}: <one line saying what this finds>.

use crate::{{Check, CheckContext}};
use cardinal_diag::{{Code, Diagnostic}};
use cardinal_grain::AnalyzedModel;

pub struct NewCheck;

impl Check for NewCheck {{
    fn code(&self) -> Code {{
        Code::{code}
    }}

    fn run(&self, _cx: &CheckContext<'_>, _model: &AnalyzedModel, _out: &mut Vec<Diagnostic>) {{
        // Walk model.plan, read facts with model.facts(node), push findings to out.
    }}
}}
"#
        ),
    )?;
    std::fs::write(
        ws.join(format!("docs/explain/{code}.md")),
        format!(
            "# {code}: <title>\n\n<What it finds and why it matters.>\n\n## Broken\n\n```sql\n```\n\n## Fixed\n\n```sql\n```\n"
        ),
    )?;
    let fx = ws.join(format!("fixtures/{lower}"));
    std::fs::create_dir_all(fx.join("models"))?;
    std::fs::write(
        fx.join("dbt_project.yml"),
        format!("name: {lower}\nversion: \"1.0.0\"\nconfig-version: 2\nprofile: {lower}\n"),
    )?;
    std::fs::write(
        fx.join("profiles.yml"),
        format!(
            "{lower}:\n  target: dev\n  outputs:\n    dev:\n      type: duckdb\n      path: warehouse.duckdb\n      threads: 1\n"
        ),
    )?;
    std::fs::write(fx.join("setup.sql"), "-- create source tables here\n")?;
    std::fs::write(fx.join("models/example.sql"), "select 1 as id\n")?;
    println!(
        "Created:\n  {}\n  docs/explain/{code}.md\n  fixtures/{lower}/",
        src.display()
    );
    println!("Then: add Code::{code} in crates/cardinal-diag/src/code.rs, register the check in");
    println!("cardinal_check::model_checks(), add the explain page in cardinal-cli's explain.rs,");
    println!("and run `cargo xtask fixtures {lower}` followed by `cargo insta review`.");
    Ok(())
}

/// Nightly corpus run: per project, share planned without E-codes, share with known
/// grain, and findings by code. New findings are then triaged by hand.
fn corpus(dirs: &[String]) -> Result<()> {
    if dirs.is_empty() {
        bail!("usage: cargo xtask corpus DIR... (each a dbt project with target/manifest.json)");
    }
    let bin = ws_bin()?;
    let mut report = Vec::new();
    println!(
        "{:<40} {:>7} {:>8} {:>7}  findings",
        "project", "models", "planned", "grain"
    );
    for d in dirs {
        let out = Command::new(&bin)
            .args(["coverage", "--format", "json", "--project-dir", d])
            .output()?;
        let cov: serde_json::Value = serde_json::from_slice(&out.stdout)
            .with_context(|| format!("{d}: {}", String::from_utf8_lossy(&out.stderr)))?;
        let out = Command::new(&bin)
            .args([
                "check",
                "--format",
                "json",
                "--min-severity",
                "info",
                "--no-cache",
                "--project-dir",
                d,
            ])
            .env("CARDINAL_ALLOW_STALE", "1")
            .output()?;
        let check: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
        let models = cov["models"].as_u64().unwrap_or(0);
        let pct = |n: u64| if models == 0 { 0 } else { n * 100 / models };
        let by_code = &check["summary"]["findings_by_code"];
        println!(
            "{:<40} {:>7} {:>7}% {:>6}%  {}",
            d,
            models,
            pct(cov["models_planned"].as_u64().unwrap_or(0)),
            pct(cov["models_with_known_grain"].as_u64().unwrap_or(0)),
            by_code
        );
        report
            .push(serde_json::json!({"project": d, "coverage": cov, "findings_by_code": by_code}));
    }
    let path = workspace().join("target/corpus-report.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    println!("\nwrote {}", path.display());
    Ok(())
}

fn ws_bin() -> Result<PathBuf> {
    run(Command::new(env!("CARGO"))
        .args(["build", "--release", "-p", "cardinal-cli"])
        .current_dir(workspace()))?;
    Ok(workspace().join("target/release/cardinal"))
}
