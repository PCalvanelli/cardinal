//! Aggregates and DISTINCT ON: the grouping keys, and what each aggregate output is.

use super::filter::apply_flags;
use super::project::{Out, carry_deleted, classify_out, project_facts};
use super::{Analyzer, column_names};
use crate::aggregates::{aggregate, is_distinct_count};
use crate::evidence::Evidence;
use crate::expr::{col_index, expr_cols, summed_col, unalias};
use crate::facts::{
    ColSet, DistinctSite, GrainStatus, Ind, Key, Measure, MeasureKind, MeasureSite, RelationFacts,
    Unit, UnknownReason,
};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::Expr;
use cardinal_sql::datafusion_expr::logical_plan::{Aggregate, DistinctOn};
use cardinal_sql::{SourceText, SqlSpan};
use std::collections::BTreeMap;
use std::sync::Arc;

impl Analyzer<'_> {
    /// The span of the call `name(…)` whose arguments read the given expressions' columns.
    pub(super) fn call_span<'e>(
        &self,
        name: &str,
        args: impl Iterator<Item = &'e Expr>,
    ) -> Option<SqlSpan> {
        let text = SourceText::new(self.sql);
        args.flat_map(|a| a.column_refs())
            .find_map(|c| c.spans().first())
            .and_then(|s| SqlSpan::from_df(&s))
            .map(|s| text.enclosing_call(s, name).unwrap_or(s))
    }

    /// What an aggregate's outputs are: averages and percentiles, sums and counts (for
    /// ratios), and stocks and amounts it keeps apart by group.
    fn aggregate_measures(&mut self, a: &Aggregate, input: &RelationFacts, f: &mut RelationFacts) {
        let schema = a.input.schema();
        let g = a.group_expr.len();
        // Plain group columns: input position -> output position.
        let group_of: BTreeMap<usize, usize> = a
            .group_expr
            .iter()
            .enumerate()
            .filter_map(|(i, e)| match unalias(e) {
                Expr::Column(c) => col_index(schema, c).map(|p| (p, i)),
                _ => None,
            })
            .collect();
        let bare: ColSet = group_of.keys().copied().collect();
        let grouped = input.closure(&bare);
        let constant = input.closure(&ColSet::new());
        // Output group columns that determine `x`, when every group agrees on it.
        let per_group = |x: &ColSet| -> Option<ColSet> {
            if x.is_subset(&constant) || !x.is_subset(&grouped) {
                return None;
            }
            let m = if x.is_subset(&bare) {
                x.clone()
            } else {
                input.minimize(&bare, x, &[])
            };
            Some(m.iter().map(|c| group_of[c]).collect())
        };
        for (j, e) in a.aggr_expr.iter().enumerate() {
            let Expr::AggregateFunction(af) = unalias(e) else {
                continue;
            };
            let out = g + j;
            let name = af.func.name().to_ascii_lowercase();
            if aggregate(&name).is_some_and(|a| a.counting) {
                f.aggregated.insert(out);
            }
            if let Some(what) = aggregate(&name).and_then(|a| a.computed) {
                let order = af.params.order_by.iter().map(|o| &o.expr);
                let span = self.call_span(&name, af.params.args.iter().chain(order));
                f.measures.push(Measure {
                    col: out,
                    kind: MeasureKind::NonAdditive {
                        agree_on: ColSet::new(),
                    },
                    site: Arc::new(MeasureSite::Computed {
                        relation: self.relation.clone(),
                        span,
                        what,
                        over: vec![],
                    }),
                });
            }
            if !aggregate(&name).is_some_and(|a| a.keeps_unit) || af.params.args.len() != 1 {
                continue;
            }
            let Some(c) = summed_col(&af.params.args[0], schema) else {
                continue;
            };
            // A per-group total of a stock is still a stock, as long as each group
            // stays within one value of what it is a stock over.
            if let Some(m) = input.measures.iter().find(|m| m.col == c)
                && let MeasureKind::Stock { over: Some(over) } = &m.kind
                && let Some(over) = per_group(over)
            {
                f.measures.push(Measure {
                    col: out,
                    kind: MeasureKind::Stock { over: Some(over) },
                    site: m.site.clone(),
                });
            }
            // An amount summed within one unit keeps that unit.
            if let Some(u) = input.units.iter().find(|u| u.col == c)
                && let Some(x) = &u.unit
                && let Some(unit) = per_group(x)
            {
                f.units.push(Unit {
                    col: out,
                    unit: Some(unit),
                    site: u.site.clone(),
                });
            }
        }
    }

    pub(super) fn aggregate(&mut self, a: &Aggregate) -> RelationFacts {
        let input = self.visit(&a.input);
        let schema = a.input.schema();
        if a.group_expr
            .iter()
            .any(|g| matches!(g, Expr::GroupingSet(_)))
        {
            return RelationFacts::unknown(
                a.schema.fields().len(),
                UnknownReason::UnsupportedOperator("GROUPING SETS / ROLLUP / CUBE".into()),
            );
        }
        let mut outs: Vec<Out> = a
            .group_expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        for e in &a.aggr_expr {
            let non_null =
                matches!(unalias(e), Expr::AggregateFunction(f) if f.func.name() == "count");
            outs.push(Out::Opaque { non_null });
        }
        let mut f = project_facts(&input, &outs);
        // An aggregate that reads a flag (`count_if(not _fivetran_deleted)`) decides what
        // to do with deleted rows.
        let mut deleted = input.deleted.clone();
        apply_flags(&mut deleted, &a.aggr_expr, schema);
        f.deleted = carry_deleted(&deleted, &outs);
        let g = a.group_expr.len();
        let group: ColSet = (0..g).collect();
        let at = a
            .group_expr
            .iter()
            .flat_map(|e| e.column_refs())
            .find_map(|c| c.spans().first())
            .and_then(|s| SqlSpan::from_df(&s));
        let why = self.arena.push(Evidence::GroupBy {
            at,
            cols: a
                .group_expr
                .iter()
                .map(|e| e.schema_name().to_string())
                .collect(),
        });
        f.keys.push(Key { cols: group, why });
        f.repeats = vec![None; f.width];
        self.distinct_counts(a, &input, &mut f);
        self.aggregate_measures(a, &input, &mut f);
        f.normalize();
        f.status = GrainStatus::Known;
        f
    }

    /// Record each `count(distinct x)` of an aggregate, with the group columns `x`
    /// determines: two groups that count the same value agree on those. Only when the
    /// input's grain is fully known, so a missing dependency cannot invent an overlap.
    fn distinct_counts(&mut self, a: &Aggregate, input: &RelationFacts, f: &mut RelationFacts) {
        if !input.status.is_known() || input.keys.is_empty() {
            return;
        }
        let schema = a.input.schema();
        let g = a.group_expr.len();
        let text = SourceText::new(self.sql);
        for (j, e) in a.aggr_expr.iter().enumerate() {
            let Expr::AggregateFunction(af) = unalias(e) else {
                continue;
            };
            let name = af.func.name().to_ascii_lowercase();
            if !is_distinct_count(&name, af.params.distinct) {
                continue;
            }
            let Some(x) = af
                .params
                .args
                .iter()
                .map(|a| counted_col(a, schema))
                .collect::<Option<ColSet>>()
            else {
                continue;
            };
            if x.is_empty() {
                continue;
            }
            let cl = input.closure(&x);
            let mut agree_on = ColSet::new();
            for (i, ge) in a.group_expr.iter().enumerate() {
                if expr_cols(ge, schema).is_some_and(|c| c.is_subset(&cl)) {
                    agree_on.insert(i);
                }
            }
            if agree_on.len() == g {
                // Every group holds its own values: the counts never overlap.
                continue;
            }
            let span = af
                .params
                .args
                .iter()
                .flat_map(|a| a.column_refs())
                .find_map(|c| c.spans().first())
                .and_then(|s| SqlSpan::from_df(&s))
                .map(|s| text.enclosing_call(s, &name).unwrap_or(s));
            let per: Vec<String> = a
                .group_expr
                .iter()
                .map(|e| match unalias(e) {
                    Expr::Column(c) => c.name.clone(),
                    other => other.schema_name().to_string(),
                })
                .collect();
            let site = DistinctSite {
                relation: self.relation.clone(),
                span,
                counted: column_names(schema, &x),
                across: per
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !agree_on.contains(i))
                    .map(|(_, n)| n.clone())
                    .collect(),
                per,
            };
            f.measures.push(Measure {
                col: g + j,
                kind: MeasureKind::NonAdditive { agree_on },
                site: Arc::new(MeasureSite::Distinct(site)),
            });
        }
    }

    pub(super) fn distinct_on(&mut self, d: &DistinctOn) -> RelationFacts {
        let input = self.visit(&d.input);
        let schema = d.input.schema();
        let outs: Vec<Out> = d
            .select_expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        let mut f = project_facts(&input, &outs);
        let key: Option<ColSet> = d
            .on_expr
            .iter()
            .map(|on| d.select_expr.iter().position(|s| unalias(s) == unalias(on)))
            .collect();
        f.ranks.clear();
        let all = std::mem::take(&mut f.covers);
        let (kept, lost): (Vec<Ind>, Vec<Ind>) = match &key {
            Some(cols) => {
                let cl = f.closure(cols);
                let mut k = Vec::new();
                let mut l = Vec::new();
                for c in all {
                    match c.remap(|x| cl.contains(&x).then_some(x)) {
                        Some(kept) => k.push(kept),
                        None => l.push(c),
                    }
                }
                (k, l)
            }
            None => (vec![], all),
        };
        f.covers = kept;
        self.lose(
            &lost,
            None,
            "DISTINCT ON",
            Some("it keeps one row per value of the DISTINCT ON columns".into()),
        );
        match key {
            Some(cols) => {
                let why = self.arena.push(Evidence::Distinct { at: None });
                f.keys.push(Key { cols, why });
                f.repeats = vec![None; f.width];
                f.normalize();
                f.status = GrainStatus::Known;
            }
            None => f.settle_status(GrainStatus::Unknown(UnknownReason::UnsupportedOperator(
                "DISTINCT ON an expression that is not selected".into(),
            ))),
        }
        f
    }
}

/// The input column whose values a distinct-count argument counts: the column itself, or
/// a CASE whose every result is that column or NULL (`case when active then user_id end`).
/// None for any other expression, since it could map different values to one.
fn counted_col(e: &Expr, schema: &DFSchema) -> Option<usize> {
    match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        Expr::Case(case) => {
            let mut col = None;
            for (_, then) in &case.when_then_expr {
                let c = counted_col(then, schema)?;
                if col.is_some_and(|p| p != c) {
                    return None;
                }
                col = Some(c);
            }
            match case.else_expr.as_deref().map(unalias) {
                None => {}
                Some(Expr::Literal(v, _)) if v.is_null() => {}
                Some(other) => {
                    if counted_col(other, schema)? != col? {
                        return None;
                    }
                }
            }
            col
        }
        _ => None,
    }
}
