//! What a downstream model needs from an upstream relation: its output schema and
//! finished facts. Serializable, so it is also what the fact cache stores.

use crate::evidence::{Evidence, EvidenceArena, EvidenceId, NO_EVIDENCE};
use crate::facts::{
    ColSet, DistinctCount, DistinctSite, Fd, GrainStatus, Ind, JoinSite, Key, RelationFacts,
    RowLoss, UnknownReason,
};
use cardinal_dbt::{Assumption, DeclOrigin, DeclaredKey, Relation, RelationId};
use cardinal_sql::datafusion_common::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use cardinal_sql::types::warehouse_type;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// What the key rests on. A declared key rests on itself.
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryFd {
    pub lhs: Vec<usize>,
    pub rhs: Vec<usize>,
    pub entity: bool,
    #[serde(default)]
    pub assumptions: Vec<Assumption>,
}

/// An inclusion fact (see `Ind`) between this relation and another one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryInd {
    pub relation: RelationId,
    /// (column of this relation, column of `relation`).
    pub pairs: Vec<(usize, String)>,
    pub assumptions: Vec<Assumption>,
}

/// A distinct-count column (see `DistinctCount`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryDistinct {
    pub col: usize,
    pub agree_on: Vec<usize>,
    pub site: DistinctSite,
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
    /// What each non-null column's non-nullness rests on, when it rests on anything.
    #[serde(default)]
    pub not_null_why: BTreeMap<usize, Vec<Assumption>>,
    /// Relations every row of which appears in this one.
    #[serde(default)]
    pub covers: Vec<SummaryInd>,
    /// Relations this one's values come from.
    #[serde(default)]
    pub within: Vec<SummaryInd>,
    /// Rank columns and their partitions (see `RelationFacts::ranks`).
    #[serde(default)]
    pub ranks: Vec<(usize, Vec<usize>)>,
    /// Where rows of the relations it reads may be lost, here or upstream.
    #[serde(default)]
    pub losses: Vec<RowLoss>,
    /// Columns holding distinct counts.
    #[serde(default)]
    pub distinct: Vec<SummaryDistinct>,
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
        let upstream = |assumptions: &[Assumption], summary: String, arena: &mut EvidenceArena| {
            if assumptions.is_empty() && summary.is_empty() {
                return NO_EVIDENCE;
            }
            arena.push(Evidence::Upstream {
                relation: self.relation.clone(),
                display: self.display.clone(),
                summary,
                assumptions: assumptions.to_vec(),
            })
        };
        let keys = self
            .keys
            .iter()
            .map(|k| {
                let why = match &k.declared {
                    Some(origin) if k.why.is_empty() => arena.push(Evidence::Declared {
                        relation: self.relation.clone(),
                        display: self.display.clone(),
                        origin: origin.clone(),
                        cols: k
                            .cols
                            .iter()
                            .map(|c| self.columns[*c].name.clone())
                            .collect(),
                    }),
                    _ => arena.push(Evidence::Upstream {
                        relation: self.relation.clone(),
                        display: self.display.clone(),
                        summary: k.why.clone(),
                        assumptions: k.assumptions.clone(),
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
                why: upstream(&f.assumptions, String::new(), arena),
                entity: f.entity,
            })
            .collect();
        let inds = |v: &[SummaryInd], arena: &mut EvidenceArena| -> Vec<Ind> {
            v.iter()
                .filter(|i| i.pairs.iter().all(|(c, _)| *c < self.columns.len()))
                .map(|i| Ind {
                    relation: i.relation.clone(),
                    pairs: i.pairs.clone(),
                    why: upstream(&i.assumptions, String::new(), arena),
                })
                .collect()
        };
        let covers = inds(&self.covers, arena);
        let within = inds(&self.within, arena);
        let nn_why = self
            .not_null_why
            .iter()
            .filter(|(_, a)| !a.is_empty())
            .map(|(c, a)| (*c, upstream(a, String::new(), arena)))
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
            covers,
            within,
            ranks: self
                .ranks
                .iter()
                .map(|(r, p)| (*r, p.iter().copied().collect()))
                .collect(),
            nn_why,
            distinct: self
                .distinct
                .iter()
                .filter(|d| {
                    d.col < self.columns.len() && d.agree_on.iter().all(|c| *c < self.columns.len())
                })
                .map(|d| DistinctCount {
                    col: d.col,
                    agree_on: d.agree_on.iter().copied().collect(),
                    site: Arc::new(d.site.clone()),
                })
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
/// failed): its catalog or YAML columns and its declared keys, non-null columns and
/// references, each an assumption.
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
                    assumptions: vec![key_assumption(&rel.id, &columns, &cols)],
                    cols,
                    why: String::new(),
                    declared: Some(k.origin.clone()),
                });
            }
        }
    }
    let mut not_null = Vec::new();
    let mut not_null_why = BTreeMap::new();
    for n in rel.not_null.keys() {
        if let Some(i) = columns.iter().position(|c| &c.name == n) {
            not_null.push(i);
            not_null_why.insert(
                i,
                vec![Assumption::NotNull {
                    relation: rel.id.clone(),
                    column: n.clone(),
                }],
            );
        }
    }
    let within = declared_references(rel, &columns, &[]);
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
        not_null_why,
        covers: vec![],
        within,
        ranks: vec![],
        losses: vec![],
        distinct: vec![],
    }
}

fn key_assumption(rel: &RelationId, columns: &[SummaryColumn], cols: &[usize]) -> Assumption {
    let names: Vec<String> = cols.iter().map(|c| columns[*c].name.clone()).collect();
    Assumption::Key {
        relation: rel.clone(),
        cols: names,
    }
}

/// meta.cardinal.references as within facts, skipping any the SQL already proves.
fn declared_references(
    rel: &Relation,
    columns: &[SummaryColumn],
    proven: &[SummaryInd],
) -> Vec<SummaryInd> {
    let mut out = Vec::new();
    for r in &rel.references {
        let pos: Option<Vec<usize>> = r
            .cols
            .iter()
            .map(|c| columns.iter().position(|x| &x.name == c))
            .collect();
        let Some(pos) = pos else { continue };
        let mut pairs: Vec<(usize, String)> =
            pos.into_iter().zip(r.to_cols.iter().cloned()).collect();
        pairs.sort();
        let already = proven.iter().any(|p| {
            p.relation == r.to
                && p.assumptions.is_empty()
                && pairs.iter().all(|x| p.pairs.contains(x))
        });
        if !already {
            out.push(SummaryInd {
                relation: r.to.clone(),
                pairs,
                assumptions: vec![Assumption::References {
                    relation: rel.id.clone(),
                    cols: r.cols.clone(),
                    to: r.to.clone(),
                    to_cols: r.to_cols.clone(),
                }],
            });
        }
    }
    out
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
    let assume =
        |id: EvidenceId| -> Vec<Assumption> { arena.assumptions(id).into_iter().collect() };
    let mut keys: Vec<SummaryKey> = facts
        .keys
        .iter()
        .map(|k| SummaryKey {
            cols: k.cols.iter().copied().collect(),
            why: arena.explain(k.why),
            declared: None,
            assumptions: assume(k.why),
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
                    assumptions: vec![key_assumption(&rel.id, &columns, &v)],
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
    let mut not_null_why: BTreeMap<usize, Vec<Assumption>> = facts
        .nn_why
        .iter()
        .map(|(c, w)| (*c, assume(*w)))
        .filter(|(_, a)| !a.is_empty())
        .collect();
    for n in rel.not_null.keys() {
        if let Some(i) = columns.iter().position(|c| &c.name == n)
            && not_null.insert(i)
        {
            not_null_why.insert(
                i,
                vec![Assumption::NotNull {
                    relation: rel.id.clone(),
                    column: n.clone(),
                }],
            );
        }
    }
    let status = match &facts.status {
        GrainStatus::Unknown(r) if !keys.is_empty() => GrainStatus::Partial(r.clone()),
        s => s.clone(),
    };
    let inds = |v: &[Ind]| -> Vec<SummaryInd> {
        v.iter()
            .map(|i| SummaryInd {
                relation: i.relation.clone(),
                pairs: i.pairs.clone(),
                assumptions: assume(i.why),
            })
            .collect()
    };
    let covers = inds(&facts.covers);
    let mut within = inds(&facts.within);
    within.extend(declared_references(rel, &columns, &within));
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
                assumptions: assume(f.why),
            })
            .collect(),
        not_null: not_null.into_iter().collect(),
        status,
        repeats: facts
            .repeats
            .iter()
            .map(|r| r.as_deref().cloned())
            .collect(),
        not_null_why,
        covers,
        within,
        ranks: facts
            .ranks
            .iter()
            .map(|(r, p)| (*r, p.iter().copied().collect()))
            .collect(),
        losses: vec![],
        distinct: facts
            .distinct
            .iter()
            .map(|d| SummaryDistinct {
                col: d.col,
                agree_on: d.agree_on.iter().copied().collect(),
                site: (*d.site).clone(),
            })
            .collect(),
    };
    (summary, checks)
}
