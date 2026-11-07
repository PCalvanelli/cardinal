//! Scans and projections: how each output column is computed from the input, and
//! what keys, dependencies and measures survive.

use super::Analyzer;
use crate::evidence::{Evidence, EvidenceId};
use crate::expr::{col_index, expr_cols, unalias};
use crate::facts::{
    ColSet, Computed, Deleted, Fd, Ind, Key, Measure, MeasureKind, MeasureSite, RelationFacts,
    UnknownReason, closure, minimize,
};
use cardinal_dbt::RelationId;
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_common::tree_node::TreeNode;
use cardinal_sql::datafusion_expr::logical_plan::{Projection, TableScan};
use cardinal_sql::datafusion_expr::{BinaryExpr, Expr, Operator};
use cardinal_sql::{CardinalTable, SourceText, SqlSpan, Tokens};
use std::collections::BTreeMap;
use std::sync::Arc;

impl Analyzer<'_> {
    pub(super) fn table_scan(&mut self, s: &TableScan) -> RelationFacts {
        let width = s.projected_schema.fields().len();
        let Some(t) = s.source.as_ref().downcast_ref::<CardinalTable>() else {
            return RelationFacts::unknown(
                width,
                UnknownReason::UnsupportedOperator("an unknown table".into()),
            );
        };
        let id = RelationId(t.relation.clone());
        let Some(summary) = self.upstream.summary(&id) else {
            return RelationFacts::unknown(width, UnknownReason::PlanError(id));
        };
        let mut full = summary.to_facts(&mut self.arena);
        self.losses.extend(summary.losses.iter().cloned());
        let pairs: Vec<(usize, String)> = summary
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| (i, c.name.clone()))
            .collect();
        if !pairs.is_empty() {
            let why = self.arena.push(Evidence::Identity {
                relation: id.clone(),
            });
            let ident = Ind {
                relation: id.clone(),
                pairs,
                why,
            };
            // SqlToRel never pushes filters or limits into a scan, but stay safe.
            if s.filters.is_empty() && s.fetch.is_none() {
                full.covers.insert(0, ident.clone());
            }
            full.within.insert(0, ident);
        }
        match &s.projection {
            None => full,
            Some(p) => {
                let outs: Vec<Out> = p.iter().map(|i| Out::Col(*i)).collect();
                project_facts(&full, &outs)
            }
        }
    }

    pub(super) fn projection(&mut self, p: &Projection) -> RelationFacts {
        let input = self.visit(&p.input);
        let schema = p.input.schema();
        let outs: Vec<Out> = p
            .expr
            .iter()
            .map(|e| classify_out(e, schema, &input))
            .collect();
        let mut f = project_facts(&input, &outs);
        if input.aggregated.is_empty() {
            return f;
        }
        for (j, e) in p.expr.iter().enumerate() {
            if is_ratio(e, schema, &input.aggregated) {
                let span = self.alias_line(p.schema.field(j).name());
                f.measures.retain(|m| m.col != j);
                f.measures.push(Measure {
                    col: j,
                    kind: MeasureKind::NonAdditive {
                        agree_on: ColSet::new(),
                    },
                    site: Arc::new(MeasureSite::Computed {
                        relation: self.relation.clone(),
                        span,
                        what: Computed::Ratio,
                        over: vec![],
                    }),
                });
            } else if let Out::Expr { cols, .. } = &outs[j]
                && !cols.is_empty()
                && cols.is_subset(&input.aggregated)
            {
                // `coalesce(sum(a), 0)` is still a sum.
                f.aggregated.insert(j);
            }
        }
        f.normalize();
        f
    }

    /// Where output column `name` is computed (`<expr> as name`), for labels on a column
    /// the plan rewrote, such as a ratio of two aggregates: the expression before `as` on
    /// that line, or the whole line when the expression ends on an earlier one. Comments
    /// and strings do not count, and the alias may be quoted.
    fn alias_line(&self, name: &str) -> Option<SqlSpan> {
        let text = SourceText::new(self.sql);
        let t = Tokens::new(self.sql);
        let alias = |i: usize| t.name(i).is_some_and(|n| n.eq_ignore_ascii_case(name));
        let (kw, alias) = t
            .words("as")
            .find_map(|i| t.next(i).filter(|&n| alias(n)).map(|n| (i, n)))?;
        let at = t.get(kw).start;
        let end = t.name_range(alias)?.end;
        let (line, _) = self.line_of(Some(text.span(at, end)))?;
        let expr_start = text.offset(line.start);
        let expr = self.sql[expr_start.min(at)..at].trim_end();
        if expr.is_empty() {
            return Some(line);
        }
        Some(text.span(expr_start, expr_start + expr.len()))
    }
}

/// How one output column of a projection-like node is computed from its input.
#[derive(Clone, Debug)]
pub(super) enum Out {
    Col(usize),
    Lit { non_null: bool },
    Expr { cols: ColSet, non_null: bool },
    Opaque { non_null: bool },
}

/// A division whose numerator and denominator both read aggregated columns:
/// `sum(a) / sum(b)`, `count(*) / count(distinct u)`. Row-level division
/// (`amount / fx_rate`, `cents / 100`) and `sum(cents) / 100` are conversions.
fn is_ratio(e: &Expr, schema: &DFSchema, aggregated: &ColSet) -> bool {
    let reads = |x: &Expr| {
        x.column_refs()
            .into_iter()
            .filter_map(|c| col_index(schema, c))
            .any(|c| aggregated.contains(&c))
    };
    e.exists(|x| {
        Ok(matches!(
            x,
            Expr::BinaryExpr(BinaryExpr { left, op: Operator::Divide | Operator::IntegerDivide, right })
                if reads(left) && reads(right)
        ))
    })
    .unwrap_or(false)
}

fn has_nondeterminism(e: &Expr) -> bool {
    e.is_volatile()
        || e.exists(|x| {
            Ok(matches!(
                x,
                Expr::ScalarSubquery(_)
                    | Expr::Exists(_)
                    | Expr::InSubquery(_)
                    | Expr::AggregateFunction(_)
                    | Expr::WindowFunction(_)
                    | Expr::OuterReferenceColumn(..)
                    | Expr::Placeholder(_)
            ))
        })
        .unwrap_or(true)
}

pub(super) fn classify_out(e: &Expr, schema: &DFSchema, input: &RelationFacts) -> Out {
    let e = unalias(e);
    match e {
        Expr::Column(c) => match col_index(schema, c) {
            Some(p) => Out::Col(p),
            None => Out::Opaque { non_null: false },
        },
        Expr::Literal(v, _) => Out::Lit {
            non_null: !v.is_null(),
        },
        Expr::Cast(c) if matches!(unalias(&c.expr), Expr::Column(_)) => {
            match expr_cols(&c.expr, schema) {
                Some(cols) => {
                    let nn = cols.iter().all(|p| input.not_null.contains(p));
                    Out::Expr { cols, non_null: nn }
                }
                None => Out::Opaque { non_null: false },
            }
        }
        _ if has_nondeterminism(e) => Out::Opaque { non_null: false },
        _ => match expr_cols(e, schema) {
            Some(cols) => Out::Expr {
                cols,
                non_null: false,
            },
            None => Out::Opaque { non_null: false },
        },
    }
}

/// Facts of a node whose columns are each computed from one input row (projection,
/// the group-by part of an aggregate). Works in an extended space of input columns
/// followed by output columns, then reads the output facts back out.
pub(super) fn project_facts(input: &RelationFacts, outs: &[Out]) -> RelationFacts {
    let n = input.width;
    let m = outs.len();
    let w = n + m;
    let input_all: ColSet = (0..n).collect();
    let mut ext: Vec<Fd> = input.fds.clone();
    for k in &input.keys {
        ext.push(Fd {
            lhs: k.cols.clone(),
            rhs: input_all.clone(),
            why: k.why,
            entity: true,
        });
    }
    let dummy = EvidenceId(0);
    let mut computed = Vec::new();
    for (j, o) in outs.iter().enumerate() {
        let oj = n + j;
        match o {
            Out::Col(p) => {
                ext.push(Fd {
                    lhs: [*p].into(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                ext.push(Fd {
                    lhs: [oj].into(),
                    rhs: [*p].into(),
                    why: dummy,
                    entity: false,
                });
            }
            Out::Lit { .. } => {
                ext.push(Fd {
                    lhs: ColSet::new(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                computed.push(oj);
            }
            Out::Expr { cols, .. } => {
                ext.push(Fd {
                    lhs: cols.clone(),
                    rhs: [oj].into(),
                    why: dummy,
                    entity: false,
                });
                computed.push(oj);
            }
            Out::Opaque { .. } => computed.push(oj),
        }
    }
    let outs_ext: ColSet = (n..w).collect();
    let cl = |x: &ColSet| closure(w, &[], &ext, x);
    // Drop computed outputs first, then the columns that determine the most, so a
    // dependency keeps its nearest determinant (customer_id, not order_id).
    let mut drop_order = computed.clone();
    let mut plain: Vec<(usize, usize)> = (n..w)
        .filter(|c| !computed.contains(c))
        .map(|c| (cl(&ColSet::from([c])).len(), c))
        .collect();
    plain.sort_by(|a, b| b.cmp(a));
    drop_order.extend(plain.into_iter().map(|(_, c)| c));
    let min = |x: &ColSet, target: &ColSet| minimize(w, &[], &ext, x, target, &drop_order);
    let to_out = |s: &ColSet| -> ColSet { s.iter().filter(|c| **c >= n).map(|c| c - n).collect() };
    let cl_all = cl(&outs_ext);

    // Prefer outputs that copy the target's own columns, so equivalent keys (a.id and
    // b.id after a one-to-one join) stay distinct instead of collapsing into one.
    let direct = |target: &ColSet| -> ColSet {
        outs.iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, Out::Col(p) if target.contains(p)))
            .map(|(j, _)| n + j)
            .collect()
    };
    let best = |target: &ColSet| -> ColSet {
        let d = direct(target);
        if target.is_subset(&cl(&d)) {
            min(&d, target)
        } else {
            min(&outs_ext, target)
        }
    };
    let mut keys = Vec::new();
    for k in &input.keys {
        if k.cols.is_subset(&cl_all) {
            keys.push(Key {
                cols: to_out(&best(&k.cols)),
                why: k.why,
            });
        }
    }
    let mut fds = Vec::new();
    let mut sources: Vec<&Fd> = input.fds.iter().collect();
    let key_fds = input.key_fds();
    sources.extend(key_fds.iter());
    for f in sources {
        if f.lhs.is_subset(&cl_all) {
            let lhs_ext = best(&f.lhs);
            let rhs = to_out(&cl(&lhs_ext));
            let lhs = to_out(&lhs_ext);
            let rhs: ColSet = rhs.difference(&lhs).copied().collect();
            if !rhs.is_empty() {
                fds.push(Fd {
                    lhs,
                    rhs,
                    why: f.why,
                    entity: f.entity,
                });
            }
        }
    }
    for (j, o) in outs.iter().enumerate() {
        match o {
            Out::Lit { .. } => fds.push(Fd {
                lhs: ColSet::new(),
                rhs: [j].into(),
                why: dummy,
                entity: false,
            }),
            Out::Expr { cols, .. } => {
                let mut others = outs_ext.clone();
                others.remove(&(n + j));
                if cols.is_subset(&cl(&others)) {
                    let lhs = to_out(&min(&others, cols));
                    fds.push(Fd {
                        lhs,
                        rhs: [j].into(),
                        why: dummy,
                        entity: false,
                    });
                }
            }
            _ => {}
        }
    }
    let not_null = outs
        .iter()
        .enumerate()
        .filter(|(_, o)| match o {
            Out::Col(p) => input.not_null.contains(p),
            Out::Lit { non_null } | Out::Expr { non_null, .. } | Out::Opaque { non_null } => {
                *non_null
            }
        })
        .map(|(j, _)| j)
        .collect();
    let repeats = outs
        .iter()
        .map(|o| match o {
            Out::Col(p) => input.repeats.get(*p).cloned().flatten(),
            Out::Expr { cols, .. } => cols
                .iter()
                .find_map(|p| input.repeats.get(*p).cloned().flatten()),
            _ => None,
        })
        .collect();
    // Each input column's first plain copy among the outputs.
    let mut copy_of: BTreeMap<usize, usize> = BTreeMap::new();
    for (j, o) in outs.iter().enumerate() {
        if let Out::Col(p) = o {
            copy_of.entry(*p).or_insert(j);
        }
    }
    let to = |c: usize| copy_of.get(&c).copied();
    let (measures, units) = input.carried(&to);
    let nn_why = input
        .nn_why
        .iter()
        .filter_map(|(c, w)| to(*c).map(|j| (j, *w)))
        .collect();
    // A null-exempt key survives where each of its columns is copied as is.
    let null_keys = input
        .null_keys
        .iter()
        .filter_map(|k| {
            Some(Key {
                cols: k.cols.iter().map(|c| to(*c)).collect::<Option<ColSet>>()?,
                why: k.why,
            })
        })
        .collect();
    let mut f = RelationFacts {
        width: m,
        keys,
        null_keys,
        fds,
        not_null,
        status: input.status.clone(),
        repeats,
        covers: input.covers.iter().filter_map(|i| i.remap(to)).collect(),
        within: input.within.iter().filter_map(|i| i.remap(to)).collect(),
        ranks: input
            .ranks
            .iter()
            .filter_map(|(r, p)| {
                let p: Option<ColSet> = p.iter().map(|c| to(*c)).collect();
                Some((to(*r)?, p?))
            })
            .collect(),
        nn_why,
        measures,
        units,
        aggregated: input.aggregated.iter().filter_map(|c| to(*c)).collect(),
        deleted: carry_deleted(&input.deleted, outs),
    };
    f.normalize();
    f.settle_status(input.status.clone());
    f
}

/// Soft-delete flags on the outputs of a projection-like node. A flag read by an
/// expression (`coalesce(_fivetran_deleted, false) as deleted`) is still carried, in
/// that column.
pub(super) fn carry_deleted(deleted: &[Deleted], outs: &[Out]) -> Vec<Deleted> {
    deleted
        .iter()
        .map(|d| {
            let col = d.col.and_then(|c| {
                outs.iter()
                    .position(|o| matches!(o, Out::Col(p) if *p == c))
                    .or_else(|| {
                        outs.iter()
                            .position(|o| matches!(o, Out::Expr { cols, .. } if cols.contains(&c)))
                    })
            });
            Deleted { col, ..d.clone() }
        })
        .collect()
}

pub(super) fn has_nondeterminism_args(args: &[Expr]) -> bool {
    args.iter().any(has_nondeterminism)
}
