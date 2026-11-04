//! Operator-layer checks on single SQL statements: G010 (set operations by position),
//! G011 (filters that drop nulls), G012 (UNION merging rows), G013 (count(*) over an
//! outer join) and G014 (soft-delete flags dropped).

use cardinal_check::{
    Check, CheckContext, CountOuterJoin, FilterDropsNulls, Locator, SetOpByPosition,
    SoftDeleteDropped, UnionMergesRows,
};
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
    t.add(
        "payments",
        &["payment_id", "customer_id", "amount"],
        &[key(&["payment_id"])],
    );
    t.add(
        "refunds",
        &["refund_id", "customer_id", "amount"],
        &[key(&["refund_id"])],
    );
    t.set_type("payments", "amount", "Decimal128(10, 2)");
    t.set_type("refunds", "amount", "Decimal128(10, 2)");
    t.add(
        "raw_orders",
        &["order_id", "customer_id", "amount", "_fivetran_deleted"],
        &[key(&["order_id"])],
    );
    t.set_type("raw_orders", "_fivetran_deleted", "Boolean");
    t.soft_delete("raw_orders", "_fivetran_deleted");
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

// ---- G012: UNION merges rows -------------------------------------------------------

#[test]
fn union_of_amounts_without_keys_merges_rows() {
    let out = run(
        &UnionMergesRows,
        &tables(),
        "select customer_id, amount from payments
         union
         select customer_id, amount from refunds",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    let d = &out[0];
    assert!(d.message.contains("amount"), "{d:#?}");
    let span = d.primary.span.expect("points at UNION");
    assert_eq!((span.start.line, span.start.col), (2, 10), "{d:#?}");
    let fix = d.fix.as_ref().unwrap().message.as_str();
    assert!(
        fix.contains("UNION ALL") && fix.contains("(payment_id)"),
        "{fix}"
    );
}

#[test]
fn union_that_loses_nothing_is_fine() {
    for sql in [
        // UNION ALL keeps every row.
        "select customer_id, amount from payments union all select customer_id, amount from refunds",
        // Identifiers only: a deliberate de-duplication.
        "select customer_id from payments union select customer_id from refunds",
        // Each branch keeps its key, so its rows are distinct.
        "select payment_id, customer_id, amount from payments
         union select refund_id, customer_id, amount from refunds",
    ] {
        let out = run(&UnionMergesRows, &tables(), sql);
        assert!(out.is_empty(), "{sql}\n{out:#?}");
    }
}

// ---- G013: count(*) over an outer join ------------------------------------------------

#[test]
fn count_star_over_left_join_counts_unmatched_rows() {
    let out = run(
        &CountOuterJoin,
        &tables(),
        "select c.customer_id, count(*) as order_count
         from customers c
         left join orders o on c.customer_id = o.customer_id
         group by c.customer_id",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    let d = &out[0];
    assert_eq!(
        d.message,
        "count(*) counts 1, not 0, for customers rows with no orders row"
    );
    let span = d.primary.span.expect("points at count(*)");
    assert_eq!((span.start.line, span.start.col), (1, 23), "{d:#?}");
    assert_eq!(
        d.fix.as_ref().unwrap().message,
        "count a column of orders that is null on unmatched rows: count(o.customer_id)"
    );
    assert_eq!(d.secondary.len(), 1, "points at the join: {d:#?}");
}

#[test]
fn counts_that_skip_unmatched_rows_are_fine() {
    for sql in [
        // Counting the joined side's column skips the nulls.
        "select c.customer_id, count(o.order_id) as n from customers c
           left join orders o on c.customer_id = o.customer_id group by c.customer_id",
        // Distinct counts too, and inner joins have no unmatched rows.
        "select c.customer_id, count(distinct o.order_id) as n from customers c
           left join orders o on c.customer_id = o.customer_id group by c.customer_id",
        "select c.customer_id, count(*) as n from customers c
           join orders o on c.customer_id = o.customer_id group by c.customer_id",
        // A lookup: each order has one customer, so count(*) counts orders.
        "select o.customer_id, count(*) as n from orders o
           left join customers c on o.customer_id = c.customer_id group by o.customer_id",
        // Grouping by the joined side's column puts unmatched rows in their own group.
        "select o.status, count(*) as n from customers c
           left join orders o on c.customer_id = o.customer_id group by o.status",
        // A WHERE on the joined side drops unmatched rows (G011 reports it).
        "select c.customer_id, count(*) as n from customers c
           left join orders o on c.customer_id = o.customer_id
           where o.status = 'paid' group by c.customer_id",
    ] {
        let out = run(&CountOuterJoin, &tables(), sql);
        assert!(out.is_empty(), "{sql}\n{out:#?}");
    }
}

#[test]
fn count_of_preserved_column_and_right_join() {
    let out = run(
        &CountOuterJoin,
        &tables(),
        "select c.region, count(c.customer_id) as n
         from orders o
         right join customers c on o.customer_id = c.customer_id
         group by c.region",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    assert!(
        out[0].message.starts_with("count(c.customer_id) counts 1"),
        "{out:#?}"
    );
}

// ---- G014: soft-delete flags ----------------------------------------------------------

#[test]
fn dropping_the_flag_without_a_filter_keeps_deleted_rows() {
    let out = run(
        &SoftDeleteDropped,
        &tables(),
        "select order_id, customer_id, amount from raw_orders",
    );
    assert_eq!(out.len(), 1, "{out:#?}");
    let d = &out[0];
    assert!(
        d.message.contains("`_fivetran_deleted` is dropped"),
        "{d:#?}"
    );
    let span = d.primary.span.expect("points at the source");
    assert_eq!((span.start.line, span.start.col), (1, 43), "{d:#?}");
}

#[test]
fn filtering_or_keeping_the_flag_is_fine() {
    for sql in [
        "select order_id, amount from raw_orders where not _fivetran_deleted",
        "select order_id, amount from raw_orders where coalesce(_fivetran_deleted, false) = false",
        // Kept for a later model to filter, as is or renamed.
        "select * from raw_orders",
        "select order_id, _fivetran_deleted as is_deleted from raw_orders",
        "select order_id, coalesce(_fivetran_deleted, false) as is_deleted from raw_orders",
        // Applied in a join condition, or decided on in an aggregate.
        "select c.customer_id, o.amount from customers c
           left join raw_orders o on c.customer_id = o.customer_id and not o._fivetran_deleted",
        "select customer_id, sum(case when _fivetran_deleted then 0 else amount end) as live
           from raw_orders group by customer_id",
    ] {
        let out = run(&SoftDeleteDropped, &tables(), sql);
        assert!(out.is_empty(), "{sql}\n{out:#?}");
    }
}

#[test]
fn dropping_inside_a_join_or_aggregate_counts() {
    for sql in [
        "select c.customer_id, sum(o.amount) as total from customers c
           left join raw_orders o on c.customer_id = o.customer_id group by c.customer_id",
        "with live as (select order_id, amount from raw_orders)
         select * from live",
    ] {
        let out = run(&SoftDeleteDropped, &tables(), sql);
        assert_eq!(out.len(), 1, "{sql}\n{out:#?}");
    }
}

#[test]
fn codes_are_operator_layer() {
    for c in [Code::G010, Code::G011, Code::G012, Code::G013, Code::G014] {
        assert_eq!(c.layer(), cardinal_diag::Layer::Operator);
    }
}
