//! Evidence is stored in an arena per model so facts stay cheap to clone and every
//! diagnostic can print its chain of reasons.

use crate::facts::Cardinality;
use derive_dbt::{DeclOrigin, RelationId};
use derive_sql::SqlSpan;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EvidenceId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Evidence {
    Declared {
        relation: RelationId,
        display: String,
        origin: DeclOrigin,
    },
    /// A fact proven inside an upstream model; `summary` is its rendered chain.
    Upstream {
        relation: RelationId,
        display: String,
        summary: String,
    },
    JoinKept {
        at: Option<SqlSpan>,
        cardinality: Cardinality,
        on: Vec<String>,
        from: Vec<EvidenceId>,
    },
    GroupBy {
        at: Option<SqlSpan>,
        cols: Vec<String>,
    },
    Distinct {
        at: Option<SqlSpan>,
    },
    EqualityFilter {
        at: Option<SqlSpan>,
        text: String,
    },
    RowNumber {
        at: Option<SqlSpan>,
        partition: Vec<String>,
    },
    UnionDiscriminator {
        column: String,
        from: Vec<EvidenceId>,
    },
    Derived {
        from: Vec<EvidenceId>,
    },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvidenceArena {
    items: Vec<Evidence>,
}

impl EvidenceArena {
    pub fn push(&mut self, e: Evidence) -> EvidenceId {
        if let Some(i) = self.items.iter().position(|x| *x == e) {
            return EvidenceId(i as u32);
        }
        self.items.push(e);
        EvidenceId(self.items.len() as u32 - 1)
    }

    pub fn get(&self, id: EvidenceId) -> &Evidence {
        &self.items[id.0 as usize]
    }

    /// Wrap a fact carried through an operator that keeps it unchanged.
    pub fn derived(&mut self, from: EvidenceId) -> EvidenceId {
        from
    }

    /// The chain of reasons behind a fact, outermost first, e.g.
    /// "one-to-many join on order_id; key of shop.order_items from meta.derive.grain".
    pub fn explain(&self, id: EvidenceId) -> String {
        let mut parts = Vec::new();
        self.walk(id, &mut parts, 0);
        let mut seen = std::collections::HashSet::new();
        parts.retain(|p| seen.insert(p.clone()));
        parts.join("; ")
    }

    fn walk(&self, id: EvidenceId, out: &mut Vec<String>, depth: usize) {
        if depth > 32 {
            return;
        }
        match self.get(id) {
            Evidence::Declared {
                display, origin, ..
            } => out.push(format!("key of {display} from {}", origin.describe())),
            Evidence::Upstream {
                display, summary, ..
            } => {
                if summary.is_empty() {
                    out.push(format!("grain of {display}"))
                } else {
                    out.push(summary.clone())
                }
            }
            Evidence::JoinKept {
                cardinality,
                on,
                from,
                ..
            } => {
                if on.is_empty() {
                    out.push(format!("{} join", cardinality.label()));
                } else {
                    out.push(format!("{} join on {}", cardinality.label(), on.join(", ")));
                }
                for f in from {
                    self.walk(*f, out, depth + 1);
                }
            }
            Evidence::GroupBy { cols, .. } => {
                if cols.is_empty() {
                    out.push("aggregate with no GROUP BY returns one row".into())
                } else {
                    out.push(format!("GROUP BY {}", cols.join(", ")))
                }
            }
            Evidence::Distinct { .. } => out.push("SELECT DISTINCT".into()),
            Evidence::EqualityFilter { text, .. } => out.push(format!("filter {text}")),
            Evidence::RowNumber { partition, .. } => out.push(format!(
                "row_number() partitioned by {} filtered to one row",
                if partition.is_empty() {
                    "nothing".to_string()
                } else {
                    partition.join(", ")
                }
            )),
            Evidence::UnionDiscriminator { column, from } => {
                out.push(format!("UNION ALL branches told apart by {column}"));
                for f in from {
                    self.walk(*f, out, depth + 1);
                }
            }
            Evidence::Derived { from } => {
                for f in from {
                    self.walk(*f, out, depth + 1);
                }
            }
        }
    }

    /// The declaration at the root of a chain, if the fact rests on exactly one.
    pub fn declared_root(&self, id: EvidenceId) -> Option<(&str, &DeclOrigin)> {
        match self.get(id) {
            Evidence::Declared {
                display, origin, ..
            } => Some((display, origin)),
            Evidence::Derived { from } if from.len() == 1 => self.declared_root(from[0]),
            _ => None,
        }
    }
}
