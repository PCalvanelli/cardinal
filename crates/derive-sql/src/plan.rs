use crate::provider::DeriveContext;
use crate::span::SqlSpan;
use datafusion_common::DataFusionError;
use datafusion_expr::LogicalPlan;
use datafusion_sql::planner::{ParserOptions, SqlToRel};
use datafusion_sql::sqlparser::ast::Statement;
use datafusion_sql::sqlparser::dialect::Dialect;
use datafusion_sql::sqlparser::parser::Parser;
use datafusion_sql::sqlparser::tokenizer::{Token, Tokenizer};
use std::borrow::Cow;

/// Prefix dbt gives ephemeral models inlined as CTEs.
pub const EPHEMERAL_CTE_PREFIX: &str = "__dbt__cte__";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanFailureKind {
    Parse,
    UnresolvedFunction(String),
    UnresolvedRelation(String),
    UnresolvedColumn(String),
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct PlanFailure {
    pub kind: PlanFailureKind,
    pub message: String,
    pub span: Option<SqlSpan>,
}

/// Parse and plan one compiled model. The plan is returned exactly as `SqlToRel`
/// produces it: no optimizer pass, so structure still mirrors the SQL that was written.
pub fn plan_sql(
    sql: &str,
    dialect: &dyn Dialect,
    ctx: &DeriveContext<'_>,
) -> Result<LogicalPlan, PlanFailure> {
    let sql = normalize_quoted_idents(sql, dialect);
    let mut statements = Parser::new(dialect)
        .try_with_sql(&sql)
        .and_then(|mut p| p.parse_statements())
        .map_err(|e| PlanFailure {
            kind: PlanFailureKind::Parse,
            span: parse_error_span(&e.to_string()),
            message: e.to_string(),
        })?;
    if statements.len() != 1 {
        return Err(PlanFailure {
            kind: PlanFailureKind::Unsupported,
            message: format!("expected one SELECT statement, found {}", statements.len()),
            span: None,
        });
    }
    let mut statement = statements.remove(0);
    let Statement::Query(query) = &mut statement else {
        return Err(PlanFailure {
            kind: PlanFailureKind::Unsupported,
            message: "compiled SQL is not a query".into(),
            span: None,
        });
    };
    // Ephemeral models arrive as CTEs; drop them so the relation resolves to the
    // ephemeral model's own analyzed facts instead of being re-analyzed inline.
    if let Some(with) = &mut query.with {
        with.cte_tables.retain(|c| {
            !c.alias
                .name
                .value
                .to_lowercase()
                .starts_with(EPHEMERAL_CTE_PREFIX)
        });
        if with.cte_tables.is_empty() {
            query.with = None;
        }
    }
    let options = ParserOptions {
        parse_float_as_decimal: false,
        enable_ident_normalization: true,
        support_varchar_with_length: true,
        enable_options_value_normalization: false,
        collect_spans: true,
        map_string_types_to_utf8view: false,
        default_null_ordering: ParserOptions::default().default_null_ordering,
    };
    SqlToRel::new_with_options(ctx, options)
        .sql_statement_to_plan(statement)
        .map_err(classify)
}

fn classify(e: DataFusionError) -> PlanFailure {
    let span = e
        .diagnostic()
        .and_then(|d| d.span)
        .and_then(|s| SqlSpan::from_df(&s));
    let message = e.strip_backtrace();
    let first_line = message.lines().next().unwrap_or("").to_string();
    let quoted = |after: &str| -> Option<String> {
        let i = message.find(after)? + after.len();
        let rest = &message[i..];
        let end = rest.find(['\'', '"', ' ', '\n']).unwrap_or(rest.len());
        Some(rest[..end].to_string())
    };
    let kind = if let Some(name) = quoted("Invalid function '") {
        PlanFailureKind::UnresolvedFunction(name)
    } else if let Some(name) = quoted("There is no UDF named \"") {
        PlanFailureKind::UnresolvedFunction(name)
    } else if let Some(name) = quoted("derive: unresolved relation ") {
        PlanFailureKind::UnresolvedRelation(name)
    } else if message.contains("No field named") {
        let name = quoted("No field named ")
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        PlanFailureKind::UnresolvedColumn(name)
    } else if matches!(e, DataFusionError::SQL(..)) {
        PlanFailureKind::Parse
    } else {
        PlanFailureKind::Unsupported
    };
    PlanFailure {
        kind,
        message: first_line,
        span,
    }
}

fn parse_error_span(msg: &str) -> Option<SqlSpan> {
    let li = msg.find("Line: ")? + 6;
    let line: u32 = msg[li..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let ci = msg.find("Column: ")? + 8;
    let col: u32 = msg[ci..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    let lc = crate::span::LineCol { line, col };
    Some(SqlSpan {
        start: lc,
        end: crate::span::LineCol { line, col: col + 1 },
    })
}

/// Lowercase quoted identifiers so `"ORDERS"."ORDER_ID"` (Snowflake) resolves against
/// lowercased catalog names. Byte lengths are unchanged, so spans stay valid.
fn normalize_quoted_idents<'s>(sql: &'s str, dialect: &dyn Dialect) -> Cow<'s, str> {
    let Ok(tokens) = Tokenizer::new(dialect, sql).tokenize() else {
        return Cow::Borrowed(sql);
    };
    let needs = tokens.iter().any(|t| {
        matches!(t, Token::Word(w) if w.quote_style.is_some() && w.value.chars().any(|c| c.is_uppercase()))
    });
    if !needs {
        return Cow::Borrowed(sql);
    }
    let mut out = String::with_capacity(sql.len());
    for t in tokens {
        match t {
            Token::Word(mut w) if w.quote_style.is_some() => {
                w.value = w.value.to_lowercase();
                out.push_str(&Token::Word(w).to_string());
            }
            other => out.push_str(&other.to_string()),
        }
    }
    Cow::Owned(out)
}
