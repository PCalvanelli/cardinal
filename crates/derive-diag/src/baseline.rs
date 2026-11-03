//! Baselines: today's findings, recorded so CI fails only on new ones. Entries are
//! keyed by code, model and normalized expression, not line number, so unrelated edits
//! do not churn the file.

use crate::{Code, Diagnostic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BaselineEntry {
    pub code: Code,
    pub model: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub version: u32,
    pub entries: BTreeSet<BaselineEntry>,
}

impl Baseline {
    pub fn entry(d: &Diagnostic) -> BaselineEntry {
        BaselineEntry {
            code: d.code,
            model: d.relation.clone(),
            fingerprint: d.fingerprint.clone(),
        }
    }

    pub fn from_findings(findings: &[Diagnostic]) -> Self {
        Baseline {
            version: 1,
            entries: findings.iter().map(Self::entry).collect(),
        }
    }

    pub fn contains(&self, d: &Diagnostic) -> bool {
        self.entries.contains(&Self::entry(d))
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Baseline {
                version: 1,
                ..Default::default()
            }),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut s = serde_json::to_string_pretty(self).expect("baseline serializes");
        s.push('\n');
        std::fs::write(path, s)
    }
}
