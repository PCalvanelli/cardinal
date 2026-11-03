//! What a downstream model needs from an upstream relation: its output schema and
//! finished facts. Serializable, so it is also what the fact cache stores.

use crate::evidence::{Evidence, EvidenceArena};
use crate::facts::{ColSet, Fd, GrainStatus, JoinSite, Key, RelationFacts, UnknownReason};
use derive_dbt::{DeclOrigin, DeclaredKey, Relation, RelationId};
use derive_sql::datafusion_common::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use derive_sql::types::warehouse_type;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryColumn {
    pub name: String,
    /// Arrow type, as Arrow displays it.
    pub data_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryKey {
    pub cols: Vec<usize>,
    /// Rendered evidence chain.
    pub why: String,
    pub declared: Option<DeclOrigin>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryFd {
    pub lhs: Vec<usize>,
    pub rhs: Vec<usize>,
    pub entity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationSummary {
    pub relation: RelationId,
    pub display: String,
    pub columns: Vec<SummaryColumn>,
    pub keys: Vec<SummaryKey>,
    pub fds: Vec<SummaryFd>,
    pub not_null: Vec<usize>,
    pub status: GrainStatus,
    pub repeats: Vec<Option<JoinSite>>,
    /// False when the columns are a guess (no catalog entry, no YAML columns).
    pub columns_known: bool,
}

impl RelationSummary {
    pub fn schema(&self) -> SchemaRef {
        Arc::new(Schema::new(
            self.columns
                .iter()
                .map(|c| {
                    Field::new(
                        &c.name,
                        DataType::from_str(&c.data_type).unwrap_or(DataType::Utf8),
                        true,
                    )
                })
                .collect::<Vec<_>>(),
        ))
    }

    pub fn col(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn has_known_grain(&self) -> bool {
        !self.keys.is_empty()
    }

    /// Facts for a TableScan of this relation, with evidence in the reading model's arena.
    pub fn to_facts(&self, arena: &mut EvidenceArena) -> RelationFacts {
        let upstream = arena.push(Evidence::Upstream {
            relation: self.relation.clone(),
            display: self.display.clone(),
            summary: String::new(),
        });
        let keys = self
            .keys
            .iter()
            .map(|k| {
                let why = match &k.declared {
                    Some(origin) => arena.push(Evidence::Declared {
                        relation: self.relation.clone(),
                        display: self.display.clone(),
                        origin: origin.clone(),
                    }),
                    None => arena.push(Evidence::Upstream {
                        relation: self.relation.clone(),
                        display: self.display.clone(),
                        summary: k.why.clone(),
                    }),
                };
                Key {
                    cols: k.cols.iter().copied().collect(),
                    why,
                }
            })
            .collect();
        let fds = self
            .fds
            .iter()
            .map(|f| Fd {
                lhs: f.lhs.iter().copied().collect(),
                rhs: f.rhs.iter().copied().collect(),
                why: upstream,
                entity: f.entity,
            })
            .collect();
        RelationFacts {
            width: self.columns.len(),
            keys,
            fds,
            not_null: self.not_null.iter().copied().collect(),
            status: self.status.clone(),
            repeats: self
                .repeats
                .iter()
                .map(|r| r.clone().map(Arc::new))
                .collect(),
        }
    }
}

/// Positions of declared key columns, or None if a column is missing from the schema.
fn declared_cols(columns: &[SummaryColumn], key: &DeclaredKey) -> Option<Vec<usize>> {
    key.cols
        .iter()
        .map(|c| columns.iter().position(|x| &x.name == c))
        .collect()
}

/// A relation that is not planned (source, seed, Python model, or a model whose SQL
/// failed): its catalog or YAML columns and its declared keys.
pub fn summary_from_declared(rel: &Relation, unknown: UnknownReason) -> RelationSummary {
    let columns: Vec<SummaryColumn> = rel
        .columns
        .iter()
        .map(|c| SummaryColumn {
            name: c.name.clone(),
            data_type: warehouse_type(c.data_type.as_deref().unwrap_or("varchar")).to_string(),
        })
        .collect();
    let mut keys = Vec::new();
    for k in &rel.declared_keys {
        if let Some(cols) = declared_cols(&columns, k) {
            let mut cols = cols;
            cols.sort();
            cols.dedup();
            if !keys.iter().any(|e: &SummaryKey| e.cols == cols) {
                keys.push(SummaryKey {
                    cols,
                    why: String::new(),
                    declared: Some(k.origin.clone()),
                });
            }
        }
    }
    let not_null = rel
        .not_null
        .iter()
        .filter_map(|n| columns.iter().position(|c| &c.name == n))
        .collect();
    let status = if keys.is_empty() {
        GrainStatus::Unknown(unknown)
    } else {
        GrainStatus::Known
    };
    RelationSummary {
        relation: rel.id.clone(),
        display: rel.display.clone(),
        repeats: vec![None; columns.len()],
        columns_known: !columns.is_empty(),
        columns,
        keys,
        fds: vec![],
        not_null,
        status,
    }
}

/// Result of comparing a model's declared keys against what its SQL proves.
pub struct DeclaredCheck {
    pub key: DeclaredKey,
    /// The key's columns are not all in the output.
    pub missing_columns: Vec<String>,
    pub proven: bool,
}

/// Summarize a planned model's output facts. Declared keys the analysis cannot prove
/// are still added, since the dbt test enforces them at run time, and reported back.
pub fn summarize(
    rel: &Relation,
    schema: &Schema,
    facts: &RelationFacts,
    arena: &EvidenceArena,
) -> (RelationSummary, Vec<DeclaredCheck>) {
    let columns: Vec<SummaryColumn> = schema
        .fields()
        .iter()
        .map(|f| SummaryColumn {
            name: f.name().to_lowercase(),
            data_type: f.data_type().to_string(),
        })
        .collect();
    let mut keys: Vec<SummaryKey> = facts
        .keys
        .iter()
        .map(|k| SummaryKey {
            cols: k.cols.iter().copied().collect(),
            why: arena.explain(k.why),
            declared: None,
        })
        .collect();
    let mut checks = Vec::new();
    for d in &rel.declared_keys {
        let missing: Vec<String> = d
            .cols
            .iter()
            .filter(|c| !columns.iter().any(|x| &x.name == *c))
            .cloned()
            .collect();
        if !missing.is_empty() {
            checks.push(DeclaredCheck {
                key: d.clone(),
                missing_columns: missing,
                proven: false,
            });
            continue;
        }
        let cols: ColSet = declared_cols(&columns, d)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let proven = facts.is_superkey(&cols);
        checks.push(DeclaredCheck {
            key: d.clone(),
            missing_columns: vec![],
            proven,
        });
        if !proven {
            let v: Vec<usize> = cols.into_iter().collect();
            if !keys.iter().any(|k| k.cols == v) {
                keys.push(SummaryKey {
                    cols: v,
                    why: String::new(),
                    declared: Some(d.origin.clone()),
                });
            }
        } else if let Some(k) = keys
            .iter_mut()
            .find(|k| k.cols == cols.iter().copied().collect::<Vec<_>>())
            && k.why.is_empty()
        {
            k.declared = Some(d.origin.clone());
        }
    }
    let mut not_null: ColSet = facts.not_null.clone();
    not_null.extend(
        rel.not_null
            .iter()
            .filter_map(|n| columns.iter().position(|c| &c.name == n)),
    );
    let status = match &facts.status {
        GrainStatus::Unknown(r) if !keys.is_empty() => GrainStatus::Partial(r.clone()),
        s => s.clone(),
    };
    let summary = RelationSummary {
        relation: rel.id.clone(),
        display: rel.display.clone(),
        columns_known: true,
        columns,
        keys,
        fds: facts
            .fds
            .iter()
            .map(|f| SummaryFd {
                lhs: f.lhs.iter().copied().collect(),
                rhs: f.rhs.iter().copied().collect(),
                entity: f.entity,
            })
            .collect(),
        not_null: not_null.into_iter().collect(),
        status,
        repeats: facts
            .repeats
            .iter()
            .map(|r| r.as_deref().cloned())
            .collect(),
    };
    (summary, checks)
}
