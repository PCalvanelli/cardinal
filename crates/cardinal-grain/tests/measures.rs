//! Measure facts: which columns can't be freely added up (averages, ratios, percentiles,
//! running totals, declared stocks), which amounts carry a unit, and how both move
//! through plan nodes and across models.
use cardinal_grain::testing::{GrainCase, grain_case, key};

fn shop() -> GrainCase {
    grain_case()
        .table(
            "orders",
            &[
                "order_id",
                "customer_id",
                "order_date",
                "amount",
                "cents",
                "fx_rate",
                "currency",
            ],
            key(&["order_id"]),
        )
        .table(
            "customers",
            &["customer_id", "region"],
            key(&["customer_id"]),
        )
        .table(
            "balances",
            &["account_id", "snapshot_date", "balance", "currency"],
            key(&["account_id", "snapshot_date"]),
        )
        .table("accounts", &["account_id", "owner"], key(&["account_id"]))
        .stock("balances", "balance", &["snapshot_date"])
        .unit("orders", "amount", "currency")
}

const DAILY: &str = "select order_date, sum(amount) as revenue, count(*) as n
                       from orders group by order_date";

#[test]
fn averages_and_percentiles_are_non_additive() {
    shop()
        .sql(
            "select customer_id, avg(amount) as aov, median(amount) as mid, sum(amount) as total
              from orders group by customer_id",
        )
        .expect_measure("aov", "non-additive average")
        .expect_measure("mid", "non-additive percentile")
        .expect_measure("total", "additive");
}

#[test]
fn ratios_of_aggregates_are_non_additive() {
    shop()
        .sql(
            "select customer_id,
                    sum(amount) / sum(cents) as r1,
                    count(*) / count(distinct order_date) as r2,
                    100.0 * sum(amount) / nullif(sum(cents), 0) as r3
              from orders group by customer_id",
        )
        .expect_measure("r1", "non-additive ratio")
        .expect_measure("r2", "non-additive ratio")
        .expect_measure("r3", "non-additive ratio");
}

#[test]
fn conversions_stay_additive() {
    shop()
        .sql("select order_id, amount / fx_rate as usd, cents / 100 as dollars from orders")
        .expect_measure("usd", "additive")
        .expect_measure("dollars", "additive");
    // Dividing one aggregate by a constant or a picked value converts it.
    shop()
        .sql(
            "select customer_id, sum(cents) / 100 as dollars, sum(amount) / max(fx_rate) as usd
              from orders group by customer_id",
        )
        .expect_measure("dollars", "additive")
        .expect_measure("usd", "additive");
}

#[test]
fn ratios_of_upstream_aggregates_cross_models() {
    shop()
        .model("daily", DAILY)
        .sql("select order_date, revenue / n as aov from daily")
        .expect_measure("aov", "non-additive ratio");
    shop()
        .model(
            "daily",
            "select order_date, coalesce(sum(amount), 0) as revenue, count(*) as n
              from orders group by order_date",
        )
        .sql("select order_date, revenue / n as aov from daily")
        .expect_measure("aov", "non-additive ratio");
}

#[test]
fn running_totals_are_stocks_over_their_order() {
    shop()
        .sql(
            "select order_id, order_date,
                    sum(amount) over (partition by customer_id order by order_date) as running,
                    sum(amount) over (order by order_date rows between 6 preceding and current row) as rolling,
                    sum(amount) over (partition by customer_id) as customer_total,
                    avg(amount) over (partition by customer_id) as customer_avg
              from orders",
        )
        .expect_measure("running", "stock running total over (order_date)")
        .expect_measure("rolling", "stock running total over (order_date)")
        .expect_measure("customer_total", "additive")
        .expect_measure("customer_avg", "non-additive average");
}

#[test]
fn declared_stocks_carry_through_projections_and_joins() {
    shop()
        .sql("select account_id, snapshot_date, balance from balances")
        .expect_measure("balance", "stock declared over (snapshot_date)");
    shop()
        .sql(
            "select b.snapshot_date, a.owner, b.balance
              from balances b join accounts a on b.account_id = a.account_id",
        )
        .expect_measure("balance", "stock declared over (snapshot_date)");
    // Dropping the date when nothing kept determines it keeps the fact with its date
    // unknown: the rows still come from different dates, so a later sum is still wrong.
    shop()
        .sql("select account_id, balance from balances")
        .expect_measure("balance", "stock declared over unknown");
    shop()
        .model("acct", "select account_id, balance from balances")
        .sql("select balance from acct")
        .expect_measure("balance", "stock declared over unknown");
    // One date: every row agrees, so there is nothing to protect.
    shop()
        .sql("select account_id, balance from balances where snapshot_date = 20260101")
        .expect_measure("balance", "additive");
}

#[test]
fn unions_and_expressions_drop_measures() {
    shop()
        .sql(
            "select snapshot_date, balance from balances
              union all select snapshot_date, balance from balances",
        )
        .expect_measure("balance", "additive");
    shop()
        .sql("select snapshot_date, balance * 2 as b2 from balances")
        .expect_measure("b2", "additive");
    shop()
        .sql(
            "select customer_id, a from (select customer_id, avg(amount) as a from orders group by customer_id) t
              union all select customer_id, amount from orders",
        )
        .expect_measure("a", "additive");
}

#[test]
fn per_date_totals_of_a_stock_stay_stocks() {
    shop()
        .sql("select snapshot_date, sum(balance) as total from balances group by snapshot_date")
        .expect_measure("total", "stock declared over (snapshot_date)");
    // Summed across dates: G008 reports it, and the result is no longer a balance.
    shop()
        .sql("select account_id, sum(balance) as total from balances group by account_id")
        .expect_measure("total", "additive");
}

#[test]
fn measures_cross_model_boundaries() {
    shop()
        .model(
            "aov",
            "select customer_id, avg(amount) as aov from orders group by customer_id",
        )
        .sql("select c.region, a.aov from aov a join customers c on a.customer_id = c.customer_id")
        .expect_measure("aov", "non-additive average");
    shop()
        .model(
            "running",
            "select order_id, order_date, sum(amount) over (order by order_date) as running from orders",
        )
        .sql("select order_date, running from running")
        .expect_measure("running", "stock running total over (order_date)");
    shop()
        .model(
            "snap",
            "select account_id, snapshot_date, balance from balances",
        )
        .sql("select snapshot_date, balance from snap")
        .expect_measure("balance", "stock declared over (snapshot_date)");
}

#[test]
fn units_follow_their_unit_column() {
    shop()
        .sql("select amount, currency from orders")
        .expect_unit("amount", Some(Some(&["currency"])));
    // The key determines the currency, so it stands in for it.
    shop()
        .sql("select order_id, amount from orders")
        .expect_unit("amount", Some(Some(&["order_id"])));
    // Units vary and nothing says which.
    shop()
        .sql("select customer_id, amount from orders")
        .expect_unit("amount", Some(None));
    // Filtered to one currency: every row has the same unit.
    shop()
        .sql("select customer_id, amount from orders where currency = 1")
        .expect_unit("amount", None);
    // A conversion is a different value.
    shop()
        .sql("select currency, amount * fx_rate as usd from orders")
        .expect_unit("usd", None);
}

#[test]
fn aggregates_keep_units_per_unit_group() {
    shop()
        .sql("select currency, sum(amount) as total from orders group by currency")
        .expect_unit("total", Some(Some(&["currency"])));
    shop()
        .sql(
            "select order_date, currency, sum(amount) as total, max(amount) as biggest
              from orders group by order_date, currency",
        )
        .expect_unit("total", Some(Some(&["currency"])))
        .expect_unit("biggest", Some(Some(&["currency"])));
    // Mixed: G009 reports it, and the total carries no unit.
    shop()
        .sql("select order_date, sum(amount) as total from orders group by order_date")
        .expect_unit("total", None);
    shop()
        .sql(
            "select order_date, sum(amount) as total from orders where currency = 1 group by order_date",
        )
        .expect_unit("total", None);
}

#[test]
fn units_cross_model_boundaries() {
    shop()
        .model(
            "o",
            "select order_id, customer_id, amount, currency from orders",
        )
        .sql("select customer_id, amount, currency from o")
        .expect_unit("amount", Some(Some(&["currency"])));
    shop()
        .model("o", "select customer_id, amount from orders")
        .sql("select customer_id, amount from o")
        .expect_unit("amount", Some(None));
    shop()
        .model(
            "t",
            "select currency, sum(amount) as total from orders group by currency",
        )
        .sql("select total, currency from t")
        .expect_unit("total", Some(Some(&["currency"])));
}

#[test]
fn summary_round_trips_through_json() {
    let m = cardinal_grain::SummaryMeasure {
        col: 2,
        kind: cardinal_grain::MeasureKind::Stock {
            over: Some([1].into_iter().collect()),
        },
        site: cardinal_grain::MeasureSite::Computed {
            relation: cardinal_dbt::RelationId("model.test.r".into()),
            span: None,
            what: cardinal_grain::Computed::RunningTotal,
            over: vec!["order_date".into()],
        },
    };
    let back: cardinal_grain::SummaryMeasure =
        serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back, m);
    let u = cardinal_grain::SummaryUnit {
        col: 0,
        unit: None,
        site: cardinal_grain::UnitSite {
            relation: cardinal_dbt::RelationId("source.test.orders".into()),
            column: "amount".into(),
            unit: "currency".into(),
        },
    };
    let back: cardinal_grain::SummaryUnit =
        serde_json::from_str(&serde_json::to_string(&u).unwrap()).unwrap();
    assert_eq!(back, u);
}
