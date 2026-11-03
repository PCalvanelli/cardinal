//! Facts attached to each plan node, and the two primitives every rule and check is
//! phrased in: attribute closure and the superkey test.

use crate::evidence::EvidenceId;
use derive_dbt::RelationId;
use derive_sql::SqlSpan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::Arc;

/// Column positions in a plan node's schema.
pub type ColSet = BTreeSet<usize>;

pub const MAX_KEYS: usize = 8;
pub const MAX_FDS: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub cols: ColSet,
    pub why: EvidenceId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fd {
    pub lhs: ColSet,
    pub rhs: ColSet,
    pub why: EvidenceId,
    /// The left side is the key of some entity (a relation's key carried through),
    /// not just the inputs of a computed expression. Only entity determinants count
    /// as a column's native grain.
    pub entity: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnknownReason {
    SourceWithoutKey(RelationId),
    UnsupportedOperator(String),
    PlanError(RelationId),
    PythonModel(RelationId),
}

impl UnknownReason {
    pub fn describe(&self, display: &dyn Fn(&RelationId) -> String) -> String {
        match self {
            UnknownReason::SourceWithoutKey(r) => format!("{} declares no key", display(r)),
            UnknownReason::UnsupportedOperator(op) => format!("{op} loses the grain"),
            UnknownReason::PlanError(r) => format!("{} could not be planned", display(r)),
            UnknownReason::PythonModel(r) => format!("{} is a Python model", display(r)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrainStatus {
    Known,
    /// Some keys are known, but an input or operator was unknown.
    Partial(UnknownReason),
    Unknown(UnknownReason),
}

impl GrainStatus {
    pub fn reason(&self) -> Option<&UnknownReason> {
        match self {
            GrainStatus::Known => None,
            GrainStatus::Partial(r) | GrainStatus::Unknown(r) => Some(r),
        }
    }
    pub fn is_known(&self) -> bool {
        matches!(self, GrainStatus::Known)
    }
    /// The first non-Known status among inputs.
    pub fn combine<'a>(it: impl IntoIterator<Item = &'a GrainStatus>) -> GrainStatus {
        it.into_iter()
            .find(|s| !s.is_known())
            .cloned()
            .unwrap_or(GrainStatus::Known)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Cardinality {
    OneToOne,
    ManyToOne,
    OneToMany,
    ManyToMany,
}

impl Cardinality {
    pub fn label(self) -> &'static str {
        match self {
            Cardinality::OneToOne => "one-to-one",
            Cardinality::ManyToOne => "many-to-one",
            Cardinality::OneToMany => "one-to-many",
            Cardinality::ManyToMany => "many-to-many",
        }
    }
}

/// A join where rows of one side repeat, recorded so a fan-out found downstream can
/// point back at it, even across models.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JoinSite {
    pub relation: RelationId,
    /// In that relation's compiled SQL.
    pub span: Option<SqlSpan>,
    pub cardinality: Cardinality,
    /// The side whose rows multiply this column's rows, e.g. "shop.order_items".
    pub repeated_by: String,
}

impl JoinSite {
    pub fn message(&self) -> String {
        format!(
            "{}: rows repeat per row of {}",
            self.cardinality.label(),
            self.repeated_by
        )
    }
}

#[derive(Clone, Debug)]
pub struct RelationFacts {
    pub width: usize,
    /// Candidate keys. Empty means no key is known: rows are a bag.
    pub keys: Vec<Key>,
    /// Functional dependencies lhs -> rhs that hold in every row.
    pub fds: Vec<Fd>,
    /// Columns proven non-null (not merely typed non-null).
    pub not_null: ColSet,
    pub status: GrainStatus,
    /// Per column: the nearest join below this node where that column's rows repeat.
    pub repeats: Vec<Option<Arc<JoinSite>>>,
}

impl RelationFacts {
    pub fn unknown(width: usize, reason: UnknownReason) -> Self {
        RelationFacts {
            width,
            keys: vec![],
            fds: vec![],
            not_null: ColSet::new(),
            status: GrainStatus::Unknown(reason),
            repeats: vec![None; width],
        }
    }

    pub fn all(&self) -> ColSet {
        (0..self.width).collect()
    }

    /// Every column determined by `x` under this node's dependencies and keys.
    pub fn closure(&self, x: &ColSet) -> ColSet {
        closure(self.width, &self.keys, &self.fds, x)
    }

    /// True if `x` determines some key, so no two rows agree on `x`.
    pub fn is_superkey(&self, x: &ColSet) -> bool {
        let c = self.closure(x);
        self.keys.iter().any(|k| k.cols.is_subset(&c))
    }

    /// Shrink `x` to a minimal subset whose closure still covers `target`.
    /// Columns listed earlier in `drop_first` are tried for removal first.
    pub fn minimize(&self, x: &ColSet, target: &ColSet, drop_first: &[usize]) -> ColSet {
        minimize(self.width, &self.keys, &self.fds, x, target, drop_first)
    }

    /// The key whose evidence explains that `x` is a superkey.
    pub fn key_covered_by(&self, x: &ColSet) -> Option<&Key> {
        let c = self.closure(x);
        self.keys.iter().find(|k| k.cols.is_subset(&c))
    }

    /// Keys as dependencies on this node's columns: key -> all columns.
    pub fn key_fds(&self) -> Vec<Fd> {
        self.keys
            .iter()
            .map(|k| Fd {
                lhs: k.cols.clone(),
                rhs: self.all(),
                why: k.why,
                entity: true,
            })
            .collect()
    }

    /// Entity determinants: every key and every entity dependency's left side.
    pub fn entity_sets(&self) -> Vec<ColSet> {
        let mut v: Vec<ColSet> = self.keys.iter().map(|k| k.cols.clone()).collect();
        v.extend(
            self.fds
                .iter()
                .filter(|f| f.entity && !f.lhs.is_empty())
                .map(|f| f.lhs.clone()),
        );
        v.sort();
        v.dedup();
        v
    }

    /// Minimal entity determinants of column `c` (sets not containing `c` that
    /// determine it), smallest first.
    pub fn determinants(&self, c: usize) -> Vec<ColSet> {
        let mut cands: Vec<ColSet> = self
            .entity_sets()
            .into_iter()
            .filter(|l| !l.contains(&c) && !l.is_empty() && self.closure(l).contains(&c))
            .map(|l| self.minimize(&l, &ColSet::from([c]), &[]))
            .filter(|l| !l.is_empty())
            .collect();
        cands.sort();
        cands.dedup();
        let snapshot = cands.clone();
        cands.retain(|s| !snapshot.iter().any(|t| t != s && t.is_subset(s)));
        // Nearest entity first: the determinant that determines the least. With
        // order_id -> customer_id -> name, name belongs to customer_id.
        cands.sort_by_key(|s| (self.closure(s).len(), s.len(), s.clone()));
        cands
    }

    /// A column's native grain: the entity it really belongs to.
    pub fn native_grain(&self, c: usize) -> Option<ColSet> {
        if self.closure(&ColSet::new()).contains(&c) {
            return Some(ColSet::new());
        }
        if let Some(d) = self.determinants(c).into_iter().next() {
            return Some(d);
        }
        let mut owning: Vec<ColSet> = self
            .entity_sets()
            .into_iter()
            .filter(|l| l.contains(&c))
            .collect();
        owning.sort_by_key(|s| (s.len(), s.clone()));
        owning.into_iter().next()
    }

    /// Remove dominated keys and dependencies; reduce each key to a minimal set.
    pub fn normalize(&mut self) {
        let width = self.width;
        let mut keys: Vec<Key> = Vec::new();
        let snapshot_keys = self.keys.clone();
        for k in snapshot_keys {
            // Drop columns the rest of the key already determines (constants, equalities).
            let cols = minimize(width, &self.keys, &self.fds, &k.cols, &k.cols, &[]);
            if !keys.iter().any(|e| e.cols.is_subset(&cols)) {
                keys.retain(|e| !cols.is_subset(&e.cols));
                keys.push(Key { cols, why: k.why });
            }
        }
        keys.sort_by_key(|k| (k.cols.len(), k.cols.clone()));
        keys.truncate(MAX_KEYS);
        self.keys = keys;

        let mut fds: Vec<Fd> = Vec::new();
        for mut f in std::mem::take(&mut self.fds) {
            f.rhs.retain(|c| !f.lhs.contains(c));
            if f.rhs.is_empty() {
                continue;
            }
            if let Some(e) = fds
                .iter_mut()
                .find(|e| e.lhs == f.lhs && e.entity == f.entity)
            {
                e.rhs.extend(f.rhs);
            } else {
                fds.push(f);
            }
        }
        fds.truncate(MAX_FDS);
        self.fds = fds;
        if self.repeats.len() < width {
            self.repeats.resize(width, None);
        }
    }

    /// Set status from the inputs' combined status, given the keys now known.
    pub fn settle_status(&mut self, inputs: GrainStatus) {
        self.status = match inputs {
            GrainStatus::Known => GrainStatus::Known,
            GrainStatus::Partial(r) | GrainStatus::Unknown(r) => {
                if self.keys.is_empty() {
                    GrainStatus::Unknown(r)
                } else {
                    GrainStatus::Partial(r)
                }
            }
        };
    }
}

pub fn closure(width: usize, keys: &[Key], fds: &[Fd], x: &ColSet) -> ColSet {
    let mut c = x.clone();
    loop {
        let before = c.len();
        for f in fds {
            if f.lhs.is_subset(&c) {
                c.extend(f.rhs.iter().copied());
            }
        }
        if keys.iter().any(|k| k.cols.is_subset(&c)) {
            return (0..width).collect();
        }
        if c.len() == before {
            return c;
        }
    }
}

pub fn minimize(
    width: usize,
    keys: &[Key],
    fds: &[Fd],
    x: &ColSet,
    target: &ColSet,
    drop_first: &[usize],
) -> ColSet {
    let mut cur = x.clone();
    let mut order: Vec<usize> = drop_first
        .iter()
        .copied()
        .filter(|c| cur.contains(c))
        .collect();
    order.extend(
        cur.iter()
            .rev()
            .copied()
            .filter(|c| !drop_first.contains(c)),
    );
    for c in order {
        cur.remove(&c);
        if !target.is_subset(&closure(width, keys, fds, &cur)) {
            cur.insert(c);
        }
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[usize]) -> ColSet {
        v.iter().copied().collect()
    }

    #[test]
    fn closure_and_superkey() {
        // order_id(0) line_id(1) shipping_fee(2) line_amount(3)
        let f = RelationFacts {
            width: 4,
            keys: vec![Key {
                cols: set(&[0, 1]),
                why: EvidenceId(0),
            }],
            fds: vec![Fd {
                lhs: set(&[0]),
                rhs: set(&[2]),
                why: EvidenceId(0),
                entity: true,
            }],
            not_null: ColSet::new(),
            status: GrainStatus::Known,
            repeats: vec![None; 4],
        };
        assert_eq!(f.closure(&set(&[0])), set(&[0, 2]));
        assert!(f.is_superkey(&set(&[0, 1])));
        assert!(!f.is_superkey(&set(&[0, 2])));
        assert_eq!(f.native_grain(2), Some(set(&[0])));
        assert_eq!(f.native_grain(3), Some(set(&[0, 1])));
        assert_eq!(f.native_grain(1), Some(set(&[0, 1])));
        assert_eq!(f.native_grain(0), Some(set(&[0])));
    }
}
