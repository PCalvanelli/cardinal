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

pub fn template(dialect: &str) -> String {
    format!(
        r#"# cardinal.toml
# Dialect detected from target/manifest.json (metadata.adapter_type).
dialect = "{dialect}"

# Severity per code: "error", "warning", "info" or "off".
[severity]
# G003 = "error"

# Severity for files under a path; the longest matching path wins.
# [[override]]
# path = "models/legacy/"
# G001 = "warning"

# Warehouse functions DataFusion does not know.
# [[stub]]
# name = "convert_timezone"
# returns = "timestamp"      # a warehouse type, or "arg0" for the first argument's type
# deterministic = true
# kind = "aggregate"         # for aggregates; add idempotent = true if duplicates cannot change it

[baseline]
path = ".cardinal/baseline.json"

# Where `cardinal guess` writes the dbt tests that check its guesses.
# [guess]
# path = "tests/cardinal"
"#
    )
}
