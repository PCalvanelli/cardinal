//! Test support: table-driven grain cases. SQL and declared keys in, expected keys and
//! native grains out.

use crate::analyze::{AnalyzedModel, UpstreamFacts, analyze};
use crate::facts::GrainStatus;
use crate::summary::{RelationSummary, SummaryColumn, SummaryKey};
use cardinal_dbt::{DeclOrigin, RelationId};
use cardinal_sql::provider::{Functions, TableLookup};
use cardinal_sql::{CardinalContext, CardinalTable, plan_sql};
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
                assumptions: vec![],
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
                not_null_why: Default::default(),
                covers: vec![],
                within: vec![],
                ranks: vec![],
                losses: vec![],
                measures: vec![],
                units: vec![],
                aggregated: vec![],
            },
        );
        self.by_name.insert(name.to_string(), id);
    }

    /// Declare non-null columns and references (cols of `name` appear in `to.to_cols`)
    /// on a table added earlier.
    pub fn declare(&mut self, name: &str, not_null: &[&str], refs: &[(&[&str], &str, &[&str])]) {
        let id = self.by_name[name].clone();
        let to_ids: Vec<RelationId> = refs
            .iter()
            .map(|(_, to, _)| self.by_name[*to].clone())
            .collect();
        let s = self.summaries.get_mut(&id).expect("table");
        let pos = |s: &RelationSummary, c: &str| s.col(c).expect("column");
        for c in not_null {
            let i = pos(s, c);
            if !s.not_null.contains(&i) {
                s.not_null.push(i);
            }
            s.not_null_why.insert(
                i,
                vec![cardinal_dbt::Assumption::NotNull {
                    relation: id.clone(),
                    column: c.to_string(),
                }],
            );
        }
        for ((cols, _, to_cols), to) in refs.iter().zip(to_ids) {
            let mut pairs: Vec<(usize, String)> = cols
                .iter()
                .zip(to_cols.iter())
                .map(|(c, t)| (pos(s, c), t.to_string()))
                .collect();
            pairs.sort();
            s.within.push(crate::summary::SummaryInd {
                relation: to.clone(),
                pairs,
                assumptions: vec![cardinal_dbt::Assumption::References {
                    relation: id.clone(),
                    cols: cols.iter().map(|c| c.to_string()).collect(),
                    to,
                    to_cols: to_cols.iter().map(|c| c.to_string()).collect(),
                }],
            });
        }
    }

    /// Declare a column's measure and unit on a table added earlier, the way
    /// meta.cardinal.measure and meta.cardinal.unit do.
    pub fn declare_measure(
        &mut self,
        name: &str,
        measures: &[cardinal_dbt::DeclaredMeasure],
        units: &[cardinal_dbt::DeclaredUnit],
    ) {
        let id = self.by_name[name].clone();
        let s = self.summaries.get_mut(&id).expect("table");
        let cols: Vec<cardinal_dbt::Column> = s
            .columns
            .iter()
            .map(|c| cardinal_dbt::Column {
                name: c.name.clone(),
                data_type: None,
            })
            .collect();
        let mut rel = test_relation(&id, name);
        rel.kind = cardinal_dbt::Kind::Source;
        rel.columns = cols;
        rel.measures = measures.to_vec();
        rel.units = units.to_vec();
        let mut decl = crate::summary::summary_from_declared(
            &rel,
            crate::facts::UnknownReason::SourceWithoutKey(id.clone()),
        );
        s.measures.append(&mut decl.measures);
        s.units.append(&mut decl.units);
    }

    pub fn id(&self, name: &str) -> RelationId {
        self.by_name[name].clone()
    }

    pub fn insert_summary(&mut self, name: &str, s: RelationSummary) {
        self.by_name.insert(name.to_string(), s.relation.clone());
        self.summaries.insert(s.relation.clone(), s);
    }
}

impl TableLookup for Tables {
    fn lookup(&self, parts: &[String]) -> Option<Arc<CardinalTable>> {
        let id = self.by_name.get(parts.last()?)?;
        let s = self.summaries.get(id)?;
        Some(Arc::new(CardinalTable {
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

/// A bare model relation for tests.
fn test_relation(id: &RelationId, name: &str) -> cardinal_dbt::Relation {
    cardinal_dbt::Relation {
        id: id.clone(),
        kind: cardinal_dbt::Kind::Model {
            materialized: "view".into(),
        },
        display: name.into(),
        package: "test".into(),
        file: format!("models/{name}.sql"),
        compiled_sql: None,
        raw_sql: None,
        compiled_path: None,
        relation_parts: vec![name.into()],
        depends_on: vec![],
        columns: vec![],
        columns_from: cardinal_dbt::project::ColumnsFrom::Missing,
        declared_keys: vec![],
        not_null: Default::default(),
        references: vec![],
        exactly_once: vec![],
        measures: vec![],
        units: vec![],
        tests: vec![],
        contract_enforced: false,
        tags: vec![],
        incremental: None,
    }
}

pub fn functions() -> &'static Functions {
    static F: OnceLock<Functions> = OnceLock::new();
    F.get_or_init(|| Functions::new("duckdb", &[]))
}

/// Plan and analyze `sql` against `tables`.
pub fn analyze_sql(tables: &Tables, sql: &str) -> AnalyzedModel {
    let (d, _) = cardinal_sql::dialect::dialect_for_adapter("duckdb");
    let ctx = CardinalContext {
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
    pub fn declare(
        mut self,
        name: &str,
        not_null: &[&str],
        refs: &[(&[&str], &str, &[&str])],
    ) -> Self {
        self.tables.declare(name, not_null, refs);
        self
    }

    /// Declare `col` of `table` a stock over `over` (or non-additive, when `over` is
    /// empty).
    pub fn stock(mut self, table: &str, col: &str, over: &[&str]) -> Self {
        let kind = if over.is_empty() {
            cardinal_dbt::MeasureDecl::NonAdditive
        } else {
            cardinal_dbt::MeasureDecl::Stock {
                over: over.iter().map(|s| s.to_string()).collect(),
            }
        };
        self.tables.declare_measure(
            table,
            &[cardinal_dbt::DeclaredMeasure {
                column: col.into(),
                kind,
            }],
            &[],
        );
        self
    }

    /// Declare that `unit` holds the unit of `col` in `table`.
    pub fn unit(mut self, table: &str, col: &str, unit: &str) -> Self {
        self.tables.declare_measure(
            table,
            &[],
            &[cardinal_dbt::DeclaredUnit {
                column: col.into(),
                unit: unit.into(),
            }],
        );
        self
    }

    /// Analyze `sql` as an upstream model named `name`, and make its summary readable
    /// by later SQL, the way the engine passes facts between models.
    pub fn model(mut self, name: &str, sql: &str) -> Self {
        let m = analyze_sql(&self.tables, sql);
        let id = RelationId(format!("model.test.{name}"));
        let mut rel = test_relation(&id, name);
        rel.compiled_sql = Some(sql.into());
        rel.raw_sql = Some(sql.into());
        let schema = m.plan.schema().as_arrow().clone();
        let (summary, _) = crate::summary::summarize(&rel, &schema, m.output(), &m.arena);
        self.tables.insert_summary(name, summary);
        self
    }

    /// The verdict for `table` on the model.
    pub fn verdict(&self, table: &str) -> crate::verdict::Verdict {
        let m = self.run();
        let id = self.tables.id(table);
        crate::verdict::verdict(&m, &self.tables.summaries[&id], true)
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

    /// `col` is a distinct count whose shared columns are `agree_on`, or (None) carries
    /// no distinct-count fact.
    pub fn expect_distinct(self, col: &str, agree_on: Option<&[&str]>) -> Self {
        let m = self.run();
        let c = Self::col(&m, col);
        let got = m
            .output()
            .measures
            .iter()
            .find(|d| d.col == c && d.distinct().is_some())
            .and_then(|d| match &d.kind {
                crate::MeasureKind::NonAdditive { agree_on } => Some(Self::names(&m, agree_on)),
                _ => None,
            });
        let want = agree_on.map(|a| {
            let mut v: Vec<String> = a.iter().map(|s| s.to_string()).collect();
            v.sort();
            v
        });
        assert_eq!(
            got,
            want,
            "distinct count {col} for:\n{}\n{}",
            self.sql,
            m.plan.display_indent()
        );
        self
    }

    /// `col`'s measure, as text: "non-additive average", "stock over (d)", or "additive"
    /// when it carries no measure fact.
    pub fn expect_measure(self, col: &str, want: &str) -> Self {
        let m = self.run();
        let c = Self::col(&m, col);
        let got = match m.output().measures.iter().find(|d| d.col == c) {
            None => "additive".to_string(),
            Some(d) => {
                let what = match &*d.site {
                    crate::MeasureSite::Distinct(_) => "distinct count".to_string(),
                    crate::MeasureSite::Computed { what, .. } => what.label().to_string(),
                    crate::MeasureSite::Declared { .. } => "declared".to_string(),
                };
                match &d.kind {
                    crate::MeasureKind::NonAdditive { .. } => format!("non-additive {what}"),
                    crate::MeasureKind::Stock { over: Some(over) } => {
                        format!("stock {what} over ({})", Self::names(&m, over).join(", "))
                    }
                    crate::MeasureKind::Stock { over: None } => {
                        format!("stock {what} over unknown")
                    }
                }
            }
        };
        assert_eq!(
            got,
            want,
            "measure {col} for:\n{}\n{}",
            self.sql,
            m.plan.display_indent()
        );
        self
    }

    /// `col`'s unit: Some(Some(cols)) when these columns determine it, Some(None) when it
    /// varies and nothing says how, None when it carries no unit fact.
    pub fn expect_unit(self, col: &str, want: Option<Option<&[&str]>>) -> Self {
        let m = self.run();
        let c = Self::col(&m, col);
        let got = m
            .output()
            .units
            .iter()
            .find(|u| u.col == c)
            .map(|u| u.unit.as_ref().map(|x| Self::names(&m, x)));
        let want = want.map(|w| {
            w.map(|a| {
                let mut v: Vec<String> = a.iter().map(|s| s.to_string()).collect();
                v.sort();
                v
            })
        });
        assert_eq!(
            got,
            want,
            "unit of {col} for:\n{}\n{}",
            self.sql,
            m.plan.display_indent()
        );
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
