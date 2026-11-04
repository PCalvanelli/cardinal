//! Facts attached to each plan node, and the two primitives every rule and check is
//! phrased in: attribute closure and the superkey test.

use crate::evidence::EvidenceId;
use cardinal_dbt::RelationId;
use cardinal_sql::SqlSpan;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Column positions in a plan node's schema.
pub type ColSet = BTreeSet<usize>;

pub const MAX_KEYS: usize = 8;
pub const MAX_FDS: usize = 512;
pub const MAX_INDS: usize = 48;

/// An inclusion dependency between a plan node and a named relation: pairs of (node
/// column, relation column name), sorted by node column. Comparisons are null-safe
/// (IS NOT DISTINCT FROM), which keeps every rule sound under projection.
///
/// As a `covers` fact: every row r of the relation has a node row t with
/// t[cols] = r[names], so no row of the relation was lost.
///
/// As a `within` fact: every node row t whose paired columns are not all null has a row
/// r of the relation with r[names] = t[cols], so the node's values come from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ind {
    pub relation: RelationId,
    pub pairs: Vec<(usize, String)>,
    pub why: EvidenceId,
}

impl Ind {
    /// Move to new column positions; pairs whose column has no new position are dropped
    /// (both kinds of fact survive projection). None when no pair is left.
    pub fn remap(&self, f: impl Fn(usize) -> Option<usize>) -> Option<Ind> {
        let mut pairs: Vec<(usize, String)> = self
            .pairs
            .iter()
            .filter_map(|(c, n)| f(*c).map(|c| (c, n.clone())))
            .collect();
        pairs.sort();
        pairs.dedup();
        (!pairs.is_empty()).then(|| Ind {
            relation: self.relation.clone(),
            pairs,
            why: self.why,
        })
    }

    /// The node column paired with the relation's column `name`.
    pub fn col_for(&self, name: &str) -> Option<usize> {
        self.pairs.iter().find(|(_, n)| n == name).map(|(c, _)| *c)
    }

    pub fn has_pair(&self, col: usize, name: &str) -> bool {
        self.pairs.iter().any(|(c, n)| *c == col && n == name)
    }
}

/// Where rows of a relation can be lost: a filter, an inner join, a LIMIT.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LossSite {
    /// The model whose SQL drops the rows.
    pub model: RelationId,
    /// In that model's compiled SQL.
    pub span: Option<SqlSpan>,
    /// "filter `status = 'complete'`", "inner join on customer_id".
    pub what: String,
    /// Why the analysis could not show that no rows are lost, when there is more to say.
    pub because: Option<String>,
}

/// Rows of `relation` may be lost at `site`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RowLoss {
    pub relation: RelationId,
    pub site: LossSite,
}

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
            UnknownReason::SourceWithoutKey(r) => format!("{} has no known key", display(r)),
            UnknownReason::UnsupportedOperator(op) => format!("{op} loses track of the key"),
            UnknownReason::PlanError(r) => format!("Cardinal couldn't read {}", display(r)),
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

/// Where a column was computed as `count(distinct …)`, recorded so a later sum of it can
/// point back here, even across models.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DistinctSite {
    pub relation: RelationId,
    /// In that relation's compiled SQL.
    pub span: Option<SqlSpan>,
    /// The columns whose distinct values are counted, e.g. ["user_id"].
    pub counted: Vec<String>,
    /// The GROUP BY at that site, e.g. ["event_date"].
    pub per: Vec<String>,
    /// The GROUP BY columns the counted columns do not determine: a value can be counted
    /// once for each of their values.
    pub across: Vec<String>,
}

/// What a computed measure is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Computed {
    /// avg(x)
    Average,
    /// A division of two aggregates, such as sum(a) / sum(b).
    Ratio,
    /// median or a percentile.
    Percentile,
    /// sum(x) over (order by t): a running or rolling total.
    RunningTotal,
}

impl Computed {
    /// "average", "running total".
    pub fn label(self) -> &'static str {
        match self {
            Computed::Average => "average",
            Computed::Ratio => "ratio",
            Computed::Percentile => "percentile",
            Computed::RunningTotal => "running total",
        }
    }
    /// "an average", "a ratio".
    pub fn noun(self) -> &'static str {
        match self {
            Computed::Average => "an average",
            Computed::Ratio => "a ratio",
            Computed::Percentile => "a percentile",
            Computed::RunningTotal => "a running total",
        }
    }
    /// "averages", "ratios".
    pub fn plural(self) -> &'static str {
        match self {
            Computed::Average => "averages",
            Computed::Ratio => "ratios",
            Computed::Percentile => "percentiles",
            Computed::RunningTotal => "running totals",
        }
    }
}

/// Where a measure's kind comes from: the SQL that computed it, or a declaration.
/// Recorded so a later sum of it can point back here, even across models.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeasureSite {
    /// `count(distinct …)`.
    Distinct(DistinctSite),
    Computed {
        relation: RelationId,
        /// In that relation's compiled SQL.
        span: Option<SqlSpan>,
        what: Computed,
        /// A running total's ORDER BY columns.
        over: Vec<String>,
    },
    /// `meta.cardinal.measure` on a column.
    Declared {
        relation: RelationId,
        column: String,
        /// A stock's `over` columns; empty for a non-additive column.
        over: Vec<String>,
    },
}

impl MeasureSite {
    pub fn relation(&self) -> &RelationId {
        match self {
            MeasureSite::Distinct(d) => &d.relation,
            MeasureSite::Computed { relation, .. } | MeasureSite::Declared { relation, .. } => {
                relation
            }
        }
    }
    pub fn span(&self) -> Option<SqlSpan> {
        match self {
            MeasureSite::Distinct(d) => d.span,
            MeasureSite::Computed { span, .. } => *span,
            MeasureSite::Declared { .. } => None,
        }
    }
}

/// How a column may be added up.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MeasureKind {
    /// Summing it over more than one row is wrong, except over rows that differ on
    /// `agree_on`. For a distinct count, two rows that count the same value always agree
    /// on `agree_on`, so summing over rows that differ there is exact. Empty for
    /// averages, ratios and percentiles.
    NonAdditive { agree_on: ColSet },
    /// A stock, such as a balance or a running total: summing rows that differ on
    /// `over` is wrong. A sum is safe when every row it adds agrees on `over`. None when
    /// the `over` columns were dropped while they still varied: rows can come from
    /// different `over` values and nothing says which (see `MeasureSite` for their names).
    Stock { over: Option<ColSet> },
}

/// Column `col` is a measure that can't be freely added up (see `MeasureKind`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Measure {
    pub col: usize,
    pub kind: MeasureKind,
    pub site: Arc<MeasureSite>,
}

impl Measure {
    /// Move to new column positions; None if the column or a column of its kind has none.
    pub fn remap(&self, f: impl Fn(usize) -> Option<usize>) -> Option<Measure> {
        let set = |s: &ColSet| s.iter().map(|c| f(*c)).collect::<Option<ColSet>>();
        Some(Measure {
            col: f(self.col)?,
            kind: match &self.kind {
                MeasureKind::NonAdditive { agree_on } => MeasureKind::NonAdditive {
                    agree_on: set(agree_on)?,
                },
                MeasureKind::Stock { over: Some(o) } => MeasureKind::Stock {
                    over: Some(set(o)?),
                },
                MeasureKind::Stock { over: None } => MeasureKind::Stock { over: None },
            },
            site: self.site.clone(),
        })
    }

    /// The distinct count this measure is, if it is one.
    pub fn distinct(&self) -> Option<&DistinctSite> {
        match &*self.site {
            MeasureSite::Distinct(d) => Some(d),
            _ => None,
        }
    }

    /// What the SQL computed it as, if it was computed.
    pub fn computed(&self) -> Option<Computed> {
        match &*self.site {
            MeasureSite::Computed { what, .. } => Some(*what),
            _ => None,
        }
    }
}

/// Where a column's unit was declared: `meta.cardinal.unit`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnitSite {
    pub relation: RelationId,
    /// The declared column, e.g. "amount".
    pub column: String,
    /// The column holding its unit there, e.g. "currency".
    pub unit: String,
}

/// Column `col` is an amount whose unit varies by row. `unit` holds columns that
/// determine the unit (usually the unit column itself), or None when the unit column
/// was dropped while it still varied: the rows can be in different units and nothing
/// says which.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unit {
    pub col: usize,
    pub unit: Option<ColSet>,
    pub site: Arc<UnitSite>,
}

pub const MAX_MEASURES: usize = 32;

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
    /// Relations every row of which appears in this node.
    pub covers: Vec<Ind>,
    /// Relations this node's values come from.
    pub within: Vec<Ind>,
    /// Columns computed by row_number, rank or dense_rank, with their partition: each
    /// partition has a row where the column is 1.
    pub ranks: Vec<(usize, ColSet)>,
    /// Evidence for non-null columns that rest on a declaration. A column in `not_null`
    /// with no entry here is proven by the SQL.
    pub nn_why: BTreeMap<usize, EvidenceId>,
    /// Columns that can't be freely added up (see `Measure`).
    pub measures: Vec<Measure>,
    /// Amounts whose unit varies by row (see `Unit`).
    pub units: Vec<Unit>,
    /// Columns holding the result of a counting or summing aggregate (sum, count, avg),
    /// grouped or windowed. Dividing two of them makes a ratio.
    pub aggregated: ColSet,
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
            covers: vec![],
            within: vec![],
            ranks: vec![],
            nn_why: BTreeMap::new(),
            measures: vec![],
            units: vec![],
            aggregated: ColSet::new(),
        }
    }

    /// Like `closure`, also returning the evidence of every dependency and key that
    /// fired. A superset of what the result needs, so assumptions are never missed.
    pub fn closure_why(&self, x: &ColSet) -> (ColSet, Vec<EvidenceId>) {
        let mut c = x.clone();
        let mut why = Vec::new();
        loop {
            let before = c.len();
            for f in &self.fds {
                if f.lhs.is_subset(&c) && !f.rhs.is_subset(&c) {
                    c.extend(f.rhs.iter().copied());
                    why.push(f.why);
                }
            }
            if let Some(k) = self.keys.iter().find(|k| k.cols.is_subset(&c)) {
                why.push(k.why);
                return (self.all(), why);
            }
            if c.len() == before {
                return (c, why);
            }
        }
    }

    /// The evidence that `x` is a superkey, or None if it is not one.
    pub fn superkey_why(&self, x: &ColSet) -> Option<Vec<EvidenceId>> {
        let (c, why) = self.closure_why(x);
        self.keys
            .iter()
            .any(|k| k.cols.is_subset(&c))
            .then_some(why)
    }

    /// Evidence for non-null columns `cols` that rest on declarations.
    pub fn not_null_why(&self, cols: &ColSet) -> Vec<EvidenceId> {
        cols.iter()
            .filter_map(|c| self.nn_why.get(c).copied())
            .collect()
    }

    /// Mark `c` non-null as proven by the SQL.
    pub fn prove_not_null(&mut self, c: usize) {
        self.not_null.insert(c);
        self.nn_why.remove(&c);
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
        self.covers = dedup_inds(std::mem::take(&mut self.covers));
        self.within = dedup_inds(std::mem::take(&mut self.within));
        let nn = self.not_null.clone();
        self.nn_why.retain(|c, _| nn.contains(c));
        let mut measures: Vec<Measure> = Vec::new();
        for m in std::mem::take(&mut self.measures) {
            if m.col < width && !measures.iter().any(|e| e.col == m.col) {
                measures.push(m);
            }
        }
        measures.truncate(MAX_MEASURES);
        self.measures = measures;
        let mut units: Vec<Unit> = Vec::new();
        for u in std::mem::take(&mut self.units) {
            if u.col < width && !units.iter().any(|e| e.col == u.col) {
                units.push(u);
            }
        }
        units.truncate(MAX_MEASURES);
        self.units = units;
        self.aggregated.retain(|c| *c < width);
    }

    /// Columns of this node that a copy of `kept` determines in place of `target`: `target`
    /// itself when every column is kept, else a minimal kept set that determines it.
    /// None when `kept` does not determine `target`.
    fn stand_in(&self, target: &ColSet, kept: &ColSet) -> Option<ColSet> {
        if target.is_subset(kept) {
            return Some(target.clone());
        }
        target
            .is_subset(&self.closure(kept))
            .then(|| self.minimize(kept, target, &[]))
    }

    /// This node's measures and units on the outputs of a projection, where `to` maps an
    /// input column to its first plain copy. A computed column carries nothing. A stock
    /// or unit whose columns are dropped moves to kept columns that determine them; a
    /// constant one is dropped, since every row then agrees on it; otherwise it is kept
    /// with its `over` or unit unknown, so a later sum of it is still caught.
    pub fn carried(&self, to: &dyn Fn(usize) -> Option<usize>) -> (Vec<Measure>, Vec<Unit>) {
        let kept: ColSet = (0..self.width).filter(|c| to(*c).is_some()).collect();
        let constant = self.closure(&ColSet::new());
        let map = |s: &ColSet| -> ColSet { s.iter().filter_map(|c| to(*c)).collect() };
        let measures = self
            .measures
            .iter()
            .filter_map(|m| match &m.kind {
                MeasureKind::NonAdditive { .. } => m.remap(to),
                MeasureKind::Stock { over } => {
                    let col = to(m.col)?;
                    let over = match over {
                        None => None,
                        Some(o) if o.is_subset(&constant) => return None,
                        Some(o) => self.stand_in(o, &kept).map(|o| map(&o)),
                    };
                    Some(Measure {
                        col,
                        kind: MeasureKind::Stock { over },
                        site: m.site.clone(),
                    })
                }
            })
            .collect();
        let units = self
            .units
            .iter()
            .filter_map(|u| {
                let col = to(u.col)?;
                let unit = match &u.unit {
                    None => None,
                    Some(x) if x.is_subset(&constant) => return None,
                    Some(x) => self.stand_in(x, &kept).map(|x| map(&x)),
                };
                Some(Unit {
                    col,
                    unit,
                    site: u.site.clone(),
                })
            })
            .collect();
        (measures, units)
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

/// Drop inclusion facts implied by another for the same relation with the same
/// evidence (a subset of its pairs), and cap the count.
pub fn dedup_inds(mut v: Vec<Ind>) -> Vec<Ind> {
    v.sort_by(|a, b| b.pairs.len().cmp(&a.pairs.len()));
    let mut out: Vec<Ind> = Vec::new();
    for i in v {
        if !out.iter().any(|o| {
            o.relation == i.relation
                && (o.why == i.why || o.pairs == i.pairs)
                && i.pairs.iter().all(|p| o.pairs.contains(p))
        }) {
            out.push(i);
        }
    }
    out.truncate(MAX_INDS);
    out
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
            covers: vec![],
            within: vec![],
            ranks: vec![],
            nn_why: BTreeMap::new(),
            measures: vec![],
            units: vec![],
            aggregated: ColSet::new(),
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
