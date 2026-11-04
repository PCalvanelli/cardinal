//! Distinct-count facts: which `count(distinct …)` columns are recorded, with which
//! shared columns, and how the fact moves through plan nodes and across models.
use cardinal_grain::testing::{GrainCase, grain_case, key, no_key};

const EVENTS: &[&str] = &["event_id", "user_id", "event_date", "kind"];
const DAU: &str =
    "select event_date, count(distinct user_id) as dau from events group by event_date";

fn shop() -> GrainCase {
    grain_case()
        .table("events", EVENTS, key(&["event_id"]))
        .table("raw_events", EVENTS, no_key())
        .table("users", &["user_id", "customer_id"], key(&["user_id"]))
        .table(
            "orders",
            &["order_id", "customer_id", "order_date"],
            key(&["order_id"]),
        )
        .table("calendar", &["day", "month"], key(&["day"]))
}

#[test]
fn daily_distinct_users_overlap_across_days() {
    shop().sql(DAU).expect_distinct("dau", Some(&[]));
}

#[test]
fn distinct_keys_never_overlap() {
    // Each order has one customer and one date, so the groups' order sets are disjoint.
    shop()
        .sql(
            "select customer_id, order_date, count(distinct order_id) as n
              from orders group by customer_id, order_date",
        )
        .expect_distinct("n", None);
}

#[test]
fn determined_group_columns_are_shared() {
    // A user belongs to one customer: two groups counting the same user share customer_id.
    shop()
        .sql(
            "select u.customer_id, e.event_date, count(distinct e.user_id) as n
              from events e join users u on e.user_id = u.user_id
              group by u.customer_id, e.event_date",
        )
        .expect_distinct("n", Some(&["customer_id"]));
}

#[test]
fn unknown_input_grain_records_nothing() {
    shop()
        .sql(
            "select event_date, count(distinct user_id) as dau from raw_events group by event_date",
        )
        .expect_distinct("dau", None);
}

#[test]
fn approximate_and_case_forms_count() {
    shop()
        .sql(
            "select event_date, approx_count_distinct(user_id) as a,
                    count(distinct case when kind = 1 then user_id end) as b
              from events group by event_date",
        )
        .expect_distinct("a", Some(&[]))
        .expect_distinct("b", Some(&[]));
}

#[test]
fn computed_arguments_are_skipped() {
    // user_id + 1 could merge values in general, so nothing is claimed about it.
    shop()
        .sql("select event_date, count(distinct user_id + 1) as n from events group by event_date")
        .expect_distinct("n", None);
}

#[test]
fn plain_count_records_nothing() {
    shop()
        .sql("select event_date, count(user_id) as n from events group by event_date")
        .expect_distinct("n", None);
}

#[test]
fn projection_renames_and_drops() {
    shop()
        .sql(&format!("select event_date as d, dau as n from ({DAU}) t"))
        .expect_distinct("n", Some(&[]));
    // Dropping a shared column drops the fact rather than guessing.
    shop()
        .sql(
            "select n from (select u.customer_id, e.event_date, count(distinct e.user_id) as n
              from events e join users u on e.user_id = u.user_id
              group by u.customer_id, e.event_date) t",
        )
        .expect_distinct("n", None);
}

#[test]
fn union_drops_the_fact() {
    shop()
        .sql(&format!(
            "select * from ({DAU}) a union all select * from ({DAU}) b"
        ))
        .expect_distinct("dau", None);
}

#[test]
fn join_keeps_the_fact_on_either_side() {
    shop()
        .sql(&format!(
            "select c.month, t.dau from calendar c join ({DAU}) t on c.day = t.event_date"
        ))
        .expect_distinct("dau", Some(&[]));
    shop()
        .sql(
            "select t.n, c.month from (select u.customer_id, e.event_date, count(distinct e.user_id) as n
              from events e join users u on e.user_id = u.user_id
              group by u.customer_id, e.event_date) t join calendar c on c.day = t.event_date",
        )
        .expect_distinct("n", None);
}

#[test]
fn facts_cross_model_boundaries() {
    shop()
        .model("daily", DAU)
        .sql("select event_date, dau from daily")
        .expect_distinct("dau", Some(&[]));
    shop()
        .model(
            "per_customer",
            "select u.customer_id, e.event_date, count(distinct e.user_id) as n
              from events e join users u on e.user_id = u.user_id
              group by u.customer_id, e.event_date",
        )
        .sql("select customer_id, n from per_customer")
        .expect_distinct("n", Some(&["customer_id"]));
}

#[test]
fn summing_ends_the_fact() {
    shop()
        .model("daily", DAU)
        .sql("select sum(dau) as total from daily")
        .expect_distinct("total", None);
}

#[test]
fn summary_round_trips_through_json() {
    let site = cardinal_grain::DistinctSite {
        relation: cardinal_dbt::RelationId("model.test.daily".into()),
        span: None,
        counted: vec!["user_id".into()],
        per: vec!["event_date".into()],
        across: vec!["event_date".into()],
    };
    let s = cardinal_grain::SummaryMeasure {
        col: 1,
        kind: cardinal_grain::MeasureKind::NonAdditive {
            agree_on: Default::default(),
        },
        site: cardinal_grain::MeasureSite::Distinct(site),
    };
    let back: cardinal_grain::SummaryMeasure =
        serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
}
