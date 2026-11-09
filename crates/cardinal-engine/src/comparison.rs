//! What `--state` found: the result of comparing the head with the base branch.

use cardinal_diag::Baseline;
use cardinal_diag::Diagnostic;
use std::path::PathBuf;

pub struct Comparison {
    /// The state directory, as given.
    pub dir: String,
    /// P003 findings, before suppressions and severities.
    pub findings: Vec<Diagnostic>,
    /// Every finding on the base, to tell which of the head's are not new.
    pub on_main: Baseline,
    pub lost: usize,
    pub gained: usize,
    pub kept: usize,
    /// Models on the base without compiled SQL, which are not compared.
    pub skipped: usize,
    /// Where the base's shapes came from.
    pub base: BaseShapes,
}

/// Where the base branch's shapes came from.
pub enum BaseShapes {
    /// The shapes file the base's own `cardinal check` wrote, at this path.
    Saved(PathBuf),
    /// Analyzing the base artifacts, because the shapes file could not be used: why.
    Analyzed(String),
}
