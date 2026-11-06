//! Cardinal's checks, in four invariant layers over one analysis. Each layer depends on
//! the ones before it, like the stages of a compiler:
//!
//! - code: each model's file parses (E001 to E003, G005; see `tool` and suppressions).
//! - `graph`: every ref() and declared relation resolves (E004, D002).
//! - `node`: each model's declared shape: columns, types, grain (E006, D001, G004).
//! - `operator`: each join, filter, aggregate, window and set operation, checked against
//!   the keys of what it reads (G001 to G003, G006 to G014, P001 to P003, E005).
//!
//! Checks that need a model's plan implement [`Check`] and run during analysis. Checks
//! over the finished shapes implement [`ProjectCheck`] and run after it.

mod graph;
mod node;
mod operator;
pub mod shapes;
mod sites;
mod tool;

pub use graph::d002::WrongRelation;
pub use node::d001::DeclarationMismatch;
pub use node::g004::{UnknownGrain, UnknownGrainModel, root_causes, unknown_grain};
pub use operator::g001::FanOutAggregation;
pub use operator::g002::UnprovenDeclaredKey;
pub use operator::g003::ManyToManyJoin;
pub use operator::g006::IncrementalKeyNotUnique;
pub use operator::g007::WindowOrderTies;
pub use operator::g008::NonAdditiveSum;
pub use operator::g009::MixedUnits;
pub use operator::g010::SetOpByPosition;
pub use operator::g011::FilterDropsNulls;
pub use operator::g012::UnionMergesRows;
pub use operator::g013::CountOuterJoin;
pub use operator::g014::SoftDeleteDropped;
pub use operator::p001::{ExactlyOnceContracts, contract_findings};
pub use operator::p003::{
    Bound, Guarantee, Lost, ModelChange, Side, compare as compare_guarantees, lost_guarantees,
};
pub use shapes::{SHAPES_FILE, SHAPES_SCHEMA, Shape, Shapes};
pub use tool::{plan_failure, unknown_function};

use cardinal_dbt::{Project, Relation, RelationId};
use cardinal_diag::{Code, Diagnostic, Label};
use cardinal_grain::{AnalyzedModel, DeclaredCheck};
use cardinal_sql::SqlSpan;
use cardinal_sql::provider::Functions;
use std::collections::BTreeSet;

/// Turns positions in a relation's compiled SQL into labels in the user's own files.
pub trait Locator: Sync {
    fn label(&self, relation: &RelationId, span: Option<SqlSpan>, message: String) -> Label;
    fn display(&self, relation: &RelationId) -> String;
}

pub struct CheckContext<'a> {
    pub relation: &'a Relation,
    pub locator: &'a dyn Locator,
    pub functions: &'a Functions,
    pub declared: &'a [DeclaredCheck],
}

/// A check over one model's plan, run during analysis (and cached with its result).
pub trait Check: Send + Sync {
    fn code(&self) -> Code;
    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>);
}

pub struct ProjectContext<'a> {
    pub project: &'a Project,
    /// Shapes of every analyzed relation; empty when no SQL was analyzed.
    pub shapes: &'a Shapes,
    /// The models findings are reported for.
    pub selected: &'a BTreeSet<RelationId>,
    pub locator: &'a dyn Locator,
}

/// A check over the project's finished shapes and declarations, run after analysis.
pub trait ProjectCheck: Send + Sync {
    /// The codes it can report.
    fn codes(&self) -> &'static [Code];
    /// False when it reads only the manifest and catalog, so it also runs when only the
    /// graph and node layers are checked, and on models whose SQL failed to plan.
    fn needs_sql(&self) -> bool;
    fn run(&self, cx: &ProjectContext<'_>, out: &mut Vec<Diagnostic>);
}

/// The per-model checks, in code order.
pub fn model_checks() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(FanOutAggregation),
        Box::new(UnprovenDeclaredKey),
        Box::new(ManyToManyJoin),
        Box::new(IncrementalKeyNotUnique),
        Box::new(WindowOrderTies),
        Box::new(NonAdditiveSum),
        Box::new(MixedUnits),
        Box::new(SetOpByPosition),
        Box::new(FilterDropsNulls),
        Box::new(UnionMergesRows),
        Box::new(CountOuterJoin),
        Box::new(SoftDeleteDropped),
    ]
}

/// The checks over the whole project. P003 is not here: it compares two projects, and
/// `check --state` drives it.
pub fn project_checks() -> Vec<Box<dyn ProjectCheck>> {
    vec![
        Box::new(UnknownGrain),
        Box::new(WrongRelation),
        Box::new(DeclarationMismatch),
        Box::new(ExactlyOnceContracts),
    ]
}

/// The relations whose declarations to check: the selected models, and the sources,
/// seeds and snapshots they read (whose declarations their analysis rests on).
pub(crate) fn declared_relations<'a>(cx: &ProjectContext<'a>) -> Vec<&'a Relation> {
    let mut ids: BTreeSet<RelationId> = cx.selected.clone();
    for id in cx.selected {
        ids.extend(
            cx.project
                .ancestors(id)
                .into_iter()
                .filter(|a| cx.project.get(a).is_some_and(|r| !r.is_model())),
        );
    }
    ids.iter().filter_map(|id| cx.project.get(id)).collect()
}

/// Where a declaration lives: the relation's YAML file when known, else its own file.
pub(crate) fn decl_label(rel: &Relation, message: String) -> Label {
    Label {
        file: rel.yaml_file.clone().unwrap_or_else(|| rel.file.clone()),
        span: None,
        message,
    }
}

/// Changes whenever the set of checks or their output changes, so cached results from
/// an older check set are not reused. It is in every cache key and shapes file, and
/// names the codes the model checks report (plus P003, whose shapes `--state` reads).
/// A test catches a check whose code is outside it; a change to what an existing check
/// reports needs a bump too, which no test can see.
pub const CHECKS_FINGERPRINT: &str = "G001-G014+P003";

pub(crate) fn tuple(names: &[String]) -> String {
    format!("({})", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_covers_every_model_check() {
        let range = CHECKS_FINGERPRINT.split('+').next().unwrap();
        let (lo, hi) = range.split_once('-').unwrap();
        let (lo, hi): (Code, Code) = (lo.parse().unwrap(), hi.parse().unwrap());
        for check in model_checks() {
            let c = check.code();
            assert!(
                lo <= c && c <= hi,
                "{c} is outside CHECKS_FINGERPRINT ({CHECKS_FINGERPRINT}): bump it"
            );
        }
    }
}
