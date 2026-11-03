//! Rule tests: one or more per plan-node rule.
use derive_grain::testing::{grain_case, key, no_key};

const ORDERS: &[&str] = &["order_id", "customer_id", "shipping_fee", "order_date"];
const ITEMS: &[&str] = &["order_id", "line_id", "amount"];
const CUSTOMERS: &[&str] = &["customer_id", "name", "region"];

fn shop() -> derive_grain::testing::GrainCase {
    grain_case()
        .table("orders", ORDERS, key(&["order_id"]))
        .table("order_items", ITEMS, key(&["order_id", "line_id"]))
        .table("customers", CUSTOMERS, key(&["customer_id"]))
        .table("events", &["order_id", "kind"], no_key())
}

#[test]
fn one_to_many_join_keeps_child_key() {
    grain_case()
        .table(
            "orders",
            &["order_id", "customer_id", "shipping_fee"],
            key(&["order_id"]),
        )
        .table(
            "order_items",
            &["order_id", "line_id", "amount"],
            key(&["order_id", "line_id"]),
        )
        .sql(
            "select o.order_id, i.line_id, o.shipping_fee
              from orders o join order_items i using (order_id)",
        )
        .expect_keys(&[&["order_id", "line_id"]])
        .expect_native_grain("shipping_fee", &["order_id"]);
}

#[test]
fn scan_keeps_declared_key() {
    shop()
        .sql("select * from orders")
        .expect_keys(&[&["order_id"]])
        .expect_native_grain("shipping_fee", &["order_id"]);
}

#[test]
fn projection_drops_key_when_column_dropped() {
    shop()
        .sql("select customer_id, shipping_fee from orders")
        .expect_no_keys();
}

#[test]
fn projection_keeps_key_through_alias_and_expressions() {
    shop()
        .sql("select order_id as id, shipping_fee * 2 as fee2, 'x' as tag from orders")
        .expect_keys(&[&["id"]])
        .expect_native_grain("fee2", &["id"]);
}

#[test]
fn many_to_one_join_keeps_left_key() {
    shop()
        .sql("select o.*, c.name from orders o join customers c on o.customer_id = c.customer_id")
        .expect_keys(&[&["order_id"]])
        .expect_native_grain("name", &["customer_id"]);
}

#[test]
fn left_join_many_to_one() {
    shop()
        .sql("select o.order_id, c.region from orders o left join customers c on o.customer_id = c.customer_id")
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn many_to_many_join_keeps_only_union() {
    shop()
        .sql("select o.order_id, c.customer_id from orders o cross join customers c")
        .expect_keys(&[&["order_id", "customer_id"]]);
}

#[test]
fn join_with_unknown_side_loses_grain() {
    shop()
        .sql("select o.order_id, e.kind from orders o join events e on o.order_id = e.order_id")
        .expect_no_keys()
        .expect_status_known(false);
}

#[test]
fn non_equality_condition_does_not_make_side_unique() {
    shop()
        .sql("select o.order_id, c.customer_id from orders o join customers c on o.customer_id < c.customer_id")
        .expect_keys(&[&["order_id", "customer_id"]]);
}

#[test]
fn filter_constant_shrinks_key() {
    shop()
        .sql("select * from order_items where order_id = 7")
        .expect_keys(&[&["line_id"]])
        .expect_not_null(&["order_id"]);
}

#[test]
fn aggregate_key_is_group_by() {
    shop()
        .sql("select customer_id, sum(shipping_fee) as fees from orders group by customer_id")
        .expect_keys(&[&["customer_id"]])
        .expect_native_grain("fees", &["customer_id"])
        .expect_status_known(true);
}

#[test]
fn aggregate_without_group_by_is_one_row() {
    shop()
        .sql("select count(*) as n from orders")
        .expect_keys(&[&[]]);
}

#[test]
fn aggregate_resets_unknown_input() {
    shop()
        .sql("select order_id, count(*) as n from events group by order_id")
        .expect_keys(&[&["order_id"]])
        .expect_status_known(true);
}

#[test]
fn group_by_reduces_through_dependencies() {
    shop()
        .sql("select order_id, customer_id, count(*) as n from orders group by order_id, customer_id")
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn qualify_row_number_dedup_proves_key() {
    shop()
        .sql(
            "select order_id, kind from events
              qualify row_number() over (partition by order_id order by kind desc) = 1",
        )
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn subquery_row_number_dedup_proves_key() {
    shop()
        .sql(
            "select order_id, kind from (
                select *, row_number() over (partition by order_id order by kind) as rn from events
              ) where rn = 1",
        )
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn rank_does_not_prove_key() {
    shop()
        .sql(
            "select order_id, kind from (
                select *, rank() over (partition by order_id order by kind) as rk from events
              ) where rk = 1",
        )
        .expect_no_keys();
}

#[test]
fn distinct_gives_all_columns() {
    shop()
        .sql("select distinct order_id, kind from events")
        .expect_keys(&[&["order_id", "kind"]]);
}

#[test]
fn distinct_over_keyed_input_keeps_input_key() {
    shop()
        .sql("select distinct order_id, customer_id from orders")
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn union_distinct_gives_all_columns() {
    shop()
        .sql("select order_id from orders union select order_id from order_items")
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn union_all_loses_key() {
    shop()
        .sql("select order_id from orders union all select order_id from orders")
        .expect_no_keys();
}

#[test]
fn union_all_with_discriminator_keeps_key() {
    shop()
        .sql("select order_id, 'a' as src from orders union all select order_id, 'b' as src from orders")
        .expect_keys(&[&["order_id", "src"]]);
}

#[test]
fn union_all_with_equal_literals_loses_key() {
    shop()
        .sql("select order_id, 'a' as src from orders union all select order_id, 'a' as src from orders")
        .expect_no_keys();
}

#[test]
fn limit_one_gives_empty_key() {
    shop()
        .sql("select * from events limit 1")
        .expect_keys(&[&[]]);
}

#[test]
fn cte_and_subquery_alias_keep_facts() {
    shop()
        .sql("with o as (select order_id, shipping_fee from orders) select x.* from o as x")
        .expect_keys(&[&["order_id"]]);
}

#[test]
fn one_to_one_join_keeps_both_keys() {
    grain_case()
        .table("a", &["id", "x"], key(&["id"]))
        .table("b", &["id", "y"], key(&["id"]))
        .sql("select a.id, b.id as bid, a.x, b.y from a join b on a.id = b.id")
        .expect_keys(&[&["id"], &["bid"]]);
}

#[test]
fn full_join_keeps_only_union() {
    grain_case()
        .table("a", &["id", "x"], key(&["id"]))
        .table("b", &["id", "y"], key(&["id"]))
        .sql("select a.id, b.id as bid from a full join b on a.id = b.id")
        .expect_keys(&[&["id", "bid"]]);
}

#[test]
fn composite_key_chain() {
    shop()
        .sql(
            "select i.order_id, i.line_id, i.amount, o.shipping_fee, c.region
              from order_items i
              join orders o on i.order_id = o.order_id
              left join customers c on o.customer_id = c.customer_id",
        )
        .expect_keys(&[&["order_id", "line_id"]])
        .expect_native_grain("shipping_fee", &["order_id"])
        .expect_native_grain("region", &["order_id"])
        .expect_native_grain("amount", &["line_id", "order_id"]);
}

#[test]
fn semi_join_via_in_subquery_keeps_left() {
    shop()
        .sql("select * from orders where order_id in (select order_id from events)")
        .expect_keys(&[&["order_id"]]);
}
