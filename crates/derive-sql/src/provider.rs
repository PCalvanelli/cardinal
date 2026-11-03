use crate::stubs::{self, BuiltStub, StubSpec};
use datafusion_common::arrow::datatypes::{DataType, FieldRef, SchemaRef};
use datafusion_common::config::ConfigOptions;
use datafusion_common::{Result, TableReference, plan_err};
use datafusion_expr::planner::{ContextProvider, ExprPlanner};
use datafusion_expr::{AggregateUDF, HigherOrderUDF, ScalarUDF, TableSource, WindowUDF};
use std::collections::HashMap;
use std::sync::Arc;

/// A relation as the planner sees it: an upstream model, source, seed or snapshot.
#[derive(Debug)]
pub struct DeriveTable {
    /// The dbt unique id, e.g. "model.shop.order_lines".
    pub relation: String,
    pub schema: SchemaRef,
}

impl TableSource for DeriveTable {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }
}

/// Resolves relation names in compiled SQL. Parts are lowercased and unquoted.
pub trait TableLookup: Sync {
    fn lookup(&self, parts: &[String]) -> Option<Arc<DeriveTable>>;
}

/// Where an aggregate function came from, which decides how G001 treats it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FunctionOrigin {
    Builtin,
    AdapterStub,
    ConfigStub,
}

/// DataFusion built-ins plus stubs, built once per run and shared across threads.
pub struct Functions {
    scalars: HashMap<String, Arc<ScalarUDF>>,
    aggregates: HashMap<String, Arc<AggregateUDF>>,
    windows: HashMap<String, Arc<WindowUDF>>,
    higher_order: HashMap<String, Arc<HigherOrderUDF>>,
    planners: Vec<Arc<dyn ExprPlanner>>,
    /// Stub aggregates: name -> (origin, idempotent).
    pub aggregate_stubs: HashMap<String, (FunctionOrigin, bool)>,
    options: ConfigOptions,
}

impl Functions {
    pub fn new(adapter: &str, user_stubs: &[StubSpec]) -> Self {
        let mut f = Functions {
            scalars: HashMap::new(),
            aggregates: HashMap::new(),
            windows: HashMap::new(),
            higher_order: HashMap::new(),
            planners: vec![
                Arc::new(datafusion_functions::core::planner::CoreFunctionPlanner::default()),
                Arc::new(datafusion_functions::unicode::planner::UnicodeFunctionPlanner),
                Arc::new(datafusion_functions::datetime::planner::DatetimeFunctionPlanner),
                Arc::new(datafusion_functions_nested::planner::NestedFunctionPlanner),
                Arc::new(datafusion_functions_nested::planner::FieldAccessPlanner),
                Arc::new(datafusion_functions_aggregate::planner::AggregateFunctionPlanner),
                Arc::new(datafusion_functions_window::planner::WindowFunctionPlanner),
            ],
            aggregate_stubs: HashMap::new(),
            options: ConfigOptions::default(),
        };
        for u in datafusion_functions::all_default_functions()
            .into_iter()
            .chain(datafusion_functions_nested::all_default_nested_functions())
        {
            for n in std::iter::once(u.name().to_string()).chain(u.aliases().iter().cloned()) {
                f.scalars.insert(n, u.clone());
            }
        }
        for u in datafusion_functions_nested::all_default_higher_order_functions() {
            f.higher_order.insert(u.name().to_string(), u.clone());
        }
        for u in datafusion_functions_aggregate::all_default_aggregate_functions() {
            for n in std::iter::once(u.name().to_string()).chain(u.aliases().iter().cloned()) {
                f.aggregates.insert(n, u.clone());
            }
        }
        for u in datafusion_functions_window::all_default_window_functions() {
            for n in std::iter::once(u.name().to_string()).chain(u.aliases().iter().cloned()) {
                f.windows.insert(n, u.clone());
            }
        }
        // Lookup order: built-ins, then the adapter's stub pack, then derive.toml stubs.
        // A derive.toml stub may replace an adapter stub but never a built-in.
        for (specs, origin) in [
            (stubs::adapter_pack(adapter), FunctionOrigin::AdapterStub),
            (user_stubs.to_vec(), FunctionOrigin::ConfigStub),
        ] {
            for spec in specs {
                let name = spec.name.to_ascii_lowercase();
                if f.is_builtin(&name) {
                    continue;
                }
                match stubs::build(&spec) {
                    BuiltStub::Scalar(u) => {
                        f.scalars.insert(name, u);
                    }
                    BuiltStub::Aggregate(u) => {
                        f.aggregate_stubs
                            .insert(name.clone(), (origin, spec.idempotent));
                        f.aggregates.insert(name, u);
                    }
                }
            }
        }
        f
    }

    fn is_builtin(&self, name: &str) -> bool {
        (self.scalars.contains_key(name)
            || self.aggregates.contains_key(name)
            || self.windows.contains_key(name))
            && !self.aggregate_stubs.contains_key(name)
    }
}

/// The planner's view of one model: functions plus the relations it can read.
pub struct DeriveContext<'a> {
    pub functions: &'a Functions,
    pub tables: &'a dyn TableLookup,
}

impl ContextProvider for DeriveContext<'_> {
    fn get_table_source(&self, name: TableReference) -> Result<Arc<dyn TableSource>> {
        let parts: Vec<String> = match &name {
            TableReference::Bare { table } => vec![table.to_lowercase()],
            TableReference::Partial { schema, table } => {
                vec![schema.to_lowercase(), table.to_lowercase()]
            }
            TableReference::Full {
                catalog,
                schema,
                table,
            } => {
                vec![
                    catalog.to_lowercase(),
                    schema.to_lowercase(),
                    table.to_lowercase(),
                ]
            }
        };
        match self.tables.lookup(&parts) {
            Some(t) => Ok(t),
            None => plan_err!("derive: unresolved relation {}", parts.join(".")),
        }
    }

    fn get_expr_planners(&self) -> &[Arc<dyn ExprPlanner>] {
        &self.functions.planners
    }

    fn get_function_meta(&self, name: &str) -> Option<Arc<ScalarUDF>> {
        self.functions
            .scalars
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
    fn get_higher_order_meta(&self, name: &str) -> Option<Arc<HigherOrderUDF>> {
        self.functions
            .higher_order
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
    fn get_aggregate_meta(&self, name: &str) -> Option<Arc<AggregateUDF>> {
        self.functions
            .aggregates
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
    fn get_window_meta(&self, name: &str) -> Option<Arc<WindowUDF>> {
        self.functions
            .windows
            .get(&name.to_ascii_lowercase())
            .cloned()
    }
    fn get_variable_type(&self, _variable_names: &[String]) -> Option<DataType> {
        None
    }
    fn get_variable_field(&self, _variable_names: &[String]) -> Option<FieldRef> {
        None
    }
    fn options(&self) -> &ConfigOptions {
        &self.functions.options
    }
    fn udf_names(&self) -> Vec<String> {
        self.functions.scalars.keys().cloned().collect()
    }
    fn higher_order_function_names(&self) -> Vec<String> {
        self.functions.higher_order.keys().cloned().collect()
    }
    fn udaf_names(&self) -> Vec<String> {
        self.functions.aggregates.keys().cloned().collect()
    }
    fn udwf_names(&self) -> Vec<String> {
        self.functions.windows.keys().cloned().collect()
    }
}
