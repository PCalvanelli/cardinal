//! SQL front end: turns a model's compiled SQL into a DataFusion logical plan,
//! resolved against upstream schemas, without running DataFusion's optimizer.

pub mod dialect;
pub mod lex;
pub mod plan;
pub mod provider;
pub mod span;
pub mod stubs;
pub mod types;

pub use datafusion_common;
pub use datafusion_expr;
pub use datafusion_sql::sqlparser;

pub use lex::Tokens;
pub use plan::{PlanFailure, PlanFailureKind, plan_sql};
pub use provider::{CardinalContext, CardinalTable};
pub use span::{LineCol, SourceText, SqlSpan};
pub use stubs::{StubKind, StubSpec};
