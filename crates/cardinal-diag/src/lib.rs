//! Diagnostics are plain data, rendered later as text, JSON or SARIF.

pub mod baseline;
pub mod code;
pub mod render;
pub mod sourcemap;
pub mod suppress;

pub use code::{ALL_LAYERS, Code, Layer, Severity};

use cardinal_dbt::RelationId;
use cardinal_sql::SqlSpan;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    /// Path relative to the project root.
    pub file: String,
    pub span: Option<SqlSpan>,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub message: String,
    /// Replacement text for the primary span, only when the rewrite is safe.
    pub replacement: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    /// The model the finding belongs to.
    pub relation: RelationId,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub notes: Vec<String>,
    pub fix: Option<Fix>,
    /// The chain of reasons, outermost first.
    pub evidence: Vec<String>,
    /// Normalized text of what the finding points at, stable across unrelated edits.
    pub fingerprint: String,
}

impl Diagnostic {
    pub fn sort_key(&self) -> (String, u32, u32, Code) {
        let (l, c) = self
            .primary
            .span
            .map(|s| (s.start.line, s.start.col))
            .unwrap_or((0, 0));
        (self.primary.file.clone(), l, c, self.code)
    }
}

/// File contents a renderer can show, keyed by the label's file path.
pub trait SourceFiles {
    fn text(&self, file: &str) -> Option<&str>;
}

impl SourceFiles for std::collections::HashMap<String, String> {
    fn text(&self, file: &str) -> Option<&str> {
        self.get(file).map(|s| s.as_str())
    }
}

/// Collapse whitespace and case so a fingerprint survives reformatting.
pub fn normalize_text(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
