//! Null-exempt keys: keys that hold among the rows where their columns are non-null.
//! An outer join makes them from the side it fills with nulls.
use cardinal_grain::testing::{GrainCase, grain_case, key};

const ORDERS: &[&str] = &["order_id", "customer_id", "amount"];
const ITEMS: &[&str] = &["order_id", "line_id", "qty"];
const CUSTOMERS: &[&str] = &["customer_id", "name"];

fn shop() -> GrainCase {
    grain_case()
        .table("orders", ORDERS, key(&["order_id"]))
        .table("order_items", ITEMS, key(&["order_id", "line_id"]))
        .table("customers", CUSTOMERS, key(&["customer_id"]))
        .declare("orders", &["order_id", "customer_id"], &[])
        .declare("order_items", &["order_id", "line_id"], &[])
        .declare("customers", &["customer_id"], &[])
}

const DIM_LEFT_JOIN_FACT: &str = "select c.customer_id, o.order_id, o.amount
    from customers c left join orders o on c.customer_id = o.customer_id";

#[test]
fn left_join_from_a_dimension_keeps_the_fact_key_among_matches() {
    shop()
        .sql(DIM_LEFT_JOIN_FACT)
        .expect_keys(&[&["customer_id", "order_id"]])
        .expect_null_keys(&[&["order_id"]]);
}

#[test]
fn each_fact_row_appears_at_most_once() {
    let v = shop().sql(DIM_LEFT_JOIN_FACT).verdict("orders");
    assert!(v.at_most_once.is_proven(), "{v:?}");
}

#[test]
fn right_and_full_joins_fill_the_other_side() {
    shop()
        .sql(
            "select o.order_id, c.customer_id from orders o
             right join customers c on o.customer_id = c.customer_id",
        )
        .expect_null_keys(&[&["order_id"]]);
    // A one-to-one full join: each side's key holds among its non-null values.
    grain_case()
        .table("a", &["id", "x"], key(&["id"]))
        .table("b", &["id", "y"], key(&["id"]))
        .sql("select a.id, b.id as bid from a full join b on a.id = b.id")
        .expect_keys(&[&["id", "bid"]])
        .expect_null_keys(&[&["id"], &["bid"]]);
}

#[test]
fn not_null_filter_promotes_to_a_key() {
    shop()
        .sql(&format!(
            "select * from ({DIM_LEFT_JOIN_FACT}) t where order_id is not null"
        ))
        .expect_keys(&[&["order_id"]])
        .expect_null_keys(&[]);
}

#[test]
fn equality_join_on_a_null_exempt_key_is_many_to_one() {
    // order_id holds once per order in `m`, or null; nulls never match `=`.
    shop()
        .model("m", DIM_LEFT_JOIN_FACT)
        .sql(
            "select i.order_id, i.line_id, m.customer_id
             from order_items i join m on i.order_id = m.order_id",
        )
        .expect_keys(&[&["order_id", "line_id"]]);
}

#[test]
fn null_safe_join_needs_a_strict_key() {
    shop()
        .model("m", DIM_LEFT_JOIN_FACT)
        .sql(
            "select i.order_id, i.line_id, m.customer_id
             from order_items i join m on i.order_id is not distinct from m.order_id",
        )
        // Every order item matches all of m's null rows when its order_id is null, so
        // (order_id, line_id) is not a key; the only key needs m.order_id, not selected.
        .expect_no_keys();
}

#[test]
fn many_side_not_unique_keeps_nothing() {
    // Each customer repeats per order, so customer_id is no key among matches.
    shop()
        .sql(
            "select c.customer_id, o.order_id from orders o
             left join customers c on o.customer_id = c.customer_id",
        )
        .expect_keys(&[&["order_id"]])
        .expect_null_keys(&[]);
}

#[test]
fn projection_keeps_copied_columns_only() {
    shop()
        .sql(&format!(
            "select customer_id, order_id + 1 as next_id from ({DIM_LEFT_JOIN_FACT}) t"
        ))
        .expect_null_keys(&[]);
    shop()
        .sql(&format!(
            "select customer_id, order_id as id from ({DIM_LEFT_JOIN_FACT}) t"
        ))
        .expect_null_keys(&[&["id"]]);
}
