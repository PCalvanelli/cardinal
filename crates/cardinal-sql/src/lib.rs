//! SQL front end: turns a model's compiled SQL into a DataFusion logical plan,
//! resolved against upstream schemas, without running DataFusion's optimizer.

mod dialect;
mod lex;
mod plan;
mod provider;
mod span;
mod stubs;
mod types;

pub use datafusion_common;
pub use datafusion_expr;
pub use datafusion_sql::sqlparser;

pub use dialect::dialect_for_adapter;
pub use lex::{Token, TokenKind, Tokens};
pub use plan::{PlanFailure, PlanFailureKind, plan_sql};
pub use provider::{CardinalContext, CardinalTable, FunctionOrigin, Functions, TableLookup};
pub use span::{LineCol, SourceText, SqlSpan};
pub use stubs::{StubKind, StubSpec};
pub use types::warehouse_type;
