//! `cardinal init`: a guided first run. Checks the dbt artifacts Cardinal reads, writes
//! cardinal.toml with the detected dialect and a stub for each unknown function,
//! records today's findings as the baseline, and ignores the cache in git.

use super::report::{count_by_code, coverage, pct, unlock_hint};
use super::{EXIT_TOOL, color, plural, root};
use crate::config::{NewStub, stub_blocks, template};
use cardinal_dbt::profile_adapter;
use cardinal_dbt::{Kind, LoadError, LoadWarning, Project, RelationId, supported_adapters};
use cardinal_diag::Baseline;
use cardinal_diag::{Code, Layer, Severity};
use cardinal_engine::{Analysis, Config, Findings, Options};
use cardinal_sql::StubSpec;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Rounds of planning to find unknown functions: planning stops at a model's first
/// unknown function, so each round can find the next one.
const MAX_STUB_ROUNDS: usize = 20;

enum Mark {
    Ok,
    Warn,
    Fail,
}

/// The checklist init prints, one line per item, with the fix under it.
struct Checklist {
    color: bool,
    failed: bool,
}

impl Checklist {
    fn item(&mut self, mark: Mark, text: &str, fix: Option<&str>) {
        let (sym, code) = match mark {
            Mark::Ok => ("✓", "32"),
            Mark::Warn => ("!", "33"),
            Mark::Fail => {
                self.failed = true;
                ("✗", "31")
            }
        };
        if self.color {
            println!("  \x1b[1;{code}m{sym}\x1b[0m {text}");
        } else {
            println!("  {sym} {text}");
        }
        for l in fix.into_iter().flat_map(str::lines) {
            if l.is_empty() {
                println!();
            } else {
                println!("      {l}");
            }
        }
    }
    fn ok_with(&mut self, text: &str, note: &str) {
        self.item(Mark::Ok, text, Some(note));
    }
    fn ok(&mut self, text: &str) {
        self.item(Mark::Ok, text, None);
    }
    fn warn(&mut self, text: &str, fix: Option<&str>) {
        self.item(Mark::Warn, text, fix);
    }
    fn fail(&mut self, text: &str, fix: Option<&str>) {
        self.item(Mark::Fail, text, fix);
    }
}

fn rel_path(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap_or(p).display().to_string()
}

/// `p` relative to the project, or with the home directory as `~`, so a committed
/// cardinal.toml doesn't name a user's home.
fn short_path(root: &Path, p: &Path) -> String {
    if let Ok(r) = p.strip_prefix(root) {
        return r.display().to_string();
    }
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty())
        && let Ok(r) = p.strip_prefix(&home)
    {
        return format!("~/{}", r.display());
    }
    p.display().to_string()
}

pub fn run(dir: Option<PathBuf>, force: bool) -> u8 {
    let root = match root(dir) {
        Ok(r) => r,
        Err(c) => return c,
    };
    println!("Setting up Cardinal in {}", root.display());
    let mut list = Checklist {
        color: color(true),
        failed: false,
    };

    // An existing cardinal.toml is kept unless --force; its settings apply below.
    let toml_path = root.join("cardinal.toml");
    let had_toml = toml_path.exists();
    let keep_toml = had_toml && !force;
    let mut config = Config::default();
    let mut bad_toml = false;
    if keep_toml {
        match Config::load(&root) {
            Ok(c) => config = c,
            Err(e) => {
                bad_toml = true;
                list.fail(&e, Some("fix it, or run: cardinal init --force"));
            }
        }
    }

    let loaded = cardinal_dbt::load_project(&root);
    let project = match loaded {
        Ok(mut p) => {
            if let Err(e) = p.save_guesses() {
                p.warnings.push(LoadWarning::Guesses(format!(
                    "could not record guess results: {e}"
                )));
            }
            artifact_items(&mut list, &p);
            Some(p)
        }
        Err(e) => {
            load_error_item(&mut list, &root, &e);
            None
        }
    };

    let dialect = dialect_item(&mut list, &root, project.as_ref(), &config, keep_toml);
    if !keep_toml {
        config.dialect = Some(dialect.name.clone());
    }

    // Plan every model, stubbing unknown functions until none is left.
    let analysis = project.as_ref().map(|p| analyze(p, &mut config));

    let new_stubs: Vec<NewStub> = analysis.as_ref().map_or(Vec::new(), |a| a.stubs.clone());
    if !keep_toml {
        let text = template(&dialect.name, &dialect.note, &new_stubs);
        if let Err(e) = std::fs::write(&toml_path, text) {
            list.fail(&format!("could not write cardinal.toml: {e}"), None);
            return EXIT_TOOL;
        }
        list.ok(if had_toml {
            "wrote cardinal.toml (replaced the old one)"
        } else {
            "wrote cardinal.toml"
        });
        if !new_stubs.is_empty() {
            list.warn(
                &format!(
                    "wrote a stub for {} Cardinal didn't know: {}",
                    plural(new_stubs.len(), "function", "functions"),
                    names(&new_stubs)
                ),
                Some("check each return type in cardinal.toml"),
            );
        }
    } else if !bad_toml {
        list.ok("kept cardinal.toml (cardinal init --force rewrites it)");
        if !new_stubs.is_empty() {
            list.warn(
                &format!(
                    "{} Cardinal doesn't know: {}",
                    plural(new_stubs.len(), "function", "functions"),
                    names(&new_stubs)
                ),
                Some(&format!(
                    "add to cardinal.toml, with each return type:\n\n{}",
                    stub_blocks(&new_stubs).trim_end()
                )),
            );
        }
    }

    if let (Some(p), Some(a)) = (&project, &analysis)
        && let Err(code) = baseline_item(&mut list, p, &config, a, force)
    {
        return code;
    }

    match ignore_cache(&root) {
        Ok(Ignore::Already) => {}
        Ok(Ignore::Added(file)) => list.ok(&format!("added .cardinal/cache/ to {file}")),
        Err(e) => list.warn(
            &format!("could not update .gitignore: {e}"),
            Some("add .cardinal/cache/ to .gitignore"),
        ),
    }

    println!();
    match (&project, &analysis) {
        (Some(p), Some(a)) => print_summary(p, a),
        _ => println!(
            "Next: run dbt compile && dbt docs generate, then cardinal init again to finish."
        ),
    }
    if bad_toml { EXIT_TOOL } else { 0 }
}

/// One line per artifact: the manifest, compiled SQL, the catalog, and any warning.
fn artifact_items(list: &mut Checklist, p: &Project) {
    let manifest = rel_path(&p.root, &p.target_dir.join("manifest.json"));
    let sql_models: Vec<_> = p
        .checked()
        .filter(|r| r.kind != Kind::PythonModel)
        .collect();
    let version = p
        .dbt_version
        .as_ref()
        .map(|v| format!(", dbt {v}"))
        .unwrap_or_default();
    list.ok(&format!(
        "{manifest}: {}{version}",
        plural(sql_models.len(), "model", "models")
    ));
    let compiled = sql_models
        .iter()
        .filter(|r| r.compiled_sql.is_some())
        .count();
    if sql_models.is_empty() || compiled == sql_models.len() {
        if !sql_models.is_empty() {
            list.ok(&format!(
                "compiled SQL for {}",
                if compiled == 1 {
                    "the model".to_string()
                } else {
                    format!("all {compiled} models")
                }
            ));
        }
    } else if compiled == 0 {
        list.fail(
            "no compiled SQL: the manifest looks like it came from dbt parse",
            Some("run: dbt compile"),
        );
    } else {
        list.warn(
            &format!(
                "compiled SQL for only {compiled} of {} models",
                sql_models.len()
            ),
            Some("run: dbt compile"),
        );
    }
    let mut catalog = true;
    for w in &p.warnings {
        match w {
            LoadWarning::MissingCatalog { command } => {
                catalog = false;
                list.warn(
                    &format!(
                        "{} not found: column lists come from YAML only",
                        rel_path(&p.root, &p.target_dir.join("catalog.json"))
                    ),
                    Some(&format!("run: {command}")),
                );
            }
            // Reported with the dialect.
            LoadWarning::UnknownAdapter(_) => {}
            w => list.warn(&w.to_string(), None),
        }
    }
    if catalog {
        list.ok(&rel_path(&p.root, &p.target_dir.join("catalog.json")));
    }
}

fn load_error_item(list: &mut Checklist, root: &Path, e: &LoadError) {
    match e {
        LoadError::MissingArtifact { path, command } => list.fail(
            &format!("{} not found", rel_path(root, path)),
            Some(&format!("run: {command} && dbt docs generate")),
        ),
        LoadError::Stale {
            manifest,
            newer,
            command,
        } => list.fail(
            &format!(
                "{} is older than {}",
                rel_path(root, manifest),
                newer.display()
            ),
            Some(&format!("run: {command}")),
        ),
        e => list.fail(&e.to_string(), None),
    }
}

struct Dialect {
    name: String,
    /// Comment lines for cardinal.toml saying where the dialect came from.
    note: String,
}

fn dialect_item(
    list: &mut Checklist,
    root: &Path,
    project: Option<&Project>,
    config: &Config,
    keep_toml: bool,
) -> Dialect {
    let supported = supported_adapters();
    let (name, from, note) = if let Some(d) = config.dialect.as_ref().filter(|_| keep_toml) {
        (d.clone(), "cardinal.toml".to_string(), String::new())
    } else if let Some(p) = project.filter(|p| !p.adapter.is_empty()) {
        (
            p.adapter.to_ascii_lowercase(),
            "the manifest".to_string(),
            "# Detected from target/manifest.json (metadata.adapter_type).".to_string(),
        )
    } else if let Some(pa) = profile_adapter(root) {
        let path = short_path(root, &pa.path);
        (
            pa.adapter,
            format!("profile in {path}"),
            format!("# Detected from your dbt profile in {path}."),
        )
    } else {
        (
            "generic".to_string(),
            "no manifest or profile to detect it from".to_string(),
            format!(
                "# Nothing to detect it from. Set it to your warehouse: {}.",
                supported.join(", ")
            ),
        )
    };
    if supported.contains(&name.as_str()) {
        list.ok(&format!("dialect: {name} (from {from})"));
    } else if name == "generic" {
        list.warn(
            &format!("dialect: generic ({from})"),
            Some(&format!(
                "set dialect in cardinal.toml to one of: {}",
                supported.join(", ")
            )),
        );
    } else {
        list.warn(
            &format!("dialect: {name} (from {from}) has no matching SQL dialect, so Cardinal uses the generic one"),
            None,
        );
    }
    Dialect { name, note }
}

struct FirstRun<'p> {
    analysis: Analysis<'p>,
    findings: Findings,
    stubs: Vec<NewStub>,
}

/// Plan every model, adding a stub to `config` for each unknown function until
/// planning finds no new one; then collect the findings.
fn analyze<'p>(project: &'p Project, config: &mut Config) -> FirstRun<'p> {
    let selected: BTreeSet<RelationId> = project.checked().map(|r| r.id.clone()).collect();
    let mut found: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut analysis = Analysis::new(project, config, selected, true);
    for _ in 0..MAX_STUB_ROUNDS {
        let mut new = false;
        for (id, r) in &analysis.run.results {
            for d in &r.diagnostics {
                let Some(f) = cardinal_check::unknown_function(d) else {
                    continue;
                };
                let display = project.display(id);
                let models = found.entry(f.to_string()).or_insert_with(|| {
                    new = true;
                    BTreeSet::new()
                });
                models.insert(display);
            }
        }
        if !new {
            break;
        }
        let known: BTreeSet<String> = config.stub.iter().map(|s| s.name.clone()).collect();
        for f in found.keys().filter(|f| !known.contains(*f)) {
            config.stub.push(StubSpec::scalar(f, "arg0"));
        }
        analysis = Analysis::new(project, config, analysis.selected, true);
    }
    let stubs = found
        .into_iter()
        .map(|(name, models)| NewStub {
            name,
            models: models.into_iter().collect(),
        })
        .collect();

    let shapes = analysis.shapes();
    let severities = config.severities(&[], &[]).unwrap_or_else(|_| {
        Config::default()
            .severities(&[], &[])
            .expect("the default config has valid severities")
    });
    let layers: BTreeSet<Layer> = config
        .layers
        .iter()
        .filter_map(|l| l.parse().ok())
        .collect();
    let opts = Options {
        severities: &severities,
        baseline: None,
        state: None,
        min_severity: Severity::Warning,
        layers: &layers,
        no_sql: false,
    };
    let findings = analysis.findings(&shapes, &opts);
    FirstRun {
        analysis,
        findings,
        stubs,
    }
}

fn names(stubs: &[NewStub]) -> String {
    let mut v: Vec<String> = stubs
        .iter()
        .take(5)
        .map(|s| format!("{}()", s.name))
        .collect();
    if stubs.len() > 5 {
        v.push(format!("and {} more", stubs.len() - 5));
    }
    v.join(", ")
}

/// Record today's findings, so CI fails only on new ones. Keeps an existing baseline.
fn baseline_item(
    list: &mut Checklist,
    p: &Project,
    config: &Config,
    a: &FirstRun<'_>,
    force: bool,
) -> Result<(), u8> {
    let path = config.baseline.path.as_ref().map_or_else(
        || p.root.join(".cardinal/baseline.json"),
        |b| p.root.join(b),
    );
    let shown = rel_path(&p.root, &path);
    if path.exists() && !force {
        list.ok(&format!("kept {shown} (cardinal init --force rewrites it)"));
        return Ok(());
    }
    let own: Vec<_> = a
        .findings
        .all
        .iter()
        .filter(|d| d.code != Code::P003)
        .cloned()
        .collect();
    let b = Baseline::from_findings(&own);
    if let Err(e) = b.save(&path) {
        list.fail(&format!("could not write {shown}: {e}"), None);
        return Err(EXIT_TOOL);
    }
    list.ok_with(
        &format!(
            "recorded {} in {shown}",
            plural(b.entries.len(), "finding", "findings")
        ),
        &format!("in CI, cardinal check --baseline {shown} fails only on new ones"),
    );
    Ok(())
}

enum Ignore {
    Already,
    /// The .gitignore it was added to, relative to the project root.
    Added(String),
}

/// Patterns in a .gitignore that cover .cardinal/cache/.
const CACHE_PATTERNS: &[&str] = &[
    ".cardinal",
    ".cardinal/",
    ".cardinal/cache",
    ".cardinal/cache/",
    ".cardinal/cache/*",
    "**/.cardinal",
    "**/.cardinal/",
    "**/.cardinal/cache",
    "**/.cardinal/cache/",
];

/// Add .cardinal/cache/ to the project's .gitignore unless git already ignores it.
fn ignore_cache(root: &Path) -> std::io::Result<Ignore> {
    // Ask git first: a parent directory's .gitignore may already cover it.
    // check-ignore exits 0 when ignored, 1 when not, 128 outside a repository.
    let git = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["check-ignore", "-q", "--no-index", ".cardinal/cache/x"])
        .stderr(std::process::Stdio::null())
        .status();
    if git.as_ref().is_ok_and(|s| s.code() == Some(0)) {
        return Ok(Ignore::Already);
    }
    let path = root.join(".gitignore");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let covered = text.lines().any(|l| {
        let l = l.trim();
        CACHE_PATTERNS.contains(&l.strip_prefix('/').unwrap_or(l))
    });
    if covered {
        return Ok(Ignore::Already);
    }
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    std::fs::write(&path, format!("{text}{sep}.cardinal/cache/\n"))?;
    Ok(Ignore::Added(".gitignore".into()))
}

/// The lines `check` ends with: coverage, findings by code, and what to do next.
fn print_summary(p: &Project, a: &FirstRun<'_>) {
    let cov = coverage(p, &a.analysis.run, &a.analysis.selected);
    println!(
        "Analyzed {}: {} with a known key ({}%).",
        plural(cov.models, "model", "models"),
        cov.known,
        pct(cov.known, cov.models)
    );
    let reported = &a.findings.reported;
    if reported.is_empty() {
        println!("No findings.");
    } else {
        let errors = reported
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let codes: Vec<String> = count_by_code(reported)
            .iter()
            .map(|(c, n)| format!("{c} ×{n}"))
            .collect();
        println!(
            "Findings: {}, {} ({}). Run cardinal check to see them.",
            plural(errors, "error", "errors"),
            plural(reported.len() - errors, "warning", "warnings"),
            codes.join(", ")
        );
    }
    match unlock_hint(p, &a.findings, true) {
        Some(h) => println!("Next: {h}"),
        None => println!("Next: run cardinal check."),
    }
}
