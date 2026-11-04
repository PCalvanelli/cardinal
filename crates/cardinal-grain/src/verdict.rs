//! Verdicts: whether a model contains each row of a relation exactly once.
//!
//! "Exactly once" splits into two halves, each proven or not on its own:
//!
//! - at least once: a covers fact maps the relation's key onto columns of the model, so
//!   every row of the relation reached it (nothing was filtered or joined away);
//! - at most once: those columns are a key of the model, so no row was repeated.
//!
//! Both need the relation's key to identify its rows, which needs it unique and
//! non-null. When the model aggregates and its output no longer carries the key, the
//! claim is checked on the aggregate's input instead: every row was aggregated once.

use crate::analyze::AnalyzedModel;
use crate::evidence::EvidenceId;
use crate::facts::{ColSet, Ind, JoinSite, LossSite, RelationFacts};
use crate::summary::RelationSummary;
use cardinal_dbt::{Assumption, RelationId};
use cardinal_sql::datafusion_common::tree_node::{TreeNode, TreeNodeRecursion};
use cardinal_sql::datafusion_expr::LogicalPlan;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Site {
    /// The model's rows.
    Output,
    /// The rows its top-level aggregate reads.
    AggregateInput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Reason {
    /// The relation has no known key, so its rows cannot be told apart.
    NoKey,
    /// The relation's key columns may be null, so they do not identify every row.
    KeyNullable { cols: Vec<String> },
    /// The model does not read the relation, directly or through other models.
    NotRead,
    /// The model reads the relation but does not carry its key columns.
    NotCarried { key: Vec<String> },
    /// Rows may be dropped here.
    Loss { site: LossSite },
    /// Rows repeat per row of another relation, from this join.
    Repeats { site: JoinSite },
    /// The columns carrying the key are not a key of the model.
    NotUnique { cols: Vec<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Half {
    Proven,
    NotProven { reasons: Vec<Reason> },
}

impl Half {
    pub fn is_proven(&self) -> bool {
        matches!(self, Half::Proven)
    }
    pub fn reasons(&self) -> &[Reason] {
        match self {
            Half::Proven => &[],
            Half::NotProven { reasons } => reasons,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub relation: RelationId,
    pub display: String,
    /// Declared with meta.cardinal.exactly_once, rather than inferred for a parent.
    pub declared: bool,
    pub site: Site,
    /// The relation's key, and the model columns that carry it (empty when none do).
    pub key: Vec<String>,
    pub cols: Vec<String>,
    pub at_least_once: Half,
    pub at_most_once: Half,
    /// What the proven halves rest on, including the relation's key.
    pub assumptions: Vec<Assumption>,
}

impl Verdict {
    pub fn exactly_once(&self) -> bool {
        self.at_least_once.is_proven() && self.at_most_once.is_proven()
    }

    /// "exactly once", "at most once", "at least once" or "not proven".
    pub fn label(&self) -> &'static str {
        match (
            self.at_least_once.is_proven(),
            self.at_most_once.is_proven(),
        ) {
            (true, true) => "exactly once",
            (false, true) => "at most once",
            (true, false) => "at least once",
            (false, false) => "not proven",
        }
    }

    /// The label with what it leaves open, e.g. "at most once (rows may be dropped)".
    pub fn summary(&self) -> String {
        match (
            self.at_least_once.is_proven(),
            self.at_most_once.is_proven(),
        ) {
            (true, true) => "exactly once".into(),
            (false, true) => "at most once (rows may be dropped)".into(),
            (true, false) => "at least once (rows may repeat)".into(),
            (false, false) => "not proven (rows may be dropped or repeat)".into(),
        }
    }
}

/// The node a top-level aggregate reads, below projections, sorts, limits and HAVING.
fn aggregate_input(plan: &LogicalPlan) -> Option<&LogicalPlan> {
    match plan {
        LogicalPlan::Aggregate(a) => Some(&a.input),
        LogicalPlan::Projection(p) => aggregate_input(&p.input),
        LogicalPlan::Sort(s) => aggregate_input(&s.input),
        LogicalPlan::SubqueryAlias(s) => aggregate_input(&s.input),
        LogicalPlan::Limit(l) => aggregate_input(&l.input),
        LogicalPlan::Filter(f) => aggregate_input(&f.input),
        _ => None,
    }
}

/// How `facts` carries `key` of `rel`: 2 when one covers fact maps every key column (so
/// at least once can be proven), 1 when only a within fact does, 0 when neither.
fn carry_rank(facts: &RelationFacts, rel: &RelationId, key: &[String]) -> u8 {
    let maps = |inds: &[Ind]| {
        inds.iter()
            .any(|i| &i.relation == rel && key.iter().all(|k| i.col_for(k).is_some()))
    };
    if maps(&facts.covers) {
        2
    } else if maps(&facts.within) {
        1
    } else {
        0
    }
}

/// Check that `model` contains each row of `target` exactly once.
pub fn verdict(model: &AnalyzedModel, target: &RelationSummary, declared: bool) -> Verdict {
    let rel = &target.relation;
    let mut v = Verdict {
        relation: rel.clone(),
        display: target.display.clone(),
        declared,
        site: Site::Output,
        key: vec![],
        cols: vec![],
        at_least_once: Half::Proven,
        at_most_once: Half::Proven,
        assumptions: vec![],
    };
    let both = |v: &mut Verdict, r: Reason| {
        v.at_least_once = Half::NotProven {
            reasons: vec![r.clone()],
        };
        v.at_most_once = Half::NotProven { reasons: vec![r] };
    };
    // The relation's key: the first one whose columns are all non-null.
    let names = |cols: &[usize]| -> Vec<String> {
        cols.iter()
            .map(|c| target.columns[*c].name.clone())
            .collect()
    };
    let Some(first) = target.keys.first() else {
        both(&mut v, Reason::NoKey);
        return v;
    };
    // Among the non-null keys, prefer one the model still carries: a projection may drop
    // one key's columns and keep another's. Look at the output first, then at the
    // aggregate's input when the output carries none; at each, prefer a key carried by a
    // covers fact (which can prove at least once) over one carried only by a within fact.
    let candidates: Vec<_> = target
        .keys
        .iter()
        .filter(|k| k.cols.iter().all(|c| target.not_null.contains(c)))
        .map(|k| (k, names(&k.cols)))
        .collect();
    let out = model.output();
    let agg = aggregate_input(&model.plan).and_then(|i| model.facts(i).map(|f| (i, f)));
    let sites: [Option<(Site, &LogicalPlan, &RelationFacts)>; 2] = [
        Some((Site::Output, &model.plan, out)),
        agg.map(|(i, f)| (Site::AggregateInput, i, &**f)),
    ];
    let mut chosen = None;
    for (site, node, f) in sites.into_iter().flatten() {
        let best = candidates
            .iter()
            .enumerate()
            .map(|(i, (_, n))| (carry_rank(f, rel, n), i))
            .filter(|(r, _)| *r > 0)
            .max_by_key(|(r, i)| (*r, std::cmp::Reverse(*i)));
        if let Some((rank, i)) = best {
            chosen = Some((i, site, node, f, rank));
            break;
        }
    }
    let (key_index, site, node, facts, rank) =
        chosen.unwrap_or((0, Site::Output, &model.plan, out, 0));
    let Some((key, key_names)) = candidates.get(key_index) else {
        v.key = names(&first.cols);
        let cols = first
            .cols
            .iter()
            .filter(|c| !target.not_null.contains(c))
            .map(|c| target.columns[*c].name.clone())
            .collect();
        both(&mut v, Reason::KeyNullable { cols });
        return v;
    };
    v.key = key_names.clone();
    v.site = site;
    let mut assumptions: BTreeSet<Assumption> = key.assumptions.iter().cloned().collect();
    for c in &key.cols {
        if let Some(a) = target.not_null_why.get(c) {
            assumptions.extend(a.iter().cloned());
        }
    }
    let schema = node.schema();
    let col_names = |cols: &ColSet| -> Vec<String> {
        cols.iter()
            .map(|c| schema.field(*c).name().clone())
            .collect()
    };
    let map_key = |pairs_for: &dyn Fn(&str) -> Option<usize>| -> Option<ColSet> {
        v.key.iter().map(|k| pairs_for(k)).collect()
    };

    // At least once.
    let covering = facts
        .covers
        .iter()
        .filter(|c| &c.relation == rel)
        .find_map(|c| map_key(&|k| c.col_for(k)).map(|cols| (cols, c.why)));
    let mut whys: Vec<EvidenceId> = Vec::new();
    match &covering {
        Some((_, why)) => whys.push(*why),
        None => {
            let mut reasons: Vec<Reason> = model
                .losses
                .iter()
                .filter(|l| &l.relation == rel)
                .map(|l| Reason::Loss {
                    site: l.site.clone(),
                })
                .collect();
            if reasons.is_empty() {
                reasons.push(if rank > 0 || reads(model, rel) {
                    Reason::NotCarried { key: v.key.clone() }
                } else {
                    Reason::NotRead
                });
            }
            v.at_least_once = Half::NotProven { reasons };
        }
    }

    // At most once: the columns carrying the key must be a key of the node.
    let carrying = covering.as_ref().map(|(c, _)| c.clone()).or_else(|| {
        facts
            .within
            .iter()
            .filter(|w| &w.relation == rel)
            .find_map(|w| map_key(&|k| w.col_for(k)))
    });
    match carrying {
        None => {
            let r = if reads(model, rel) {
                Reason::NotCarried { key: v.key.clone() }
            } else {
                Reason::NotRead
            };
            v.at_most_once = Half::NotProven { reasons: vec![r] };
        }
        Some(cols) => {
            v.cols = col_names(&cols);
            // Rows where these columns are null carry no row of the relation, whose key is
            // non-null, so a key among non-null values is enough (an outer join's).
            match facts.null_exempt_superkey_why(&cols) {
                Some(w) => whys.extend(w),
                None => {
                    let mut reasons: Vec<Reason> = Vec::new();
                    for c in &cols {
                        if let Some(site) = facts.repeats.get(*c).cloned().flatten() {
                            let r = Reason::Repeats {
                                site: (*site).clone(),
                            };
                            if !reasons.contains(&r) {
                                reasons.push(r);
                            }
                        }
                    }
                    if reasons.is_empty() {
                        reasons.push(Reason::NotUnique {
                            cols: v.cols.clone(),
                        });
                    }
                    v.at_most_once = Half::NotProven { reasons };
                }
            }
        }
    }
    assumptions.extend(model.arena.assumptions_of(&whys));
    v.assumptions = assumptions.into_iter().collect();
    v
}

/// Whether any node of the model has an inclusion fact for `rel`, or lost its rows.
fn reads(model: &AnalyzedModel, rel: &RelationId) -> bool {
    model.losses.iter().any(|l| &l.relation == rel) || {
        let mut found = false;
        let _ = model.plan.apply(|n| {
            if let Some(f) = model.facts(n)
                && f.covers.iter().chain(&f.within).any(|i| &i.relation == rel)
            {
                found = true;
                return Ok(TreeNodeRecursion::Stop);
            }
            Ok(TreeNodeRecursion::Continue)
        });
        found
    }
}

impl Verdict {
    /// A verdict for a relation the model does not read at all.
    pub fn not_read(relation: RelationId, display: String, declared: bool) -> Verdict {
        let half = Half::NotProven {
            reasons: vec![Reason::NotRead],
        };
        Verdict {
            relation,
            display,
            declared,
            site: Site::Output,
            key: vec![],
            cols: vec![],
            at_least_once: half.clone(),
            at_most_once: half,
            assumptions: vec![],
        }
    }
}

impl Reason {
    /// One line for a person, e.g. "inner join on customer_id in orders_enriched may drop
    /// rows: nothing shows that every orders.customer_id appears in customers.customer_id".
    pub fn text(&self, of: &str, display: &dyn Fn(&RelationId) -> String) -> String {
        let tuple = |v: &[String]| format!("({})", v.join(", "));
        match self {
            Reason::NoKey => format!("{of} has no known key, so its rows can't be told apart"),
            Reason::KeyNullable { cols } => format!(
                "{of}.{} can be null, so its key doesn't identify every row",
                cols.join(", ")
            ),
            Reason::NotRead => format!("the model doesn't read {of}"),
            Reason::NotCarried { key } => {
                format!("the model doesn't keep {of}'s key {}", tuple(key))
            }
            Reason::Loss { site } => {
                let mut s = format!("{} in {} may drop rows", site.what, display(&site.model));
                if let Some(b) = &site.because {
                    s.push_str(": ");
                    s.push_str(b);
                }
                s
            }
            Reason::Repeats { site } => {
                format!("{} in {}", site.message(), display(&site.relation))
            }
            Reason::NotUnique { cols } => {
                format!("{} doesn't identify one row of the model", tuple(cols))
            }
        }
    }
}
