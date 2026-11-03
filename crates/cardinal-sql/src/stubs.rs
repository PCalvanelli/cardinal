//! Function stubs: warehouse functions DataFusion does not know, declared by name and
//! return type so planning succeeds. Stubs are never executed.

use crate::types::stub_return_type;
use datafusion_common::arrow::datatypes::DataType;
use datafusion_common::{Result, not_impl_err};
use datafusion_expr::function::AccumulatorArgs;
use datafusion_expr::{
    Accumulator, AggregateUDF, AggregateUDFImpl, ColumnarValue, ScalarFunctionArgs, ScalarUDF,
    ScalarUDFImpl, Signature, TypeSignature, Volatility,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StubKind {
    #[default]
    Scalar,
    Aggregate,
}

/// One `[[stub]]` entry in cardinal.toml, or a built-in adapter stub.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StubSpec {
    pub name: String,
    #[serde(default)]
    pub kind: StubKind,
    /// A warehouse type name, or "arg0" for "same as the first argument".
    #[serde(default = "default_returns")]
    pub returns: String,
    #[serde(default = "yes")]
    pub deterministic: bool,
    /// Aggregates only: true when duplicate input rows cannot change the result (like max).
    #[serde(default)]
    pub idempotent: bool,
}

fn default_returns() -> String {
    "arg0".into()
}
fn yes() -> bool {
    true
}

impl StubSpec {
    pub fn scalar(name: &str, returns: &str) -> Self {
        Self {
            name: name.into(),
            kind: StubKind::Scalar,
            returns: returns.into(),
            deterministic: true,
            idempotent: false,
        }
    }
    pub fn aggregate(name: &str, returns: &str, idempotent: bool) -> Self {
        Self {
            name: name.into(),
            kind: StubKind::Aggregate,
            returns: returns.into(),
            deterministic: true,
            idempotent,
        }
    }
}

/// Built-in stub pack for an adapter: common warehouse functions DataFusion lacks.
pub fn adapter_pack(adapter: &str) -> Vec<StubSpec> {
    use StubSpec as S;
    let mut v = vec![
        // Shared across most warehouses
        S::scalar("ifnull", "arg0"),
        S::scalar("nvl", "arg0"),
        S::scalar("nvl2", "arg0"),
        S::scalar("iff", "arg0"),
        S::scalar("if", "arg0"),
        S::scalar("zeroifnull", "arg0"),
        S::scalar("div0", "double"),
        S::scalar("dateadd", "timestamp"),
        S::scalar("date_add", "arg0"),
        S::scalar("datediff", "bigint"),
        S::scalar("date_diff", "bigint"),
        S::scalar("timestampadd", "timestamp"),
        S::scalar("timestampdiff", "bigint"),
        S::scalar("convert_timezone", "timestamp"),
        S::scalar("last_day", "date"),
        S::scalar("dayofweek", "bigint"),
        S::scalar("dayname", "varchar"),
        S::scalar("monthname", "varchar"),
        S::scalar("to_varchar", "varchar"),
        S::scalar("to_number", "double"),
        S::scalar("to_decimal", "double"),
        S::scalar("try_to_number", "double"),
        S::scalar("try_cast_str", "varchar"),
        S::scalar("hash", "bigint"),
        S::scalar("equal_null", "boolean"),
        S::scalar("regexp_substr", "varchar"),
        S::scalar("regexp_extract", "varchar"),
        S::scalar("parse_json", "varchar"),
        S::scalar("try_parse_json", "varchar"),
        S::scalar("json_extract_path_text", "varchar"),
        S::scalar("json_extract_scalar", "varchar"),
        S::scalar("json_value", "varchar"),
        S::scalar("get_path", "varchar"),
        S::scalar("object_construct", "varchar"),
        S::scalar("array_size", "bigint"),
        S::scalar("generate_uuid", "varchar"),
        S::scalar("uuid_string", "varchar"),
        S::aggregate("any_value", "arg0", true),
        S::aggregate("listagg", "varchar", false),
        S::aggregate("count_if", "bigint", false),
        S::aggregate("countif", "bigint", false),
        S::aggregate("boolor_agg", "boolean", true),
        S::aggregate("booland_agg", "boolean", true),
        S::aggregate("logical_or", "boolean", true),
        S::aggregate("logical_and", "boolean", true),
        S::aggregate("approx_count_distinct", "bigint", true),
        S::aggregate("arbitrary", "arg0", true),
    ];
    match adapter.to_ascii_lowercase().as_str() {
        "snowflake" => v.extend([
            S::scalar("to_timestamp_ntz", "timestamp"),
            S::scalar("to_timestamp_tz", "timestamp_tz"),
            S::scalar("try_to_date", "date"),
            S::scalar("try_to_timestamp", "timestamp"),
            S::scalar("sysdate", "timestamp"),
            S::scalar("md5_hex", "varchar"),
            S::scalar("sha2_hex", "varchar"),
            S::scalar("array_construct", "array"),
            S::scalar("array_contains", "boolean"),
            S::scalar("strtok", "varchar"),
            S::scalar("split_to_table", "varchar"),
            S::aggregate("array_agg_distinct", "array", true),
            S::aggregate("object_agg", "varchar", false),
            S::aggregate("hll", "bigint", true),
        ]),
        "bigquery" => v.extend([
            S::scalar("safe_cast", "arg0"),
            S::scalar("safe_divide", "double"),
            S::scalar("timestamp_trunc", "timestamp"),
            S::scalar("datetime_trunc", "timestamp"),
            S::scalar("timestamp_add", "timestamp"),
            S::scalar("timestamp_sub", "timestamp"),
            S::scalar("timestamp_diff", "bigint"),
            S::scalar("datetime_diff", "bigint"),
            S::scalar("date_sub", "date"),
            S::scalar("format_date", "varchar"),
            S::scalar("format_timestamp", "varchar"),
            S::scalar("parse_date", "date"),
            S::scalar("parse_timestamp", "timestamp"),
            S::scalar("farm_fingerprint", "bigint"),
            S::scalar("generate_date_array", "array"),
            S::aggregate("approx_quantiles", "array", false),
        ]),
        "databricks" | "spark" => v.extend([
            S::scalar("date_format", "varchar"),
            S::scalar("from_utc_timestamp", "timestamp"),
            S::scalar("to_utc_timestamp", "timestamp"),
            S::scalar("unix_timestamp", "bigint"),
            S::scalar("from_unixtime", "varchar"),
            S::scalar("collect_list_size", "bigint"),
            S::scalar("get_json_object", "varchar"),
            S::scalar("sha2", "varchar"),
            S::aggregate("collect_list", "array", false),
            S::aggregate("collect_set", "array", true),
            S::aggregate("first", "arg0", true),
            S::aggregate("last", "arg0", true),
        ]),
        "postgres" | "redshift" => v.extend([
            S::scalar("age", "interval"),
            S::scalar("getdate", "timestamp"),
            S::scalar("json_extract_path", "varchar"),
            S::scalar("jsonb_extract_path_text", "varchar"),
            S::scalar("generate_series", "bigint"),
            S::aggregate("jsonb_agg", "varchar", false),
            S::aggregate("json_agg", "varchar", false),
        ]),
        "duckdb" => v.extend([
            S::scalar("strftime", "varchar"),
            S::scalar("strptime", "timestamp"),
            S::scalar("epoch", "bigint"),
            S::scalar("epoch_ms", "timestamp"),
            S::scalar("list_value", "array"),
            S::scalar("list_contains", "boolean"),
            S::scalar("hash", "bigint"),
            S::aggregate("list", "array", false),
            S::aggregate("histogram", "varchar", false),
            S::aggregate("arg_max", "arg0", true),
            S::aggregate("arg_min", "arg0", true),
        ]),
        _ => {}
    }
    v
}

fn signature(deterministic: bool) -> Signature {
    Signature::one_of(
        vec![TypeSignature::Nullary, TypeSignature::VariadicAny],
        if deterministic {
            Volatility::Immutable
        } else {
            Volatility::Volatile
        },
    )
}

fn resolve_return(returns: &Option<DataType>, args: &[DataType]) -> DataType {
    match returns {
        Some(t) => t.clone(),
        None => args.first().cloned().unwrap_or(DataType::Utf8),
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct StubScalar {
    name: String,
    signature: Signature,
    returns: Option<DataType>,
}

impl ScalarUDFImpl for StubScalar {
    fn name(&self) -> &str {
        &self.name
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, arg_types: &[DataType]) -> Result<DataType> {
        Ok(resolve_return(&self.returns, arg_types))
    }
    fn invoke_with_args(&self, _args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        not_impl_err!("{} is a Cardinal stub and is never executed", self.name)
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct StubAggregate {
    name: String,
    signature: Signature,
    returns: Option<DataType>,
}

impl AggregateUDFImpl for StubAggregate {
    fn name(&self) -> &str {
        &self.name
    }
    fn signature(&self) -> &Signature {
        &self.signature
    }
    fn return_type(&self, arg_types: &[DataType]) -> Result<DataType> {
        Ok(resolve_return(&self.returns, arg_types))
    }
    fn accumulator(&self, _acc_args: AccumulatorArgs) -> Result<Box<dyn Accumulator>> {
        not_impl_err!("{} is a Cardinal stub and is never executed", self.name)
    }
}

pub enum BuiltStub {
    Scalar(Arc<ScalarUDF>),
    Aggregate(Arc<AggregateUDF>),
}

pub fn build(spec: &StubSpec) -> BuiltStub {
    let name = spec.name.to_ascii_lowercase();
    let returns = stub_return_type(&spec.returns);
    let signature = signature(spec.deterministic);
    match spec.kind {
        StubKind::Scalar => BuiltStub::Scalar(Arc::new(ScalarUDF::new_from_impl(StubScalar {
            name,
            signature,
            returns,
        }))),
        StubKind::Aggregate => {
            BuiltStub::Aggregate(Arc::new(AggregateUDF::new_from_impl(StubAggregate {
                name,
                signature,
                returns,
            })))
        }
    }
}
