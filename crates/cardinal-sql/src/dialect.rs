use datafusion_sql::sqlparser::dialect::{
    BigQueryDialect, DatabricksDialect, Dialect, DuckDbDialect, GenericDialect, PostgreSqlDialect,
    RedshiftSqlDialect, SnowflakeDialect,
};

/// The sqlparser dialect for a dbt `metadata.adapter_type`.
/// Returns the dialect and whether it is a known match (false means generic fallback).
pub fn dialect_for_adapter(adapter: &str) -> (Box<dyn Dialect + Send + Sync>, bool) {
    match adapter.to_ascii_lowercase().as_str() {
        "snowflake" => (Box::new(SnowflakeDialect), true),
        "bigquery" => (Box::new(BigQueryDialect), true),
        "postgres" | "postgresql" => (Box::new(PostgreSqlDialect {}), true),
        "redshift" => (Box::new(RedshiftSqlDialect {}), true),
        "databricks" | "spark" => (Box::new(DatabricksDialect {}), true),
        "duckdb" => (Box::new(DuckDbDialect {}), true),
        _ => (Box::new(GenericDialect {}), false),
    }
}
