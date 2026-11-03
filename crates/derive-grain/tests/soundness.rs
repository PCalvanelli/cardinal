//! Soundness property tests. proptest generates small tables that satisfy their
//! declared keys; a fixed corpus of query shapes runs through DataFusion's executor;
//! every key, dependency and non-null fact Derive infers must hold in the real output.
//! Any failure is a release blocker. The harness also logs where DataFusion's own
//! functional dependencies find a key Derive missed, as a list of inference gaps.

use datafusion::arrow::array::{ArrayRef, Int64Array, RecordBatch};
use datafusion::arrow::datatypes::{DataType, Field, Schema};
use datafusion::arrow::util::display::array_value_to_string;
use datafusion::datasource::MemTable;
use datafusion::prelude::SessionContext;
use derive_grain::testing::{Tables, analyze_sql, key, no_key};
use derive_grain::{ColSet, RelationFacts};
use proptest::prelude::*;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, OnceLock};

struct TableDef {
    name: &'static str,
    cols: &'static [&'static str],
    key: Option<&'static [&'static str]>,
}

const TABLES: &[TableDef] = &[
    TableDef {
        name: "a",
        cols: &["id", "x", "y"],
        key: Some(&["id"]),
    },
    TableDef {
        name: "b",
        cols: &["id", "a_id", "z"],
        key: Some(&["id"]),
    },
    TableDef {
        name: "c",
        cols: &["a_id", "k", "w"],
        key: Some(&["a_id", "k"]),
    },
    TableDef {
        name: "u",
        cols: &["p", "q"],
        key: None,
    },
];

/// (SQL, first output column) for each base shape; wrappers multiply these.
const BASES: &[(&str, &str)] = &[
    ("select * from a", "id"),
    ("select id, x from a", "id"),
    ("select x, y from a", "x"),
    ("select id * 2 as d, x from a", "d"),
    ("select coalesce(x, 0) as x0, id from a", "x0"),
    (
        "select a.id, b.id as bid, b.z from a join b on a.id = b.a_id",
        "id",
    ),
    ("select a.*, b.z from b join a on b.a_id = a.id", "id"),
    ("select a.id, c.k, c.w from a join c on a.id = c.a_id", "id"),
    (
        "select a.id, a.x, c.k from a left join c on a.id = c.a_id",
        "id",
    ),
    (
        "select c.a_id, c.k, a.x from c left join a on c.a_id = a.id",
        "a_id",
    ),
    ("select b.id, c.k from b join c on b.a_id = c.a_id", "id"),
    ("select a.id, u.q from a join u on a.id = u.p", "id"),
    ("select a.id, u.q from a left join u on a.id = u.p", "id"),
    (
        "select a.id as aid, b.id as bid from a full join b on a.id = b.id",
        "aid",
    ),
    ("select a.id, b.id as bid from a cross join b", "id"),
    ("select a.id, b.id as bid from a join b on a.x < b.z", "id"),
    (
        "select a.id, b.z from a join b on a.id = b.a_id and b.z > 1",
        "id",
    ),
    (
        "select b.id, b.a_id, a.x from b left join a on b.a_id = a.id and a.y > 0",
        "id",
    ),
    (
        "select a.id, b.id as bid from a left join b on a.id = b.a_id",
        "id",
    ),
    (
        "select a.id, b.id as bid from a right join b on a.id = b.a_id",
        "id",
    ),
    (
        "select a1.id, a2.id as id2 from a a1 join a a2 on a1.x = a2.x",
        "id",
    ),
    (
        "select a.id, a.x, b.id as bid from a join b using (id)",
        "id",
    ),
    ("select * from a where x = 2", "id"),
    ("select * from c where a_id = 1", "a_id"),
    ("select * from a where id = x", "id"),
    ("select * from a where x is not null and y > 0", "id"),
    ("select x, count(*) as n from a group by x", "x"),
    ("select x, y, sum(id) as s from a group by x, y", "x"),
    ("select id, x, count(*) as n from a group by id, x", "id"),
    ("select count(*) as n from a", "n"),
    ("select a_id, max(w) as m from c group by a_id", "a_id"),
    ("select p, count(*) as n from u group by p", "p"),
    ("select distinct p, q from u", "p"),
    ("select distinct x from a", "x"),
    ("select distinct id, x from a", "id"),
    (
        "select id, 'a' as src from a union all select id, 'b' as src from b",
        "id",
    ),
    ("select id from a union all select id from b", "id"),
    ("select id from a union select id from b", "id"),
    (
        "select p, q from (select *, row_number() over (partition by p order by q) as rn from u) t where rn = 1",
        "p",
    ),
    (
        "select p, q from u qualify row_number() over (partition by p order by q) = 1",
        "p",
    ),
    (
        "select p, q, row_number() over (partition by p order by q) as rn from u",
        "p",
    ),
    ("select p, sum(q) over (partition by p) as s from u", "p"),
    (
        "select id, x, rank() over (partition by x order by id) as r from a",
        "id",
    ),
    ("select id, x from a order by x limit 1", "id"),
    ("select * from u limit 1", "p"),
    (
        "select a.id, s.n from a join (select a_id, count(*) as n from c group by a_id) s on a.id = s.a_id",
        "id",
    ),
    (
        "select a.id from a where a.id in (select a_id from c)",
        "id",
    ),
    (
        "select a.id, x from a where exists (select 1 from b where b.a_id = a.id)",
        "id",
    ),
    (
        "with t as (select a_id, k from c where w > 0) select t.a_id, t.k, a.y from t join a on t.a_id = a.id",
        "a_id",
    ),
    (
        "select a.x, c.k, sum(c.w) as w from a join c on a.id = c.a_id group by a.x, c.k",
        "x",
    ),
    (
        "select a.id, b.id as bid, c.k from a join b on a.id = b.a_id join c on a.id = c.a_id",
        "id",
    ),
    (
        "select a.id, b.id as bid from a, b where a.id = b.a_id",
        "id",
    ),
    (
        "select id, case when x > 1 then 'hi' else 'lo' end as band from a",
        "id",
    ),
];

fn wrap(sql: &str, first: &str) -> Vec<String> {
    vec![
        sql.to_string(),
        format!("select * from ({sql}) t where {first} is not null"),
        format!("select distinct * from ({sql}) t"),
        format!("select {first}, count(*) as n_ from ({sql}) t group by {first}"),
        format!("select * from ({sql}) t limit 1"),
        format!(
            "select * from ({sql}) t qualify row_number() over (partition by {first} order by {first}) = 1"
        ),
    ]
}

fn corpus() -> Vec<String> {
    BASES.iter().flat_map(|(s, f)| wrap(s, f)).collect()
}

fn derive_tables() -> Tables {
    let mut t = Tables::default();
    for d in TABLES {
        t.add(
            d.name,
            d.cols,
            &[match d.key {
                Some(k) => key(k),
                None => no_key(),
            }],
        );
    }
    t
}

struct Analyzed {
    sql: String,
    facts: RelationFacts,
}

fn analyzed() -> &'static Vec<Analyzed> {
    static A: OnceLock<Vec<Analyzed>> = OnceLock::new();
    A.get_or_init(|| {
        let tables = derive_tables();
        corpus()
            .into_iter()
            .map(|sql| {
                let m = analyze_sql(&tables, &sql);
                let facts = (**m.output()).clone();
                Analyzed { sql, facts }
            })
            .collect()
    })
}

type Rows = Vec<Vec<Option<i64>>>;

/// Rows for a table with unique, non-null key columns and small, nullable other values.
fn table_strategy(def: &'static TableDef) -> impl Strategy<Value = Rows> {
    let width = def.cols.len();
    let key_idx: Vec<usize> = def
        .key
        .unwrap_or(&[])
        .iter()
        .map(|k| def.cols.iter().position(|c| c == k).unwrap())
        .collect();
    let cell = prop_oneof![4 => (0i64..4).prop_map(Some), 1 => Just(None)];
    proptest::collection::vec(proptest::collection::vec(cell, width), 0..7).prop_map(move |rows| {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for mut r in rows {
            for &k in &key_idx {
                r[k] = Some(r[k].unwrap_or(0));
            }
            let kv: Vec<Option<i64>> = key_idx.iter().map(|&k| r[k]).collect();
            if key_idx.is_empty() || seen.insert(kv) {
                out.push(r);
            }
        }
        out
    })
}

fn batch(def: &TableDef, rows: &Rows) -> (Arc<Schema>, RecordBatch) {
    let schema = Arc::new(Schema::new(
        def.cols
            .iter()
            .map(|c| Field::new(*c, DataType::Int64, true))
            .collect::<Vec<_>>(),
    ));
    let cols: Vec<ArrayRef> = (0..def.cols.len())
        .map(|i| {
            Arc::new(Int64Array::from(
                rows.iter().map(|r| r[i]).collect::<Vec<_>>(),
            )) as ArrayRef
        })
        .collect();
    let b = RecordBatch::try_new(schema.clone(), cols).unwrap();
    (schema, b)
}

fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    })
}

fn execute(data: &[Rows], sql: &str) -> Vec<Vec<String>> {
    rt().block_on(async {
        let ctx = SessionContext::new();
        for (def, rows) in TABLES.iter().zip(data) {
            let (schema, b) = batch(def, rows);
            ctx.register_table(
                def.name,
                Arc::new(MemTable::try_new(schema, vec![vec![b]]).unwrap()),
            )
            .unwrap();
        }
        let batches = ctx
            .sql(sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .collect()
            .await
            .unwrap();
        let mut rows = Vec::new();
        for b in batches {
            for r in 0..b.num_rows() {
                rows.push(
                    b.columns()
                        .iter()
                        .map(|c| {
                            if c.is_null(r) {
                                "∅".to_string()
                            } else {
                                array_value_to_string(c, r).unwrap()
                            }
                        })
                        .collect(),
                );
            }
        }
        rows
    })
}

fn project(row: &[String], cols: &ColSet) -> Vec<String> {
    cols.iter().map(|c| row[*c].clone()).collect()
}

fn check(a: &Analyzed, rows: &[Vec<String>]) -> Result<(), String> {
    let f = &a.facts;
    for k in &f.keys {
        let mut seen = BTreeSet::new();
        for r in rows {
            if !seen.insert(project(r, &k.cols)) {
                return Err(format!("key {:?} has duplicates in {rows:?}", k.cols));
            }
        }
    }
    for d in &f.fds {
        let mut m: HashMap<Vec<String>, Vec<String>> = HashMap::new();
        for r in rows {
            let l = project(r, &d.lhs);
            let rv = project(r, &d.rhs);
            if let Some(prev) = m.insert(l.clone(), rv.clone())
                && prev != rv
            {
                return Err(format!(
                    "dependency {:?} -> {:?} fails: {l:?} maps to {prev:?} and {rv:?}",
                    d.lhs, d.rhs
                ));
            }
        }
    }
    for c in &f.not_null {
        if rows.iter().any(|r| r[*c] == "∅") {
            return Err(format!("column {c} inferred non-null has a null"));
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    #[test]
    fn inferred_facts_hold(
        a in table_strategy(&TABLES[0]),
        b in table_strategy(&TABLES[1]),
        c in table_strategy(&TABLES[2]),
        u in table_strategy(&TABLES[3]),
    ) {
        let data = vec![a, b, c, u];
        for q in analyzed() {
            let rows = execute(&data, &q.sql);
            if let Err(e) = check(q, &rows) {
                prop_assert!(false, "unsound for `{}`: {e}\ninput: {data:?}", q.sql);
            }
        }
    }
}

#[test]
fn corpus_size() {
    assert!(
        analyzed().len() >= 300,
        "corpus has {} shapes",
        analyzed().len()
    );
}

/// Inference gaps: keys DataFusion's own functional dependencies find that Derive misses.
/// Logged, not asserted; each is a rule to strengthen.
#[test]
fn log_inference_gaps() {
    let gaps = rt().block_on(async {
        let ctx = SessionContext::new();
        for def in TABLES {
            let (schema, b) = batch(def, &vec![]);
            ctx.register_table(
                def.name,
                Arc::new(MemTable::try_new(schema, vec![vec![b]]).unwrap()),
            )
            .unwrap();
        }
        let mut gaps = Vec::new();
        for q in analyzed() {
            let Ok(plan) = ctx.state().create_logical_plan(&q.sql).await else {
                continue;
            };
            let width = plan.schema().fields().len();
            for dep in plan.schema().functional_dependencies().iter() {
                let src: ColSet = dep.source_indices.iter().copied().collect();
                let covers_all = dep.target_indices.len() == width;
                if covers_all && !dep.nullable && !q.facts.is_superkey(&src) {
                    gaps.push(format!("{:?} in `{}`", src, q.sql));
                }
            }
        }
        gaps
    });
    eprintln!(
        "{} inference gaps vs DataFusion (note DataFusion has no declared keys here):",
        gaps.len()
    );
    for g in gaps.iter().take(20) {
        eprintln!("  {g}");
    }
}
