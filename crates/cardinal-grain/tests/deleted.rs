//! Soft-delete flags: carried until a filter, join condition or aggregate reads them, and
//! recorded where a model drops one unread.
use cardinal_grain::testing::{GrainCase, grain_case, key};

fn shop() -> GrainCase {
    grain_case()
        .table(
            "raw_orders",
            &["order_id", "customer_id", "amount", "_fivetran_deleted"],
            key(&["order_id"]),
        )
        .soft_delete("raw_orders", "_fivetran_deleted")
}

/// (dropped by, via) for each flag still on the output; None while carried.
fn deleted(c: &GrainCase) -> Vec<Option<(String, String)>> {
    c.run()
        .output()
        .deleted
        .iter()
        .map(|d| {
            d.dropped
                .as_ref()
                .map(|x| (x.model.0.clone(), x.via.0.clone()))
        })
        .collect()
}

#[test]
fn carried_through_a_model_and_dropped_downstream() {
    let c = shop()
        .model("stg_orders", "select * from raw_orders")
        .sql("select order_id, amount from stg_orders");
    assert_eq!(
        deleted(&c),
        vec![Some((
            "model.test.m".to_string(),
            "model.test.stg_orders".to_string()
        ))]
    );
}

#[test]
fn filtered_upstream_is_gone() {
    let c = shop()
        .model(
            "stg_orders",
            "select * from raw_orders where not _fivetran_deleted",
        )
        .sql("select order_id, amount from stg_orders");
    assert_eq!(deleted(&c), vec![]);
}

#[test]
fn dropped_upstream_is_not_dropped_again() {
    let c = shop()
        .model("stg_orders", "select order_id, amount from raw_orders")
        .sql("select order_id from stg_orders");
    // Recorded once, at stg_orders; this model only passes the record on.
    assert_eq!(
        deleted(&c),
        vec![Some((
            "model.test.stg_orders".to_string(),
            "source.test.raw_orders".to_string()
        ))]
    );
}

#[test]
fn carried_while_selected() {
    let c = shop().sql("select order_id, _fivetran_deleted from raw_orders");
    assert_eq!(deleted(&c), vec![None]);
}
