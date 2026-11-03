//! Preservation rules: which operators keep every row of a relation, and the
//! exactly-once verdicts built on them.
use cardinal_grain::testing::{GrainCase, grain_case, key, no_key};
use cardinal_grain::{Half, Reason, Site};

const ORDERS: &[&str] = &["order_id", "customer_id", "status", "fee"];
const ITEMS: &[&str] = &["order_id", "line_id", "amount"];
const CUSTOMERS: &[&str] = &["customer_id", "name", "updated_at"];

/// orders.customer_id references customers; order_items.order_id references orders.
fn shop() -> GrainCase {
    grain_case()
        .table("orders", ORDERS, key(&["order_id"]))
        .table("order_items", ITEMS, key(&["order_id", "line_id"]))
        .table("customers", CUSTOMERS, key(&["customer_id"]))
        .table("customer_versions", CUSTOMERS, no_key())
        .declare(
            "orders",
            &["order_id", "customer_id"],
            &[(&["customer_id"], "customers", &["customer_id"])],
        )
        .declare(
            "order_items",
            &["order_id", "line_id"],
            &[(&["order_id"], "orders", &["order_id"])],
        )
        .declare("customers", &["customer_id"], &[])
        .declare("customer_versions", &["customer_id"], &[])
}

fn reasons(h: &Half) -> String {
    format!("{:?}", h.reasons())
}

#[test]
fn scan_is_exactly_once() {
    let v = shop().sql("select * from orders").verdict("orders");
    assert!(v.exactly_once(), "{v:?}");
    assert_eq!(v.site, Site::Output);
    assert_eq!(v.cols, vec!["order_id"]);
}

#[test]
fn many_to_one_join_on_declared_reference_keeps_every_row() {
    let v = shop()
        .sql(
            "select o.order_id, c.name from orders o
             join customers c on o.customer_id = c.customer_id",
        )
        .verdict("orders");
    assert!(v.exactly_once(), "{v:?}");
    // The proof rests on the reference, the non-null join column, and the keys.
    let a = format!("{:?}", v.assumptions);
    assert!(a.contains("References"), "{a}");
    assert!(a.contains("NotNull"), "{a}");
}

#[test]
fn inner_join_without_reference_may_drop_rows() {
    let v = shop()
        .sql(
            "select i.order_id, i.line_id, c.name from order_items i
             join orders o on i.order_id = o.order_id
             join customers c on o.customer_id = c.customer_id",
        )
        .verdict("order_items");
    // order_items -> orders -> customers: both references are declared.
    assert!(v.exactly_once(), "{v:?}");

    let v = shop()
        .sql(
            "select c.customer_id, o.order_id from customers c
             join orders o on c.customer_id = o.customer_id",
        )
        .verdict("customers");
    assert!(
        !v.at_least_once.is_proven(),
        "customers without orders drop out: {v:?}"
    );
    assert!(
        reasons(&v.at_least_once).contains("nothing shows that every customers.customer_id"),
        "{v:?}"
    );
}

#[test]
fn left_join_keeps_left_rows() {
    let v = shop()
        .sql(
            "select c.customer_id, o.order_id from customers c
             left join orders o on c.customer_id = o.customer_id",
        )
        .verdict("customers");
    assert!(v.at_least_once.is_proven(), "{v:?}");
    assert!(!v.at_most_once.is_proven(), "one row per order: {v:?}");
    assert!(
        matches!(v.at_most_once.reasons()[0], Reason::Repeats { .. }),
        "{v:?}"
    );
}

#[test]
fn filter_drops_rows() {
    let v = shop()
        .sql("select * from orders where status = 'complete'")
        .verdict("orders");
    assert!(!v.at_least_once.is_proven());
    assert!(v.at_most_once.is_proven());
    assert_eq!(v.label(), "at most once");
    let r = reasons(&v.at_least_once);
    assert!(
        r.contains("filter `select * from orders where status = 'complete'`"),
        "{r}"
    );
}

#[test]
fn not_null_filter_on_non_null_column_keeps_rows() {
    let v = shop()
        .sql("select * from orders where customer_id is not null")
        .verdict("orders");
    assert!(v.exactly_once(), "{v:?}");
}

#[test]
fn dedup_keeps_one_row_per_key() {
    // A versions table deduplicated to its latest row per customer covers every
    // customer_id, so a join to it keeps every order.
    let case = shop().sql(
        "select o.order_id, c.name from orders o
         join (
             select * from customer_versions
             qualify row_number() over (partition by customer_id order by updated_at desc) = 1
         ) c on o.customer_id = c.customer_id",
    );
    let v = case.verdict("orders");
    // orders reference customers, not customer_versions, so this is not proven...
    assert!(!v.at_least_once.is_proven(), "{v:?}");
    // ...but the dedup keeps every customer_id of customer_versions, so orders that
    // reference customer_versions keep every row.
    let v = shop()
        .declare(
            "orders",
            &[],
            &[(&["customer_id"], "customer_versions", &["customer_id"])],
        )
        .sql(
            "select o.order_id, c.name from orders o
             join (
                 select * from customer_versions
                 qualify row_number() over (partition by customer_id order by updated_at desc) = 1
             ) c on o.customer_id = c.customer_id",
        )
        .verdict("orders");
    assert!(v.exactly_once(), "{v:?}");
    let v = shop()
        .sql(
            "select customer_id, name from customer_versions
             qualify row_number() over (partition by customer_id order by updated_at desc) = 1",
        )
        .verdict("customer_versions");
    assert!(
        matches!(v.at_least_once, Half::NotProven { .. }),
        "customer_versions has no key, so its rows cannot be identified: {v:?}"
    );
    assert!(reasons(&v.at_least_once).contains("NoKey"), "{v:?}");
}

#[test]
fn dedup_and_reference_together() {
    // orders reference customers; the dedup model covers customers, so the join keeps
    // every order.
    let v = shop()
        .sql(
            "select o.order_id, c.name from orders o
             join (
                 select * from customers
                 qualify row_number() over (partition by customer_id order by updated_at desc) = 1
             ) c on o.customer_id = c.customer_id",
        )
        .verdict("orders");
    assert!(v.exactly_once(), "{v:?}");
}

#[test]
fn aggregate_input_is_checked_when_output_drops_the_key() {
    let v = shop()
        .sql(
            "select o.status, sum(i.amount) as amount from order_items i
             join orders o on i.order_id = o.order_id
             group by o.status",
        )
        .verdict("order_items");
    assert_eq!(v.site, Site::AggregateInput, "{v:?}");
    assert!(v.exactly_once(), "{v:?}");

    // Orders fan out per line before the aggregate: at least once, not at most once.
    let v = shop()
        .sql(
            "select o.status, sum(o.fee) as fees from orders o
             join order_items i on i.order_id = o.order_id
             group by o.status",
        )
        .verdict("orders");
    assert_eq!(v.site, Site::AggregateInput, "{v:?}");
    assert!(
        !v.at_least_once.is_proven(),
        "orders without items drop out: {v:?}"
    );
    assert!(!v.at_most_once.is_proven(), "{v:?}");
}

#[test]
fn group_by_key_is_exactly_once_at_output() {
    let v = shop()
        .sql("select order_id, sum(amount) as amount from order_items group by order_id")
        .verdict("orders");
    // order_items.order_id is within orders, but orders without items are missing.
    assert!(v.at_most_once.is_proven(), "{v:?}");
    assert!(!v.at_least_once.is_proven(), "{v:?}");
}

#[test]
fn union_all_keeps_rows_of_each_branch() {
    let v = shop()
        .sql(
            "select order_id as id, 'order' as kind from orders
             union all
             select customer_id as id, 'customer' as kind from customers",
        )
        .verdict("orders");
    assert!(v.at_least_once.is_proven(), "{v:?}");
}

#[test]
fn limit_drops_rows() {
    let v = shop()
        .sql("select * from orders order by order_id limit 10")
        .verdict("orders");
    assert!(!v.at_least_once.is_proven());
    assert!(reasons(&v.at_least_once).contains("LIMIT"));
}

#[test]
fn model_that_does_not_read_relation() {
    let v = shop().sql("select * from customers").verdict("orders");
    assert!(reasons(&v.at_least_once).contains("NotRead"), "{v:?}");
}

#[test]
fn nullable_key_is_reported() {
    let v = grain_case()
        .table("t", &["id", "x"], key(&["id"]))
        .sql("select * from t")
        .verdict("t");
    assert!(reasons(&v.at_least_once).contains("KeyNullable"), "{v:?}");
}

#[test]
fn extra_on_condition_blocks_the_proof() {
    let v = shop()
        .sql(
            "select o.order_id, c.name from orders o
             join customers c on o.customer_id = c.customer_id and c.name <> ''",
        )
        .verdict("orders");
    assert!(
        reasons(&v.at_least_once).contains("conditions besides column equalities"),
        "{v:?}"
    );
}

#[test]
fn using_join_column_stands_for_both_sides() {
    // The output's order_id is o.order_id; i.order_id equals it on every row.
    let v = shop()
        .sql(
            "select o.order_id, i.line_id from orders o
             join order_items i using (order_id)",
        )
        .verdict("order_items");
    assert!(v.exactly_once(), "{v:?}");
    assert_eq!(v.cols, vec!["order_id", "line_id"]);
}
