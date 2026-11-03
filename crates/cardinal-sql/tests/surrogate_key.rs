//! dbt_utils.generate_surrogate_key compiles to an md5() call on every adapter.
//! Each case is the SQL dbt emits for that adapter's hash/concat/type_string macros.

use cardinal_sql::dialect::dialect_for_adapter;
use cardinal_sql::plan::plan_sql;
use cardinal_sql::provider::{CardinalContext, CardinalTable, Functions, TableLookup};
use datafusion_common::arrow::datatypes::{DataType, Field, Schema};
use std::sync::Arc;

struct Orders;

impl TableLookup for Orders {
    fn lookup(&self, parts: &[String]) -> Option<Arc<CardinalTable>> {
        (parts.last()? == "orders").then(|| {
            Arc::new(CardinalTable {
                relation: "model.shop.orders".into(),
                schema: Arc::new(Schema::new(vec![
                    Field::new("order_id", DataType::Int64, true),
                    Field::new("customer_id", DataType::Utf8, true),
                ])),
            })
        })
    }
}

fn key_expr(adapter: &str, ty: &str) -> String {
    let field = |c: &str| format!("coalesce(cast({c} as {ty}), '_dbt_utils_surrogate_key_null_')");
    let concat = if matches!(adapter, "databricks") {
        format!(
            "concat({}, '-', {})",
            field("order_id"),
            field("customer_id")
        )
    } else {
        format!("{} || '-' || {}", field("order_id"), field("customer_id"))
    };
    let hash = format!("md5(cast({concat} as {ty}))");
    if adapter == "bigquery" {
        format!("to_hex({hash})")
    } else {
        hash
    }
}

#[test]
fn generate_surrogate_key_plans_on_every_adapter() {
    for (adapter, ty) in [
        ("snowflake", "varchar"),
        ("postgres", "TEXT"),
        ("redshift", "varchar"),
        ("duckdb", "VARCHAR"),
        ("databricks", "string"),
        ("bigquery", "string"),
    ] {
        let functions = Functions::new(adapter, &[]);
        let ctx = CardinalContext {
            functions: &functions,
            tables: &Orders,
        };
        let (dialect, _) = dialect_for_adapter(adapter);
        let sql = format!(
            "select {} as order_key, order_id from orders",
            key_expr(adapter, ty)
        );
        if let Err(e) = plan_sql(&sql, dialect.as_ref(), &ctx) {
            panic!("{adapter}: {:?}: {}\n{sql}", e.kind, e.message);
        }
    }
}
