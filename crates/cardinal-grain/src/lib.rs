//! Grain analysis: which column sets are keys, which functional dependencies hold,
//! which columns are provably non-null, and why, for every node of a model's plan.

pub mod analyze;
pub mod evidence;
pub mod facts;
pub mod summary;

pub use analyze::{AnalyzedModel, JoinRecord, UpstreamFacts, analyze, node_id};
pub use evidence::{Evidence, EvidenceArena, EvidenceId};
pub use facts::{
    Cardinality, ColSet, Fd, GrainStatus, JoinSite, Key, RelationFacts, UnknownReason,
};
pub use summary::{
    DeclaredCheck, RelationSummary, SummaryColumn, summarize, summary_from_declared,
};

#[doc(hidden)]
pub mod testing;
