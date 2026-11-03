//! Test support: table-driven grain cases. SQL and declared keys in, expected keys and
//! native grains out.

use crate::analyze::{AnalyzedModel, UpstreamFacts, analyze};
use crate::facts::GrainStatus;
use crate::summary::{RelationSummary, SummaryColumn, SummaryKey};
use derive_dbt::{DeclOrigin, RelationId};
use derive_sql::provider::{Functions, TableLookup};
use derive_sql::{DeriveContext, DeriveTable, plan_sql};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

pub struct KeySpec(Option<Vec<String>>);

pub fn key(cols: &[&str]) -> KeySpec {
    KeySpec(Some(cols.iter().map(|s| s.to_string()).collect()))
}
pub fn no_key() -> KeySpec {
    KeySpec(None)
}

#[derive(Default)]
pub struct Tables {
    pub summaries: BTreeMap<RelationId, RelationSummary>,
    pub by_name: BTreeMap<String, RelationId>,
}

impl Tables {
    pub fn add(&mut self, name: &str, cols: &[&str], keys: &[KeySpec]) {
        let id = RelationId(format!("source.test.{name}"));
        let columns: Vec<SummaryColumn> = cols
            .iter()
            .map(|c| SummaryColumn {
                name: c.to_string(),
                data_type: "Int64".into(),
            })
            .collect();
        let keys: Vec<SummaryKey> = keys
            .iter()
            .filter_map(|k| k.0.as_ref())
            .map(|k| SummaryKey {
                cols: k
                    .iter()
                    .map(|c| cols.iter().position(|x| x == c).expect("key column"))
                    .collect(),
                why: String::new(),
                declared: Some(DeclOrigin::MetaGrain),
            })
            .collect();
        let status = if keys.is_empty() {
            GrainStatus::Unknown(crate::facts::UnknownReason::SourceWithoutKey(id.clone()))
        } else {
            GrainStatus::Known
        };
        self.summaries.insert(
            id.clone(),
            RelationSummary {
                relation: id.clone(),
                display: name.to_string(),
                repeats: vec![None; columns.len()],
                columns,
                keys,
                fds: vec![],
                not_null: vec![],
                status,
                columns_known: true,
            },
        );
        self.by_name.insert(name.to_string(), id);
    }

    pub fn insert_summary(&mut self, name: &str, s: RelationSummary) {
        self.by_name.insert(name.to_string(), s.relation.clone());
        self.summaries.insert(s.relation.clone(), s);
    }
}

impl TableLookup for Tables {
    fn lookup(&self, parts: &[String]) -> Option<Arc<DeriveTable>> {
        let id = self.by_name.get(parts.last()?)?;
        let s = self.summaries.get(id)?;
        Some(Arc::new(DeriveTable {
            relation: id.0.clone(),
            schema: s.schema(),
        }))
    }
}

impl UpstreamFacts for Tables {
    fn summary(&self, id: &RelationId) -> Option<&RelationSummary> {
        self.summaries.get(id)
    }
}

pub fn functions() -> &'static Functions {
    static F: OnceLock<Functions> = OnceLock::new();
    F.get_or_init(|| Functions::new("duckdb", &[]))
}

/// Plan and analyze `sql` against `tables`.
pub fn analyze_sql(tables: &Tables, sql: &str) -> AnalyzedModel {
    let (d, _) = derive_sql::dialect::dialect_for_adapter("duckdb");
    let ctx = DeriveContext {
        functions: functions(),
        tables,
    };
    let plan =
        plan_sql(sql, d.as_ref(), &ctx).unwrap_or_else(|e| panic!("plan failed: {e:?}\n{sql}"));
    analyze(
        RelationId("model.test.m".into()),
        "m".into(),
        sql.into(),
        plan,
        tables,
    )
}

pub struct GrainCase {
    tables: Tables,
    sql: String,
}

pub fn grain_case() -> GrainCase {
    GrainCase {
        tables: Tables::default(),
        sql: String::new(),
    }
}

impl GrainCase {
    pub fn table(mut self, name: &str, cols: &[&str], k: KeySpec) -> Self {
        self.tables.add(name, cols, &[k]);
        self
    }
    pub fn table_keys(mut self, name: &str, cols: &[&str], keys: &[KeySpec]) -> Self {
        self.tables.add(name, cols, keys);
        self
    }
    pub fn sql(mut self, sql: &str) -> Self {
        self.sql = sql.into();
        self
    }

    pub fn run(&self) -> AnalyzedModel {
        analyze_sql(&self.tables, &self.sql)
    }

    fn names(m: &AnalyzedModel, cols: &crate::ColSet) -> Vec<String> {
        let schema = m.plan.schema();
        let mut v: Vec<String> = cols
            .iter()
            .map(|c| schema.field(*c).name().clone())
            .collect();
        v.sort();
        v
    }

    fn col(m: &AnalyzedModel, name: &str) -> usize {
        m.plan
            .schema()
            .fields()
            .iter()
            .position(|f| f.name() == name)
            .unwrap_or_else(|| {
                panic!(
                    "no output column {name}: {:?}",
                    m.plan.schema().field_names()
                )
            })
    }

    pub fn expect_keys(self, expected: &[&[&str]]) -> Self {
        let m = self.run();
        let mut got: Vec<Vec<String>> = m
            .output()
            .keys
            .iter()
            .map(|k| Self::names(&m, &k.cols))
            .collect();
        got.sort();
        let mut want: Vec<Vec<String>> = expected
            .iter()
            .map(|k| {
                let mut v: Vec<String> = k.iter().map(|s| s.to_string()).collect();
                v.sort();
                v
            })
            .collect();
        want.sort();
        assert_eq!(
            got,
            want,
            "keys for:\n{}\n{}",
            self.sql,
            m.plan.display_indent()
        );
        self
    }

    pub fn expect_no_keys(self) -> Self {
        self.expect_keys(&[])
    }

    pub fn expect_native_grain(self, col: &str, grain: &[&str]) -> Self {
        let m = self.run();
        let c = Self::col(&m, col);
        let got = m.output().native_grain(c).map(|g| Self::names(&m, &g));
        let mut want: Vec<String> = grain.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(got, Some(want), "native grain of {col} for:\n{}", self.sql);
        self
    }

    pub fn expect_not_null(self, cols: &[&str]) -> Self {
        let m = self.run();
        for c in cols {
            let i = Self::col(&m, c);
            assert!(
                m.output().not_null.contains(&i),
                "{c} should be non-null in:\n{}",
                self.sql
            );
        }
        self
    }

    pub fn expect_status_known(self, known: bool) -> Self {
        let m = self.run();
        assert_eq!(
            m.output().status.is_known(),
            known,
            "status {:?} for:\n{}",
            m.output().status,
            self.sql
        );
        self
    }
}
