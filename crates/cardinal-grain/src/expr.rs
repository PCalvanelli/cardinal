//! Reading an expression against the schema of the node it reads. The analysis and the
//! checks both resolve columns here, so they agree on which input column an expression
//! means.

use crate::facts::ColSet;
use cardinal_sql::datafusion_common::{Column, DFSchema};
use cardinal_sql::datafusion_expr::Expr;
use cardinal_sql::datafusion_expr::expr::Alias;

/// The expression under any `AS` aliases.
pub fn unalias(e: &Expr) -> &Expr {
    match e {
        Expr::Alias(Alias { expr, .. }) => unalias(expr),
        other => other,
    }
}

/// The input position of column `c`. An unqualified name means the first input column
/// with that name, as in DataFusion.
pub fn col_index(schema: &DFSchema, c: &Column) -> Option<usize> {
    schema.maybe_index_of_column(c)
}

/// Input positions of every column an expression reads; None if one cannot be resolved.
pub fn expr_cols(e: &Expr, schema: &DFSchema) -> Option<ColSet> {
    e.column_refs()
        .into_iter()
        .map(|c| col_index(schema, c))
        .collect()
}

/// Input positions of every column the expressions read; None if one cannot be resolved.
pub fn exprs_cols(exprs: &[Expr], schema: &DFSchema) -> Option<ColSet> {
    let mut out = ColSet::new();
    for e in exprs {
        out.extend(expr_cols(e, schema)?);
    }
    Some(out)
}

/// The column an aggregate argument adds up: `v`, `cast(v as …)` or
/// `coalesce(v, <literal>)`. Anything else (`v * rate`, `case when v > 100 then 1 end`)
/// is a different value.
pub fn summed_col(e: &Expr, schema: &DFSchema) -> Option<usize> {
    match unalias(e) {
        Expr::Column(c) => col_index(schema, c),
        Expr::Cast(c) => summed_col(&c.expr, schema),
        Expr::TryCast(c) => summed_col(&c.expr, schema),
        Expr::ScalarFunction(f)
            if matches!(
                f.func.name().to_ascii_lowercase().as_str(),
                "coalesce" | "ifnull" | "nvl"
            ) && f.args.len() == 2
                && matches!(unalias(&f.args[1]), Expr::Literal(..)) =>
        {
            summed_col(&f.args[0], schema)
        }
        _ => None,
    }
}
