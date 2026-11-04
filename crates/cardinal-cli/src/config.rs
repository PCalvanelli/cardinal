//! cardinal.toml: severity per code and per path, function stubs, baseline path.

use cardinal_diag::{Code, Severity};
use cardinal_sql::StubSpec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Off,
    On(Severity),
}

impl Level {
    pub fn parse(s: &str) -> Result<Level, String> {
        if s.trim().eq_ignore_ascii_case("off") || s.trim().eq_ignore_ascii_case("allow") {
            Ok(Level::Off)
        } else {
            s.parse().map(Level::On)
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Overrides the dialect detected from the manifest's adapter_type.
    #[serde(default)]
    pub dialect: Option<String>,
    #[serde(default)]
    pub severity: BTreeMap<String, String>,
    /// Severity for files under a path, e.g. `models/legacy/`. Longest match wins.
    #[serde(default, rename = "override")]
    pub overrides: Vec<PathOverride>,
    #[serde(default)]
    pub stub: Vec<StubSpec>,
    #[serde(default)]
    pub baseline: BaselineConfig,
    #[serde(default)]
    pub guess: GuessConfig,
    /// Layers `check` checks by default: code, graph, node, operator. Empty means all.
    #[serde(default)]
    pub layers: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuessConfig {
    /// Where `cardinal guess` writes its tests, relative to the project root.
    /// Default: `<first test-path>/cardinal`, usually tests/cardinal.
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathOverride {
    pub path: String,
    #[serde(flatten)]
    pub severity: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineConfig {
    pub path: Option<String>,
}

pub struct Severities {
    global: BTreeMap<Code, Level>,
    paths: Vec<(String, BTreeMap<Code, Level>)>,
    cli: BTreeMap<Code, Level>,
}

fn parse_map(m: &BTreeMap<String, String>, ctx: &str) -> Result<BTreeMap<Code, Level>, String> {
    m.iter()
        .map(|(k, v)| {
            let code: Code = k.parse().map_err(|e| format!("{ctx}: {e}"))?;
            Ok((
                code,
                Level::parse(v).map_err(|e| format!("{ctx}.{k}: {e}"))?,
            ))
        })
        .collect()
}

impl Config {
    pub fn load(root: &Path) -> Result<Config, String> {
        let path = root.join("cardinal.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s).map_err(|e| format!("cardinal.toml: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(format!("cardinal.toml: {e}")),
        }
    }

    pub fn severities(&self, deny: &[Code], allow: &[Code]) -> Result<Severities, String> {
        let global = parse_map(&self.severity, "[severity]")?;
        let mut paths = Vec::new();
        for o in &self.overrides {
            paths.push((
                o.path.trim_end_matches('/').to_string(),
                parse_map(&o.severity, "[[override]]")?,
            ));
        }
        paths.sort_by_key(|(p, _)| std::cmp::Reverse(p.len()));
        let mut cli = BTreeMap::new();
        for c in deny {
            cli.insert(*c, Level::On(Severity::Error));
        }
        for c in allow {
            cli.insert(*c, Level::Off);
        }
        Ok(Severities { global, paths, cli })
    }

    /// A stable fingerprint of everything in the config that changes analysis results.
    pub fn analysis_fingerprint(&self) -> String {
        serde_json::to_string(&(&self.dialect, &self.stub)).unwrap_or_default()
    }
}

impl Severities {
    pub fn resolve(&self, code: Code, file: &str) -> Level {
        self.configured(code, file)
            .unwrap_or(Level::On(code.default_severity()))
    }

    /// The level set for `code` by flags or cardinal.toml, or None when it is not set.
    pub fn configured(&self, code: Code, file: &str) -> Option<Level> {
        if let Some(l) = self.cli.get(&code) {
            return Some(*l);
        }
        for (p, m) in &self.paths {
            if (file == p || file.starts_with(&format!("{p}/"))) && m.contains_key(&code) {
                return Some(m[&code]);
            }
        }
        self.global.get(&code).copied()
    }
}

/// A `[[stub]]` `cardinal init` writes for a function the models call that Cardinal
/// doesn't know, with the models that call it.
#[derive(Clone)]
pub struct NewStub {
    pub name: String,
    pub models: Vec<String>,
}

/// A TOML basic string.
fn toml_str(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

/// The `[[stub]]` blocks for `stubs`, ready to paste into cardinal.toml.
pub fn stub_blocks(stubs: &[NewStub]) -> String {
    let mut out = String::new();
    for s in stubs {
        let mut used: Vec<&str> = s.models.iter().take(3).map(String::as_str).collect();
        if s.models.len() > 3 {
            used.push("...");
        }
        out.push_str(&format!(
            "[[stub]]\nname = {}\nreturns = \"arg0\"   # TODO: the return type (a warehouse type); \"arg0\" is the first argument's type\n# kind = \"aggregate\"   # if it is an aggregate\n# Called by {}\n\n",
            toml_str(&s.name),
            used.join(", ")
        ));
    }
    out
}

/// A starter cardinal.toml. `dialect_note` says where the dialect came from, as
/// comment lines.
pub fn template(dialect: &str, dialect_note: &str, stubs: &[NewStub]) -> String {
    let stubs = if stubs.is_empty() {
        String::new()
    } else {
        format!(
            "# `cardinal init` found these functions in your models and wrote a stub for each.\n# Check each return type.\n{}",
            stub_blocks(stubs)
        )
    };
    format!(
        r#"# cardinal.toml
{dialect_note}
dialect = {dialect}

# Layers `cardinal check` checks: "code", "graph", "node", "operator". Default: all.
# layers = ["code", "graph", "node", "operator"]

# Severity per code: "error", "warning", "info" or "off".
[severity]
# G003 = "error"

# Severity for files under a path; the longest matching path wins.
# [[override]]
# path = "models/legacy/"
# G001 = "warning"

# Warehouse functions DataFusion does not know. Stubs are never run; only the
# return type matters.
# [[stub]]
# name = "convert_timezone"
# returns = "timestamp"      # a warehouse type, or "arg0" for the first argument's type
# deterministic = true
# kind = "aggregate"         # for aggregates; add idempotent = true if duplicates cannot change it

{stubs}# Today's findings, recorded by `cardinal init` or `cardinal check --update-baseline`.
# In CI, `cardinal check --baseline .cardinal/baseline.json` fails only on new findings.
# Uncomment to apply the baseline to every run, which hides the findings it holds.
# [baseline]
# path = ".cardinal/baseline.json"

# Where `cardinal guess` writes the dbt tests that check its guesses.
# [guess]
# path = "tests/cardinal"
"#,
        dialect = toml_str(dialect),
    )
}

#[cfg(test)]
mod layer_tests {
    use super::*;

    #[test]
    fn template_parses_with_layers_uncommented() {
        let t = template("duckdb", "# note", &[]).replace("# layers = ", "layers = ");
        let c: Config = toml::from_str(&t).unwrap();
        assert_eq!(c.layers, vec!["code", "graph", "node", "operator"]);
    }

    #[test]
    fn template_stubs_parse() {
        let stubs = [
            NewStub {
                name: "my_udf".into(),
                models: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            },
            NewStub {
                name: "odd\"name".into(),
                models: vec!["a".into()],
            },
        ];
        let t = template("snowflake", "# Detected.", &stubs);
        let c: Config = toml::from_str(&t).unwrap();
        assert_eq!(c.dialect.as_deref(), Some("snowflake"));
        let names: Vec<&str> = c.stub.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["my_udf", "odd\"name"]);
        assert!(c.stub.iter().all(|s| s.returns == "arg0"));
        assert!(c.baseline.path.is_none());
        assert!(t.contains("# Called by a, b, c, ..."), "{t}");
    }
}
