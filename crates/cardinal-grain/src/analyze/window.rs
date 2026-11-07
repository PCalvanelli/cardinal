//! Window functions: row numbers and ranks that make keys, and windowed measures.

use super::project::has_nondeterminism_args;
use super::{Analyzer, column_names};
use crate::aggregates::aggregate;
use crate::evidence::Evidence;
use crate::expr::{col_index, expr_cols, unalias};
use crate::facts::{
    ColSet, Computed, Fd, GrainStatus, Key, Measure, MeasureKind, MeasureSite, RelationFacts,
};
use cardinal_sql::datafusion_common::DFSchema;
use cardinal_sql::datafusion_expr::expr::WindowFunction;
use cardinal_sql::datafusion_expr::logical_plan::Window;
use cardinal_sql::datafusion_expr::{Expr, WindowFunctionDefinition};
use cardinal_sql::{SourceText, SqlSpan};
use std::sync::Arc;

impl Analyzer<'_> {
    pub(super) fn window(&mut self, w: &Window) -> RelationFacts {
        let input = self.visit(&w.input);
        let schema = w.input.schema();
        let n = input.width;
        let mut f = (*input).clone();
        f.width = n + w.window_expr.len();
        f.repeats.resize(f.width, None);
        let text = SourceText::new(self.sql);
        for (j, e) in w.window_expr.iter().enumerate() {
            let out = n + j;
            let Expr::WindowFunction(wf) = unalias(e) else {
                continue;
            };
            let partition: Option<ColSet> = wf
                .params
                .partition_by
                .iter()
                .map(|p| match unalias(p) {
                    Expr::Column(c) => col_index(schema, c),
                    _ => None,
                })
                .collect();
            if let Some(p) = &partition
                && is_rank(e)
            {
                f.ranks.push((out, p.clone()));
            }
            self.window_measure(wf, schema, &input, out, &mut f);
            if is_row_number(e).is_some() {
                f.not_null.insert(out);
                if let Some(p) = &partition {
                    let span = wf
                        .params
                        .partition_by
                        .iter()
                        .flat_map(|p| p.column_refs())
                        .find_map(|c| c.spans().first())
                        .and_then(|s| SqlSpan::from_df(&s))
                        .and_then(|s| text.enclosing_call(s, "over").or(Some(s)));
                    let why = self.arena.push(Evidence::RowNumber {
                        at: span,
                        partition: column_names(schema, p),
                    });
                    let mut cols = p.clone();
                    cols.insert(out);
                    f.keys.push(Key { cols, why });
                    // (partition, row_number) is a key whatever the input is.
                    f.status = GrainStatus::Known;
                }
            } else if wf.params.order_by.is_empty()
                && let Some(p) = partition
                && !has_nondeterminism_args(&wf.params.args)
            {
                // Without ORDER BY the value depends only on the partition.
                let why = self.arena.push(Evidence::Derived { from: vec![] });
                f.fds.push(Fd {
                    lhs: p,
                    rhs: [out].into(),
                    why,
                    entity: false,
                });
            }
        }
        f.normalize();
        f
    }

    /// Averages, percentiles and running totals a window computes, and window sums.
    fn window_measure(
        &mut self,
        wf: &WindowFunction,
        schema: &DFSchema,
        input: &RelationFacts,
        out: usize,
        f: &mut RelationFacts,
    ) {
        let WindowFunctionDefinition::AggregateUDF(u) = &wf.fun else {
            return;
        };
        let name = u.name().to_ascii_lowercase();
        if aggregate(&name).is_some_and(|a| a.counting) {
            f.aggregated.insert(out);
        }
        let span = self.call_span(&name, wf.params.args.iter());
        let frame = &wf.params.window_frame;
        let whole = frame.start_bound.is_unbounded() && frame.end_bound.is_unbounded();
        let (kind, what, over) = if let Some(what) = aggregate(&name).and_then(|a| a.computed) {
            (
                MeasureKind::NonAdditive {
                    agree_on: ColSet::new(),
                },
                what,
                vec![],
            )
        } else if aggregate(&name).is_some_and(|a| a.totals)
            && !wf.params.order_by.is_empty()
            && !whole
        {
            // A running or rolling total: each row adds up the rows before it.
            let Some(over) = wf
                .params
                .order_by
                .iter()
                .map(|o| expr_cols(&o.expr, schema))
                .collect::<Option<Vec<ColSet>>>()
                .map(|v| v.into_iter().flatten().collect::<ColSet>())
            else {
                return;
            };
            if over.is_empty() || over.is_subset(&input.closure(&ColSet::new())) {
                return;
            }
            let names = column_names(schema, &over);
            (
                MeasureKind::Stock { over: Some(over) },
                Computed::RunningTotal,
                names,
            )
        } else {
            return;
        };
        f.measures.push(Measure {
            col: out,
            kind,
            site: Arc::new(MeasureSite::Computed {
                relation: self.relation.clone(),
                span,
                what,
                over,
            }),
        });
    }
}

fn is_row_number(e: &Expr) -> Option<&WindowFunction> {
    match unalias(e) {
        Expr::WindowFunction(wf) => match &wf.fun {
            WindowFunctionDefinition::WindowUDF(u) if u.name() == "row_number" => Some(wf),
            _ => None,
        },
        _ => None,
    }
}

/// row_number, rank or dense_rank: every partition has a row where the value is 1.
fn is_rank(e: &Expr) -> bool {
    match unalias(e) {
        Expr::WindowFunction(wf) => match &wf.fun {
            WindowFunctionDefinition::WindowUDF(u) => {
                matches!(u.name(), "row_number" | "rank" | "dense_rank")
            }
            _ => false,
        },
        _ => false,
    }
}
