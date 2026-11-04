//! Cardinal's checks. Each check reads an annotated plan and pushes Diagnostic values.

mod g001;
mod g002;
mod g003;
mod g004;
mod g006;
mod g007;
mod g008;
mod g009;
mod p001;
mod p003;
mod sites;
mod tool;

pub use g001::FanOutAggregation;
pub use g002::UnprovenDeclaredKey;
pub use g003::ManyToManyJoin;
pub use g004::{UnknownGrainModel, unknown_grain};
pub use g006::IncrementalKeyNotUnique;
pub use g007::WindowOrderTies;
pub use g008::NonAdditiveSum;
pub use g009::MixedUnits;
pub use p001::contract_findings;
pub use p003::{
    Bound, Guarantee, Lost, ModelChange, Side, compare as compare_guarantees, lost_guarantees,
};
pub use tool::plan_failure;

use cardinal_dbt::{Relation, RelationId};
use cardinal_diag::{Code, Diagnostic, Label};
use cardinal_grain::{AnalyzedModel, DeclaredCheck};
use cardinal_sql::SqlSpan;
use cardinal_sql::provider::Functions;

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

pub trait Check: Send + Sync {
    fn code(&self) -> Code;
    fn run(&self, cx: &CheckContext<'_>, model: &AnalyzedModel, out: &mut Vec<Diagnostic>);
}

/// The per-model checks, in code order. G004 and G005 run over the whole project.
pub fn model_checks() -> Vec<Box<dyn Check>> {
    vec![
        Box::new(FanOutAggregation),
        Box::new(UnprovenDeclaredKey),
        Box::new(ManyToManyJoin),
        Box::new(IncrementalKeyNotUnique),
        Box::new(WindowOrderTies),
        Box::new(NonAdditiveSum),
        Box::new(MixedUnits),
    ]
}

/// Changes whenever the set of checks or their output changes, so cached results from
/// an older check set are not reused.
pub const CHECKS_FINGERPRINT: &str = "G001-G009+P003";

pub(crate) fn tuple(names: &[String]) -> String {
    format!("({})", names.join(", "))
}
