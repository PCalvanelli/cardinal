//! Operator-layer checks on single SQL statements: G010 (set operations by position)
//! and G011 (filters that drop nulls).

use cardinal_check::{Check, CheckContext, FilterDropsNulls, Locator, SetOpByPosition};
use cardinal_dbt::RelationId;
use cardinal_diag::{Code, Diagnostic, Label, Severity};
use cardinal_grain::testing::{Tables, analyze_sql, functions, key, no_key, test_relation};
use cardinal_sql::SqlSpan;

struct Plain;

impl Locator for Plain {
    fn label(&self, relation: &RelationId, span: Option<SqlSpan>, message: String) -> Label {
        Label {
            file: relation.0.clone(),
            span,
            message,
        }
    }
    fn display(&self, relation: &RelationId) -> String {
        relation.0.clone()
    }
}

fn tables() -> Tables {
    let mut t = Tables::default();
    t.add(
        "customers",
        &["customer_id", "name", "region"],
        &[key(&["customer_id"])],
    );
    t.add(
        "leads",
        &["lead_id", "region", "name"],
        &[key(&["lead_id"])],
    );
    t.add(
        "orders",
        &["order_id", "customer_id", "status"],
        &[key(&["order_id"])],
    );
    t.add("blocked", &["customer_id", "reason"], &[no_key()]);
    t
}

fn run(check: &dyn Check, tables: &Tables, sql: &str) -> Vec<Diagnostic> {
    let model = analyze_sql(tables, sql);
    let rel = test_relation(&model.relation, "m");
    let cx = CheckContext {
        relation: &rel,
        locator: &Plain,
        functions: functions(),
        declared: &[],
    };
    let mut out = Vec::new();
    check.run(&cx, &model, &mut out);
    assert!(out.iter().all(|d| d.code == check.code()));
    out
}

// ---- G010: set operations match by position -------------------------------------

#[test]
fn union_with_swapped_columns_is_an_error() {
    let out = run(
        &SetOpByPosition,
        &tables(),
        "select customer_id, name, region from customers
         union all
         select lead_id, region, name from leads",
    );
    // lead_id -> customer_id is a rename (warning); region/name are swapped (errors).
    let mut got: Vec<(Severity, String)> = out
        .iter()
        .map(|d| (d.severity, d.primary.message.clone()))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            (
                Severity::Warning,
                "`lead_id` goes into the `customer_id` column by position".into()
            ),
            (
                Severity::Error,
                "`name` lands in the `region` column".into()
            ),
            (
                Severity::Error,
                "`region` lands in the `name` column".into()
            ),
        ]
    );
    let swap = out
        .iter()
        .find(|d| d.primary.message.starts_with("`region`"))
        .unwrap();
    let span = swap.primary.span.expect("points at the column");
    assert_eq!((span.start.line, span.start.col), (3, 26), "{swap:?}");
}

#[test]
fn union_aligned_by_alias_is_fine() {
    let out = run(
        &SetOpByPosition,
        &tables(),
        "select customer_id, name, region, 'customer' as kind from customers
         union all
         select lead_id as customer_id, name, region, 'lead' from leads",
    );
    assert!(out.is_empty(), "{out:#?}");
}

#[test]
fn union_distinct_and_three_branches() {
    let out = run(
        &SetOpByPosition,
        &tables(),
        "select customer_id, name from customers
         union
         select customer_id, name from customers
         union
         select lead_id, region from leads",
    );
    let msgs: Vec<&str> = out.iter().map(|d| d.primary.message.as_str()).collect();
    assert!(
        msgs.contains(&"`region` goes into the `name` column by position"),
        "{msgs:?}"
    );
}

// ---- G011: filters that drop nulls ---------------------------------------------------

#[test]
fn not_in_nullable_subquery() {
    let out = run(
        &FilterDropsNulls,
        &tables(),
        "select order_id from orders
         where customer_id not in (select customer_id from blocked)",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(
        out[0].message.starts_with("NOT IN keeps no rows"),
        "{out:#?}"
    );
    assert!(
        out[0]
            .primary
            .message
            .contains("the subquery's customer_id")
    );
}

#[test]
fn not_in_non_null_subquery_is_fine() {
    let mut t = tables();
    t.declare("blocked", &["customer_id"], &[]);
    for sql in [
        // Declared not null.
        "select order_id from orders where customer_id not in (select customer_id from blocked)",
        // NOT EXISTS has no such trap; IN (not negated) neither.
        "select order_id from orders o where customer_id in (select customer_id from blocked)",
    ] {
        let out = run(&FilterDropsNulls, &t, sql);
        assert!(out.is_empty(), "{sql}\n{out:#?}");
    }
    // Filtering the nulls out inside the subquery proves it too.
    let out = run(
        &FilterDropsNulls,
        &tables(),
        "select order_id from orders where customer_id not in
           (select customer_id from blocked where customer_id is not null)",
    );
    assert!(out.is_empty(), "{out:#?}");
}

#[test]
fn not_in_list_with_null() {
    let out = run(
        &FilterDropsNulls,
        &tables(),
        "select order_id from orders where status not in ('void', null)",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
}

#[test]
fn where_on_left_joins_right_side_turns_it_inner() {
    let out = run(
        &FilterDropsNulls,
        &tables(),
        "select o.order_id, c.region
         from orders o
         left join customers c on o.customer_id = c.customer_id
         where c.region = 'EU'",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    let d = &out[0];
    assert_eq!(
        d.message,
        "WHERE on customers's columns turns the left join into an inner join"
    );
    let span = d.primary.span.expect("points at the column");
    assert_eq!(span.start.line, 4, "{d:#?}");
    assert_eq!(d.secondary.len(), 1, "points at the join: {d:#?}");
}

#[test]
fn where_through_a_cte_still_counts() {
    let out = run(
        &FilterDropsNulls,
        &tables(),
        "with j as (
            select o.order_id, c.region as customer_region
            from orders o left join customers c on o.customer_id = c.customer_id
         )
         select * from j where customer_region <> 'EU'",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
}

#[test]
fn deliberate_null_handling_is_fine() {
    for sql in [
        // Conditions on the preserved side.
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id where o.status = 'paid'",
        // Anti-join and explicit null tests.
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id where c.customer_id is null",
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id where c.customer_id is not null",
        // Keeping unmatched rows on purpose.
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id where c.region = 'EU' or c.region is null",
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id where coalesce(c.region, 'EU') = 'EU'",
        // The condition in ON keeps the left join a left join.
        "select o.order_id from orders o left join customers c
           on o.customer_id = c.customer_id and c.region = 'EU'",
        // Inner joins fill no nulls.
        "select o.order_id from orders o join customers c
           on o.customer_id = c.customer_id where c.region = 'EU'",
    ] {
        let out = run(&FilterDropsNulls, &tables(), sql);
        assert!(out.is_empty(), "{sql}\n{out:#?}");
    }
}

#[test]
fn codes_are_operator_layer() {
    for c in [Code::G010, Code::G011] {
        assert_eq!(c.layer(), cardinal_diag::Layer::Operator);
    }
}
