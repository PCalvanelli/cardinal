//! How relations and column lists are named, shared by the project model, Cardinal's
//! guesses and the assumption ledger.

use serde::{Deserialize, Serialize};
use std::fmt;

/// A dbt node id, e.g. "model.shop.order_lines".
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RelationId(pub String);

impl fmt::Display for RelationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A column list in prose: `customer_id`, or `(order_id, line)` for several.
pub(crate) fn tuple(cols: &[String]) -> String {
    if cols.len() == 1 {
        cols[0].clone()
    } else {
        format!("({})", cols.join(", "))
    }
}
