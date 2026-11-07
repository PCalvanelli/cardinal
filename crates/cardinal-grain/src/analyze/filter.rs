//! Filters: which rows a predicate keeps, the columns it proves non-null, and the
//! soft-delete flags it applies.

use super::{Analyzer, int_literal, literal_of};
use crate::evidence::Evidence;
use crate::expr::{col_index, unalias};
use crate::facts::{ColSet, Deleted, Fd, Ind, RelationFacts};
use cardinal_sql::datafusion_common::{DFSchema, ScalarValue};
use cardinal_sql::datafusion_expr::logical_plan::Filter;
use cardinal_sql::datafusion_expr::utils::split_conjunction;
use cardinal_sql::datafusion_expr::{BinaryExpr, Expr, Operator};
use cardinal_sql::{SourceText, SqlSpan};

impl Analyzer<'_> {
    pub(super) fn filter(&mut self, f: &Filter) -> RelationFacts {
        let input = self.visit(&f.input);
        let mut facts = (*input).clone();
        let schema = f.input.schema();
        let text = SourceText::new(self.sql);
        let conjuncts = split_conjunction(&f.predicate);
        apply_flags(&mut facts.deleted, conjuncts.iter().copied(), schema);
        self.filter_covers(&conjuncts, schema, &input, &mut facts);
        for conj in conjuncts {
            match conj {
                Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
                    let lc = match unalias(left) {
                        Expr::Column(c) => col_index(schema, c),
                        _ => None,
                    };
                    let rc = match unalias(right) {
                        Expr::Column(c) => col_index(schema, c),
                        _ => None,
                    };
                    let span = conj
                        .column_refs()
                        .into_iter()
                        .find_map(|c| c.spans().first())
                        .and_then(|s| SqlSpan::from_df(&s));
                    if *op == Operator::Eq {
                        match (lc, rc, literal_of(left), literal_of(right)) {
                            (Some(c), None, _, Some(v)) | (None, Some(c), Some(v), _)
                                if !v.is_null() =>
                            {
                                let why = self.arena.push(Evidence::EqualityFilter {
                                    at: span,
                                    text: span.map(|s| text.slice(s).to_string()).unwrap_or_else(
                                        || format!("{} = {}", schema.field(c).name(), v),
                                    ),
                                });
                                facts.fds.push(Fd {
                                    lhs: ColSet::new(),
                                    rhs: [c].into(),
                                    why,
                                    entity: false,
                                });
                                facts.prove_not_null(c);
                            }
                            (Some(a), Some(b), _, _) => {
                                let why = self.arena.push(Evidence::EqualityFilter {
                                    at: span,
                                    text: format!(
                                        "{} = {}",
                                        schema.field(a).name(),
                                        schema.field(b).name()
                                    ),
                                });
                                facts.fds.push(Fd {
                                    lhs: [a].into(),
                                    rhs: [b].into(),
                                    why,
                                    entity: false,
                                });
                                facts.fds.push(Fd {
                                    lhs: [b].into(),
                                    rhs: [a].into(),
                                    why,
                                    entity: false,
                                });
                                facts.prove_not_null(a);
                                facts.prove_not_null(b);
                            }
                            _ => {}
                        }
                    } else if matches!(
                        op,
                        Operator::Lt
                            | Operator::LtEq
                            | Operator::Gt
                            | Operator::GtEq
                            | Operator::NotEq
                            | Operator::LikeMatch
                            | Operator::ILikeMatch
                    ) {
                        for c in lc.into_iter().chain(rc) {
                            facts.prove_not_null(c);
                        }
                    }
                }
                Expr::IsNotNull(e)
                | Expr::Like(cardinal_sql::datafusion_expr::expr::Like { expr: e, .. }) => {
                    if let Expr::Column(c) = unalias(e)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                Expr::Between(b) if !b.negated => {
                    if let Expr::Column(c) = unalias(&b.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                Expr::InList(l) if !l.negated => {
                    if let Expr::Column(c) = unalias(&l.expr)
                        && let Some(i) = col_index(schema, c)
                    {
                        facts.prove_not_null(i);
                    }
                }
                _ => {}
            }
        }
        facts.normalize();
        facts
    }

    /// Which covers facts survive a filter: all of them when every conjunct is true for
    /// every row, those a rank filter keeps one row per partition of, or none.
    fn filter_covers(
        &mut self,
        conjuncts: &[&Expr],
        schema: &DFSchema,
        input: &RelationFacts,
        facts: &mut RelationFacts,
    ) {
        let mut partition: Option<ColSet> = None;
        let mut drops: Option<&Expr> = None;
        let mut ranked: Option<&Expr> = None;
        for conj in conjuncts {
            match keeps(conj, schema, input) {
                Keeps::All => {}
                Keeps::OnePerPartition(p) if partition.is_none() => {
                    partition = Some(p);
                    ranked = Some(conj);
                }
                // Two rank filters can keep different rows and together keep none.
                _ => {
                    drops = drops.or(Some(conj));
                }
            }
        }
        let span_of = |e: &Expr| {
            e.column_refs()
                .into_iter()
                .find_map(|c| c.spans().first())
                .and_then(|s| SqlSpan::from_df(&s))
        };
        if drops.is_none() && partition.is_none() {
            return;
        }
        facts.ranks.clear();
        let all = std::mem::take(&mut facts.covers);
        let (kept, lost): (Vec<Ind>, Vec<Ind>) = match (&partition, drops) {
            (Some(p), None) => {
                // Keep the pairs on columns the partition determines: each partition's
                // kept row has the same values there as every row of the partition.
                let (cl, why) = input.closure_why(p);
                let mut k = Vec::new();
                let mut l = Vec::new();
                for c in all {
                    match c.remap(|x| cl.contains(&x).then_some(x)) {
                        Some(kept) => k.push(kept),
                        None => l.push(c),
                    }
                }
                let at = ranked.and_then(span_of);
                let text = self
                    .line_of(at)
                    .map(|(_, t)| t)
                    .unwrap_or_else(|| "a rank filter".into());
                let k = k
                    .into_iter()
                    .map(|mut c| {
                        let mut from = vec![c.why];
                        from.extend(why.iter().copied());
                        c.why = self.arena.push(Evidence::Preserved {
                            at,
                            what: format!("`{text}` keeps a row of every partition"),
                            from,
                        });
                        c
                    })
                    .collect();
                (k, l)
            }
            _ => (vec![], all),
        };
        facts.covers = kept;
        if !lost.is_empty() {
            let at = drops.or(ranked).and_then(span_of);
            let (span, what) = match self.line_of(at) {
                Some((s, t)) => (Some(s), format!("filter `{t}`")),
                None => (
                    None,
                    format!(
                        "filter `{}`",
                        drops.or(ranked).map(|e| e.to_string()).unwrap_or_default()
                    ),
                ),
            };
            let because = match (&partition, drops) {
                (Some(_), None) => Some(
                    "the rank filter keeps one row per partition, which does not determine these columns"
                        .to_string(),
                ),
                _ => None,
            };
            self.lose(&lost, span, &what, because);
        }
    }
}

/// Drop the soft-delete flags that `exprs` read: a filter, join condition or aggregate
/// that reads one applies it, or at least decides what to do with deleted rows.
pub(super) fn apply_flags<'e>(
    deleted: &mut Vec<Deleted>,
    exprs: impl IntoIterator<Item = &'e Expr>,
    schema: &DFSchema,
) {
    let read: ColSet = exprs
        .into_iter()
        .flat_map(|e| e.column_refs())
        .filter_map(|c| col_index(schema, c))
        .collect();
    deleted.retain(|d| !d.col.is_some_and(|c| read.contains(&c)));
}

/// How a filter conjunct treats the rows of a relation the input covers.
enum Keeps {
    /// True for every input row.
    All,
    /// Keeps at least one row of every partition of this rank column.
    OnePerPartition(ColSet),
    /// May drop rows.
    Some,
}

/// Classify one filter conjunct against the filter's input facts.
fn keeps(conj: &Expr, schema: &DFSchema, input: &RelationFacts) -> Keeps {
    let col = |e: &Expr| match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        _ => None,
    };
    match conj {
        Expr::IsNotNull(e) if col(e).is_some_and(|c| input.not_null.contains(&c)) => Keeps::All,
        Expr::Literal(ScalarValue::Boolean(Some(true)), _) => Keeps::All,
        Expr::BinaryExpr(BinaryExpr { left, op, right }) => {
            // Put the column on the left: `1 = rn` is `rn = 1`, `3 >= rn` is `rn <= 3`.
            let (c, op, v) = match (col(left), col(right)) {
                (Some(c), None) => (c, *op, int_literal(right)),
                (None, Some(c)) => match op.swap() {
                    Some(o) => (c, o, int_literal(left)),
                    None => return Keeps::Some,
                },
                _ => return Keeps::Some,
            };
            let Some(v) = v else { return Keeps::Some };
            let Some((_, p)) = input.ranks.iter().find(|(r, _)| *r == c) else {
                return Keeps::Some;
            };
            // Every partition has a row ranked 1.
            let keeps_rank_one = match op {
                Operator::Eq => v == 1,
                Operator::LtEq => v >= 1,
                Operator::Lt => v >= 2,
                _ => false,
            };
            if keeps_rank_one {
                Keeps::OnePerPartition(p.clone())
            } else {
                Keeps::Some
            }
        }
        _ => Keeps::Some,
    }
}
