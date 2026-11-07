//! UNION: what holds on every branch, and the literal columns that tell branches apart.

use super::{Analyzer, literal_of};
use crate::evidence::{Evidence, EvidenceId};
use crate::facts::{ColSet, GrainStatus, Ind, Key, RelationFacts, UnknownReason};
use cardinal_sql::datafusion_common::ScalarValue;
use cardinal_sql::datafusion_expr::LogicalPlan;
use cardinal_sql::datafusion_expr::logical_plan::{Distinct, Union};
use std::collections::BTreeMap;
use std::sync::Arc;

impl Analyzer<'_> {
    pub(super) fn union(&mut self, u: &Union) -> RelationFacts {
        let branches: Vec<Arc<RelationFacts>> = u.inputs.iter().map(|i| self.visit(i)).collect();
        let width = u.schema.fields().len();
        let status = GrainStatus::combine(branches.iter().map(|b| &b.status));
        let not_null: ColSet = (0..width)
            .filter(|c| branches.iter().all(|b| b.not_null.contains(c)))
            .collect();
        let mut nn_why = BTreeMap::new();
        for c in &not_null {
            let from: Vec<EvidenceId> = branches
                .iter()
                .filter_map(|b| b.nn_why.get(c).copied())
                .collect();
            if !from.is_empty() {
                nn_why.insert(*c, self.arena.push(Evidence::Derived { from }));
            }
        }
        // Every branch's rows are in the result.
        let covers: Vec<Ind> = branches.iter().flat_map(|b| b.covers.clone()).collect();
        // A value comes from a relation only if it does in every branch.
        let mut within = Vec::new();
        for w in &branches[0].within {
            let mut pairs = w.pairs.clone();
            let mut from = vec![w.why];
            for b in &branches[1..] {
                let best = b
                    .within
                    .iter()
                    .filter(|o| o.relation == w.relation)
                    .map(|o| {
                        let common: Vec<(usize, String)> = pairs
                            .iter()
                            .filter(|p| o.pairs.contains(p))
                            .cloned()
                            .collect();
                        (common, o.why)
                    })
                    .max_by_key(|(c, _)| c.len());
                match best {
                    Some((common, why)) => {
                        pairs = common;
                        from.push(why);
                    }
                    None => pairs.clear(),
                }
            }
            if !pairs.is_empty() {
                within.push(Ind {
                    relation: w.relation.clone(),
                    pairs,
                    why: self.arena.push(Evidence::Derived { from }),
                });
            }
        }
        let mut f = RelationFacts {
            not_null,
            status: GrainStatus::Known,
            covers,
            within,
            nn_why,
            // Branch columns line up by position.
            deleted: branches.iter().flat_map(|b| b.deleted.clone()).collect(),
            ..RelationFacts::unknown(width, UnknownReason::UnsupportedOperator(String::new()))
        };
        // A constant discriminator column with distinct literals per branch, plus a key
        // shared by every branch, keeps (key, discriminator) unique.
        'disc: for c in 0..width {
            let lits: Option<Vec<ScalarValue>> = u
                .inputs
                .iter()
                .map(|b| branch_literal(b, c).cloned())
                .collect();
            let Some(lits) = lits else { continue };
            if lits.iter().any(|v| v.is_null()) {
                continue;
            }
            for i in 0..lits.len() {
                for j in i + 1..lits.len() {
                    if lits[i] == lits[j] {
                        continue 'disc;
                    }
                }
            }
            for k in &branches[0].keys {
                if branches.iter().all(|b| b.is_superkey(&k.cols)) {
                    let froms: Vec<EvidenceId> = branches
                        .iter()
                        .filter_map(|b| b.key_covered_by(&k.cols).map(|k| k.why))
                        .collect();
                    let why = self.arena.push(Evidence::UnionDiscriminator {
                        column: u.schema.field(c).name().clone(),
                        from: froms,
                    });
                    let mut cols = k.cols.clone();
                    cols.insert(c);
                    f.keys.push(Key { cols, why });
                    break 'disc;
                }
            }
        }
        f.normalize();
        if f.keys.is_empty() {
            let reason = status
                .reason()
                .cloned()
                .unwrap_or(UnknownReason::UnsupportedOperator(
                    "UNION ALL without a discriminator column".into(),
                ));
            f.status = GrainStatus::Unknown(reason);
        } else {
            f.settle_status(status);
        }
        f
    }
}

/// The literal at output position `c` of a union branch, if the branch is a projection
/// that selects a constant there.
fn branch_literal(plan: &LogicalPlan, c: usize) -> Option<&ScalarValue> {
    match plan {
        LogicalPlan::Projection(p) => p.expr.get(c).and_then(literal_of),
        LogicalPlan::SubqueryAlias(s) => branch_literal(&s.input, c),
        LogicalPlan::Sort(s) => branch_literal(&s.input, c),
        LogicalPlan::Limit(l) => branch_literal(&l.input, c),
        LogicalPlan::Distinct(Distinct::All(i)) => branch_literal(i, c),
        _ => None,
    }
}
