//! Grain analysis: which column sets are keys, which functional dependencies hold,
//! which columns are provably non-null, and why, for every node of a model's plan.

mod aggregates;
mod analyze;
mod evidence;
mod expr;
mod facts;
mod summary;
mod verdict;

pub use aggregates::{Aggregate, aggregate};
pub use analyze::{AnalyzedModel, JoinRecord, JoinSide, UpstreamFacts, analyze, node_id};
pub use evidence::{Evidence, EvidenceArena, EvidenceId};
pub use expr::{col_index, expr_cols, exprs_cols, summed_col, unalias};
pub use facts::{
    Cardinality, ColSet, Computed, Deleted, DistinctSite, Fd, FlagDrop, GrainStatus, Ind, JoinSite,
    Key, LossSite, Measure, MeasureKind, MeasureSite, RelationFacts, RowLoss, SOFT_DELETE_FLAGS,
    Unit, UnitSite, UnknownReason,
};
pub use summary::{
    DeclaredCheck, RelationSummary, SummaryColumn, SummaryInd, SummaryKey, SummaryMeasure,
    SummaryUnit, summarize, summary_from_declared,
};
pub use verdict::{Half, Reason, Site, Verdict, verdict};

#[doc(hidden)]
pub mod testing;
