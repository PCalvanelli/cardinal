//! `cardinal guess`: likely keys for sources and seeds that declare none, and likely
//! references where one would let Cardinal prove a join keeps every row. Each guess
//! becomes a dbt singular test, so dbt checks it against the data; Cardinal never
//! connects to the warehouse.
//!
//! Key candidates, best first:
//!
//! - usage: columns other models join this table on, as the side expected to be unique
//!   (a column joined against another relation's key and named after that relation,
//!   like `customer_id` in orders, is a reference instead, and is left out);
//! - names: `<singular table>_id`, `<table>_id`, `id`, `<table>_key`, `uuid`,
//!   `_fivetran_id`, and `(<parent>_id, index | line_number | position)`.
//!
//! Columns a model deduplicates on with `row_number() ... = 1` repeat in the raw table,
//! so they are never guessed as its key.

use crate::engine::RunResult;
use cardinal_dbt::{Guess, GuessLog, Hypothesis, Kind, Project, Relation, RelationId};
use cardinal_sql::Tokens;
use std::collections::{BTreeMap, BTreeSet};

/// Tables to guess keys for, at most.
pub const MAX_TABLES: usize = 20;
/// Key candidates per table, at most.
pub const MAX_PER_TABLE: usize = 2;
/// Reference guesses, at most.
pub const MAX_REFERENCES: usize = 20;

/// A table without a known key, and the models it hides.
pub struct Keyless<'a> {
    pub relation: &'a Relation,
    pub hides: Vec<RelationId>,
}

/// How other models use a table's columns.
#[derive(Default, Debug)]
pub struct Usage {
    /// Column sets the table is joined on as the lookup side, and by which models.
    pub joined_on: BTreeMap<Vec<String>, BTreeSet<String>>,
    /// Column sets joined against another relation's key and named after it.
    pub references: BTreeSet<Vec<String>>,
    /// Column sets a model deduplicates the table on.
    pub deduped_on: BTreeSet<Vec<String>>,
}

/// Lowercase and drop a trailing plural: "categories" -> "category", "boxes" -> "box".
pub fn singular(name: &str) -> String {
    let n = name.to_lowercase();
    if let Some(s) = n.strip_suffix("ies") {
        format!("{s}y")
    } else if ["sses", "xes", "ches", "shes"]
        .iter()
        .any(|e| n.ends_with(e))
    {
        n[..n.len() - 2].to_string()
    } else if n.ends_with('s') && !n.ends_with("ss") {
        n[..n.len() - 1].to_string()
    } else {
        n
    }
}

/// The table part of a relation's display name: "app.customers" -> "customers".
fn table_name(rel: &Relation) -> &str {
    rel.display.rsplit('.').next().unwrap_or(&rel.display)
}

/// Score of a column name as the table's own identifier, best highest; 0 if it is not.
fn id_name_score(table: &str, col: &str) -> i32 {
    let t = table.to_lowercase();
    let s = singular(&t);
    if col == format!("{s}_id") || col == format!("{t}_id") {
        30
    } else if col == "id" {
        28
    } else if col == format!("{t}_key") || col == format!("{s}_key") {
        26
    } else if col == "uuid" {
        22
    } else if col == "_fivetran_id" {
        20
    } else {
        0
    }
}

fn plain(col: &str) -> bool {
    !col.is_empty()
        && col
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Key candidates for one table, best first, with why each is guessed.
pub fn key_candidates(rel: &Relation, usage: &Usage) -> Vec<(Vec<String>, Vec<String>)> {
    let table = table_name(rel);
    let cols: Vec<&str> = rel
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .filter(|c| plain(c))
        .collect();
    let has = |c: &str| cols.contains(&c);
    let mut scored: BTreeMap<Vec<String>, (i32, Vec<String>)> = BTreeMap::new();
    for c in &cols {
        let s = id_name_score(table, c);
        if s > 0 {
            scored.insert(
                vec![c.to_string()],
                (s, vec![format!("{c} is named like the table's id")]),
            );
        }
    }
    let own_id = |c: &str| id_name_score(table, c) > 0;
    for parent in cols.iter().filter(|c| c.ends_with("_id") && !own_id(c)) {
        for pos in ["index", "line_number", "position"] {
            if has(pos) {
                let mut k = vec![parent.to_string(), pos.to_string()];
                k.sort();
                scored.insert(
                    k,
                    (18, vec![format!("{pos} numbers the rows of each {parent}")]),
                );
            }
        }
    }
    for (k, models) in &usage.joined_on {
        if !k.iter().all(|c| has(c)) {
            continue;
        }
        let e = scored.entry(k.clone()).or_insert((0, vec![]));
        e.0 += 10 * models.len().min(3) as i32;
        e.1.push(format!(
            "{} {} on it",
            list(models),
            verb(models.len(), "joins", "join")
        ));
    }
    let mut out: Vec<(i32, Vec<String>, Vec<String>)> = scored
        .into_iter()
        .filter(|(k, _)| !usage.deduped_on.contains(k) && !usage.references.contains(k))
        .map(|(k, (s, why))| (s, k, why))
        .collect();
    out.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.len().cmp(&b.1.len()))
            .then(a.1.cmp(&b.1))
    });
    out.into_iter()
        .take(MAX_PER_TABLE)
        .map(|(_, k, why)| (k, why))
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", verb(n, one, many))
}

fn verb<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// "a", "a and b", "a, b and 2 more"
fn list(names: &BTreeSet<String>) -> String {
    let v: Vec<&String> = names.iter().collect();
    match v.len() {
        0 => String::new(),
        1 => v[0].clone(),
        2 => format!("{} and {}", v[0], v[1]),
        n => format!("{}, {} and {} more", v[0], v[1], n - 2),
    }
}

/// The `partition by` of one `row_number() over (...)`: its columns, without table
/// qualifiers, and the qualifier they share, if they have one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DedupPartition {
    pub cols: Vec<String>,
    pub qualifier: Option<String>,
}

/// The code tokens strictly between `open` and the `)` at `close`, with their depth inside
/// that pair of parentheses (a `(` has the depth outside it).
fn inside(t: &Tokens<'_>, open: usize, close: usize) -> Vec<(usize, i32)> {
    let mut depth = 0;
    let mut out = Vec::new();
    let mut i = open;
    while let Some(k) = t.next(i).filter(|&k| k < close) {
        if t.is_punct(k, ')') {
            depth -= 1;
        }
        out.push((k, depth));
        if t.is_punct(k, '(') {
            depth += 1;
        }
        i = k;
    }
    out
}

/// Where `first` then `second` start, as words at depth 0 of `toks`.
fn words_at_top(t: &Tokens<'_>, toks: &[(usize, i32)], first: &str, second: &str) -> Option<usize> {
    toks.windows(2).position(|w| {
        w[0].1 == 0 && w[1].1 == 0 && t.is_word(w[0].0, first) && t.is_word(w[1].0, second)
    })
}

/// A plain column reference, `col` or `q.col` (parts may be quoted): (qualifier, column).
fn column_ref(t: &Tokens<'_>, item: &[(usize, i32)]) -> Option<(Option<String>, String)> {
    if item.len().is_multiple_of(2) {
        return None;
    }
    let mut parts = Vec::new();
    for (n, &(i, _)) in item.iter().enumerate() {
        if n % 2 == 1 {
            if !t.is_punct(i, '.') {
                return None;
            }
        } else {
            parts.push(t.name(i).filter(|p| plain(p))?.to_string());
        }
    }
    let col = parts.pop()?;
    Some((parts.pop(), col))
}

/// The `partition by` of each `row_number() over (...)` in `sql`. Only plain column
/// references count: a partition on an expression, or on columns with different
/// qualifiers, is left out, as is a `row_number()` without `partition by`.
pub fn dedup_partitions(sql: &str) -> Vec<DedupPartition> {
    let lower = sql.to_lowercase();
    let t = Tokens::new(&lower);
    let punct = |i: Option<usize>, c| i.filter(|&k| t.is_punct(k, c));
    let mut out = Vec::new();
    for i in t.words("row_number") {
        // row_number ( ) over (
        let over = punct(punct(t.next(i), '(').and_then(|k| t.next(k)), ')')
            .and_then(|k| t.next(k))
            .filter(|&k| t.is_word(k, "over"));
        let Some(open) = punct(over.and_then(|k| t.next(k)), '(') else {
            continue;
        };
        let Some(close) = t.closing(open) else {
            continue;
        };
        let body = inside(&t, open, close);
        let Some(p) = words_at_top(&t, &body, "partition", "by") else {
            continue;
        };
        let rest = &body[p + 2..];
        let rest = &rest[..words_at_top(&t, rest, "order", "by").unwrap_or(rest.len())];
        let Some(refs) = rest
            .split(|&(k, d)| d == 0 && t.is_punct(k, ','))
            .map(|item| column_ref(&t, item))
            .collect::<Option<Vec<_>>>()
        else {
            continue;
        };
        let quals: BTreeSet<&Option<String>> = refs.iter().map(|(q, _)| q).collect();
        if quals.len() != 1 {
            continue;
        }
        let qualifier = refs[0].0.clone();
        let mut cols: Vec<String> = refs.into_iter().map(|(_, c)| c).collect();
        cols.sort();
        cols.dedup();
        if !cols.is_empty() {
            out.push(DedupPartition { cols, qualifier });
        }
    }
    out
}

const NOT_ALIAS: &[&str] = &[
    "where",
    "join",
    "on",
    "left",
    "right",
    "inner",
    "outer",
    "full",
    "cross",
    "natural",
    "group",
    "order",
    "limit",
    "union",
    "except",
    "intersect",
    "using",
    "having",
    "window",
    "qualify",
    "lateral",
    "select",
    "from",
    "with",
    "as",
];

/// Whether `sql` (lowercase) reads the table named `table` under the name `alias`: as
/// `table`, `"table"`, `... table alias` or `... table as alias`.
fn reads_as(sql: &Tokens<'_>, table: &str, alias: &str) -> bool {
    if table.is_empty() {
        return false;
    }
    if table == alias {
        return true;
    }
    let t = sql;
    (0..t.len())
        .filter(|&i| t.name(i) == Some(table))
        .filter_map(|i| t.next(i))
        .filter_map(|n| {
            if t.is_word(n, "as") {
                t.next(n)
            } else {
                Some(n)
            }
        })
        .any(|n| t.name(n) == Some(alias) && !NOT_ALIAS.contains(&alias))
}

/// The tables among `tables` that `model_sql` deduplicates on `p`: the one its
/// qualifier names, when that is clear; otherwise the only one with all its columns.
fn dedup_tables<'a>(
    model_sql: &str,
    p: &DedupPartition,
    tables: &[&'a Relation],
) -> Vec<&'a Relation> {
    let has_cols = |r: &Relation| {
        p.cols
            .iter()
            .all(|c| r.columns.iter().any(|x| &x.name == c))
    };
    let with_cols: Vec<&Relation> = tables.iter().copied().filter(|r| has_cols(r)).collect();
    if let Some(q) = &p.qualifier {
        let lower = model_sql.to_lowercase();
        let sql = Tokens::new(&lower);
        let named: Vec<&Relation> = tables
            .iter()
            .copied()
            .filter(|r| {
                let ident = r
                    .relation_parts
                    .last()
                    .cloned()
                    .unwrap_or_else(|| table_name(r).to_lowercase());
                reads_as(&sql, &ident, q)
            })
            .collect();
        if named.len() == 1 {
            return named.into_iter().filter(|r| has_cols(r)).collect();
        }
    }
    if with_cols.len() == 1 {
        with_cols
    } else {
        vec![]
    }
}

/// How every model uses each keyless table's columns, from the joins the analysis saw.
pub fn usage(
    project: &Project,
    run: &RunResult,
    tables: &BTreeSet<RelationId>,
) -> BTreeMap<RelationId, Usage> {
    let mut out: BTreeMap<RelationId, Usage> = BTreeMap::new();
    for (model, r) in &run.results {
        let Some(m) = project.get(model) else {
            continue;
        };
        for j in &r.joins {
            if j.sides.len() != 2 {
                continue;
            }
            for s in 0..2 {
                let (side, other) = (&j.sides[s], &j.sides[1 - s]);
                for t in tables {
                    // The table's column behind each join column, when every one has one.
                    let cols: Option<Vec<String>> = side
                        .from
                        .iter()
                        .map(|f| f.iter().find(|(rel, _)| rel == t).map(|(_, c)| c.clone()))
                        .collect();
                    let Some(mut cols) = cols.filter(|c| !c.is_empty()) else {
                        continue;
                    };
                    cols.sort();
                    cols.dedup();
                    let rel = project.get(t).unwrap();
                    let named_after_other = cols.len() == 1
                        && cols[0].ends_with("_id")
                        && id_name_score(table_name(rel), &cols[0]) == 0;
                    let u = out.entry(t.clone()).or_default();
                    if other.unique && named_after_other {
                        u.references.insert(cols);
                    } else {
                        u.joined_on
                            .entry(cols)
                            .or_default()
                            .insert(m.display.clone());
                    }
                }
            }
        }
        if let Some(sql) = &m.compiled_sql {
            let read: Vec<&Relation> = m
                .depends_on
                .iter()
                .filter(|d| tables.contains(*d))
                .filter_map(|d| project.get(d))
                .collect();
            for p in dedup_partitions(sql) {
                for rel in dedup_tables(sql, &p, &read) {
                    out.entry(rel.id.clone())
                        .or_default()
                        .deduped_on
                        .insert(p.cols.clone());
                }
            }
        }
    }
    out
}

/// Every guess worth making, keys first (most models hidden first), then references.
pub fn propose(
    project: &Project,
    run: &RunResult,
    keyless: &[Keyless<'_>],
    log: &GuessLog,
) -> (Vec<Guess>, Vec<RelationId>) {
    let display = |id: &RelationId| project.display(id);
    let failed = |h: &Hypothesis| log.get(h).is_some_and(|g| !g.passed);
    let tables: BTreeSet<RelationId> = keyless.iter().map(|k| k.relation.id.clone()).collect();
    let usage = usage(project, run, &tables);
    let empty = Usage::default();
    let mut guesses = Vec::new();
    let mut no_candidate = Vec::new();
    // The key each table is guessed to have, for reference guesses into it.
    let mut pending: BTreeMap<RelationId, Vec<Vec<String>>> = BTreeMap::new();
    for k in keyless.iter().take(MAX_TABLES) {
        let rel = k.relation;
        let cands: Vec<(Vec<String>, Vec<String>)> =
            key_candidates(rel, usage.get(&rel.id).unwrap_or(&empty))
                .into_iter()
                .filter(|(cols, _)| {
                    !failed(&Hypothesis::Key {
                        relation: rel.id.clone(),
                        columns: cols.clone(),
                    })
                })
                .collect();
        if cands.is_empty() {
            no_candidate.push(rel.id.clone());
        }
        for (cols, why) in cands {
            let h = Hypothesis::Key {
                relation: rel.id.clone(),
                columns: cols.clone(),
            };
            pending.entry(rel.id.clone()).or_default().push(cols);
            guesses.push(Guess {
                test_name: h.test_name(&display),
                hypothesis: h,
                why,
                unlocks: format!(
                    "lets Cardinal check {} that {} on it",
                    plural(k.hides.len(), "model", "models"),
                    verb(k.hides.len(), "depends", "depend")
                ),
            });
        }
    }
    let preferred = |cands: &[(RelationId, String)], not: Option<&RelationId>| {
        let ok: Vec<&(RelationId, String)> = cands
            .iter()
            .filter(|(r, _)| Some(r) != not && project.get(r).is_some())
            .collect();
        ok.iter()
            .find(|(r, _)| matches!(project.get(r).unwrap().kind, Kind::Source | Kind::Seed))
            .or(ok.first())
            .map(|x| (*x).clone())
    };
    let mut refs = Vec::new();
    for (model, r) in &run.results {
        let Some(m) = project.get(model) else {
            continue;
        };
        for j in &r.joins {
            if j.sides.len() != 2 {
                continue;
            }
            for s in 0..2 {
                let (side, other) = (&j.sides[s], &j.sides[1 - s]);
                if !side.needs_reference || side.from.len() != 1 {
                    continue;
                }
                let Some((child, col)) = preferred(&side.from[0], Some(model)) else {
                    continue;
                };
                let Some((parent, pcol)) = preferred(&other.covers[0], Some(&child)) else {
                    continue;
                };
                let keyed = |id: &RelationId| {
                    run.results
                        .get(id)
                        .is_some_and(|x| x.summary.has_known_grain())
                        || pending.contains_key(id)
                };
                let parent_keyed = other.unique
                    || pending
                        .get(&parent)
                        .is_some_and(|ks| ks.contains(&vec![pcol.clone()]));
                if !keyed(&child) || !parent_keyed {
                    continue;
                }
                let h = Hypothesis::References {
                    relation: child.clone(),
                    columns: vec![col.clone()],
                    to: parent.clone(),
                    to_columns: vec![pcol.clone()],
                };
                let declared = project.get(&child).is_some_and(|c| {
                    c.references
                        .iter()
                        .any(|r| r.cols == [col.clone()] && r.to == parent)
                });
                if declared || failed(&h) || refs.iter().any(|g: &Guess| g.hypothesis == h) {
                    continue;
                }
                refs.push(Guess {
                    test_name: h.test_name(&display),
                    why: vec![format!(
                        "{} joins {} to {} on it, and drops the rows that have no match",
                        m.display,
                        display(&child),
                        display(&parent)
                    )],
                    unlocks: format!(
                        "lets Cardinal prove {} keeps every row of {}",
                        m.display,
                        display(&child)
                    ),
                    hypothesis: h,
                });
            }
        }
    }
    guesses.extend(refs.into_iter().take(MAX_REFERENCES));
    (guesses, no_candidate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cardinal_dbt::{Column, Kind};

    fn rel(display: &str, cols: &[&str]) -> Relation {
        Relation {
            id: RelationId(format!("source.p.{display}")),
            kind: Kind::Source,
            display: display.into(),
            package: "p".into(),
            file: "models/sources.yml".into(),
            compiled_sql: None,
            raw_sql: None,
            compiled_path: None,
            relation_parts: vec![],
            depends_on: vec![],
            columns: cols
                .iter()
                .map(|c| Column {
                    name: c.to_string(),
                    data_type: None,
                })
                .collect(),
            columns_from: cardinal_dbt::ColumnsFrom::Catalog,
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
            snapshot: None,
            decl_problems: vec![],
            yaml_file: None,
        }
    }

    fn keys(c: Vec<(Vec<String>, Vec<String>)>) -> Vec<Vec<String>> {
        c.into_iter().map(|x| x.0).collect()
    }

    #[test]
    fn singular_forms() {
        assert_eq!(singular("customers"), "customer");
        assert_eq!(singular("categories"), "category");
        assert_eq!(singular("boxes"), "box");
        assert_eq!(singular("addresses"), "address");
        assert_eq!(
            singular("status"),
            "statu",
            "a crude rule, but only for names"
        );
        assert_eq!(singular("glass"), "glass");
    }

    #[test]
    fn names_rank_table_id_first() {
        let r = rel("app.customers", &["name", "id", "customer_id", "uuid"]);
        assert_eq!(
            keys(key_candidates(&r, &Usage::default())),
            vec![vec!["customer_id".to_string()], vec!["id".to_string()]]
        );
        let r = rel("app.events", &["kind", "occurred_at"]);
        assert!(key_candidates(&r, &Usage::default()).is_empty());
    }

    #[test]
    fn usage_outranks_names_and_dedups_exclude() {
        let r = rel("app.accounts", &["id", "code", "owner_id"]);
        let mut u = Usage::default();
        u.joined_on.entry(vec!["code".into()]).or_default().extend([
            "a".to_string(),
            "b".into(),
            "c".into(),
            "d".into(),
        ]);
        let c = key_candidates(&r, &u);
        assert_eq!(c[0].0, vec!["code"]);
        assert!(
            c[0].1[0].contains("a, b and 2 more join on it"),
            "{:?}",
            c[0].1
        );
        assert_eq!(c[1].0, vec!["id"]);
        u.deduped_on.insert(vec!["code".into()]);
        u.references.insert(vec!["id".into()]);
        assert!(
            key_candidates(&r, &u).is_empty(),
            "a dedup means the raw table repeats"
        );
    }

    #[test]
    fn composite_line_keys() {
        let r = rel("app.order_lines", &["order_id", "line_number", "sku"]);
        assert_eq!(
            keys(key_candidates(&r, &Usage::default())),
            vec![vec!["line_number".to_string(), "order_id".to_string()]]
        );
    }

    fn part(cols: &[&str], q: Option<&str>) -> DedupPartition {
        DedupPartition {
            cols: cols.iter().map(|c| c.to_string()).collect(),
            qualifier: q.map(str::to_string),
        }
    }

    #[test]
    fn finds_dedup_partitions() {
        let sql = "select * from (select *, row_number() over (partition by e.event_id, e.\"kind\" order by ts) as rn from events e) where rn = 1";
        assert_eq!(
            dedup_partitions(sql),
            vec![part(&["event_id", "kind"], Some("e"))]
        );
        assert!(dedup_partitions("select row_number() over (order by x) from t").is_empty());
    }

    #[test]
    fn dedup_partition_stays_inside_its_over() {
        // The row_number has no partition by; a later window's must not be taken.
        let sql = "select row_number() over (order by ts) as rn, \
                   sum(x) over (partition by account_id) as s from t";
        assert!(
            dedup_partitions(sql).is_empty(),
            "{:?}",
            dedup_partitions(sql)
        );
        // Nested parentheses in the order by do not end the window early.
        let sql = "select row_number() over (partition by id order by coalesce(a, b) desc) as rn, \
                   lag(x) over (partition by other) from t";
        assert_eq!(dedup_partitions(sql), vec![part(&["id"], None)]);
        // A partition on an expression is not a set of columns.
        let sql = "select row_number() over (partition by coalesce(a, b), c order by ts) from t";
        assert!(dedup_partitions(sql).is_empty());
        // Nor are columns of different tables.
        let sql =
            "select row_number() over (partition by x.a, y.b order by ts) from x join y on 1=1";
        assert!(dedup_partitions(sql).is_empty());
    }

    #[test]
    fn dedup_reads_sql_not_comments_or_strings() {
        // A row_number in a string or a comment is not one.
        let sql = "select 'row_number() over (partition by a)' as s \
                   -- row_number() over (partition by b)\nfrom t";
        assert!(dedup_partitions(sql).is_empty());
        // Comments inside the window do not hide its columns, nor does a `)` in a string.
        let sql = "select row_number() over (partition by /* key */ id, kind -- why\n \
                   order by coalesce(x, ')')) from t";
        assert_eq!(dedup_partitions(sql), vec![part(&["id", "kind"], None)]);
        // A constant is not a column.
        let sql = "select row_number() over (partition by 1 order by ts) from t";
        assert!(dedup_partitions(sql).is_empty());
    }

    #[test]
    fn dedup_attributed_to_the_right_table() {
        let mut events = rel("app.events", &["event_id", "kind"]);
        events.relation_parts = vec!["db".into(), "app".into(), "events".into()];
        let mut logs = rel("app.logs", &["event_id", "line"]);
        logs.relation_parts = vec!["db".into(), "app".into(), "logs".into()];
        let tables = [&events, &logs];
        let sql = "select row_number() over (partition by l.event_id order by ts) as rn \
                   from \"db\".\"app\".\"events\" e join \"db\".\"app\".\"logs\" as l on e.event_id = l.event_id";
        let p = &dedup_partitions(sql)[0];
        let got: Vec<&str> = dedup_tables(sql, p, &tables)
            .iter()
            .map(|r| r.display.as_str())
            .collect();
        assert_eq!(got, vec!["app.logs"]);
        // Unqualified and both tables have the column: no way to tell, so neither.
        let sql = "select row_number() over (partition by event_id order by ts) from events, logs";
        let p = &dedup_partitions(sql)[0];
        assert!(dedup_tables(sql, p, &tables).is_empty());
        // Unqualified, but only one table has the column.
        let sql = "select row_number() over (partition by kind order by ts) from events, logs";
        let p = &dedup_partitions(sql)[0];
        let got: Vec<&str> = dedup_tables(sql, p, &tables)
            .iter()
            .map(|r| r.display.as_str())
            .collect();
        assert_eq!(got, vec!["app.events"]);
    }
}
